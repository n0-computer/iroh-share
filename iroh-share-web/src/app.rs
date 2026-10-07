//! The interface state machine, ported from the desktop GUI's `App`.
//!
//! Everything here is plain Rust without browser APIs, so it is unit-tested
//! natively. The page sends [`Intent`]s, the network side feeds [`Update`]s, and
//! the state queues [`Command`]s for the network side and [`Effect`]s for the
//! page. [`State::view`](crate::view) renders the whole page as one object.

use std::{collections::BTreeMap, path::PathBuf};

use iroh::{EndpointAddr, EndpointId};
use iroh_share_proto::{
    BlobTicket, DiscoveryMode, DownloadSource, Job, JobState, Name, NameTarget, PairingTicket,
    PathCompletions, PathKind, WatchEvent,
};
use serde::{Deserialize, Serialize};

/// Ticket files larger than this are rejected without reading them.
pub const MAX_TICKET_FILE: usize = 16_384;
/// After the tab was hidden this long, start a fresh session on return.
const RECONNECT_AFTER_MS: f64 = 5_000.0;
const DEFAULT_RECORDS_URL: &str = "https://example.com/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Page {
    Data,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddMode {
    Share,
    Import,
    Download,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Removal {
    Data(u64),
    Name(String),
    Daemon(EndpointId),
}

/// Inline name editor. Data targets are chosen by path, never by typed ID.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    pub label: String,
    /// Which table the editor is shown in.
    pub content: bool,
    pub target: EditorTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditorTarget {
    Records(String),
    Job(Option<u64>),
}

/// A daemon this browser has paired with, saved in local storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDaemon {
    #[serde(with = "addr_ticket")]
    pub addr: EndpointAddr,
    /// Optional label chosen by the user; falls back to the short endpoint ID.
    #[serde(default)]
    pub name: Option<String>,
}

impl SavedDaemon {
    pub fn id(&self) -> EndpointId {
        self.addr.id
    }
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => self.addr.id.fmt_short().to_string(),
        }
    }
}

/// What the browser persists: the daemon list and the one used most recently.
/// Pairing tickets and daemon-owned data are never stored.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Persisted {
    #[serde(default)]
    pub daemons: Vec<SavedDaemon>,
    #[serde(default)]
    pub current: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextField {
    PairTicket,
    PairName,
    AddPath,
    AddTicket,
    AddSource,
    NewRecords,
    EditorText,
    DaemonName,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlagField {
    IncludeDirectoryName,
    Discover,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DroppedFile {
    pub name: String,
    #[serde(default)]
    pub mime: String,
    /// The contents, or `None` when the page did not read it (too large).
    #[serde(default)]
    pub text: Option<String>,
}

/// Everything the page can ask for.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Intent {
    SetText {
        field: TextField,
        value: String,
    },
    SetFlag {
        field: FlagField,
        value: bool,
    },
    SetMode {
        mode: AddMode,
    },
    SetPage {
        page: Page,
    },
    SetEditorJob {
        id: Option<u64>,
    },
    Pair,
    CancelPairing,
    AddDaemon,
    SwitchDaemon {
        id: String,
    },
    RenameDaemon,
    ForgetDaemon,
    Select {
        id: u64,
    },
    AddName {
        id: u64,
    },
    UpdateData {
        id: u64,
    },
    RemoveData {
        id: u64,
    },
    RemoveName {
        label: String,
    },
    EditName {
        label: String,
        content: bool,
    },
    CancelEdit,
    SaveName,
    CreateRecordsName,
    ExportName {
        label: String,
    },
    ExportAll,
    SubmitAdd,
    CancelUpdate,
    Complete,
    DismissCandidates,
    ChooseCandidate {
        path: String,
    },
    Confirm,
    CancelConfirm,
    Drop {
        files: Vec<DroppedFile>,
        #[serde(default)]
        text: Option<String>,
    },
    Visibility {
        hidden: bool,
    },
}

/// Work for the network side.
#[derive(Debug)]
pub enum Command {
    Pair(PairingTicket),
    /// Start watching this daemon; replaces any previous session.
    Connect(EndpointAddr),
    Disconnect,
    Rpc(Action),
}

#[derive(Debug)]
pub enum Action {
    CompletePath {
        id: u64,
        path: PathBuf,
    },
    Share {
        path: PathBuf,
        include_directory_name: bool,
    },
    Import {
        source: DownloadSource,
        id: Option<u64>,
    },
    Download {
        source: DownloadSource,
        target: PathBuf,
    },
    Remove(u64),
    CreateName {
        label: String,
        target: NameTarget,
    },
    UpdateName {
        label: String,
        target: NameTarget,
    },
    RemoveName(String),
    ExportNames,
    ExportName {
        label: String,
        file_name: String,
    },
    ImportNames(Vec<u8>),
}

/// Results from the network side.
#[derive(Debug)]
pub enum Update {
    Connecting,
    Disconnected(String),
    Event(WatchEvent),
    ImportDirectory(PathBuf),
    Paired(Result<EndpointAddr, String>),
    Completion {
        id: u64,
        result: Result<PathCompletions, String>,
    },
    NameSaved(Result<Name, String>),
    Published(Result<Job, String>),
    Imported(Result<Job, String>),
    Downloaded(Result<Job, String>),
    ActionResult(Result<String, String>),
    /// A ZIP to save on this computer, with its suggested file name.
    Exported(Result<(String, Vec<u8>), String>),
}

/// Browser-only side effects for the page.
#[derive(Debug, PartialEq)]
pub enum Effect {
    SaveFile {
        name: String,
        bytes: Vec<u8>,
    },
    /// Focus an input, optionally moving the cursor to the end.
    Focus {
        key: &'static str,
        cursor_end: bool,
    },
}

pub struct State {
    pub daemons: Vec<SavedDaemon>,
    pub current: Option<EndpointId>,
    pub client_id: String,
    pub pairing: bool,
    pub pair_ticket: String,
    pub pair_name: String,
    /// Whether a watch session runs, even while the pairing screen is shown.
    pub session_active: bool,
    /// Bumped whenever the session changes; replies tagged with an older
    /// generation belong to a previous daemon and are dropped.
    pub generation: u64,
    pub ready: bool,
    pub busy: bool,
    pub status: String,
    pub page: Page,
    pub jobs: BTreeMap<u64, Job>,
    pub names: BTreeMap<String, Name>,
    pub selected: Option<u64>,
    pub removal: Option<Removal>,
    pub import_directory: Option<PathBuf>,
    pub add_mode: AddMode,
    pub add_path: String,
    pub add_ticket: String,
    pub add_source: String,
    pub include_directory_name: bool,
    pub discover: bool,
    /// Data being updated from a ticket.
    pub import_id: Option<u64>,
    pub editor: Option<Editor>,
    pub new_records: String,
    pub completion_id: u64,
    pub candidates: Vec<PathBuf>,
    pub daemon_name: String,
    hidden_at: Option<f64>,
    persist: bool,
    commands: Vec<Command>,
    effects: Vec<Effect>,
}

impl State {
    /// `ticket` is a pairing ticket from the page address, to review before use.
    pub fn new(persisted: Persisted, client_id: String, ticket: Option<String>) -> Self {
        let current = persisted
            .current
            .and_then(|id| id.parse::<EndpointId>().ok())
            .filter(|id| persisted.daemons.iter().any(|d| d.id() == *id))
            .or_else(|| persisted.daemons.first().map(SavedDaemon::id));
        let mut state = Self {
            daemons: persisted.daemons,
            current,
            client_id,
            pairing: false,
            pair_ticket: String::new(),
            pair_name: String::new(),
            session_active: false,
            generation: 0,
            ready: false,
            busy: false,
            status: String::new(),
            page: Page::Data,
            jobs: BTreeMap::new(),
            names: BTreeMap::new(),
            selected: None,
            removal: None,
            import_directory: None,
            add_mode: AddMode::Share,
            add_path: String::new(),
            add_ticket: String::new(),
            add_source: String::new(),
            include_directory_name: false,
            discover: false,
            import_id: None,
            editor: None,
            new_records: default_records(),
            completion_id: 0,
            candidates: Vec::new(),
            daemon_name: String::new(),
            hidden_at: None,
            persist: false,
            commands: Vec::new(),
            effects: Vec::new(),
        };
        state.load_daemon_name();
        match ticket {
            Some(ticket) => {
                state.pairing = true;
                state.pair_ticket = ticket;
                state.status = "Review the ticket and connect.".into();
            }
            None if state.current.is_none() => state.pairing = true,
            None => state.connect(),
        }
        state
    }

    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The daemon list to save, if it changed.
    pub fn take_persist(&mut self) -> Option<Persisted> {
        std::mem::take(&mut self.persist).then(|| Persisted {
            daemons: self.daemons.clone(),
            current: self.current.map(|id| id.to_string()),
        })
    }

    pub fn current_daemon(&self) -> Option<&SavedDaemon> {
        self.daemons.iter().find(|d| Some(d.id()) == self.current)
    }

    pub fn can_act(&self) -> bool {
        self.ready && !self.busy
    }

    /// Handle one page intent. `now` is a millisecond clock.
    pub fn dispatch(&mut self, intent: Intent, now: f64) {
        match intent {
            Intent::SetText { field, value } => match field {
                TextField::PairTicket => self.pair_ticket = value,
                TextField::PairName => self.pair_name = value,
                TextField::AddPath => {
                    self.add_path = value;
                    self.dismiss_candidates();
                }
                TextField::AddTicket => self.add_ticket = value,
                TextField::AddSource => self.add_source = value,
                TextField::NewRecords => self.new_records = value,
                TextField::EditorText => {
                    if let Some(Editor {
                        target: EditorTarget::Records(text),
                        ..
                    }) = &mut self.editor
                    {
                        *text = value;
                    }
                }
                TextField::DaemonName => self.daemon_name = value,
            },
            Intent::SetFlag { field, value } => match field {
                FlagField::IncludeDirectoryName => self.include_directory_name = value,
                FlagField::Discover => self.discover = value,
            },
            Intent::SetMode { mode } => {
                self.add_mode = mode;
                self.import_id = None;
                self.dismiss_candidates();
            }
            Intent::SetPage { page } => {
                self.page = page;
                self.editor = None;
            }
            Intent::SetEditorJob { id } => {
                if let Some(Editor {
                    target: EditorTarget::Job(job),
                    ..
                }) = &mut self.editor
                {
                    *job = id;
                }
            }
            Intent::Pair => self.pair(),
            Intent::CancelPairing => {
                if self.current.is_some() {
                    self.pair_ticket.clear();
                    self.pair_name.clear();
                    self.pairing = false;
                    if !self.session_active {
                        self.connect();
                    }
                }
            }
            Intent::AddDaemon => {
                self.pair_ticket.clear();
                self.pair_name.clear();
                self.pairing = true;
            }
            Intent::SwitchDaemon { id } => match id.parse::<EndpointId>() {
                Ok(id) if Some(id) != self.current && self.daemons.iter().any(|d| d.id() == id) => {
                    self.switched(Some(id));
                }
                Ok(_) => {}
                Err(e) => self.status = format!("Invalid daemon: {e}"),
            },
            Intent::RenameDaemon => {
                let name = Some(self.daemon_name.trim().to_owned()).filter(|n| !n.is_empty());
                if let Some(daemon) = self
                    .daemons
                    .iter_mut()
                    .find(|d| Some(d.id()) == self.current)
                {
                    daemon.name = name;
                    self.persist = true;
                }
                self.load_daemon_name();
            }
            Intent::ForgetDaemon => {
                if let Some(id) = self.current {
                    self.removal = Some(Removal::Daemon(id));
                }
            }
            Intent::Select { id } => self.selected = Some(id),
            Intent::AddName { id } => {
                let target = NameTarget::Job(id);
                let label = self.automatic_name_label(&target);
                self.send(Action::CreateName { label, target });
            }
            Intent::UpdateData { id } => {
                if self.can_act() && self.jobs.get(&id).is_some_and(updatable) {
                    self.import_id = Some(id);
                    self.add_mode = AddMode::Import;
                    self.add_ticket.clear();
                    self.effects.push(Effect::Focus {
                        key: "add-ticket",
                        cursor_end: false,
                    });
                }
            }
            Intent::RemoveData { id } => {
                if self.can_act() {
                    self.removal = Some(Removal::Data(id));
                }
            }
            Intent::RemoveName { label } => {
                if self.can_act() {
                    self.removal = Some(Removal::Name(label));
                }
            }
            Intent::EditName { label, content } => {
                if let Some(name) = self.names.get(&label).filter(|_| self.can_act()) {
                    let target = match &name.target {
                        NameTarget::Job(id) => EditorTarget::Job(Some(*id)),
                        NameTarget::Url(url) => {
                            EditorTarget::Records(iroh_share_proto::redirect_records(url))
                        }
                        NameTarget::Records(text) => EditorTarget::Records(text.clone()),
                    };
                    self.editor = Some(Editor {
                        label,
                        content,
                        target,
                    });
                }
            }
            Intent::CancelEdit => self.editor = None,
            Intent::SaveName => match self.editor_target() {
                Ok((label, target)) => self.send(Action::UpdateName { label, target }),
                Err(e) => self.status = e,
            },
            Intent::CreateRecordsName => {
                let records = self.new_records.trim();
                if self.editor.is_none() && !records.is_empty() {
                    let target = NameTarget::Records(records.to_owned());
                    let label = self.automatic_name_label(&target);
                    self.send(Action::CreateName { label, target });
                }
            }
            Intent::ExportName { label } => {
                if let Some(name) = self.names.get(&label) {
                    let file_name = format!("{}.zip", name.key);
                    self.send(Action::ExportName { label, file_name });
                }
            }
            Intent::ExportAll => self.send(Action::ExportNames),
            Intent::SubmitAdd => {
                if let Ok(action) = self.add_action() {
                    self.send(action);
                }
            }
            Intent::CancelUpdate => {
                self.import_id = None;
                self.add_ticket.clear();
            }
            Intent::Complete => self.complete(),
            Intent::DismissCandidates => self.dismiss_candidates(),
            Intent::ChooseCandidate { path } => {
                self.add_path = path;
                self.dismiss_candidates();
                self.effects.push(Effect::Focus {
                    key: "add-path",
                    cursor_end: true,
                });
            }
            Intent::Confirm => self.confirm(),
            Intent::CancelConfirm => self.removal = None,
            Intent::Drop { files, text } => self.handle_drop(files, text),
            Intent::Visibility { hidden } => {
                if hidden {
                    self.hidden_at = Some(now);
                } else if let Some(at) = self.hidden_at.take() {
                    // A frozen tab leaves the relay connection stale and iroh would
                    // wait out its reconnect backoff; a fresh session is faster.
                    if self.session_active && now - at > RECONNECT_AFTER_MS {
                        self.connect();
                    }
                }
            }
        }
    }

    /// Restore names from a ZIP the page read from this computer.
    pub fn import_names(&mut self, archive: Vec<u8>) {
        self.send(Action::ImportNames(archive));
    }

    /// Apply a network result. The caller drops results from older generations.
    pub fn update(&mut self, update: Update) {
        match update {
            Update::Connecting => {
                self.clear_session();
                self.status = "Connecting…".into();
            }
            Update::Disconnected(error) => {
                self.clear_session();
                self.status = format!("Disconnected: {error}. Retrying…");
            }
            Update::Event(event) => match event {
                WatchEvent::JobUpdated(job) => {
                    self.jobs.insert(job.id, *job);
                }
                WatchEvent::JobRemoved { id } => {
                    self.jobs.remove(&id);
                    if self.selected == Some(id) {
                        self.selected = None;
                    }
                }
                WatchEvent::NameUpdated(name) => {
                    self.names.insert(name.label.clone(), *name);
                }
                WatchEvent::NameRemoved { label } => {
                    self.names.remove(&label);
                }
                WatchEvent::SnapshotComplete => {
                    self.ready = true;
                    self.status = "Connected".into();
                }
                // Only sent for clients from 0.1.7 and earlier.
                WatchEvent::GatewayUpdated(_) => {}
            },
            Update::ImportDirectory(path) => self.import_directory = Some(path),
            Update::Paired(result) => {
                self.busy = false;
                match result {
                    Ok(addr) => {
                        let id = addr.id;
                        let name = Some(self.pair_name.trim().to_owned()).filter(|n| !n.is_empty());
                        match self.daemons.iter_mut().find(|d| d.id() == id) {
                            Some(daemon) => {
                                daemon.addr = addr;
                                daemon.name = name.or(daemon.name.take());
                            }
                            None => self.daemons.push(SavedDaemon { addr, name }),
                        }
                        self.pair_ticket.clear();
                        self.pair_name.clear();
                        self.switched(Some(id));
                    }
                    Err(error) => self.status = error,
                }
            }
            Update::Completion { id, result } => {
                if id != self.completion_id {
                    return;
                }
                match result {
                    Ok(paths) => {
                        let directories_only = self.add_mode == AddMode::Download;
                        self.candidates = paths
                            .candidates
                            .into_iter()
                            .filter(|p| !directories_only || p.kind == PathKind::Directory)
                            .map(|p| p.path)
                            .collect();
                        let prefix = if self.candidates.len() == 1 {
                            self.candidates[0].to_string_lossy().into_owned()
                        } else {
                            paths.common_prefix.to_string_lossy().into_owned()
                        };
                        if !prefix.is_empty() && !self.candidates.is_empty() {
                            self.add_path = prefix;
                            self.effects.push(Effect::Focus {
                                key: "add-path",
                                cursor_end: true,
                            });
                        }
                        if self.candidates.is_empty() {
                            self.status = "No matching paths".into();
                        }
                        if paths.truncated {
                            self.status = "More paths available; refine the prefix".into();
                        }
                    }
                    Err(error) => self.status = error,
                }
            }
            Update::NameSaved(result) => {
                self.busy = false;
                match result {
                    Ok(name) => {
                        if self.editor.is_none() {
                            self.new_records = default_records();
                        }
                        self.status = format!(
                            "Name saved: {}. Publication status is shown with the name.",
                            name.key.url()
                        );
                        self.names.insert(name.label.clone(), name);
                        self.editor = None;
                    }
                    Err(error) => self.status = format!("Could not save name: {error}"),
                }
            }
            Update::Published(result) => self.added(result, |state| {
                state.add_path.clear();
                state.dismiss_candidates();
                "Content added. Links and the ticket appear when it is ready."
            }),
            Update::Imported(result) => self.added(result, |state| {
                state.import_id = None;
                state.add_ticket.clear();
                "Import started. Linked names will follow when the content is ready."
            }),
            Update::Downloaded(result) => self.added(result, |state| {
                state.add_source.clear();
                "Download added. Its link and ticket appear when it is ready."
            }),
            Update::ActionResult(result) => {
                self.busy = false;
                self.status = result.unwrap_or_else(|e| e);
            }
            Update::Exported(result) => {
                self.busy = false;
                match result {
                    Ok((name, bytes)) => {
                        self.status = format!(
                            "Saved {name}. It contains private signing keys; keep it safe."
                        );
                        self.effects.push(Effect::SaveFile { name, bytes });
                    }
                    Err(error) => self.status = error,
                }
            }
        }
    }

    /// Shared handling for requests that create data. Input is kept on failure.
    fn added(&mut self, result: Result<Job, String>, ok: impl FnOnce(&mut Self) -> &'static str) {
        self.busy = false;
        match result {
            Ok(job) => {
                self.status = ok(self).into();
                self.selected = Some(job.id);
                self.page = Page::Data;
                // Watch may already have delivered a newer state.
                self.jobs.entry(job.id).or_insert(job);
            }
            Err(error) => self.status = error,
        }
    }

    fn send(&mut self, action: Action) {
        if !self.can_act() {
            return;
        }
        self.busy = true;
        self.status = "Working…".into();
        self.commands.push(Command::Rpc(action));
    }

    fn pair(&mut self) {
        if self.busy {
            return;
        }
        match self.pair_ticket.trim().parse::<PairingTicket>() {
            Ok(ticket) => {
                self.busy = true;
                self.status = "Pairing…".into();
                self.commands.push(Command::Pair(ticket));
            }
            Err(e) => self.status = format!("Invalid pairing ticket: {e}"),
        }
    }

    fn connect(&mut self) {
        let Some(addr) = self.current_daemon().map(|d| d.addr.clone()) else {
            return;
        };
        self.generation += 1;
        self.clear_session();
        self.pairing = false;
        self.session_active = true;
        self.status = "Connecting…".into();
        self.commands.push(Command::Connect(addr));
    }

    /// Drops everything that belongs to the daemon session.
    fn clear_session(&mut self) {
        self.ready = false;
        self.busy = false;
        self.jobs.clear();
        self.names.clear();
        self.selected = None;
        self.import_id = None;
        self.import_directory = None;
        self.add_ticket.clear();
        self.editor = None;
        self.dismiss_candidates();
        // Forgetting must stay possible while a daemon is unreachable.
        if !matches!(self.removal, Some(Removal::Daemon(_))) {
            self.removal = None;
        }
    }

    /// Reconnects after the current daemon changed, or asks for a ticket if none is left.
    fn switched(&mut self, current: Option<EndpointId>) {
        self.current = current;
        self.persist = true;
        self.removal = None;
        self.page = Page::Data;
        self.load_daemon_name();
        if current.is_some() {
            self.connect();
        } else {
            self.generation += 1;
            self.clear_session();
            self.session_active = false;
            self.commands.push(Command::Disconnect);
            self.pairing = true;
            self.status = "Add a daemon with its pairing ticket.".into();
        }
    }

    fn load_daemon_name(&mut self) {
        self.daemon_name = self
            .current_daemon()
            .and_then(|d| d.name.clone())
            .unwrap_or_default();
    }

    fn confirm(&mut self) {
        let Some(removal) = self.removal.clone() else {
            return;
        };
        match removal {
            Removal::Daemon(id) => {
                self.daemons.retain(|d| d.id() != id);
                let next = if self.current == Some(id) {
                    self.daemons.first().map(SavedDaemon::id)
                } else {
                    self.current
                };
                self.switched(next);
            }
            _ if !self.can_act() => {}
            Removal::Data(id) => {
                self.removal = None;
                self.send(Action::Remove(id));
            }
            Removal::Name(label) => {
                self.removal = None;
                self.send(Action::RemoveName(label));
            }
        }
    }

    fn complete(&mut self) {
        if !self.session_active || self.add_mode == AddMode::Import {
            return;
        }
        self.dismiss_candidates();
        self.commands.push(Command::Rpc(Action::CompletePath {
            id: self.completion_id,
            path: self.add_path.clone().into(),
        }));
    }

    fn dismiss_candidates(&mut self) {
        self.completion_id += 1;
        self.candidates.clear();
    }

    fn editor_target(&self) -> Result<(String, NameTarget), String> {
        let editor = self.editor.as_ref().ok_or("No name is being edited")?;
        let target = match &editor.target {
            EditorTarget::Job(Some(id)) if self.jobs.contains_key(id) => NameTarget::Job(*id),
            EditorTarget::Job(_) => return Err("Select data to name".into()),
            EditorTarget::Records(text) if !text.trim().is_empty() => {
                NameTarget::Records(text.trim().to_owned())
            }
            EditorTarget::Records(_) => return Err("Enter DNS records".into()),
        };
        Ok((editor.label.clone(), target))
    }

    /// The add row's request, or why it cannot be sent yet.
    pub fn add_action(&self) -> Result<Action, String> {
        let path = self.add_path.trim();
        match self.add_mode {
            AddMode::Share => {
                if path.is_empty() {
                    return Err("Enter a path on the daemon".into());
                }
                Ok(Action::Share {
                    path: path.into(),
                    include_directory_name: self.include_directory_name,
                })
            }
            AddMode::Import => {
                if self.add_ticket.trim().is_empty() {
                    return Err("Paste a collection ticket".into());
                }
                let source = import_source(&self.add_ticket)?;
                if let Some(id) = self.import_id {
                    if !self.jobs.get(&id).is_some_and(updatable) {
                        return Err("Only idle or failed data can be updated".into());
                    }
                }
                Ok(Action::Import {
                    source,
                    id: self.import_id,
                })
            }
            AddMode::Download => {
                if path.is_empty() {
                    return Err("Enter a destination directory on the daemon".into());
                }
                if self.add_source.trim().is_empty() {
                    return Err("Enter a URL, hash, or ticket".into());
                }
                let mut source: DownloadSource =
                    self.add_source.trim().parse().map_err(|e| format!("{e}"))?;
                if self.discover {
                    source.discovery = DiscoveryMode::Mainline;
                }
                Ok(Action::Download {
                    source,
                    target: path.into(),
                })
            }
        }
    }

    /// Browsers expose dropped file contents, not daemon paths, so only ticket
    /// files (or dragged ticket text) are accepted. They open the import row for
    /// review; nothing starts until the user confirms.
    fn handle_drop(&mut self, files: Vec<DroppedFile>, text: Option<String>) {
        if self.pairing {
            return;
        }
        if !self.can_act() {
            self.status =
                "Drop ignored: wait until connected and the current request finishes.".into();
            return;
        }
        let text = if files.is_empty() {
            text.unwrap_or_default()
        } else {
            let Some(file) = files.iter().find(|f| {
                f.name.ends_with(".ticket") || f.name.ends_with(".sendme") || f.mime == "text/plain"
            }) else {
                self.status = "A browser cannot share local files or folders with the daemon. Paste a ticket or drop a .ticket file.".into();
                return;
            };
            if files.len() != 1 {
                self.status = "Drop one ticket at a time, separately from files to publish.".into();
                return;
            }
            match &file.text {
                Some(text) if text.len() <= MAX_TICKET_FILE => text.clone(),
                _ => {
                    self.status = "Cannot read ticket: Ticket file is too large".into();
                    return;
                }
            }
        };
        let text = text.trim().to_owned();
        if let Err(error) = import_source(&text) {
            self.status = format!("Cannot read ticket: {error}");
            return;
        }
        if self.import_id.is_none() {
            self.add_mode = AddMode::Import;
        }
        self.add_ticket = text;
        self.page = Page::Data;
    }

    /// Internal label from the path basename (or URL host), unique among names.
    pub fn automatic_name_label(&self, target: &NameTarget) -> String {
        let base = match target {
            NameTarget::Job(id) => self
                .jobs
                .get(id)
                .and_then(|job| job_path(job).file_name())
                .map(|name| name.to_string_lossy().into_owned()),
            NameTarget::Url(url) => url.host_str().map(str::to_owned),
            NameTarget::Records(_) => Some("dns".into()),
        }
        .unwrap_or_else(|| "name".into());
        let base: String = base.chars().filter(|c| !c.is_control()).take(24).collect();
        let base = if base.trim().is_empty() {
            "name"
        } else {
            base.trim()
        };
        let mut label = base.to_owned();
        let mut suffix = 2;
        while self.names.contains_key(&label) {
            label = format!("{base}-{suffix}");
            suffix += 1;
        }
        label
    }
}

/// Only idle or failed data accepts a ticket update.
pub fn updatable(job: &Job) -> bool {
    matches!(
        job.state,
        JobState::Seeding { .. } | JobState::Failed { .. }
    )
}

pub fn job_path(job: &Job) -> &PathBuf {
    match &job.kind {
        iroh_share_proto::JobKind::Share { path, .. } => path,
        iroh_share_proto::JobKind::Download { target, .. } => target,
    }
}

pub fn import_source(ticket: &str) -> Result<DownloadSource, String> {
    let ticket: BlobTicket = ticket
        .trim()
        .parse()
        .map_err(|_| "expected a collection ticket".to_owned())?;
    DownloadSource::try_from(ticket).map_err(|e| e.to_string())
}

fn default_records() -> String {
    iroh_share_proto::redirect_records(&DEFAULT_RECORDS_URL.parse().expect("valid URL"))
}

/// Saved daemon addresses use the endpoint ticket encoding.
mod addr_ticket {
    use iroh::EndpointAddr;
    use iroh_tickets::{endpoint::EndpointTicket, Ticket};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(addr: &EndpointAddr, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&EndpointTicket::new(addr.clone()).to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<EndpointAddr, D::Error> {
        let text = String::deserialize(d)?;
        EndpointTicket::decode_string(&text)
            .map(Into::into)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_blobs::BlobFormat;
    use iroh_share_proto::{Hash, JobKind, PathCandidate};

    fn key(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn daemon(seed: u8) -> SavedDaemon {
        SavedDaemon {
            addr: key(seed).into(),
            name: None,
        }
    }

    /// A connected state with one daemon, like the GUI tests' fixture.
    fn connected() -> State {
        let persisted = Persisted {
            daemons: vec![daemon(1)],
            current: None,
        };
        let mut state = State::new(persisted, "client".into(), None);
        state.take_commands();
        state.update(Update::Event(WatchEvent::SnapshotComplete));
        state
    }

    fn job(id: u64, path: &str) -> Job {
        Job {
            id,
            kind: JobKind::Share {
                path: path.into(),
                include_directory_name: false,
            },
            state: JobState::Queued,
        }
    }

    fn collection_ticket() -> String {
        BlobTicket::new(key(9).into(), Hash::new(b"collection"), BlobFormat::HashSeq).to_string()
    }

    #[test]
    fn starts_by_pairing_without_daemons_and_connects_to_saved_one() {
        let mut state = State::new(Persisted::default(), "client".into(), None);
        assert!(state.pairing);
        assert!(state.take_commands().is_empty());

        let persisted = Persisted {
            daemons: vec![daemon(1), daemon(2)],
            current: Some(key(2).to_string()),
        };
        let mut state = State::new(persisted, "client".into(), None);
        assert_eq!(state.current, Some(key(2)));
        assert!(matches!(&state.take_commands()[..], [Command::Connect(a)] if a.id == key(2)));
    }

    #[test]
    fn failed_name_save_preserves_draft_and_disconnect_clears_import_target() {
        let mut state = connected();
        state.names.insert(
            "website".into(),
            Name {
                label: "website".into(),
                key: iroh_share_proto::NameKey([3; 32]),
                target: NameTarget::Records("@ 300 IN A 1.2.3.4".into()),
                state: iroh_share_proto::NameState::NoRecords,
            },
        );
        state.dispatch(
            Intent::EditName {
                label: "website".into(),
                content: false,
            },
            0.0,
        );
        state.dispatch(Intent::SaveName, 0.0);
        assert!(state.busy);
        state.update(Update::NameSaved(Err("network error".into())));
        assert!(!state.busy);
        assert!(state.editor.is_some());
        assert!(state.status.contains("network error"));

        state.jobs.insert(3, job(3, "/data"));
        state.import_id = Some(3);
        state.add_ticket = "ticket".into();
        state.update(Update::Disconnected("gone".into()));
        assert!(state.import_id.is_none());
        assert!(state.add_ticket.is_empty());
        assert!(state.editor.is_none());
    }

    #[test]
    fn disconnect_invalidates_selection_and_confirmation_and_blocks_requests() {
        let mut state = connected();
        state.update(Update::Event(WatchEvent::JobUpdated(Box::new(job(
            3, "/data",
        )))));
        state.dispatch(Intent::Select { id: 3 }, 0.0);
        state.dispatch(Intent::RemoveData { id: 3 }, 0.0);
        assert_eq!(state.removal, Some(Removal::Data(3)));
        state.update(Update::Disconnected("lost connection".into()));
        assert!(!state.ready && state.jobs.is_empty());
        assert!(state.selected.is_none() && state.removal.is_none());
        state.dispatch(Intent::AddName { id: 3 }, 0.0);
        assert!(state.take_commands().is_empty());
    }

    #[test]
    fn stale_completion_is_ignored_and_download_keeps_only_directories() {
        let mut state = connected();
        state.dispatch(
            Intent::SetMode {
                mode: AddMode::Download,
            },
            0.0,
        );
        state.add_path = "/remote/d".into();
        state.dispatch(Intent::Complete, 0.0);
        let Some(Command::Rpc(Action::CompletePath { id, path })) = state.take_commands().pop()
        else {
            panic!("expected completion request");
        };
        assert_eq!(path, PathBuf::from("/remote/d"));
        let completions = || PathCompletions {
            common_prefix: "/remote/d".into(),
            truncated: false,
            candidates: vec![
                PathCandidate {
                    path: "/remote/data/".into(),
                    kind: PathKind::Directory,
                },
                PathCandidate {
                    path: "/remote/document.txt".into(),
                    kind: PathKind::File,
                },
            ],
        };
        state.update(Update::Completion {
            id: id - 1,
            result: Ok(completions()),
        });
        assert!(state.candidates.is_empty());
        state.update(Update::Completion {
            id,
            result: Ok(completions()),
        });
        assert_eq!(state.add_path, "/remote/data/");
        assert_eq!(state.candidates, vec![PathBuf::from("/remote/data/")]);
        // Editing invalidates outstanding completions.
        state.dispatch(
            Intent::SetText {
                field: TextField::AddPath,
                value: "/x".into(),
            },
            0.0,
        );
        assert!(state.candidates.is_empty());
    }

    #[test]
    fn ticket_drop_opens_review_without_starting_a_transfer() {
        let mut state = connected();
        let file = |name: &str, text: &str| DroppedFile {
            name: name.into(),
            mime: String::new(),
            text: Some(text.into()),
        };
        state.dispatch(
            Intent::Drop {
                files: vec![file("folder", "")],
                text: None,
            },
            0.0,
        );
        assert!(state.status.contains("cannot share local files"));
        // A bare hash is not a ticket.
        let hash = DownloadSource::from(Hash::new(b"x")).to_string();
        state.dispatch(
            Intent::Drop {
                files: vec![file("bad.ticket", &hash)],
                text: None,
            },
            0.0,
        );
        assert!(state.status.starts_with("Cannot read ticket"));
        assert_eq!(state.add_mode, AddMode::Share);

        let ticket = collection_ticket();
        state.dispatch(
            Intent::Drop {
                files: vec![file("valid.ticket", &format!("{ticket}\n"))],
                text: None,
            },
            0.0,
        );
        assert_eq!(state.add_mode, AddMode::Import);
        assert_eq!(state.add_ticket, ticket);
        assert!(state.take_commands().is_empty());
        assert!(matches!(
            state.add_action(),
            Ok(Action::Import { id: None, .. })
        ));
    }

    #[test]
    fn daemons_switch_and_pending_forget_survives_reconnects() {
        let persisted = Persisted {
            daemons: vec![daemon(1), daemon(2)],
            current: Some(key(1).to_string()),
        };
        let mut state = State::new(persisted, "client".into(), None);
        let first = state.generation;
        state.take_commands();
        state.jobs.insert(7, job(7, "/x"));
        state.dispatch(
            Intent::SwitchDaemon {
                id: key(2).to_string(),
            },
            0.0,
        );
        assert_eq!(state.current, Some(key(2)));
        assert!(state.generation > first);
        assert!(state.jobs.is_empty() && !state.ready);
        assert!(state.take_persist().is_some());

        state.dispatch(Intent::ForgetDaemon, 0.0);
        state.update(Update::Disconnected("unreachable".into()));
        assert!(matches!(state.removal, Some(Removal::Daemon(_))));
        state.dispatch(Intent::Confirm, 0.0);
        assert_eq!(state.current, Some(key(1)));
        assert_eq!(state.daemons.len(), 1);

        state.dispatch(Intent::ForgetDaemon, 0.0);
        state.dispatch(Intent::Confirm, 0.0);
        assert!(state.pairing && state.current.is_none());
        assert!(matches!(
            state.take_commands().last(),
            Some(Command::Disconnect)
        ));
    }

    #[test]
    fn pairing_saves_daemon_with_name_and_connects() {
        let mut state = State::new(Persisted::default(), "client".into(), None);
        state.pair_name = "NAS".into();
        state.update(Update::Paired(Ok(key(4).into())));
        assert_eq!(state.daemons[0].display_name(), "NAS");
        assert!(!state.pairing);
        assert!(matches!(&state.take_commands()[..], [Command::Connect(_)]));
        let saved = serde_json::to_string(&state.take_persist().unwrap()).unwrap();
        let restored: Persisted = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.daemons, state.daemons);
    }

    #[test]
    fn automatic_labels_use_basename_and_stay_unique() {
        let mut state = connected();
        state.jobs.insert(1, job(1, "/home/me/site/"));
        let target = NameTarget::Job(1);
        assert_eq!(state.automatic_name_label(&target), "site");
        state.names.insert(
            "site".into(),
            Name {
                label: "site".into(),
                key: iroh_share_proto::NameKey([1; 32]),
                target: target.clone(),
                state: iroh_share_proto::NameState::Disabled,
            },
        );
        assert_eq!(state.automatic_name_label(&target), "site-2");
    }

    #[test]
    fn returning_after_a_while_restarts_the_session() {
        let mut state = connected();
        let generation = state.generation;
        state.dispatch(Intent::Visibility { hidden: true }, 1_000.0);
        state.dispatch(Intent::Visibility { hidden: false }, 2_000.0);
        assert_eq!(state.generation, generation);
        state.dispatch(Intent::Visibility { hidden: true }, 3_000.0);
        state.dispatch(Intent::Visibility { hidden: false }, 30_000.0);
        assert!(state.generation > generation);
        assert!(matches!(
            state.take_commands().last(),
            Some(Command::Connect(_))
        ));
    }
}
