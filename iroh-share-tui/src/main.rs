mod completion;
mod daemons;
mod links;
mod model;
mod network;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use iroh_share_proto::{DownloadSource, Job, JobKind, JobState};
use model::Model;
use n0_future::StreamExt;
use network::{Action, Update};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::{io::IsTerminal, path::PathBuf, time::Duration};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(about = "Terminal interface for the iroh-share daemon")]
struct Args {
    /// Pair with the daemon using its printed one-client ticket, then open the TUI.
    #[arg(value_name = "PAIRING_TICKET", conflicts_with = "endpoint")]
    ticket: Option<iroh_share_proto::PairingTicket>,
    /// Override the platform-specific iroh-share-tui config directory.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Save the daemon endpoint ID to connect to, including on future starts.
    #[arg(long)]
    endpoint: Option<iroh_share_proto::EndpointId>,
    /// Print this client's persistent endpoint ID and exit (for authorization).
    #[arg(long)]
    print_id: bool,
}

#[derive(Default, Clone)]
enum Input {
    #[default]
    Browse,
    Share(String),
    Source(String),
    Import {
        value: String,
        id: Option<u64>,
    },
    Target {
        source: DownloadSource,
        value: String,
    },
    Remove(u64),
    NameLabel {
        value: String,
        job: Option<u64>,
    },
    NameTarget {
        label: String,
        value: String,
        create: bool,
    },
    NameData {
        label: String,
        create: bool,
    },
    RemoveName(String),
}

#[derive(Default)]
struct App {
    model: Model,
    names_view: bool,
    daemons: daemons::Page,
    config_dir: PathBuf,
    /// The current daemon changed; the network worker must restart.
    reconnect: bool,
    pair_request: Option<iroh_share_proto::PairingTicket>,
    client_id: Option<iroh_share_proto::EndpointId>,
    server_id: Option<iroh_share_proto::EndpointId>,
    link_action: Option<links::Action>,
    input: Input,
    completion: completion::Completion,
    table: TableState,
    status: String,
    busy: bool,
    details_scroll: u16,
    downloads_open: bool,
    include_directory_name: bool,
    import_directory: Option<PathBuf>,
    pending_input: Option<Input>,
}

impl App {
    fn update(&mut self, update: Update) {
        match update {
            Update::ImportDirectory(path) => self.import_directory = Some(path),
            Update::Added { job, new } => {
                self.busy = false;
                self.pending_input = None;
                let id = job.id;
                self.model.jobs.entry(id).or_insert(job);
                self.model.selected = Some(id);
                self.names_view = false;
                self.status = if new {
                    "Content added. Give it a name, or press Esc to skip."
                } else {
                    "Update started. Linked names will follow when ready."
                }
                .into();
                if new {
                    self.input = Input::NameLabel {
                        value: String::new(),
                        job: Some(id),
                    };
                }
            }
            Update::Completion { id, result } => {
                if let Input::Share(value) | Input::Target { value, .. } = &mut self.input {
                    if let Err(error) = self.completion.receive(id, result, value) {
                        self.status = format!("Cannot complete path: {error}");
                    }
                }
            }
            Update::Connecting => {
                self.reset_session();
                self.status = "Connecting to daemon...".into();
            }
            Update::Disconnected(error) => {
                self.reset_session();
                self.status = format!("Disconnected: {error}. Retrying...");
            }
            Update::Event(event) => {
                let was_ready = self.model.ready;
                self.model.apply(event);
                if !was_ready && self.model.ready {
                    self.status = "Connected".into();
                }
            }
            Update::ActionResult(result) => {
                self.busy = false;
                match result {
                    Ok(message) => {
                        self.status = message;
                        self.pending_input = None;
                    }
                    Err(error) => {
                        self.status = error;
                        if let Some(input) = self.pending_input.take() {
                            self.input = input;
                        }
                    }
                }
            }
        }
    }

    fn reset_session(&mut self) {
        self.model.reset();
        self.import_directory = None;
        self.pending_input = None;
        self.input = Input::Browse;
        self.completion.reset();
        self.busy = false;
    }

    /// Reloads the saved daemons after the current one changed and reconnects.
    fn daemon_changed(&mut self, current: Option<iroh_share_proto::EndpointId>) {
        let daemons = iroh_share_proto::client::saved_daemons(&self.config_dir).unwrap_or_default();
        self.daemons.load(daemons, current);
        self.server_id = current;
        self.reconnect = true;
        self.reset_session();
        self.names_view = false;
        match current {
            Some(_) => self.daemons.open = false,
            None => {
                self.daemons.add();
                self.status = "No daemons left. Paste a pairing ticket to add one.".into();
            }
        }
    }

    fn paired(&mut self, result: Result<iroh_share_proto::EndpointId, String>) {
        self.daemons.pairing = false;
        match result {
            Ok(id) => {
                self.daemons.status = "Daemon added. Press n to name it.".into();
                self.daemon_changed(Some(id));
            }
            Err(error) => self.daemons.status = format!("Pairing failed: {error}"),
        }
    }

    fn daemon_key(&mut self, key: KeyEvent) -> bool {
        let config = self.config_dir.clone();
        match self.daemons.key(key) {
            daemons::Action::None => {}
            daemons::Action::Back => self.daemons.open = false,
            daemons::Action::Quit => return true,
            daemons::Action::Switch(id) => {
                match iroh_share_proto::client::select_daemon(&config, id) {
                    Ok(()) => self.daemon_changed(Some(id)),
                    Err(error) => self.daemons.status = format!("{error:#}"),
                }
            }
            daemons::Action::Pair(ticket) => self.pair_request = Some(ticket),
            daemons::Action::Rename(id, name) => {
                match iroh_share_proto::client::rename_daemon(&config, id, Some(name)) {
                    Ok(()) => {
                        let current = self.daemons.current;
                        let daemons =
                            iroh_share_proto::client::saved_daemons(&config).unwrap_or_default();
                        self.daemons.load(daemons, current);
                        self.daemons.status = "Name saved".into();
                    }
                    Err(error) => self.daemons.status = format!("{error:#}"),
                }
            }
            daemons::Action::Forget(id) => {
                match iroh_share_proto::client::forget_daemon(&config, id) {
                    Ok(current) if current == self.daemons.current => {
                        let daemons =
                            iroh_share_proto::client::saved_daemons(&config).unwrap_or_default();
                        self.daemons.load(daemons, current);
                        self.daemons.status = "Daemon forgotten".into();
                    }
                    Ok(current) => {
                        self.daemon_changed(current);
                        self.daemons.status = "Daemon forgotten".into();
                    }
                    Err(error) => self.daemons.status = format!("{error:#}"),
                }
            }
        }
        false
    }

    fn submit(&mut self, action: Action, tx: &mpsc::Sender<Action>) {
        if !self.model.ready || self.busy {
            return;
        }
        match tx.try_send(action) {
            Ok(()) => {
                self.busy = true;
                self.status = "Request in progress...".into();
            }
            Err(error) => self.status = format!("Cannot submit request: {error}"),
        }
    }

    fn key(&mut self, key: KeyEvent, tx: &mpsc::Sender<Action>) -> bool {
        if matches!(self.input, Input::Share(_))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('r')
        {
            self.include_directory_name = !self.include_directory_name;
            return false;
        }
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.daemons.open {
            return self.daemon_key(key);
        }
        if matches!(self.input, Input::Browse)
            && key.code == KeyCode::Char('m')
            && key.modifiers.is_empty()
        {
            self.daemons.open = true;
            return false;
        }
        if matches!(self.input, Input::Browse)
            && key.modifiers.is_empty()
            && matches!(key.code, KeyCode::Char('c' | 'o' | 't'))
        {
            if key.code == KeyCode::Char('t') {
                self.link_action = if !self.names_view {
                    self.model
                        .selected
                        .and_then(|id| self.model.jobs.get(&id))
                        .and_then(|job| {
                            if let JobState::Seeding { ticket, .. } = &job.state {
                                Some(links::Action::Copy {
                                    text: ticket.to_string(),
                                    label: "Ticket",
                                })
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };
                if self.link_action.is_none() {
                    self.status = "No ticket available for this selection".into();
                }
                return false;
            }
            if let Some(url) = links::selected_url(&self.model, self.names_view) {
                self.link_action = Some(if key.code == KeyCode::Char('c') {
                    links::Action::Copy {
                        text: url.to_string(),
                        label: "URL",
                    }
                } else {
                    links::Action::Open(url)
                });
            } else {
                self.status = "No URL available for this selection".into();
            }
            return false;
        }
        if matches!(self.input, Input::Browse) && !self.busy {
            match key.code {
                KeyCode::Char('N') => {
                    self.model.raw_names = !self.model.raw_names;
                    self.names_view = self.model.raw_names;
                    self.model.select_visible_name();
                    return false;
                }
                KeyCode::Char('D') => {
                    self.downloads_open = !self.downloads_open;
                    return false;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Tab && matches!(self.input, Input::Browse) {
            if self.names_view {
                self.names_view = false;
            } else {
                self.names_view = true;
                self.model.raw_names = false;
                self.model.select_visible_name();
            }
            self.details_scroll = 0;
            return false;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            if self.model.ready {
                if let Input::Share(value) | Input::Target { value, .. } = &mut self.input {
                    if let Some(id) = self
                        .completion
                        .complete(value, key.code == KeyCode::BackTab)
                    {
                        if let Err(error) = tx.try_send(Action::CompletePath {
                            id,
                            path: PathBuf::from(value.as_str()),
                        }) {
                            self.completion.reset();
                            self.status = format!("Cannot request completion: {error}");
                        }
                    }
                }
            }
            return false;
        }
        self.completion.reset();
        if key.code == KeyCode::Esc {
            self.input = Input::Browse;
            return false;
        }
        if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Input::NameTarget { label, create, .. } = &self.input {
                self.input = Input::NameData {
                    label: label.clone(),
                    create: *create,
                };
                return false;
            }
        }
        match &mut self.input {
            Input::NameData { .. } => match key.code {
                KeyCode::Up => self.model.step(false),
                KeyCode::Down => self.model.step(true),
                KeyCode::Enter => self.accept_input(tx),
                _ => {}
            },
            Input::Browse => match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.names_view {
                        self.model.step_name(true);
                    } else {
                        self.model.step(true);
                    }
                    self.details_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.names_view {
                        self.model.step_name(false);
                    } else {
                        self.model.step(false);
                    }
                    self.details_scroll = 0;
                }
                KeyCode::PageDown => self.details_scroll = self.details_scroll.saturating_add(3),
                KeyCode::PageUp => self.details_scroll = self.details_scroll.saturating_sub(3),
                KeyCode::Char('i') if self.model.ready && !self.busy => {
                    self.input = Input::Import {
                        value: String::new(),
                        id: None,
                    };
                }
                KeyCode::Char('u') if !self.names_view && self.model.ready && !self.busy => {
                    if let Some(id) = self.model.selected {
                        if self.model.jobs.get(&id).is_some_and(|job| {
                            matches!(
                                job.state,
                                JobState::Seeding { .. } | JobState::Failed { .. }
                            )
                        }) {
                            self.input = Input::Import {
                                value: String::new(),
                                id: Some(id),
                            };
                        } else {
                            self.status = "Wait for the current transfer to finish".into();
                        }
                    }
                }
                KeyCode::Char('r') if !self.names_view && self.model.ready && !self.busy => {
                    if let Some(id) = self.model.selected {
                        if self
                            .model
                            .jobs
                            .get(&id)
                            .is_some_and(|job| matches!(job.kind, JobKind::Share { .. }))
                            && self
                                .model
                                .names
                                .values()
                                .any(|name| name.target == iroh_share_proto::NameTarget::Job(id))
                        {
                            self.submit(Action::Refresh(id), tx);
                        } else {
                            self.status =
                                "Refresh requires a shared local directory with a name".into();
                        }
                    }
                }
                KeyCode::Char('s') if self.model.ready && !self.busy => {
                    self.input = Input::Share(String::new())
                }
                KeyCode::Char('d') if self.downloads_open && self.model.ready && !self.busy => {
                    self.input = Input::Source(String::new())
                }
                KeyCode::Char('n') if self.model.ready && !self.busy => {
                    self.input = Input::NameLabel {
                        value: String::new(),
                        job: if self.names_view {
                            None
                        } else {
                            self.model.selected
                        },
                    };
                }
                KeyCode::Char('e') if self.names_view && self.model.ready && !self.busy => {
                    if let Some(label) = self.model.selected_name.clone() {
                        let value = self
                            .model
                            .names
                            .get(&label)
                            .map(|name| match &name.target {
                                iroh_share_proto::NameTarget::Records(text) => text.clone(),
                                iroh_share_proto::NameTarget::Url(url) => url.to_string(),
                                iroh_share_proto::NameTarget::Job(id) => format!("data:{id}"),
                            })
                            .unwrap_or_default();
                        if let Some(iroh_share_proto::Name {
                            target: iroh_share_proto::NameTarget::Job(id),
                            ..
                        }) = self.model.names.get(&label)
                        {
                            self.model.selected = Some(*id);
                        }
                        self.input = if self.model.names.get(&label).is_some_and(|name| {
                            matches!(name.target, iroh_share_proto::NameTarget::Job(_))
                        }) {
                            Input::NameData {
                                label,
                                create: false,
                            }
                        } else {
                            Input::NameTarget {
                                label,
                                value,
                                create: false,
                            }
                        };
                    }
                }
                KeyCode::Char('x') if self.names_view && self.model.ready && !self.busy => {
                    if let Some(label) = self.model.selected_name.clone() {
                        self.input = Input::RemoveName(label);
                    }
                }
                KeyCode::Char('x') if self.model.ready && !self.busy => {
                    if let Some(id) = self.model.selected {
                        self.input = Input::Remove(id);
                    }
                }
                _ => {}
            },
            Input::RemoveName(label) => match key.code {
                KeyCode::Char('y') => {
                    let label = label.clone();
                    self.input = Input::Browse;
                    self.submit(Action::RemoveName(label), tx);
                }
                KeyCode::Char('n') => self.input = Input::Browse,
                _ => {}
            },
            Input::Remove(id) => match key.code {
                KeyCode::Char('y') => {
                    let id = *id;
                    self.input = Input::Browse;
                    self.completion.reset();
                    self.submit(Action::Remove(id), tx);
                }
                KeyCode::Char('n') => self.input = Input::Browse,
                _ => {}
            },
            Input::Share(value)
            | Input::Source(value)
            | Input::Import { value, .. }
            | Input::Target { value, .. }
            | Input::NameLabel { value, .. }
            | Input::NameTarget { value, .. } => match key.code {
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    value.push(c)
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Enter => self.accept_input(tx),
                _ => {}
            },
        }
        false
    }

    fn paste(&mut self, text: String) {
        if self.daemons.open {
            self.daemons.paste(text);
            return;
        }
        self.completion.reset();
        match &mut self.input {
            Input::Share(value)
            | Input::Source(value)
            | Input::Import { value, .. }
            | Input::Target { value, .. }
            | Input::NameLabel { value, .. }
            | Input::NameTarget { value, .. } => {
                value.extend(text.chars().filter(|c| !c.is_control()))
            }
            _ => {}
        }
    }

    fn accept_input(&mut self, tx: &mpsc::Sender<Action>) {
        if self.busy || !self.model.ready {
            return;
        }
        let draft = self.input.clone();
        match std::mem::take(&mut self.input) {
            Input::Share(value) if !value.is_empty() => self.submit(
                if self.include_directory_name {
                    Action::ShareWithDirectoryName(value.into())
                } else {
                    Action::Share(value.into())
                },
                tx,
            ),
            Input::Import { value, id } => {
                match value
                    .trim()
                    .parse::<iroh_share_proto::BlobTicket>()
                    .map_err(|e| e.to_string())
                    .and_then(|ticket| DownloadSource::try_from(ticket).map_err(|e| e.to_string()))
                {
                    Ok(source) => self.submit(Action::Import { source, id }, tx),
                    Err(error) => {
                        self.status = format!("Invalid ticket: {error}");
                        self.input = Input::Import { value, id };
                    }
                }
            }
            Input::NameData { label, create } => {
                if let Some(id) = self
                    .model
                    .selected
                    .filter(|id| self.model.jobs.contains_key(id))
                {
                    let target = iroh_share_proto::NameTarget::Job(id);
                    self.submit(
                        if create {
                            Action::CreateName { label, target }
                        } else {
                            Action::UpdateName { label, target }
                        },
                        tx,
                    );
                    self.names_view = true;
                    self.model.raw_names = false;
                } else {
                    self.status = "Select a data path first".into();
                    self.input = Input::NameData { label, create };
                }
            }
            Input::Source(value) => match value.trim().parse() {
                Ok(source) => {
                    self.input = Input::Target {
                        source,
                        value: String::new(),
                    }
                }
                Err(error) => {
                    self.status = format!("Invalid download source: {error}");
                    self.input = Input::Source(value);
                }
            },
            Input::Target { source, value } if !value.is_empty() => self.submit(
                Action::Download {
                    source,
                    target: value.into(),
                },
                tx,
            ),
            Input::NameLabel { value, job } if !value.is_empty() => {
                if let Some(id) = job {
                    self.submit(
                        Action::CreateName {
                            label: value,
                            target: iroh_share_proto::NameTarget::Job(id),
                        },
                        tx,
                    );
                    self.names_view = true;
                    self.model.raw_names = false;
                } else {
                    self.input = Input::NameTarget {
                        label: value,
                        value: String::new(),
                        create: true,
                    };
                }
            }
            Input::NameTarget {
                label,
                value,
                create,
            } => {
                let target = value
                    .parse()
                    .map(iroh_share_proto::NameTarget::Url)
                    .map_err(|e| format!("Invalid URL: {e}"));
                match target {
                    Ok(target) => {
                        self.model.raw_names = !target.is_content();
                        self.names_view = true;
                        self.submit(
                            if create {
                                Action::CreateName { label, target }
                            } else {
                                Action::UpdateName { label, target }
                            },
                            tx,
                        );
                    }
                    Err(error) => {
                        self.status = error;
                        self.input = Input::NameTarget {
                            label,
                            value,
                            create,
                        };
                    }
                }
            }
            input => self.input = input,
        }
        if self.busy {
            self.pending_input = Some(draft);
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        if self.daemons.open {
            self.daemons.draw(frame);
            return;
        }
        let [heading, jobs, details, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(8),
            Constraint::Length(5),
        ])
        .areas(frame.area());
        let connection = if self.model.ready {
            "connected"
        } else {
            "connecting"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                "iroh-share ".bold().cyan(),
                format!(
                    "  {} · {connection} · {} items",
                    clean(&self.daemons.current_name().unwrap_or_default()),
                    self.model.jobs.len()
                )
                .into(),
            ])),
            heading,
        );
        if !self.names_view {
            let rows = self.model.jobs.values().map(|job| {
                let (state, progress) = summary(&job.state);
                Row::new(vec![
                    state.into(),
                    progress,
                    description(job),
                    self.model
                        .names
                        .values()
                        .filter(|name| name.target == iroh_share_proto::NameTarget::Job(job.id))
                        .map(|name| format!("{} ({})", name.label, name_status(&name.state)))
                        .collect::<Vec<_>>()
                        .join(", "),
                ])
            });
            self.table.select(
                self.model
                    .jobs
                    .keys()
                    .position(|id| Some(*id) == self.model.selected),
            );
            let table = Table::new(
                rows,
                [
                    Constraint::Length(13),
                    Constraint::Length(20),
                    Constraint::Min(10),
                    Constraint::Percentage(25),
                ],
            )
            .header(Row::new(["State", "Progress", "Path", "Names"]).bold())
            .block(Block::bordered().title(" Data "))
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .highlight_symbol("› ");
            frame.render_stateful_widget(table, jobs, &mut self.table);
        } else {
            let rows = self.model.visible_names().map(|name| {
                Row::new(vec![
                    name.label.clone(),
                    name_status(&name.state).into(),
                    match &name.target {
                        iroh_share_proto::NameTarget::Records(_) => "DNS records".into(),
                        iroh_share_proto::NameTarget::Url(url) => compact_url(url.as_str()),
                        iroh_share_proto::NameTarget::Job(id) => self
                            .model
                            .jobs
                            .get(id)
                            .map(description)
                            .unwrap_or_else(|| format!("Data {id} unavailable")),
                    },
                ])
            });
            self.table.select(
                self.model
                    .visible_names()
                    .position(|name| Some(&name.label) == self.model.selected_name.as_ref()),
            );
            let table = Table::new(
                rows,
                [
                    Constraint::Length(20),
                    Constraint::Length(15),
                    Constraint::Min(10),
                ],
            )
            .header(Row::new(["Name", "Publication", "Target"]).bold())
            .block(Block::bordered().title(if self.model.raw_names {
                " Names · standalone URLs · N collapse "
            } else {
                " Data · content names · Tab data "
            }))
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .highlight_symbol("› ");
            frame.render_stateful_widget(table, jobs, &mut self.table);
        }
        let enrollment_id = self.client_id.filter(|_| !self.model.ready);
        let detail = if let Some(client_id) = enrollment_id {
            format!("Client endpoint: {}\nDaemon endpoint: {}\n\nGet a ticket on the daemon machine: iroh-share control pair\nThen run: iroh-share-tui <pairing-ticket>\nThe TUI retries automatically.", client_id, self.server_id.map(|id| id.to_string()).unwrap_or_else(|| "not configured".into()))
        } else if self.names_view {
            self.model
                .selected_name
                .as_ref()
                .and_then(|label| self.model.names.get(label))
                .map(|name| {
                    let status = match &name.state {
                        iroh_share_proto::NameState::Published { url, .. }
                        | iroh_share_proto::NameState::Publishing { url } => {
                            compact_url(url.as_str())
                        }
                        iroh_share_proto::NameState::Failed { error } => error.message.clone(),
                        _ => String::new(),
                    };
                    format!(
                        "URL: {}  [c copy]\n{} · {}\n{}",
                        compact_url(name.key.url().as_str()),
                        clean(&name.label),
                        name_status(&name.state),
                        clean(&status)
                    )
                })
                .unwrap_or_else(|| {
                    "Press n to create a name, or select data and press n to name it.".into()
                })
        } else {
            self.model
                .selected
                .and_then(|id| self.model.jobs.get(&id))
                .map(|job| {
                    let mut details = job_details(job);
                    for name in self
                        .model
                        .names
                        .values()
                        .filter(|name| name.target == iroh_share_proto::NameTarget::Job(job.id))
                    {
                        details.push_str(&format!(
                            "\n{} · {} · {}",
                            clean(&name.label),
                            name_status(&name.state),
                            name.key.url()
                        ));
                    }
                    details
                })
                .unwrap_or_else(|| {
                    "No data selected. Press s to publish a local path or i to import a ticket."
                        .into()
                })
        };
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: false })
                .scroll((self.details_scroll, 0))
                .block(Block::bordered().title(" Details · PgUp/PgDn to scroll ")),
            details,
        );
        let [actions, navigation, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(footer);
        frame.render_widget(
            Paragraph::new(if self.names_view {
                "n Create · e Retarget · x Remove · c Copy URL · o Open · ↑/↓ Select"
            } else {
                "s Publish · i Import · u Update · r Refresh · n Name · c Copy · t Ticket · o Open · x Remove"
            }).cyan(),
            actions,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Tab {} · m Daemons · N Names {} · D Downloads {}{} · q Quit",
                if self.names_view {
                    "Data"
                } else {
                    "Content names"
                },
                if self.model.raw_names { "▼" } else { "▶" },
                if self.downloads_open { "▼" } else { "▶" },
                if self.downloads_open {
                    " (d download)"
                } else {
                    ""
                },
            )),
            navigation,
        );
        frame.render_widget(
            Paragraph::new(clean(&self.status)).wrap(Wrap { trim: false }),
            status,
        );
        let prompt = match &self.input {
            Input::Browse => None,
            Input::NameLabel { value, job } => Some((
                if job.is_some() {
                    "Name for selected data"
                } else {
                    "New name label"
                },
                value.clone(),
            )),
            Input::NameData { .. } => Some(("Follow data · Up/Down to select", self.model.selected.and_then(|id| self.model.jobs.get(&id)).map(description).unwrap_or_else(|| "No data available".into()))),
            Input::NameTarget { value, .. } => Some(("Target URL · Ctrl+D to select data", value.clone())),
            Input::RemoveName(label) => Some((
                "Remove name",
                format!("Remove {label} and its key? Published records expire later. [y/n]"),
            )),
            Input::Share(value) => Some(("Share path", value.clone())),
            Input::Import { value, id } => Some((if id.is_some() { "Update from ticket · names are kept" } else { "Import ticket" }, value.clone())),
            Input::Source(value) => Some(("Download URL, hash, or ticket", value.clone())),
            Input::Target { value, .. } => Some(("Download target directory", value.clone())),
            Input::Remove(_) => Some((
                "Remove data",
                "Stop sharing this data? Files and names are kept; names stop following updates. [y/n]".into(),
            )),
        };
        if let Some((title, value)) = prompt {
            let [_, middle, _] = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(9),
                Constraint::Fill(1),
            ])
            .areas(frame.area());
            let [_, area, _] = Layout::horizontal([
                Constraint::Percentage(10),
                Constraint::Percentage(80),
                Constraint::Percentage(10),
            ])
            .areas(middle);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!(
                    "{}▏\n\n{}\nEnter to submit · Esc to cancel",
                    clean(&value),
                    if matches!(self.input, Input::Share(_) | Input::Target { .. }) {
                        if matches!(self.input, Input::Share(_)) {
                            format!(
                                "{} · Ctrl+R: include directory name [{}]",
                                clean(&self.completion.hint()),
                                if self.include_directory_name {
                                    "on"
                                } else {
                                    "off"
                                }
                            )
                        } else {
                            clean(&self.completion.hint())
                        }
                    } else if matches!(self.input, Input::Import { .. }) {
                        self.import_directory
                            .as_ref()
                            .map(|path| format!("Import folder on daemon: {}", path.display()))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    }
                ))
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(title)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
                area,
            );
        }
    }
}

fn name_status(state: &iroh_share_proto::NameState) -> &'static str {
    use iroh_share_proto::NameState::*;
    match state {
        Disabled => "Disabled",
        WaitingForJob => "Waiting for data",
        NoRecords => "No records yet",
        Publishing { .. } | PublishingRecords => "Publishing",
        Published { .. } | PublishedRecords { .. } => "Published",
        Failed { .. } => "Failed",
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn description(job: &Job) -> String {
    clean(&match &job.kind {
        JobKind::Share { path, .. } => path.display().to_string(),
        JobKind::Download { target, .. } => target.display().to_string(),
    })
}

fn counts(done: u64, total: u64, files: u64, all_files: u64) -> String {
    format!("{done}/{total} B · {files}/{all_files} files")
}

fn summary(state: &JobState) -> (&'static str, String) {
    match state {
        JobState::Queued => ("Queued", String::new()),
        JobState::Importing { progress: p } => (
            "Importing",
            counts(p.bytes_done, p.bytes_total, p.files_done, p.files_total),
        ),
        JobState::Downloading { progress: p, .. } => (
            "Downloading",
            match p.bytes_total {
                Some(total) => format!("{}/{total} B", p.bytes_done),
                None => format!("{} B", p.bytes_done),
            },
        ),
        JobState::Exporting { progress: p, .. } => (
            "Exporting",
            counts(p.bytes_done, p.bytes_total, p.files_done, p.files_total),
        ),
        JobState::Seeding { active_uploads, .. } if *active_uploads > 0 => {
            ("Seeding", format!("{active_uploads} active uploads"))
        }
        JobState::Seeding { .. } => ("Seeding", String::new()),
        JobState::Failed { error } => ("Failed", clean(&error.message)),
    }
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

fn job_details(job: &Job) -> String {
    let (state, progress) = summary(&job.state);
    let extra = match &job.state {
        JobState::Seeding { ticket, .. } => {
            return format!(
                "{}\n{state} {progress}\nURL: {}.blake3.net  [c copy]\nTicket: {}  [t copy]",
                description(job),
                abbreviate(&z32::encode(ticket.hash().as_bytes())),
                abbreviate(&ticket.to_string()),
            );
        }
        JobState::Downloading { source, .. } => {
            format!(
                "Hash: {}\nSource: {}",
                abbreviate(&source.hash().to_string()),
                abbreviate(&source.to_string())
            )
        }
        JobState::Exporting { root_hash, .. } => {
            format!("Hash: {}", abbreviate(&root_hash.to_string()))
        }
        JobState::Failed { error } => format!("Error: {}", clean(&error.message)),
        _ => String::new(),
    };
    format!("{}\n{state} {progress}\n{extra}", description(job))
}

fn default_config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|base| base.join("iroh-share-tui"))
        .context("cannot determine the user config directory; provide --config-dir")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config_dir = match args.config_dir {
        Some(path) => path,
        None => default_config_dir()?,
    };
    if !args.print_id {
        anyhow::ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "iroh-share-tui requires an interactive terminal"
        );
    }
    if let Some(ticket) = args.ticket {
        iroh_share_proto::client::pair(&config_dir, &ticket).await?;
    }
    if let Some(endpoint) = args.endpoint {
        iroh_share_proto::client::configure_endpoint(&config_dir, Some(endpoint))?;
    }
    let key = iroh_share_proto::client::load_or_create_key(&config_dir.join("control-client.key"))?;
    if args.print_id {
        println!("{}", key.public());
        return Ok(());
    }
    if iroh_share_proto::client::configured_endpoint(&config_dir)?.is_none() {
        use std::io::Write;
        eprintln!(
            "Paste the ticket printed by the daemon, or create one with iroh-share control pair."
        );
        loop {
            eprint!("Pairing ticket: ");
            std::io::stderr().flush()?;
            let mut input = String::new();
            anyhow::ensure!(
                std::io::stdin().read_line(&mut input)? > 0,
                "pairing input closed"
            );
            let ticket = match input.trim().parse::<iroh_share_proto::PairingTicket>() {
                Ok(ticket) => ticket,
                Err(_) => {
                    eprintln!("Invalid ticket. Paste the complete iroh-share ticket.");
                    continue;
                }
            };
            match iroh_share_proto::client::pair(&config_dir, &ticket).await {
                Ok(()) => break,
                Err(error) => eprintln!("Pairing failed: {error:#}"),
            }
        }
    }
    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, config_dir).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut ratatui::DefaultTerminal, config_dir: PathBuf) -> Result<()> {
    let (mut actions_tx, actions_rx) = mpsc::channel(8);
    let (updates_tx, mut updates_rx) = mpsc::channel(256);
    let mut worker = Some(tokio::spawn(network::run(
        config_dir.clone(),
        actions_rx,
        updates_tx,
    )));
    // Holds the update channel open while no daemon is saved and no worker runs.
    let mut idle_updates = None;
    let (paired_tx, mut paired_rx) = mpsc::channel(1);
    let mut events = EventStream::new();
    let server_id = iroh_share_proto::client::configured_endpoint(&config_dir)?;
    let mut app = App {
        client_id: Some(
            iroh_share_proto::client::load_or_create_key(&config_dir.join("control-client.key"))?
                .public(),
        ),
        server_id,
        config_dir: config_dir.clone(),
        ..Default::default()
    };
    app.daemons.load(
        iroh_share_proto::client::saved_daemons(&config_dir)?,
        server_id,
    );
    let (link_tx, mut link_results) = links::worker();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let result = async {
        loop {
            tokio::select! {
                _ = tick.tick() => { terminal.draw(|frame| app.draw(frame))?;
                    if terminal.size()?.height > 1 { links::draw_link(links::selected_url(&app.model, app.names_view).as_ref(), terminal.size()?.width)?; } }
                Some(outcome) = link_results.recv() => { app.status = links::complete(outcome); }
                Some(result) = paired_rx.recv() => app.paired(result),
                update = updates_rx.recv() => { app.update(update.context("connection worker stopped")?); }
                event = events.next() => match event.context("terminal input closed")?? {
                    Event::Key(key) => if app.key(key, &actions_tx) { return Ok(()); },
                    Event::Paste(text) => app.paste(text),
                    Event::Resize(_, _) => { terminal.draw(|frame| app.draw(frame))?;
                    if terminal.size()?.height > 1 { links::draw_link(links::selected_url(&app.model, app.names_view).as_ref(), terminal.size()?.width)?; } }
                    _ => {}
                }
            }
            if let Some(action) = app.link_action.take() {
                if link_tx.try_send(action).is_err() {
                    app.status = "Clipboard/browser action queue is busy".into();
                }
            }
            if let Some(ticket) = app.pair_request.take() {
                let config_dir = config_dir.clone();
                let paired_tx = paired_tx.clone();
                tokio::spawn(async move {
                    let id = ticket.addr.id;
                    let result = iroh_share_proto::client::pair(&config_dir, &ticket)
                        .await
                        .map(|()| id)
                        .map_err(|error| format!("{error:#}"));
                    let _ = paired_tx.send(result).await;
                });
            }
            if std::mem::take(&mut app.reconnect) {
                if let Some(worker) = worker.take() {
                    worker.abort();
                }
                // Fresh channels drop updates and commands meant for the previous daemon.
                let (tx, rx) = mpsc::channel(8);
                let (updates, receiver) = mpsc::channel(256);
                actions_tx = tx;
                updates_rx = receiver;
                if app.daemons.current.is_some() {
                    idle_updates = None;
                    worker = Some(tokio::spawn(network::run(config_dir.clone(), rx, updates)));
                } else {
                    idle_updates = Some(updates);
                }
            }
        }
    }.await;
    drop(idle_updates);
    if let Some(worker) = worker {
        worker.abort();
        let _ = worker.await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_share_proto::{JobError, WatchEvent};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn abbreviated_ticket_copies_in_full_and_does_not_intercept_input() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let id = iroh_share_proto::client::load_or_create_key(&dir.path().join("key"))?.public();
        let hash = iroh_share_proto::Hash::new(b"collection");
        let ticket = iroh_share_proto::BlobTicket::new(
            id.into(),
            hash,
            DownloadSource::from(hash).hash_and_format().format,
        );
        let full = ticket.to_string();
        let job = Job {
            id: 1,
            kind: JobKind::Share {
                path: "demo".into(),
                include_directory_name: false,
            },
            state: JobState::Seeding {
                ticket,
                active_uploads: 0,
            },
        };
        let detail = job_details(&job);
        assert!(detail.contains(&abbreviate(&full)));
        assert!(!detail.contains(&full));
        let mut app = App::default();
        app.update(Update::Event(WatchEvent::JobUpdated(Box::new(job))));
        let (tx, _) = mpsc::channel(8);
        app.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE), &tx);
        assert!(
            matches!(app.link_action.take(), Some(links::Action::Copy { text, label: "Ticket" }) if text == full)
        );
        app.names_view = true;
        app.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE), &tx);
        assert!(app.link_action.is_none());
        app.input = Input::Source(String::new());
        app.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE), &tx);
        assert!(matches!(&app.input, Input::Source(text) if text == "t"));
        Ok(())
    }

    #[test]
    fn publishing_import_retries_keep_ticket_and_secondary_sections_start_closed() -> Result<()> {
        let mut app = App::default();
        assert!(!app.model.raw_names);
        assert!(!app.downloads_open);
        let (tx, mut rx) = mpsc::channel(8);
        app.update(Update::Event(WatchEvent::SnapshotComplete));
        app.key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &tx);
        let temp = tempfile::tempdir()?;
        let endpoint =
            iroh_share_proto::client::load_or_create_key(&temp.path().join("key"))?.public();
        let hash = iroh_share_proto::Hash::from_bytes([3; 32]);
        let ticket = iroh_share_proto::BlobTicket::new(
            endpoint.into(),
            hash,
            DownloadSource::from(hash).hash_and_format().format,
        )
        .to_string();
        app.paste(ticket.clone());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(
            matches!(rx.try_recv()?, Action::Import { source, id: None } if source.hash == hash)
        );
        assert!(app.busy);
        app.update(Update::ActionResult(Err("try again".into())));
        assert!(matches!(&app.input, Input::Import { value, id: None } if value == &ticket));
        app.update(Update::Disconnected("offline".into()));
        assert!(matches!(app.input, Input::Browse));
        assert!(app.pending_input.is_none());
        Ok(())
    }

    #[test]
    fn names_are_separated_and_refresh_requires_a_linked_name() {
        use iroh_share_proto::{Name, NameKey, NameState, NameTarget};
        let mut app = App::default();
        let (tx, mut rx) = mpsc::channel(8);
        app.update(Update::Event(WatchEvent::JobUpdated(Box::new(Job {
            id: 1,
            kind: JobKind::Share {
                path: "/site".into(),
                include_directory_name: false,
            },
            state: JobState::Failed {
                error: iroh_share_proto::JobError {
                    message: "retry".into(),
                },
            },
        }))));
        app.update(Update::Event(WatchEvent::SnapshotComplete));
        app.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &tx);
        assert!(rx.try_recv().is_err());
        for (label, target) in [
            ("content", NameTarget::Job(1)),
            (
                "redirect",
                NameTarget::Url("https://example.com/".parse().unwrap()),
            ),
        ] {
            app.update(Update::Event(WatchEvent::NameUpdated(Box::new(Name {
                label: label.into(),
                key: NameKey([1; 32]),
                target,
                state: NameState::Disabled,
            }))));
        }
        assert_eq!(
            app.model
                .visible_names()
                .map(|n| n.label.as_str())
                .collect::<Vec<_>>(),
            vec!["content"]
        );
        app.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &tx);
        assert!(matches!(rx.try_recv(), Ok(Action::Refresh(1))));
        app.update(Update::ActionResult(Ok("refreshed".into())));
        app.key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT), &tx);
        assert_eq!(
            app.model
                .visible_names()
                .map(|n| n.label.as_str())
                .collect::<Vec<_>>(),
            vec!["redirect"]
        );
    }

    #[test]
    fn tab_toggles_between_data_and_content_names() {
        let mut app = App::default();
        let (tx, _rx) = mpsc::channel(8);
        app.update(Update::Event(WatchEvent::SnapshotComplete));
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert!(app.names_view);
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert!(!app.names_view);
    }

    #[test]
    fn disconnected_view_shows_enrollment() -> Result<()> {
        let root = tempfile::tempdir()?;
        let client_id =
            iroh_share_proto::client::load_or_create_key(&root.path().join("key"))?.public();
        let mut app = App {
            client_id: Some(client_id),
            server_id: Some(client_id),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(110, 30))?;
        terminal.draw(|frame| app.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("iroh-share control pair"));
        assert!(text.contains(&client_id.to_string()));
        Ok(())
    }

    #[test]
    fn url_keys_use_selection_and_do_not_intercept_prompts() {
        use iroh_share_proto::{Name, NameKey, NameState, NameTarget};
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App {
            names_view: true,
            ..Default::default()
        };
        app.model.raw_names = true;
        let url = NameKey([4; 32]).url();
        app.update(Update::Event(WatchEvent::NameUpdated(Box::new(Name {
            label: "site".into(),
            key: NameKey([4; 32]),
            target: NameTarget::Url("https://example.com/".parse().unwrap()),
            state: NameState::Disabled,
        }))));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), &tx);
        assert!(
            matches!(app.link_action.take(), Some(links::Action::Copy { text, label: "URL" }) if text == url.as_str())
        );
        app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE), &tx);
        assert!(matches!(app.link_action.take(), Some(links::Action::Open(value)) if value == url));
        assert!(rx.try_recv().is_err());
        app.input = Input::Share(String::new());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), &tx);
        assert!(matches!(&app.input, Input::Share(value) if value == "c"));
        assert!(app.link_action.is_none());
        assert!(app.key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &tx
        ));
    }

    #[test]
    fn renders_failure_and_clears_state_on_disconnect() -> Result<()> {
        let mut app = App::default();
        app.update(Update::Event(WatchEvent::JobUpdated(Box::new(Job {
            id: 9,
            kind: JobKind::Share {
                path: "file.txt".into(),
                include_directory_name: false,
            },
            state: JobState::Failed {
                error: JobError {
                    message: "file disappeared".into(),
                },
            },
        }))));
        app.update(Update::Event(WatchEvent::SnapshotComplete));
        let mut terminal = Terminal::new(TestBackend::new(100, 25))?;
        terminal.draw(|frame| app.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Failed"));
        assert!(text.contains("file disappeared"));
        app.input = Input::Remove(9);
        app.update(Update::Disconnected("connection lost".into()));
        assert!(!app.model.ready);
        assert!(app.model.jobs.is_empty());
        assert!(matches!(app.input, Input::Browse));
        Ok(())
    }

    #[test]
    fn tab_requests_daemon_completion_without_submitting() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::default();
        app.model.ready = true;
        app.input = Input::Share("~/my".into());
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        let Action::CompletePath { id, path } = rx.try_recv()? else {
            panic!("expected completion");
        };
        assert_eq!(path, PathBuf::from("~/my"));
        assert!(!app.busy);
        app.update(Update::Completion {
            id,
            result: Ok(iroh_share_proto::PathCompletions {
                common_prefix: "/daemon/my file.txt".into(),
                candidates: vec![iroh_share_proto::PathCandidate {
                    path: "/daemon/my file.txt".into(),
                    kind: iroh_share_proto::PathKind::File,
                }],
                truncated: false,
            }),
        });
        assert!(matches!(&app.input, Input::Share(value) if value == "/daemon/my file.txt"));
        assert!(rx.try_recv().is_err());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Share(value)) if value == std::path::Path::new("/daemon/my file.txt"))
        );
        Ok(())
    }

    #[test]
    fn submission_preserves_daemon_relative_paths_and_edits_discard_completion() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::default();
        app.model.ready = true;
        app.input = Input::Share("~/remote".into());
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        let Action::CompletePath { id, .. } = rx.try_recv()? else {
            panic!("expected completion");
        };
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &tx);
        app.update(Update::Completion {
            id,
            result: Err("stale error".into()),
        });
        assert!(!app.status.contains("stale error"));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Share(path)) if path == std::path::Path::new("~/remotex"))
        );
        Ok(())
    }

    #[test]
    fn removal_requires_confirmation_and_offline_actions_are_disabled() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::default();
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE), &tx);
        assert!(matches!(app.input, Input::Browse));
        app.model.ready = true;
        app.model.selected = Some(5);
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &tx);
        assert!(rx.try_recv().is_err());
        app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &tx);
        assert!(matches!(rx.try_recv(), Ok(Action::Remove(5))));
        assert!(app.busy);
    }
}
