#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod network;

use anyhow::Context as _;
use clap::Parser;
use eframe::egui;
use iroh_share_proto::{
    client, BlobTicket, DiscoveryMode, DownloadSource, GatewayConfig, GatewaySnapshot,
    GatewayState, Job, JobKind, JobState, Name, NameState, NameTarget, PairingTicket, WatchEvent,
};
use network::{Action, Update};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(about = "Desktop client for iroh-share")]
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
        .or_else(|| dirs::config_dir().map(|p| p.join("iroh-share-gui")))
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
        "Iroh Share",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, runtime, config, args.ticket)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Data,
    Settings,
}
#[derive(Clone)]
enum Removal {
    Data(u64),
    Name(String),
}
enum LocalUpdate {
    ExportNames(Option<PathBuf>),
    Paired(Result<(), String>),
    Picked(Option<PathBuf>),
    DownloadFolder(Option<PathBuf>),
    Opened(Result<(), String>),
    TicketFile(Result<String, String>),
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
    publish_label: String,
    add_mode: usize,
    include_directory_name: bool,
    name_editor_open: bool,
    name_editor_content: bool,
    import_open: bool,
    import_source: String,
    import_label: String,
    import_id: Option<u64>,
    import_directory: Option<PathBuf>,
    download_source: String,
    download_target: String,
    download_discover: bool,
    completion_id: u64,
    completing_download: bool,
    completion_cursor_end: bool,
    candidates: Vec<PathBuf>,
    name_label: String,
    name_url: String,
    new_name_records: String,
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
        let installed_local = std::fs::read_to_string(config.join("local-endpoint"))
            .ok()
            .and_then(|id| id.trim().parse::<iroh_share_proto::EndpointId>().ok())
            .is_some_and(|id| matches!(&configured, Ok(Some(server)) if *server == id));
        let local_paths = (local_paths || installed_local) && ticket.is_none();
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
            publish_label: String::new(),
            add_mode: 0,
            include_directory_name: false,
            name_editor_open: false,
            name_editor_content: true,
            import_open: false,
            import_source: String::new(),
            import_label: String::new(),
            import_id: None,
            import_directory: None,
            download_source: String::new(),
            download_target: String::new(),
            download_discover: false,
            completion_id: 0,
            completing_download: false,
            completion_cursor_end: false,
            candidates: vec![],
            name_label: String::new(),
            name_url: iroh_share_proto::redirect_records(&"https://example.com/".parse().unwrap()),
            new_name_records: iroh_share_proto::redirect_records(
                &"https://example.com/".parse().unwrap(),
            ),
            name_job: true,
            editing_name: false,
            gateway: None,
            gateway_enabled: GatewayConfig::default().enabled,
            gateway_listen: GatewayConfig::default().listen.to_string(),
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
                LocalUpdate::ExportNames(path) => {
                    if let Some(path) = path {
                        self.send(Action::ExportNames(path));
                    }
                }
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
                LocalUpdate::DownloadFolder(path) => {
                    if let Some(path) = path {
                        self.download_target = path.to_string_lossy().into();
                        self.completion_id += 1;
                        self.candidates.clear();
                    }
                }
                LocalUpdate::Picked(path) => {
                    if let Some(path) = path {
                        self.path = path.to_string_lossy().into();
                        self.completion_id += 1;
                        self.candidates.clear();
                    }
                }
                LocalUpdate::TicketFile(result) => match result {
                    Ok(ticket) if self.ready && !self.busy => {
                        self.import_source = ticket;
                        self.import_open = true;
                        self.page = Page::Data;
                    }
                    Ok(_) => self.status = "Ticket drop ignored: reconnect and try again.".into(),
                    Err(error) => self.status = format!("Cannot read ticket: {error}"),
                },
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
                    self.import_open = false;
                    self.import_id = None;
                    self.import_directory = None;
                    self.import_source.clear();
                    self.editing_name = false;
                    self.name_editor_open = false;
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
                Update::ImportDirectory(path) => self.import_directory = Some(path),
                Update::Imported { job, name_error } => {
                    self.busy = false;
                    self.import_open = false;
                    self.import_source.clear();
                    self.import_id = None;
                    self.selected = Some(job.id);
                    self.page = Page::Data;
                    if let Some(error) = name_error {
                        self.name_label = self.import_label.clone();
                        self.name_job = true;
                        self.editing_name = false;
                        self.name_editor_open = true;
                        self.name_editor_content = true;
                        self.status = format!("Import started, but the name could not be created: {error}. Retry in Content names.");
                    } else {
                        self.import_label.clear();
                        self.status =
                            "Import started. Linked names will follow when the content is ready."
                                .into();
                    }
                    self.jobs.entry(job.id).or_insert(job);
                }
                Update::NameSaved(result) => {
                    self.busy = false;
                    match result {
                        Ok(name) => {
                            if !self.editing_name {
                                self.new_name_records = iroh_share_proto::redirect_records(
                                    &"https://example.com/".parse().unwrap(),
                                );
                            }
                            self.status = format!(
                                "Name saved: {}. Publication status is shown with the name.",
                                name.key.url()
                            );
                            self.names.insert(name.label.clone(), name);
                            self.name_editor_open = false;
                            self.editing_name = false;
                            self.name_label.clear();
                            self.name_url = iroh_share_proto::redirect_records(
                                &"https://example.com/".parse().unwrap(),
                            );
                        }
                        Err(error) => self.status = format!("Could not save name: {error}"),
                    }
                }
                Update::Published { job, name_error } => {
                    self.busy = false;

                    self.selected = Some(job.id);
                    self.page = Page::Data;
                    self.name_editor_content = true;
                    self.path.clear();
                    self.candidates.clear();
                    self.completion_id += 1;
                    if let Some(error) = name_error {
                        self.name_label = self.publish_label.clone();
                        self.name_job = true;
                        self.editing_name = false;
                        self.name_editor_open = true;
                        self.status = format!("Content added, but its name could not be created: {error}. Retry in Content names.");
                    } else {
                        self.publish_label.clear();
                        self.status =
                            "Content added. Links and the ticket appear when it is ready.".into();
                    }
                    // Watch may already have delivered a newer import state.
                    self.jobs.entry(job.id).or_insert(job);
                }
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
                                self.candidates = paths
                                    .candidates
                                    .into_iter()
                                    .filter(|p| {
                                        !self.completing_download
                                            || p.kind == iroh_share_proto::PathKind::Directory
                                    })
                                    .map(|p| p.path)
                                    .collect();
                                let value = if self.completing_download {
                                    &mut self.download_target
                                } else {
                                    &mut self.path
                                };
                                let prefix = if self.candidates.len() == 1 {
                                    self.candidates[0].to_string_lossy().into_owned()
                                } else {
                                    paths.common_prefix.to_string_lossy().into_owned()
                                };
                                if !prefix.is_empty() && !self.candidates.is_empty() {
                                    *value = prefix;
                                    self.completion_cursor_end = true;
                                }
                                if self.candidates.is_empty() {
                                    self.status = "No matching paths".into();
                                }
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
    fn path_input(&mut self, ui: &mut egui::Ui, download: bool) {
        let id = ui.make_persistent_id(if download {
            "download_path"
        } else {
            "share_path"
        });
        let tab = ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
        let value = if download {
            &mut self.download_target
        } else {
            &mut self.path
        };
        let mut edit = egui::TextEdit::singleline(value)
            .id(id)
            .hint_text("Path on the daemon · Tab to complete")
            .show(ui);
        if self.completion_cursor_end && self.completing_download == download {
            edit.state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(value.chars().count()),
                )));
            edit.state.store(ui.ctx(), id);
            self.completion_cursor_end = false;
        }
        if edit.response.changed() {
            self.completion_id += 1;
            self.candidates.clear();
        }
        if tab {
            self.completing_download = download;
            self.completion_id += 1;
            self.candidates.clear();
            edit.response.request_focus();
            if let Err(e) = self.actions.try_send(Action::CompletePath {
                id: self.completion_id,
                path: value.clone().into(),
            }) {
                self.status = e.to_string();
            }
        }
        if self.completing_download == download && !self.candidates.is_empty() {
            let mut open = true;
            let mut chosen = None;
            egui::Popup::from_response(&edit.response)
                .id(id.with("candidates"))
                .open_bool(&mut open)
                .width(edit.response.rect.width())
                .show(|ui| {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for path in &self.candidates {
                                if ui
                                    .selectable_label(false, path.display().to_string())
                                    .clicked()
                                {
                                    chosen = Some(path.clone());
                                }
                            }
                        });
                });
            if let Some(path) = chosen {
                *value = path.to_string_lossy().into();
                self.completion_cursor_end = true;
                self.completion_id += 1;
                self.candidates.clear();
                edit.response.request_focus();
            } else if !open {
                self.candidates.clear();
            }
        }
    }
    fn new_content(&mut self, ui: &mut egui::Ui) {
        if self.import_open {
            self.add_mode = 1;
        }
        table_cell(ui, 300.0, 24.0, |ui| match self.add_mode {
            0 => {
                ui.horizontal(|ui| {
                    self.path_input(ui, false);
                });
                if self.local_paths {
                    ui.horizontal(|ui| {
                        for (label, directory) in
                            [("Choose folder…", true), ("Choose file…", false)]
                        {
                            if ui.button(label).clicked() {
                                let tx = self.local_tx.clone();
                                self.runtime.spawn_blocking(move || {
                                    let dialog = rfd::FileDialog::new();
                                    let path = if directory {
                                        dialog.pick_folder()
                                    } else {
                                        dialog.pick_file()
                                    };
                                    let _ = tx.send(LocalUpdate::Picked(path));
                                });
                            }
                        }
                    });
                }
            }
            1 => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_source).hint_text("Paste a ticket"),
                );
            }
            _ => {
                ui.horizontal(|ui| {
                    self.path_input(ui, true);
                });
                if self.local_paths && ui.button("Choose folder…").clicked() {
                    let tx = self.local_tx.clone();
                    self.runtime.spawn_blocking(move || {
                        let _ = tx.send(LocalUpdate::DownloadFolder(
                            rfd::FileDialog::new().pick_folder(),
                        ));
                    });
                }
            }
        });
        table_cell(ui, 240.0, 24.0, |ui| {
            let previous = self.add_mode;
            egui::ComboBox::from_id_salt("add_content_mode")
                .selected_text(["Share a path", "Import ticket", "Download"][self.add_mode])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.add_mode, 0, "Share a path");
                    ui.selectable_value(&mut self.add_mode, 1, "Import ticket");
                    ui.selectable_value(&mut self.add_mode, 2, "Download");
                });
            if previous != self.add_mode {
                self.import_open = false;
                self.import_id = None;
            }
            if let Some(id) = self.import_id {
                ui.label(
                    self.jobs
                        .get(&id)
                        .map(|j| format!("Updating {}", job_path(j).display()))
                        .unwrap_or_default(),
                );
            }
        });
        table_cell(ui, 440.0, 24.0, |ui| match self.add_mode {
            0 => {
                ui.checkbox(&mut self.include_directory_name, "Include directory name");
            }
            1 => {}
            _ => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.download_source)
                        .hint_text("URL, hash, or ticket"),
                );
                ui.checkbox(&mut self.download_discover, "Also discover providers");
            }
        });
        table_cell(ui, 300.0, 24.0, |ui| {
            let action = match self.add_mode {
                0 if !self.path.trim().is_empty() => Some(Action::Publish {
                    path: self.path.clone().into(),
                    include_directory_name: self.include_directory_name,
                    label: None,
                }),
                1 => self
                    .import_source
                    .trim()
                    .parse::<BlobTicket>()
                    .ok()
                    .and_then(|t| DownloadSource::try_from(t).ok())
                    .filter(|_| {
                        self.import_id.is_none_or(|id| {
                            self.jobs.get(&id).is_some_and(|j| {
                                matches!(
                                    j.state,
                                    JobState::Seeding { .. } | JobState::Failed { .. }
                                )
                            })
                        })
                    })
                    .map(|source| Action::Import {
                        source,
                        id: self.import_id,
                        label: None,
                    }),
                2 if !self.download_target.trim().is_empty() => self
                    .download_source
                    .trim()
                    .parse::<DownloadSource>()
                    .ok()
                    .map(|mut source| {
                        if self.download_discover {
                            source.discovery = DiscoveryMode::Mainline;
                        }
                        Action::Download {
                            source,
                            target: self.download_target.clone().into(),
                        }
                    }),
                _ => None,
            };
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.ready && !self.busy && action.is_some(),
                        egui::Button::new(if self.import_id.is_some() {
                            "Update"
                        } else {
                            "Add"
                        }),
                    )
                    .clicked()
                {
                    if let Some(action) = action {
                        self.send(action);
                    }
                }
                if self.import_id.is_some() && ui.button("Cancel").clicked() {
                    self.import_id = None;
                    self.import_open = false;
                }
            });
        });
        ui.end_row();
    }
    fn data(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        egui::ScrollArea::both()
            .id_salt("data_table_scroll")
            .max_height(280.0)
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
                                ("Public links / names", 440.0),
                                ("Actions", 300.0),
                            ],
                        );
                        for job in self.jobs.values().cloned().collect::<Vec<_>>() {
                            let path = job_path(&job);
                            let text = path.display().to_string();
                            let link_count = self.names.values().filter(|n| n.target == NameTarget::Job(job.id)).count() + 1;
                            let row_height = link_count as f32 * 24.0 + link_count.saturating_sub(1) as f32 * ui.spacing().item_spacing.y;
                            table_cell(ui, 300.0, row_height, |ui| {
                            row_line(ui, |ui| {
                            let path_width = ui.available_width() - if self.local_paths { 24.0 + ui.spacing().item_spacing.x } else { 0.0 };
                            ui.allocate_ui_with_layout(egui::vec2(path_width, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_width(path_width);
                            if ui
                                .add(
                                    egui::Button::new(&text)
                                        .selected(self.selected == Some(job.id))
                                        .frame(false)
                                        .truncate()
                                        .min_size(egui::vec2(0.0, 24.0)),
                                )
                                .on_hover_text(&text)
                                .clicked()
                            {
                                self.selected = Some(job.id);
                            }
                            });
                            if self.local_paths && ui.add_enabled_ui(path.exists(), |ui| {
                                icon_button(ui, Icon::Open, "Open directory")
                                    .on_disabled_hover_text("This path is not available on this computer")
                            }).inner.clicked() {
                                let folder = if path.is_dir() { path.clone() } else { path.parent().unwrap_or(&path).to_path_buf() };
                                let tx = self.local_tx.clone();
                                self.runtime.spawn_blocking(move || {
                                    let _ = tx.send(LocalUpdate::Opened(open::that(folder).map_err(|e| e.to_string())));
                                });
                            }
                            });
                            });
                            table_cell(ui, 240.0, row_height, |ui| {
                                table_text(ui, job_status(&job.state), 240.0);
                            });
                            table_cell(ui, 440.0, row_height, |ui| {
                                for name in self.names.values().filter(|n| n.target == NameTarget::Job(job.id)) {
                                    if name_link(ui, name, Some(self.ready && !self.busy)) {
                                        self.removal = Some(Removal::Name(name.label.clone()));
                                    }
                                }
                                if let JobState::Seeding { ticket, .. } = &job.state {
                                    link(ui, content_url(ticket));
                                } else {
                                    row_line(ui, |ui| { ui.weak("Content link available after import"); });
                                }
                            });
                            table_cell(ui, 300.0, row_height, |ui| {
                            ui.push_id(job.id, |ui| {
                                ui.allocate_ui_with_layout(egui::vec2(300.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                if let JobState::Seeding { ticket, .. } = &job.state {
                                    if icon_button(ui, Icon::Copy, "Copy ticket").clicked() {
                                        ui.ctx().copy_text(ticket.to_string());
                                        self.status = "Ticket copied".into();
                                    }
                                }
                                if ui.add_enabled(self.ready && !self.busy, egui::Button::new("Add name")).clicked() {
                                    let target = NameTarget::Job(job.id);
                                    let label = self.automatic_name_label(&target);
                                    self.send(Action::CreateName { label, target });
                                }
                                if can_refresh(&job, &self.names) && (self.local_paths && path.is_dir()) && ui.add_enabled(self.ready && !self.busy, egui::Button::new("Refresh")).on_hover_text("Rescan this local directory. Names that follow it keep the same public URL.").clicked() {
                                    self.send(Action::Refresh(job.id));
                                }
                                if ui.add_enabled(self.ready && !self.busy && matches!(job.state, JobState::Seeding { .. } | JobState::Failed { .. }), egui::Button::new("Update…")).clicked() {
                                    self.import_id = Some(job.id);
                                    self.import_source.clear();
                                    self.import_label.clear();
                                    self.import_open = true;
                                }
                                ui.add_enabled_ui(self.ready && !self.busy, |ui| {
                                    if icon_button(ui, Icon::Trash, "Remove data; files are kept").clicked() {
                                        self.removal = Some(Removal::Data(job.id));
                                    }
                                });
                                });
                            });
                            });
                            ui.end_row();
                        }
                        self.new_content(ui);
                    });
            });
    }

    fn automatic_name_label(&self, target: &NameTarget) -> String {
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

    fn name_save(&mut self, ui: &mut egui::Ui, bound: bool) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.ready && !self.busy && !self.name_url.trim().is_empty(),
                    egui::Button::new(if self.editing_name {
                        "Save"
                    } else {
                        "Add name"
                    }),
                )
                .clicked()
            {
                let target = if bound {
                    self.selected
                        .filter(|id| self.jobs.contains_key(id))
                        .map(NameTarget::Job)
                        .ok_or_else(|| "Select data to name".to_owned())
                } else {
                    Ok(NameTarget::Records(self.name_url.trim().to_owned()))
                };
                match target {
                    Ok(target) => {
                        let label = if self.editing_name {
                            self.name_label.clone()
                        } else {
                            self.automatic_name_label(&target)
                        };
                        self.send(if self.editing_name {
                            Action::UpdateName { label, target }
                        } else {
                            Action::CreateName { label, target }
                        });
                    }
                    Err(e) => self.status = e,
                }
            }
            if (self.name_editor_open || self.editing_name) && ui.button("Cancel").clicked() {
                self.name_editor_open = false;
                self.editing_name = false;
                self.name_label.clear();
                self.name_url =
                    iroh_share_proto::redirect_records(&"https://example.com/".parse().unwrap());
            }
        });
    }
    fn names(&mut self, ui: &mut egui::Ui, content: bool) {
        let standalone = |name: &&Name| {
            target_is_content(&name.target) == content
                && !matches!(&name.target, NameTarget::Job(id) if self.jobs.contains_key(id))
        };
        let names: Vec<_> = self.names.values().filter(standalone).cloned().collect();
        if content && names.is_empty() {
            return;
        }
        if content {
            ui.heading("Other content names");
        }
        ui.separator();
        egui::ScrollArea::both()
            .id_salt(("names_table_scroll", content))
            .max_height(260.0)
            .show(ui, |ui| {
                egui::Grid::new(("names_table", content))
                    .striped(true)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        table_header(
                            ui,
                            &[("Name", 440.0), ("DNS records", 560.0), ("Actions", 300.0)],
                        );
                        for name in &names {
                            if self.editing_name && self.name_label == name.label {
                                table_cell(ui, 440.0, 100.0, |ui| name_link(ui, name, None));
                                table_cell(ui, 560.0, 100.0, |ui| {
                                    dns_editor(ui, &name.label, &mut self.name_url);
                                });
                                table_cell(ui, 300.0, 100.0, |ui| {
                                    self.name_save(ui, self.name_job)
                                });
                                ui.end_row();
                                continue;
                            }
                            table_cell(ui, 440.0, 24.0, |ui| name_link(ui, name, None));
                            table_text(
                                ui,
                                match &name.target {
                                    NameTarget::Url(url) => url.to_string(),
                                    NameTarget::Records(text) => text.clone(),
                                    NameTarget::Job(id) => self
                                        .jobs
                                        .get(id)
                                        .map(|j| format!("Following {}", job_path(j).display()))
                                        .unwrap_or_else(|| "Linked data is unavailable".into()),
                                },
                                280.0,
                            );
                            table_cell(ui, 300.0, 24.0, |ui| {
                                ui.push_id(&name.label, |ui| {
                                    ui.add_enabled_ui(self.ready && !self.busy, |ui| {
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(300.0, 24.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                if ui.button("Edit").clicked() {
                                                    self.editing_name = true;
                                                    self.name_editor_open = true;
                                                    self.name_editor_content = content;
                                                    self.name_label = name.label.clone();
                                                    match &name.target {
                                                        NameTarget::Url(url) => {
                                                            self.name_job = false;
                                                            self.name_url =
                                                                iroh_share_proto::redirect_records(
                                                                    url,
                                                                );
                                                        }
                                                        NameTarget::Records(text) => {
                                                            self.name_job = false;
                                                            self.name_url = text.clone();
                                                        }
                                                        NameTarget::Job(id) => {
                                                            self.name_job = true;
                                                            self.selected = Some(*id);
                                                        }
                                                    }
                                                }
                                                if icon_button(ui, Icon::Trash, "Remove name…")
                                                    .clicked()
                                                {
                                                    self.removal =
                                                        Some(Removal::Name(name.label.clone()));
                                                }
                                            },
                                        );
                                    });
                                });
                            });
                            ui.end_row();
                        }
                        table_cell(ui, 440.0, 100.0, |ui| {
                            ui.weak("New name (advanced DNS)");
                        });
                        table_cell(ui, 560.0, 100.0, |ui| {
                            dns_editor(ui, ("new_name", content), &mut self.new_name_records);
                        });
                        table_cell(ui, 300.0, 100.0, |ui| {
                            if ui
                                .add_enabled(
                                    self.ready
                                        && !self.busy
                                        && !self.editing_name
                                        && !self.new_name_records.trim().is_empty(),
                                    egui::Button::new("Add name"),
                                )
                                .clicked()
                            {
                                let target =
                                    NameTarget::Records(self.new_name_records.trim().to_owned());
                                let label = self.automatic_name_label(&target);
                                self.send(Action::CreateName { label, target });
                            }
                        });
                        ui.end_row();
                    });
            });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        if let Some(path) = &self.import_directory {
            ui.label(format!("Ticket import folder: {}", path.display()));
        }
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
    fn handle_drop(&mut self, dropped: Vec<egui::DroppedFile>) {
        if !self.ready || self.busy {
            self.status =
                "Drop ignored: wait until connected and the current request finishes.".into();
            return;
        }
        let ticket_file = dropped.iter().position(|f| {
            let name = f
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.name.clone());
            name.ends_with(".ticket")
                || name.ends_with(".sendme")
                || (f.path.is_none() && f.mime == "text/plain")
        });
        if let Some(index) = ticket_file {
            if !self.import_open {
                self.import_id = None;
                self.import_label.clear();
            }
            if dropped.len() != 1 {
                self.status = "Drop one ticket at a time, separately from files to publish.".into();
                return;
            }
            let file = dropped[index].clone();
            let tx = self.local_tx.clone();
            self.runtime.spawn_blocking(move || {
                use std::io::Read;
                let result = (|| -> anyhow::Result<String> {
                    let bytes = if let Some(path) = file.path {
                        let mut bytes = Vec::new();
                        std::fs::File::open(path)?
                            .take(16_385)
                            .read_to_end(&mut bytes)?;
                        bytes
                    } else {
                        file.bytes
                            .context("Ticket has no readable contents")?
                            .to_vec()
                    };
                    anyhow::ensure!(bytes.len() <= 16_384, "Ticket file is too large");
                    let text = String::from_utf8(bytes)?.trim().to_owned();
                    text.parse::<BlobTicket>()?
                        .try_into()
                        .map(|_: DownloadSource| ())?;
                    Ok(text)
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(LocalUpdate::TicketFile(result));
            });
        } else if !self.local_paths {
            self.status = "For a remote daemon, paste a ticket or drop a .ticket file. Local folders require the same-filesystem setting.".into();
        } else {
            let paths: Vec<_> = dropped.into_iter().filter_map(|f| f.path).collect();
            if paths.is_empty() {
                self.status = "This drop has no filesystem paths. Paste a ticket instead.".into();
            } else {
                self.page = Page::Data;
                self.send(Action::ShareMany(paths));
            }
        }
    }

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
                ui.heading("Connect to Iroh Share"); ui.label("Paste the one-time ticket printed by your daemon. Your connection is saved for next time.");
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
                ui.strong("Iroh Share");
                for (page, title) in [(Page::Data, "Data"), (Page::Settings, "Settings")] {
                    if ui.selectable_value(&mut self.page, page, title).changed() {
                        self.name_editor_open = false;
                        self.editing_name = false;
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("page_scroll")
                .show(ui, |ui| match self.page {
                    Page::Data => {
                        egui::CollapsingHeader::new("Content")
                            .id_salt("content_panel")
                            .default_open(true)
                            .show(ui, |ui| {
                                self.data(ui);
                                self.names(ui, true);
                            });
                        ui.separator();
                        egui::CollapsingHeader::new("Names")
                            .id_salt("names_panel")
                            .default_open(false)
                            .show(ui, |ui| {
                                if ui.add_enabled(self.ready && !self.busy, egui::Button::new("Export all pkarr names…"))
                                    .on_hover_text("Save a ZIP containing all public keys, private signing keys, and current records, including names attached to content.").clicked() {
                                    let tx = self.local_tx.clone();
                                    self.runtime.spawn_blocking(move || {
                                        let path = rfd::FileDialog::new().set_title("Export pkarr names (includes private keys)").set_file_name("pkarr-names.zip").add_filter("ZIP archive", &["zip"]).save_file();
                                        let _ = tx.send(LocalUpdate::ExportNames(path));
                                    });
                                }
                                self.names(ui, false);
                            });
                    }
                    Page::Settings => self.settings(ui),
                });
        });
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            self.handle_drop(dropped);
        }

        if let Some(removal) = self.removal.clone() {
            egui::Window::new("Confirm removal").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(match &removal { Removal::Data(_) => "Stop sharing this data? Files and names are kept. Names that follow this data will no longer receive updates.", Removal::Name(_) => "Remove this name and its signing key? Cached records may remain resolvable." });
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
fn can_refresh(job: &Job, names: &BTreeMap<String, Name>) -> bool {
    matches!(job.kind, JobKind::Share { .. })
        && matches!(
            job.state,
            JobState::Seeding { .. } | JobState::Failed { .. }
        )
        && names
            .values()
            .any(|name| name.target == NameTarget::Job(job.id))
}

fn target_is_content(target: &NameTarget) -> bool {
    target.is_content()
}

fn name_status(state: &NameState) -> String {
    match state {
        NameState::Disabled => "Disabled".into(),
        NameState::WaitingForJob => "Waiting for data".into(),
        NameState::Publishing { .. } | NameState::PublishingRecords => "Publishing…".into(),
        NameState::Published { .. } | NameState::PublishedRecords { .. } => "Published".into(),
        NameState::Failed { error } => format!("Failed: {}", error.message),
    }
}

fn job_path(job: &Job) -> &PathBuf {
    match &job.kind {
        JobKind::Share { path, .. } => path,
        JobKind::Download { target, .. } => target,
    }
}
fn content_url(ticket: &BlobTicket) -> String {
    format!(
        "https://{}.blake3.net/",
        z32::encode(ticket.hash().as_bytes())
    )
}
// Grid cells center vertically; give each cell the full row height and lay out
// its contents from the top so multi-link rows keep their actions aligned.
fn table_cell<R>(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_size(egui::vec2(width, height));
            contents(ui)
        },
    )
    .inner
}

fn row_line<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        contents,
    )
    .inner
}

fn dns_editor(ui: &mut egui::Ui, id: impl std::hash::Hash, text: &mut String) {
    egui::ScrollArea::horizontal()
        .id_salt(("dns_editor_scroll", &id))
        .max_width(560.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _width: f32| {
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                ui.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(text.as_str().to_owned(), font, ui.visuals().text_color())
                })
            };
            ui.add(
                egui::TextEdit::multiline(text)
                    .id_salt(("dns_editor", &id))
                    .font(egui::TextStyle::Monospace)
                    .desired_width(560.0)
                    .desired_rows(4)
                    .layouter(&mut layouter)
                    .hint_text("@ 300 IN HTTPS 0 example.com."),
            );
        });
}

fn table_header(ui: &mut egui::Ui, columns: &[(&str, f32)]) {
    for (title, width) in columns {
        table_cell(ui, *width, 28.0, |ui| {
            ui.label(egui::RichText::new(*title).strong());
        });
    }
    ui.end_row();
}
fn table_text(ui: &mut egui::Ui, text: String, width: f32) {
    table_cell(ui, width, 24.0, |ui| {
        row_line(ui, |ui| {
            ui.add(egui::Label::new(&text).truncate())
                .on_hover_text(&text);
        });
    });
}
/// The compact label copies the full value; opening is always explicit.
fn copy_value(ui: &mut egui::Ui, value: &str, label: &str, kind: &str) -> bool {
    let id = ui.id().with(("copied", kind, value));
    let now = ui.input(|i| i.time);
    let copied = ui
        .ctx()
        .data(|d| d.get_temp::<f64>(id))
        .is_some_and(|until| now < until);
    let response = ui
        .add(
            egui::Button::new(
                egui::RichText::new(if copied { "Copied!" } else { label }).monospace(),
            )
            .truncate()
            .frame(false)
            .min_size(egui::vec2(0.0, 24.0)),
        )
        .on_hover_text(format!("Click to copy {kind}\n{value}"));
    if response.clicked() {
        ui.ctx().copy_text(value.to_owned());
        ui.ctx().data_mut(|d| d.insert_temp(id, now + 2.0));
        ui.ctx().request_repaint_after(Duration::from_secs(2));
    }
    response.clicked()
}

#[derive(Clone, Copy)]
enum Icon {
    Copy,
    Open,
    Trash,
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let response = ui
        .add(egui::Button::new("").min_size(egui::vec2(24.0, 24.0)))
        .on_hover_text(label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if ui.is_rect_visible(response.rect) {
        let painter = ui.painter();
        let stroke = ui.style().interact(&response).fg_stroke;
        let origin = response.rect.center() - egui::vec2(8.0, 8.0);
        let point = |x, y| origin + egui::vec2(x, y);
        let line = |a: (f32, f32), b: (f32, f32)| {
            painter.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
        };
        let rect = |a: (f32, f32), b: (f32, f32)| {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(a.0, a.1), point(b.0, b.1)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        };
        match icon {
            Icon::Copy => {
                line((3.0, 11.0), (1.0, 11.0));
                line((1.0, 11.0), (1.0, 1.0));
                line((1.0, 1.0), (11.0, 1.0));
                line((11.0, 1.0), (11.0, 3.0));
                rect((5.0, 5.0), (15.0, 15.0));
            }
            Icon::Open => {
                line((2.0, 6.0), (2.0, 14.0));
                line((2.0, 14.0), (10.0, 14.0));
                line((10.0, 14.0), (10.0, 10.0));
                line((2.0, 6.0), (6.0, 6.0));
                line((7.0, 9.0), (14.0, 2.0));
                line((8.0, 2.0), (14.0, 2.0));
                line((14.0, 2.0), (14.0, 8.0));
            }
            Icon::Trash => {
                line((2.0, 4.0), (14.0, 4.0));
                rect((6.0, 1.0), (10.0, 4.0));
                line((4.0, 6.0), (4.0, 15.0));
                line((4.0, 15.0), (12.0, 15.0));
                line((12.0, 15.0), (12.0, 6.0));
                line((7.0, 7.0), (7.0, 12.0));
                line((9.0, 7.0), (9.0, 12.0));
            }
        }
    }
    response
}

fn link_contents(ui: &mut egui::Ui, url: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(250.0, 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(250.0);
            copy_value(ui, url, &compact_url(url), "URL");
        },
    );
    if icon_button(ui, Icon::Copy, "Copy URL")
        .on_hover_text(url)
        .clicked()
    {
        ui.ctx().copy_text(url.to_owned());
    }
    if icon_button(ui, Icon::Open, "Open URL in browser").clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn link(ui: &mut egui::Ui, url: String) {
    row_line(ui, |ui| link_contents(ui, &url));
}

fn name_link(ui: &mut egui::Ui, name: &Name, remove: Option<bool>) -> bool {
    row_line(ui, |ui| {
        link_contents(ui, name.key.url().as_str());
        let clicked = remove.is_some_and(|enabled| {
            ui.add_enabled_ui(enabled, |ui| icon_button(ui, Icon::Trash, "Remove name…"))
                .inner
                .clicked()
        });
        let status = name_status(&name.state);
        ui.add(egui::Label::new(egui::RichText::new(&status).weak()).truncate())
            .on_hover_text(status);
        clicked
    })
}

fn abbreviate(value: &str) -> String {
    let chars: Vec<_> = value.chars().collect();
    if chars.len() <= 24 {
        return value.to_owned();
    }
    format!(
        "{}…{}",
        chars[..12].iter().collect::<String>(),
        chars[chars.len() - 6..].iter().collect::<String>()
    )
}

fn compact_url(value: &str) -> String {
    if let Ok(url) = iroh_share_proto::Url::parse(value) {
        if let Some(host) = url.host_str() {
            for suffix in [".blake3.net", ".pkarr.net"] {
                if let Some(key) = host.strip_suffix(suffix) {
                    let path = if url.path() == "/" { "" } else { url.path() };
                    return format!("{}{suffix}{}", abbreviate(key), abbreviate(path));
                }
            }
        }
    }
    abbreviate(value)
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
        JobState::Seeding { active_uploads, .. } if *active_uploads > 0 => {
            format!("Seeding · {active_uploads} active uploads")
        }
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
    fn compact_values_copy_the_original_without_opening() {
        for (value, kind) in [
            (format!("https://{}.blake3.net/", "a".repeat(52)), "URL"),
            (format!("blob{}end123", "x".repeat(180)), "ticket"),
        ] {
            let ctx = egui::Context::default();
            let label = if kind == "URL" {
                compact_url(&value)
            } else {
                abbreviate(&value)
            };
            assert!(label.contains('…'));
            assert!(!label.contains(&value));
            let mut rect = egui::Rect::NOTHING;
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 300.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    rect = ui
                        .horizontal(|ui| {
                            copy_value(ui, &value, &label, kind);
                        })
                        .response
                        .rect;
                });
            });
            let mut click = input();
            let pos = rect.left_center() + egui::vec2(8.0, 0.0);
            click.events = vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ];
            let result = ctx.run(click, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        copy_value(ui, &value, &label, kind);
                    });
                });
            });
            assert!(result
                .platform_output
                .commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == &value)));
            assert!(!result
                .platform_output
                .commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::OpenUrl(_))));
        }
    }

    #[test]
    fn content_names_include_fixed_urls_and_missing_data() {
        assert!(target_is_content(&NameTarget::Job(999)));
        let hash = iroh_share_proto::Hash::from_bytes([7; 32]);
        let root = DownloadSource::from(hash).to_string();
        assert!(target_is_content(&NameTarget::Url(
            format!("{root}index.html").parse().unwrap()
        )));
        assert!(!target_is_content(&NameTarget::Url(
            "https://example.com/".parse().unwrap()
        )));
        assert!(!target_is_content(&NameTarget::Url(
            "https://invalid.blake3.net/".parse().unwrap()
        )));
    }

    #[test]
    fn failed_name_save_preserves_draft_and_import_disconnect_clears_target() {
        let (_dir, mut app, tx, _) = fixture();
        app.name_editor_open = true;
        app.name_label = "website".into();
        app.busy = true;
        tx.try_send(Update::NameSaved(Err("network error".into())))
            .unwrap();
        app.poll();
        assert!(!app.busy);
        assert!(app.name_editor_open);
        assert_eq!(app.name_label, "website");
        app.import_open = true;
        app.import_id = Some(3);
        app.import_source = "ticket".into();
        tx.try_send(Update::Disconnected("gone".into())).unwrap();
        app.poll();
        assert!(!app.import_open);
        assert!(app.import_id.is_none());
        assert!(app.import_source.is_empty());
    }

    #[test]
    fn remote_ticket_drop_opens_review_without_starting_a_transfer() {
        let (_dir, mut app, _tx, mut commands) = fixture();
        app.ready = true;
        app.pairing = false;
        let source = DownloadSource {
            hash: iroh_share_proto::Hash::from_bytes([5; 32]),
            providers: Vec::new(),
            discovery: DiscoveryMode::Mainline,
        };
        // A bare hash is not a ticket.
        app.handle_drop(vec![egui::DroppedFile {
            name: "bad.ticket".into(),
            bytes: Some(source.to_string().into_bytes().into()),
            ..Default::default()
        }]);
        for _ in 0..100 {
            app.poll();
            if app.status.starts_with("Cannot read ticket") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.status.starts_with("Cannot read ticket"));
        assert!(!app.import_open);
        assert!(commands.try_recv().is_err());
        let endpoint = client::load_or_create_key(&_dir.path().join("drop-key"))
            .unwrap()
            .public();
        let ticket = BlobTicket::new(
            endpoint.into(),
            source.hash,
            source.hash_and_format().format,
        )
        .to_string();
        app.handle_drop(vec![egui::DroppedFile {
            name: "valid.ticket".into(),
            bytes: Some(ticket.clone().into_bytes().into()),
            ..Default::default()
        }]);
        for _ in 0..100 {
            app.poll();
            if app.import_open {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.import_source, ticket);
        assert!(app.import_open);
        assert!(!app.local_paths);
        assert!(commands.try_recv().is_err());
    }

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
    fn download_tab_completion_uses_daemon_and_keeps_only_directories() {
        let (_dir, mut app, tx, mut commands) = fixture();
        app.download_target = "/remote/d".into();
        app.path = "/share/unchanged".into();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let id = ui.make_persistent_id("download_path");
                ui.memory_mut(|m| m.request_focus(id));
                app.path_input(ui, true);
            });
        });
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                app.path_input(ui, true);
            });
        });
        let Action::CompletePath { id, path } = commands.try_recv().unwrap() else {
            panic!("expected completion RPC");
        };
        assert_eq!(path, PathBuf::from("/remote/d"));
        tx.try_send(Update::Completion {
            id,
            result: Ok(iroh_share_proto::PathCompletions {
                common_prefix: "/remote/d".into(),
                truncated: false,
                candidates: vec![
                    iroh_share_proto::PathCandidate {
                        path: "/remote/data/".into(),
                        kind: iroh_share_proto::PathKind::Directory,
                    },
                    iroh_share_proto::PathCandidate {
                        path: "/remote/document.txt".into(),
                        kind: iroh_share_proto::PathKind::File,
                    },
                ],
            }),
        })
        .unwrap();
        app.poll();
        assert_eq!(app.download_target, "/remote/data/");
        assert_eq!(app.path, "/share/unchanged");
        assert_eq!(app.candidates, vec![PathBuf::from("/remote/data/")]);
    }

    #[test]
    fn disconnect_invalidates_pending_selection_and_confirmation() {
        let (_dir, mut app, tx, mut commands) = fixture();
        tx.try_send(Update::Event(WatchEvent::JobUpdated(Box::new(Job {
            id: 3,
            kind: JobKind::Share {
                path: "/data".into(),
                include_directory_name: false,
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
            result: Ok(iroh_share_proto::PathCompletions {
                common_prefix: "/old".into(),
                candidates: vec![iroh_share_proto::PathCandidate {
                    path: "/old".into(),
                    kind: iroh_share_proto::PathKind::Directory,
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
        for page in [Page::Data, Page::Settings] {
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
