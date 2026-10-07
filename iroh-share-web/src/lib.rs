//! Browser interface for the iroh-share daemon: an iroh endpoint compiled to
//! WebAssembly that speaks the control protocol from `iroh-share-proto`.
//!
//! Browsers cannot open UDP sockets, so the endpoint is relay-only (the `N0`
//! preset). All interface logic lives in Rust: [`app`] is the state machine
//! (ported from the desktop GUI), [`view`] renders it into one object per
//! frame, and `net` talks to the daemon. The page (`web/main.js`) only turns
//! the view into DOM, forwards user intents, and performs browser-only effects
//! such as copying to the clipboard and saving files.

pub mod app;
mod net;
pub mod view;

use std::{cell::RefCell, rc::Rc};

use app::{Command, Effect, Intent, Persisted, State, Update};
use iroh::{endpoint::presets, Endpoint, SecretKey};
use iroh_share_proto::{ControlProtocol, CONTROL_ALPN};
use irpc::Client;
use tracing::level_filters::LevelFilter;
use tracing_subscriber_wasm::MakeConsoleWriter;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

const KEY_SECRET: &str = "iroh-share-web:secret";
const KEY_DAEMONS: &str = "iroh-share-web:daemons";

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .with_writer(MakeConsoleWriter::default().map_trace_level_to(tracing::Level::DEBUG))
        .without_time()
        .with_ansi(false)
        .init();
}

/// The running interface. Created once per page.
#[wasm_bindgen]
pub struct App {
    inner: Rc<Inner>,
}

struct Inner {
    state: RefCell<State>,
    endpoint: Endpoint,
    session: RefCell<Option<Session>>,
    render: js_sys::Function,
    effect: js_sys::Function,
}

/// The connection to the current daemon; dropping it stops the watch loop.
struct Session {
    client: Client<ControlProtocol>,
    _cancel: tokio::sync::oneshot::Sender<()>,
}

#[wasm_bindgen]
impl App {
    /// Start the endpoint and the interface. `render(view)` receives the whole
    /// page after every change; `effect(effect)` receives browser-only effects:
    /// `{ type: "saveFile", name, bytes }` and `{ type: "focus", key, cursorEnd }`.
    pub async fn start(render: js_sys::Function, effect: js_sys::Function) -> Result<App, JsError> {
        let storage = storage();
        let key = match storage_get(&storage, KEY_SECRET) {
            Some(hex) => hex.parse::<SecretKey>().map_err(js_err)?,
            None => SecretKey::generate(),
        };
        storage_set(&storage, KEY_SECRET, &hex(&key.to_bytes()));
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(key)
            .bind()
            .await
            .map_err(js_err)?;
        let persisted: Persisted = storage_get(&storage, KEY_DAEMONS)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let state = State::new(persisted, endpoint.id().to_string(), take_fragment_ticket());
        let inner = Rc::new(Inner {
            state: RefCell::new(state),
            endpoint,
            session: RefCell::new(None),
            render,
            effect,
        });
        flush(&inner);
        Ok(App { inner })
    }

    /// Handle a user intent, e.g. `{ type: "setPage", page: "settings" }`.
    /// See [`app::Intent`] for all of them.
    pub fn dispatch(&self, intent: JsValue) -> Result<(), JsError> {
        let text = js_sys::JSON::stringify(&intent)
            .ok()
            .and_then(|s| s.as_string())
            .ok_or_else(|| JsError::new("intent is not serializable"))?;
        let intent: Intent = serde_json::from_str(&text).map_err(js_err)?;
        self.inner
            .state
            .borrow_mut()
            .dispatch(intent, js_sys::Date::now());
        flush(&self.inner);
        Ok(())
    }

    /// Restore names from a ZIP archive the page read from this computer.
    pub fn import_names(&self, archive: Vec<u8>) {
        self.inner.state.borrow_mut().import_names(archive);
        flush(&self.inner);
    }
}

/// Apply a network result unless it belongs to an older session.
fn apply(inner: &Rc<Inner>, generation: Option<u64>, update: Update) {
    {
        let mut state = inner.state.borrow_mut();
        if generation.is_some_and(|g| g != state.generation) {
            return;
        }
        state.update(update);
    }
    flush(inner);
}

/// Run queued commands, save changed settings, render, then run effects.
/// No borrow of the state is held while calling into JS.
fn flush(inner: &Rc<Inner>) {
    let (commands, effects, persisted, view, generation) = {
        let mut state = inner.state.borrow_mut();
        (
            state.take_commands(),
            state.take_effects(),
            state.take_persist(),
            state.view(),
            state.generation,
        )
    };
    if let Some(persisted) = persisted {
        if let Ok(text) = serde_json::to_string(&persisted) {
            storage_set(&storage(), KEY_DAEMONS, &text);
        }
    }
    for command in commands {
        run(inner, command, generation);
    }
    let _ = inner.render.call1(&JsValue::NULL, &to_js(&view));
    for effect in effects {
        let object = js_sys::Object::new();
        let set = |key: &str, value: JsValue| {
            let _ = js_sys::Reflect::set(&object, &key.into(), &value);
        };
        match effect {
            Effect::SaveFile { name, bytes } => {
                set("type", "saveFile".into());
                set("name", name.into());
                set("bytes", js_sys::Uint8Array::from(bytes.as_slice()).into());
            }
            Effect::Focus { key, cursor_end } => {
                set("type", "focus".into());
                set("key", key.into());
                set("cursorEnd", cursor_end.into());
            }
        }
        let _ = inner.effect.call1(&JsValue::NULL, &object);
    }
}

fn run(inner: &Rc<Inner>, command: Command, generation: u64) {
    match command {
        Command::Pair(ticket) => {
            let inner = inner.clone();
            let endpoint = inner.endpoint.clone();
            spawn_local(async move {
                let update = net::pair(endpoint, ticket).await;
                apply(&inner, None, update);
            });
        }
        Command::Connect(addr) => {
            let client: Client<ControlProtocol> =
                irpc_iroh::client(inner.endpoint.clone(), addr, CONTROL_ALPN);
            let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
            // Replacing the session drops its cancel sender, stopping its watch loop.
            *inner.session.borrow_mut() = Some(Session {
                client: client.clone(),
                _cancel: cancel,
            });
            let watcher = inner.clone();
            spawn_local(async move {
                let emit = |update| apply(&watcher, Some(generation), update);
                tokio::select! {
                    _ = cancelled => {}
                    _ = net::watch(client, emit) => {}
                }
            });
        }
        Command::Disconnect => *inner.session.borrow_mut() = None,
        Command::Rpc(action) => {
            let client = inner.session.borrow().as_ref().map(|s| s.client.clone());
            let inner = inner.clone();
            spawn_local(async move {
                let update = match client {
                    Some(client) => net::execute(client, action).await,
                    None => Update::ActionResult(Err("Not connected".into())),
                };
                apply(&inner, Some(generation), update);
            });
        }
    }
}

/// A pairing ticket may arrive in the fragment (`#ticket=…`), which is never sent
/// to a server. Take it and remove it from the address bar right away.
fn take_fragment_ticket() -> Option<String> {
    let window = web_sys::window()?;
    let location = window.location();
    let hash = location.hash().ok()?;
    let ticket = hash
        .trim_start_matches('#')
        .split('&')
        .find_map(|pair| pair.strip_prefix("ticket="))
        .filter(|t| !t.is_empty())?
        .to_owned();
    let path = location.pathname().unwrap_or_default() + &location.search().unwrap_or_default();
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&path));
    }
    Some(ticket)
}

/// Local storage can be unavailable (private windows, blocked site data); the
/// interface then works without remembering anything.
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn storage_get(storage: &Option<web_sys::Storage>, key: &str) -> Option<String> {
    storage.as_ref()?.get_item(key).ok()?
}

fn storage_set(storage: &Option<web_sys::Storage>, key: &str, value: &str) {
    if let Some(storage) = storage {
        let _ = storage.set_item(key, value);
    }
}

fn to_js(value: &serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL)
}

fn js_err(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
