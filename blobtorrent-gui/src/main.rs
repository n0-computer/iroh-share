#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod network;

use blobtorrent_proto::{
    client, BlobTicket, GatewayConfig, GatewaySnapshot, GatewayState, Job, JobKind, JobState, Name,
    NameState, NameTarget, PairingTicket, WatchEvent,
};
use clap::Parser;
use eframe::egui;
use network::{Action, Update};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(about = "Desktop client for blobtorrent")]
struct Args {
    /// One-time ticket printed by the daemon.
    ticket: Option<PairingTicket>,
    /// Directory for this GUI's identity and configuration.
    #[arg(long)]
    config_dir: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = args
        .config_dir
        .or_else(|| dirs::config_dir().map(|p| p.join("blobtorrent-gui")))
        .ok_or_else(|| {
            anyhow::anyhow!("cannot determine config directory; provide --config-dir")
        })?;
    std::fs::create_dir_all(&config)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([640.0, 440.0]),
        persistence_path: Some(config.join("window.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "Blobtorrent",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, runtime, config, args.ticket)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Data,
    Names,
    Settings,
}
#[derive(Clone)]
enum Removal {
    Data(u64),
    Name(String),
}
enum LocalUpdate {
    Paired(Result<(), String>),
    Picked(Option<PathBuf>),
    Opened(Result<(), String>),
}

struct App {
    runtime: tokio::runtime::Runtime,
    config: PathBuf,
    worker: Option<tokio::task::JoinHandle<()>>,
    actions: mpsc::Sender<Action>,
    updates: mpsc::Receiver<Update>,
    local_tx: mpsc::UnboundedSender<LocalUpdate>,
    local_rx: mpsc::UnboundedReceiver<LocalUpdate>,
    pairing: bool,
    ticket: String,
    ready: bool,
    busy: bool,
    status: String,
    page: Page,
    jobs: BTreeMap<u64, Job>,
    names: BTreeMap<String, Name>,
    selected: Option<u64>,
    removal: Option<Removal>,
    path: String,
    download_ticket: String,
    completion_id: u64,
    candidates: Vec<PathBuf>,
    name_label: String,
    name_url: String,
    name_job: bool,
    editing_name: bool,
    gateway: Option<GatewaySnapshot>,
    gateway_enabled: bool,
    gateway_listen: String,
    gateway_index: String,
    gateway_dirty: bool,
    local_paths: bool,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        runtime: tokio::runtime::Runtime,
        config: PathBuf,
        ticket: Option<PairingTicket>,
    ) -> Self {
        let local_paths = cc
            .storage
            .and_then(|s| s.get_string("local_paths"))
            .as_deref()
            == Some("true");
        Self::create(runtime, config, ticket, local_paths)
    }
    fn create(
        runtime: tokio::runtime::Runtime,
        config: PathBuf,
        ticket: Option<PairingTicket>,
        local_paths: bool,
    ) -> Self {
        let (actions, _) = mpsc::channel(32);
        let (_, updates) = mpsc::channel(128);
        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let configured = client::configured_endpoint(&config);
        let status = configured
            .as_ref()
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        let pairing = ticket.is_some() || !matches!(configured, Ok(Some(_)));
        let local_paths = local_paths && ticket.is_none();
        let mut app = Self {
            runtime,
            config,
            worker: None,
            actions,
            updates,
            local_tx,
            local_rx,
            pairing,
            ticket: ticket.map(|t| t.to_string()).unwrap_or_default(),
            ready: false,
            busy: false,
            status,
            page: Page::Data,
            jobs: BTreeMap::new(),
            names: BTreeMap::new(),
            selected: None,
            removal: None,
            path: String::new(),
            download_ticket: String::new(),
            completion_id: 0,
            candidates: vec![],
            name_label: String::new(),
            name_url: String::new(),
            name_job: false,
            editing_name: false,
            gateway: None,
            gateway_enabled: GatewayConfig::default().enabled,
            gateway_listen: "127.0.0.1:8080".into(),
            gateway_index: String::new(),
            gateway_dirty: false,
            local_paths,
        };
        if !app.pairing {
            app.connect();
        }
        app
    }
    fn connect(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        let (actions, rx) = mpsc::channel(32);
        let (tx, updates) = mpsc::channel(128);
        self.actions = actions;
        self.updates = updates;
        self.worker = Some(
            self.runtime
                .spawn(network::run(self.config.clone(), rx, tx)),
        );
        self.pairing = false;
        self.ready = false;
        self.status = "Connecting…".into();
    }
    fn pair(&mut self) {
        match self.ticket.trim().parse::<PairingTicket>() {
            Ok(ticket) => {
                self.busy = true;
                self.status = "Pairing…".into();
                let config = self.config.clone();
                let tx = self.local_tx.clone();
                self.runtime.spawn(async move {
                    let result = client::pair(&config, &ticket)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(LocalUpdate::Paired(result));
                });
            }
            Err(e) => self.status = format!("Invalid pairing ticket: {e}"),
        }
    }
    fn send(&mut self, action: Action) {
        if !self.ready || self.busy {
            return;
        }
        match self.actions.try_send(action) {
            Ok(()) => {
                self.busy = true;
                self.status = "Working…".into();
            }
            Err(e) => self.status = format!("Cannot send request: {e}"),
        }
    }
    fn sync_gateway(&mut self, snapshot: GatewaySnapshot) {
        if !self.gateway_dirty {
            self.gateway_enabled = snapshot.config.enabled;
            self.gateway_listen = snapshot.config.listen.to_string();
            self.gateway_index = snapshot
                .config
                .index_server
                .map(|a| a.to_string())
                .unwrap_or_default();
        }
        self.gateway = Some(snapshot);
    }
    fn poll(&mut self) {
        while let Ok(update) = self.local_rx.try_recv() {
            match update {
                LocalUpdate::Paired(result) => {
                    self.busy = false;
                    match result {
                        Ok(()) => {
                            self.ticket.clear();
                            self.connect();
                        }
                        Err(e) => self.status = e,
                    }
                }
                LocalUpdate::Picked(path) => {
                    if let Some(path) = path {
                        self.path = path.to_string_lossy().into();
                        self.completion_id += 1;
                        self.candidates.clear();
                    }
                }
                LocalUpdate::Opened(result) => {
                    if let Err(e) = result {
                        self.status = e;
                    }
                }
            }
        }
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Connecting | Update::Disconnected(_) => {
                    self.status = match update {
                        Update::Disconnected(e) => format!("Disconnected: {e}. Retrying…"),
                        _ => "Connecting…".into(),
                    };
                    self.ready = false;
                    self.busy = false;
                    self.jobs.clear();
                    self.names.clear();
                    self.selected = None;
                    self.editing_name = false;
                    self.name_label.clear();
                    self.completion_id += 1;
                    self.removal = None;
                    self.candidates.clear();
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
                    WatchEvent::GatewayUpdated(snapshot) => self.sync_gateway(snapshot),
                },
                Update::ActionResult(message) => {
                    self.busy = false;
                    self.status = message;
                }
                Update::GatewaySaved(result) => {
                    self.busy = false;
                    match result {
                        Ok(snapshot) => {
                            self.gateway_dirty = false;
                            self.sync_gateway(snapshot);
                            self.status = "Gateway settings saved".into();
                        }
                        Err(e) => self.status = e,
                    }
                }
                Update::Completion { id, result } => {
                    if id == self.completion_id {
                        match result {
                            Ok(paths) => {
                                self.candidates =
                                    paths.candidates.into_iter().map(|p| p.path).collect();
                                if paths.truncated {
                                    self.status = "More paths available; refine the prefix".into();
                                }
                            }
                            Err(e) => self.status = e,
                        }
                    }
                }
            }
        }
    }
    fn path_edit(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Path");
            if ui.text_edit_singleline(&mut self.path).changed() {
                self.completion_id += 1;
                self.candidates.clear();
            }
            if ui.button("Complete").clicked() {
                self.completion_id += 1;
                if let Err(e) = self.actions.try_send(Action::CompletePath {
                    id: self.completion_id,
                    path: self.path.clone().into(),
                }) {
                    self.status = e.to_string();
                }
            }
            if self.local_paths && ui.button("Choose folder…").clicked() {
                let tx = self.local_tx.clone();
                self.runtime.spawn_blocking(move || {
                    let _ = tx.send(LocalUpdate::Picked(rfd::FileDialog::new().pick_folder()));
                });
            }
        });
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .id_salt("completion")
            .max_height(100.0)
            .show(ui, |ui| {
                for path in &self.candidates {
                    if ui.button(path.display().to_string()).clicked() {
                        chosen = Some(path.clone());
                    }
                }
            });
        if let Some(path) = chosen {
            self.path = path.to_string_lossy().into();
            self.completion_id += 1;
            self.candidates.clear();
        }
    }
    fn data(&mut self, ui: &mut egui::Ui) {
        ui.heading("Data");
        ui.add_enabled_ui(self.ready && !self.busy, |ui| {
            self.path_edit(ui);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.path.trim().is_empty(), egui::Button::new("Share"))
                    .clicked()
                {
                    self.send(Action::Share(self.path.clone().into()));
                }
                ui.label("Download ticket");
                ui.text_edit_singleline(&mut self.download_ticket);
                if ui
                    .add_enabled(!self.path.trim().is_empty(), egui::Button::new("Download"))
                    .clicked()
                {
                    match self.download_ticket.trim().parse::<BlobTicket>() {
                        Ok(ticket) => self.send(Action::Download {
                            ticket,
                            target: self.path.clone().into(),
                        }),
                        Err(e) => self.status = format!("Invalid blob ticket: {e}"),
                    }
                }
            });
        });
        if self.local_paths {
            ui.weak("Drop a folder or file to share it. Paths refer to the daemon's filesystem.");
        }
        ui.separator();
        egui::ScrollArea::both()
            .id_salt("data_table_scroll")
            .show(ui, |ui| {
                egui::Grid::new("data_table")
                    .striped(true)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        table_header(
                            ui,
                            &[
                                ("Path", 300.0),
                                ("State / progress", 240.0),
                                ("Content URL", 300.0),
                                ("Actions", 90.0),
                            ],
                        );
                        for job in self.jobs.values() {
                            let path = job_path(job);
                            let text = path.display().to_string();
                            if ui
                                .add_sized(
                                    [300.0, 24.0],
                                    egui::Button::new(&text)
                                        .selected(self.selected == Some(job.id))
                                        .frame(false)
                                        .truncate(),
                                )
                                .on_hover_text(&text)
                                .clicked()
                            {
                                self.selected = Some(job.id);
                            }
                            table_text(ui, job_status(&job.state), 240.0);
                            if let JobState::Seeding { ticket } = &job.state {
                                link(ui, content_url(ticket));
                            } else {
                                table_text(ui, "—".into(), 300.0);
                            }
                            ui.push_id(job.id, |ui| {
                                ui.menu_button("Actions", |ui| {
                                    if let JobState::Seeding { ticket } = &job.state {
                                        if ui.button("Copy ticket").clicked() {
                                            ui.ctx().copy_text(ticket.to_string());
                                            ui.close();
                                        }
                                        if self.local_paths && ui.button("Open folder").clicked() {
                                            let path = path.clone();
                                            let tx = self.local_tx.clone();
                                            self.runtime.spawn_blocking(move || {
                                                let folder = if path.is_dir() {
                                                    path.as_path()
                                                } else {
                                                    path.parent().unwrap_or(&path)
                                                };
                                                let _ = tx.send(LocalUpdate::Opened(
                                                    open::that(folder).map_err(|e| e.to_string()),
                                                ));
                                            });
                                            ui.close();
                                        }
                                    }
                                    if ui
                                        .add_enabled(
                                            self.ready && !self.busy,
                                            egui::Button::new("Remove…"),
                                        )
                                        .clicked()
                                    {
                                        self.removal = Some(Removal::Data(job.id));
                                        ui.close();
                                    }
                                });
                            });
                            ui.end_row();
                        }
                    });
                if self.jobs.is_empty() {
                    ui.weak("Share a path or download a blob to get started.");
                }
            });
    }

    fn names(&mut self, ui: &mut egui::Ui) {
        ui.heading("Names");
        ui.add_enabled_ui(self.ready && !self.busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Label");
                ui.add_enabled(
                    !self.editing_name,
                    egui::TextEdit::singleline(&mut self.name_label),
                );
                ui.checkbox(&mut self.name_job, "Follow data");
            });
            if self.name_job {
                egui::ComboBox::from_id_salt("name_data")
                    .selected_text(
                        self.selected
                            .and_then(|id| self.jobs.get(&id))
                            .map(|j| job_path(j).display().to_string())
                            .unwrap_or_else(|| "Select a path".into()),
                    )
                    .show_ui(ui, |ui| {
                        for job in self.jobs.values() {
                            ui.selectable_value(
                                &mut self.selected,
                                Some(job.id),
                                job_path(job).display().to_string(),
                            );
                        }
                    });
            } else {
                ui.horizontal(|ui| {
                    ui.label("URL");
                    ui.text_edit_singleline(&mut self.name_url);
                });
            }
            ui.horizontal(|ui| {
                if ui
                    .button(if self.editing_name {
                        "Save name"
                    } else {
                        "Create name"
                    })
                    .clicked()
                {
                    let target = if self.name_job {
                        self.selected
                            .filter(|id| self.jobs.contains_key(id))
                            .map(NameTarget::Job)
                            .ok_or_else(|| "Select a data path".to_owned())
                    } else {
                        self.name_url
                            .trim()
                            .parse()
                            .map(NameTarget::Url)
                            .map_err(|e| format!("Invalid URL: {e}"))
                    };
                    match target {
                        Ok(target) if !self.name_label.trim().is_empty() => {
                            let label = self.name_label.trim().to_owned();
                            self.send(if self.editing_name {
                                Action::UpdateName { label, target }
                            } else {
                                Action::CreateName { label, target }
                            });
                        }
                        Ok(_) => self.status = "Enter a name label".into(),
                        Err(e) => self.status = e,
                    }
                }
                if self.editing_name && ui.button("New name").clicked() {
                    self.editing_name = false;
                    self.name_label.clear();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::both()
            .id_salt("names_table_scroll")
            .show(ui, |ui| {
                egui::Grid::new("names_table")
                    .striped(true)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        table_header(
                            ui,
                            &[
                                ("Name", 140.0),
                                ("Public URL", 300.0),
                                ("Target", 280.0),
                                ("State", 140.0),
                                ("Actions", 90.0),
                            ],
                        );
                        for name in self.names.values() {
                            table_text(ui, name.label.clone(), 140.0);
                            link(ui, name.key.url().to_string());
                            table_text(
                                ui,
                                match &name.target {
                                    NameTarget::Url(url) => url.to_string(),
                                    NameTarget::Job(id) => self
                                        .jobs
                                        .get(id)
                                        .map(|j| format!("Following {}", job_path(j).display()))
                                        .unwrap_or_else(|| "Linked data is unavailable".into()),
                                },
                                280.0,
                            );
                            table_text(
                                ui,
                                match &name.state {
                                    NameState::Disabled => "Disabled".into(),
                                    NameState::WaitingForJob => "Waiting for data".into(),
                                    NameState::Publishing { .. } => "Publishing…".into(),
                                    NameState::Published { .. } => "Published".into(),
                                    NameState::Failed { error } => {
                                        format!("Failed: {}", error.message)
                                    }
                                },
                                140.0,
                            );
                            ui.push_id(&name.label, |ui| {
                                ui.add_enabled_ui(self.ready && !self.busy, |ui| {
                                    ui.menu_button("Actions", |ui| {
                                        if ui.button("Edit").clicked() {
                                            self.editing_name = true;
                                            self.name_label = name.label.clone();
                                            match &name.target {
                                                NameTarget::Url(url) => {
                                                    self.name_job = false;
                                                    self.name_url = url.to_string();
                                                }
                                                NameTarget::Job(id) => {
                                                    self.name_job = true;
                                                    self.selected = Some(*id);
                                                }
                                            }
                                            ui.close();
                                        }
                                        if ui.button("Remove…").clicked() {
                                            self.removal = Some(Removal::Name(name.label.clone()));
                                            ui.close();
                                        }
                                    });
                                });
                            });
                            ui.end_row();
                        }
                    });
                if self.names.is_empty() {
                    ui.weak("Create a name pointing to a URL or following your data.");
                }
            });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.checkbox(&mut self.local_paths, "The daemon is on this computer");
        ui.weak("Enable folder selection, drag-and-drop sharing, and opening downloaded folders. Only enable this when both apps use the same filesystem.");
        ui.separator();
        ui.heading("Gateway");
        ui.label("Browse the content-addressed web. Runs inside the daemon with its own endpoint.");
        ui.add_enabled_ui(self.ready && !self.busy, |ui| {
            self.gateway_dirty |= ui
                .checkbox(
                    &mut self.gateway_enabled,
                    "Enable gateway and start with daemon",
                )
                .changed();
            ui.horizontal(|ui| {
                ui.label("Listen address");
                self.gateway_dirty |= ui.text_edit_singleline(&mut self.gateway_listen).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Index server (blank for discovery)");
                self.gateway_dirty |= ui.text_edit_singleline(&mut self.gateway_index).changed();
            });
            if ui.button("Save / retry").clicked() {
                match gateway_config(
                    self.gateway_enabled,
                    &self.gateway_listen,
                    &self.gateway_index,
                ) {
                    Ok(config) => self.send(Action::SetGateway(config)),
                    Err(e) => self.status = e,
                }
            }
        });
        if let Some(snapshot) = &self.gateway {
            ui.label(match &snapshot.state {
                GatewayState::Disabled => "Disabled".into(),
                GatewayState::Starting => "Starting…".into(),
                GatewayState::Running { listen, .. } => format!("Listening at http://{listen}"),
                GatewayState::Failed { error } => {
                    format!("Failed: {error}. Retrying every 30 seconds.")
                }
            });
        }
        ui.weak("The HTTP listener is on the daemon's loopback interface. Closing this app leaves the gateway running.");
        ui.separator();
        ui.label(format!("Client configuration: {}", self.config.display()));
        if ui.button("Connect to another daemon…").clicked() {
            if let Some(worker) = self.worker.take() {
                worker.abort();
            }
            self.ready = false;
            self.busy = false;
            self.pairing = true;
            self.ticket.clear();
            self.jobs.clear();
            self.names.clear();
            self.removal = None;
            self.gateway = None;
            self.gateway_dirty = false;
            self.local_paths = false;
        }
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("local_paths", self.local_paths.to_string());
    }
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.ui(ctx);
    }
}
impl App {
    fn ui(&mut self, ctx: &egui::Context) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(100));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if self.busy {
                    ui.spinner();
                }
            });
        });
        if self.pairing {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Connect to Blobtorrent"); ui.label("Paste the one-time ticket printed by your daemon. Your connection is saved for next time.");
                ui.add_enabled_ui(!self.busy, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.ticket).password(true).hint_text("Pairing ticket").desired_width(f32::INFINITY));
                    if ui.button("Connect").clicked() { self.pair(); }
                    if client::configured_endpoint(&self.config).ok().flatten().is_some() && ui.button("Use saved connection").clicked() { self.ticket.clear(); self.connect(); }
                });
            });
            return;
        }
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Blobtorrent");
                for (page, title) in [
                    (Page::Data, "Data"),
                    (Page::Names, "Names"),
                    (Page::Settings, "Settings"),
                ] {
                    ui.selectable_value(&mut self.page, page, title);
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Data => self.data(ui),
            Page::Names => self.names(ui),
            Page::Settings => self.settings(ui),
        });
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            if !self.local_paths {
                self.status = "Enable ‘The daemon is on this computer’ in Settings before sharing local drops.".into();
            } else if self.ready && !self.busy {
                let paths: Vec<_> = dropped.into_iter().filter_map(|f| f.path).collect();
                if !paths.is_empty() {
                    self.send(Action::ShareMany(paths));
                }
            } else {
                self.status =
                    "Drop ignored: wait until connected and the current request finishes.".into();
            }
        }

        if let Some(removal) = self.removal.clone() {
            egui::Window::new("Confirm removal").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(match &removal { Removal::Data(_) => "Stop sharing this data? Files on disk are kept.", Removal::Name(_) => "Remove this name and its signing key? Cached records may remain resolvable." });
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.ready && !self.busy, egui::Button::new("Remove")).clicked() {
                        self.send(match removal { Removal::Data(id) => Action::Remove(id), Removal::Name(label) => Action::RemoveName(label) }); self.removal = None;
                    }
                    if ui.button("Cancel").clicked() { self.removal = None; }
                });
            });
        }
    }
}
fn job_path(job: &Job) -> &PathBuf {
    match &job.kind {
        JobKind::Share { path } => path,
        JobKind::Download { target, .. } => target,
    }
}
fn content_url(ticket: &BlobTicket) -> String {
    format!(
        "https://{}.blake3.net/",
        z32::encode(ticket.hash().as_bytes())
    )
}
fn table_header(ui: &mut egui::Ui, columns: &[(&str, f32)]) {
    for (title, width) in columns {
        ui.add_sized(
            [*width, 28.0],
            egui::Label::new(egui::RichText::new(*title).strong()),
        );
    }
    ui.end_row();
}
fn table_text(ui: &mut egui::Ui, text: String, width: f32) {
    ui.add_sized([width, 24.0], egui::Label::new(&text).truncate())
        .on_hover_text(&text);
}
fn link(ui: &mut egui::Ui, url: String) {
    ui.horizontal(|ui| {
        let response = ui
            .add_sized(
                [245.0, 24.0],
                egui::Label::new(egui::RichText::new(&url).color(ui.visuals().hyperlink_color))
                    .truncate()
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(&url)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
        }
        if ui.small_button("Copy").on_hover_text("Copy URL").clicked() {
            ui.ctx().copy_text(url);
        }
    });
}
fn job_status(state: &JobState) -> String {
    match state {
        JobState::Queued => "Queued".into(),
        JobState::Importing { progress } => format!(
            "Importing: {} / {} files · {} / {} bytes",
            progress.files_done, progress.files_total, progress.bytes_done, progress.bytes_total
        ),
        JobState::Downloading { progress, .. } => format!(
            "Downloading: {} / {} bytes",
            progress.bytes_done,
            progress
                .bytes_total
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
        JobState::Exporting { progress, .. } => format!(
            "Exporting: {} / {} files · {} / {} bytes",
            progress.files_done, progress.files_total, progress.bytes_done, progress.bytes_total
        ),
        JobState::Seeding { .. } => "Seeding".into(),
        JobState::Failed { error } => format!("Failed: {}", error.message),
    }
}
fn gateway_config(enabled: bool, listen: &str, index: &str) -> Result<GatewayConfig, String> {
    let listen: std::net::SocketAddr = listen
        .trim()
        .parse()
        .map_err(|e| format!("Invalid listen address: {e}"))?;
    if !listen.ip().is_loopback() {
        return Err("Gateway listen address must be loopback".into());
    }
    let index_server = if index.trim().is_empty() {
        None
    } else {
        Some(
            index
                .trim()
                .parse()
                .map_err(|e| format!("Invalid index server: {e}"))?,
        )
    };
    Ok(GatewayConfig {
        enabled,
        listen,
        index_server,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_form_rejects_public_bind_and_preserves_discovery() {
        assert!(gateway_config(true, "0.0.0.0:8080", "").is_err());
        let config = gateway_config(true, "[::1]:8080", " ").unwrap();
        assert!(config.enabled);
        assert!(config.index_server.is_none());
        assert!(gateway_config(false, "127.0.0.1:8080", "invalid").is_err());
    }
    fn fixture() -> (
        tempfile::TempDir,
        App,
        mpsc::Sender<Update>,
        mpsc::Receiver<Action>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut app = App::create(runtime, dir.path().to_owned(), None, false);
        let (tx, rx) = mpsc::channel(128);
        app.updates = rx;
        let (actions, commands) = mpsc::channel(32);
        app.actions = actions;
        (dir, app, tx, commands)
    }
    #[test]
    fn disconnect_invalidates_pending_selection_and_confirmation() {
        let (_dir, mut app, tx, mut commands) = fixture();
        tx.try_send(Update::Event(WatchEvent::JobUpdated(Box::new(Job {
            id: 3,
            kind: JobKind::Share {
                path: "/data".into(),
            },
            state: JobState::Queued,
        }))))
        .unwrap();
        tx.try_send(Update::Event(WatchEvent::SnapshotComplete))
            .unwrap();
        app.poll();
        assert!(app.ready);
        app.selected = Some(3);
        app.removal = Some(Removal::Data(3));
        tx.try_send(Update::Disconnected("lost connection".into()))
            .unwrap();
        app.poll();
        assert!(!app.ready);
        assert!(app.jobs.is_empty());
        assert!(app.selected.is_none());
        assert!(app.removal.is_none());
        app.send(Action::Remove(3));
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn gateway_watch_preserves_draft_and_stale_completion_is_ignored() {
        let (_dir, mut app, tx, _) = fixture();
        app.gateway_listen = "127.0.0.1:9000".into();
        app.gateway_dirty = true;
        tx.try_send(Update::Event(WatchEvent::GatewayUpdated(GatewaySnapshot {
            config: GatewayConfig::default(),
            state: GatewayState::Starting,
        })))
        .unwrap();
        app.completion_id = 2;
        tx.try_send(Update::Completion {
            id: 1,
            result: Ok(blobtorrent_proto::PathCompletions {
                common_prefix: "/old".into(),
                candidates: vec![blobtorrent_proto::PathCandidate {
                    path: "/old".into(),
                    kind: blobtorrent_proto::PathKind::Directory,
                }],
                truncated: false,
            }),
        })
        .unwrap();
        app.poll();
        assert_eq!(app.gateway_listen, "127.0.0.1:9000");
        assert!(app.candidates.is_empty());
        assert!(matches!(app.gateway.unwrap().state, GatewayState::Starting));
    }
    #[test]
    fn renders_pairing_and_all_pages_and_gates_local_drops() {
        let (_dir, mut app, _tx, mut commands) = fixture();
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            ..Default::default()
        };
        assert!(!ctx.run(input(), |ctx| app.ui(ctx)).shapes.is_empty());
        app.pairing = false;
        app.ready = true;
        for page in [Page::Data, Page::Names, Page::Settings] {
            app.page = page;
            assert!(!ctx.run(input(), |ctx| app.ui(ctx)).shapes.is_empty());
        }
        let mut drop = input();
        drop.dropped_files.push(egui::DroppedFile {
            path: Some("/shared".into()),
            ..Default::default()
        });
        let _ = ctx.run(drop.clone(), |ctx| app.ui(ctx));
        assert!(commands.try_recv().is_err());
        app.local_paths = true;
        let _ = ctx.run(drop, |ctx| app.ui(ctx));
        assert!(
            matches!(commands.try_recv().unwrap(), Action::ShareMany(paths) if paths == vec![PathBuf::from("/shared")])
        );
    }
}
