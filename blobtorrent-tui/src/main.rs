mod completion;
mod links;
mod model;
mod network;
mod settings;

use anyhow::{Context, Result};
use blobtorrent_proto::{BlobTicket, Job, JobKind, JobState};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
#[command(about = "Terminal interface for the blobtorrent daemon")]
struct Args {
    /// Pair with the daemon using its printed one-client ticket, then open the TUI.
    #[arg(value_name = "PAIRING_TICKET", conflicts_with = "endpoint")]
    ticket: Option<blobtorrent_proto::PairingTicket>,
    /// Override the platform-specific blobtorrent-tui config directory.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Save the daemon endpoint ID to connect to, including on future starts.
    #[arg(long)]
    endpoint: Option<blobtorrent_proto::EndpointId>,
    /// Print this client's persistent endpoint ID and exit (for authorization).
    #[arg(long)]
    print_id: bool,
}

#[derive(Default)]
enum Input {
    #[default]
    Browse,
    Share(String),
    Ticket(String),
    Target {
        ticket: BlobTicket,
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
    RemoveName(String),
}

#[derive(Default)]
struct App {
    model: Model,
    names_view: bool,
    settings: settings::Page,
    client_id: Option<blobtorrent_proto::EndpointId>,
    server_id: Option<blobtorrent_proto::EndpointId>,
    link_action: Option<links::Action>,
    input: Input,
    completion: completion::Completion,
    table: TableState,
    status: String,
    busy: bool,
    details_scroll: u16,
}

impl App {
    fn update(&mut self, update: Update) {
        match update {
            Update::GatewaySaved(result) => {
                self.busy = false;
                self.settings.saved(result);
            }
            Update::Event(blobtorrent_proto::WatchEvent::GatewayUpdated(snapshot)) => {
                self.settings.receive(snapshot)
            }
            Update::Completion { id, result } => {
                if let Input::Share(value) | Input::Target { value, .. } = &mut self.input {
                    if let Err(error) = self.completion.receive(id, result, value) {
                        self.status = format!("Cannot complete path: {error}");
                    }
                }
            }
            Update::Connecting => {
                self.model.reset();
                self.input = Input::Browse;
                self.completion.reset();
                self.busy = false;
                self.status = "Connecting to daemon...".into();
            }
            Update::Disconnected(error) => {
                self.model.reset();
                self.input = Input::Browse;
                self.completion.reset();
                self.busy = false;
                self.status = format!("Disconnected: {error}. Retrying...");
            }
            Update::Event(event) => {
                let was_ready = self.model.ready;
                self.model.apply(event);
                if !was_ready && self.model.ready {
                    self.status = "Connected".into();
                }
            }
            Update::ActionResult(message) => {
                self.busy = false;
                self.status = message;
            }
        }
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
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if matches!(self.input, Input::Browse)
            && (key.code == KeyCode::F(2) || key.code == KeyCode::Char(','))
            && self.settings.editing.is_none()
        {
            self.settings.open = !self.settings.open;
            return false;
        }
        if self.settings.open {
            match self.settings.key(key, self.model.ready, self.busy) {
                settings::Action::None => {}
                settings::Action::Back => {
                    self.settings.open = false;
                    self.names_view = false;
                }
                settings::Action::Quit => return true,
                settings::Action::Save(config) => self.submit(Action::SetGateway(config), tx),
            }
            return false;
        }
        if matches!(self.input, Input::Browse)
            && key.modifiers.is_empty()
            && matches!(key.code, KeyCode::Char('c' | 'o'))
        {
            if let Some(url) = links::selected_url(&self.model, self.names_view) {
                self.link_action = Some(if key.code == KeyCode::Char('c') {
                    links::Action::Copy(url)
                } else {
                    links::Action::Open(url)
                });
            } else {
                self.status = "No URL available for this selection".into();
            }
            return false;
        }
        if key.code == KeyCode::Tab && matches!(self.input, Input::Browse) {
            if self.names_view {
                self.names_view = false;
                self.settings.open = true;
            } else {
                self.names_view = true;
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
        match &mut self.input {
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
                KeyCode::Char('s') if self.model.ready && !self.busy => {
                    self.input = Input::Share(String::new())
                }
                KeyCode::Char('d') if self.model.ready && !self.busy => {
                    self.input = Input::Ticket(String::new())
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
                                blobtorrent_proto::NameTarget::Url(url) => url.to_string(),
                                blobtorrent_proto::NameTarget::Job(id) => format!("data:{id}"),
                            })
                            .unwrap_or_default();
                        self.input = Input::NameTarget {
                            label,
                            value,
                            create: false,
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
            | Input::Ticket(value)
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
        if self.settings.open {
            self.settings.paste(text);
            return;
        }
        self.completion.reset();
        match &mut self.input {
            Input::Share(value)
            | Input::Ticket(value)
            | Input::Target { value, .. }
            | Input::NameLabel { value, .. }
            | Input::NameTarget { value, .. } => {
                value.extend(text.chars().filter(|c| !c.is_control()))
            }
            _ => {}
        }
    }

    fn accept_input(&mut self, tx: &mpsc::Sender<Action>) {
        match std::mem::take(&mut self.input) {
            Input::Share(value) if !value.is_empty() => {
                self.submit(Action::Share(value.into()), tx)
            }
            Input::Ticket(value) => match value.trim().parse() {
                Ok(ticket) => {
                    self.input = Input::Target {
                        ticket,
                        value: String::new(),
                    }
                }
                Err(error) => {
                    self.status = format!("Invalid ticket: {error}");
                    self.input = Input::Ticket(value);
                }
            },
            Input::Target { ticket, value } if !value.is_empty() => self.submit(
                Action::Download {
                    ticket,
                    target: value.into(),
                },
                tx,
            ),
            Input::NameLabel { value, job } if !value.is_empty() => {
                if let Some(id) = job {
                    self.submit(
                        Action::CreateName {
                            label: value,
                            target: blobtorrent_proto::NameTarget::Job(id),
                        },
                        tx,
                    );
                    self.names_view = true;
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
                let target = if let Some(id) = value
                    .strip_prefix("data:")
                    .or_else(|| value.strip_prefix("job:"))
                {
                    id.parse()
                        .map(blobtorrent_proto::NameTarget::Job)
                        .map_err(|e| format!("Invalid data ID: {e}"))
                } else {
                    value
                        .parse()
                        .map(blobtorrent_proto::NameTarget::Url)
                        .map_err(|e| format!("Invalid URL: {e}"))
                };
                match target {
                    Ok(target) => self.submit(
                        if create {
                            Action::CreateName { label, target }
                        } else {
                            Action::UpdateName { label, target }
                        },
                        tx,
                    ),
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
    }

    fn draw(&mut self, frame: &mut Frame) {
        if self.settings.open {
            self.settings.draw(frame, self.model.ready);
            return;
        }
        let [heading, jobs, details, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        let connection = if self.model.ready {
            "connected"
        } else {
            "connecting"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                "blobtorrent ".bold().cyan(),
                format!("  {connection} · {} items", self.model.jobs.len()).into(),
            ])),
            heading,
        );
        if !self.names_view {
            let rows = self.model.jobs.values().map(|job| {
                let (state, progress) = summary(&job.state);
                Row::new(vec![state.into(), progress, description(job)])
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
                    Constraint::Length(28),
                    Constraint::Min(10),
                ],
            )
            .header(Row::new(["State", "Progress", "Path"]).bold())
            .block(Block::bordered().title(" Data "))
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .highlight_symbol("› ");
            frame.render_stateful_widget(table, jobs, &mut self.table);
        } else {
            let rows = self.model.names.values().map(|name| {
                Row::new(vec![
                    name.label.clone(),
                    name_status(&name.state).into(),
                    match &name.target {
                        blobtorrent_proto::NameTarget::Url(url) => url.to_string(),
                        blobtorrent_proto::NameTarget::Job(id) => self
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
                    .names
                    .keys()
                    .position(|label| Some(label) == self.model.selected_name.as_ref()),
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
            .block(Block::bordered().title(" Names · Tab for data "))
            .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .highlight_symbol("› ");
            frame.render_stateful_widget(table, jobs, &mut self.table);
        }
        let enrollment_id = self.client_id.filter(|_| !self.model.ready);
        let detail = if let Some(client_id) = enrollment_id {
            format!("Client endpoint: {}\nDaemon endpoint: {}\n\nGet a ticket on the daemon machine: blobtorrent control pair\nThen run: blobtorrent-tui <pairing-ticket>\nThe TUI retries automatically.", client_id, self.server_id.map(|id| id.to_string()).unwrap_or_else(|| "not configured".into()))
        } else if self.names_view {
            self.model
                .selected_name
                .as_ref()
                .and_then(|label| self.model.names.get(label))
                .map(|name| {
                    let status = match &name.state {
                        blobtorrent_proto::NameState::Published { url, .. }
                        | blobtorrent_proto::NameState::Publishing { url } => url.to_string(),
                        blobtorrent_proto::NameState::Failed { error } => error.message.clone(),
                        _ => String::new(),
                    };
                    format!(
                        "{}\n{} · {}\n{}",
                        name.key.url(),
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
                .map(job_details)
                .unwrap_or_else(|| "No data selected. Press s to share or d to download.".into())
        };
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: false })
                .scroll((self.details_scroll, 0))
                .block(Block::bordered().title(" Details · PgUp/PgDn to scroll ")),
            details,
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(if self.names_view {
                    "Tab settings · , settings · ↑/↓ select · n create · e retarget · x remove · c copy · o open · q quit"
                } else {
                    "Tab names · , settings · ↑/↓ select · s share · d download · n name · x remove · c copy · o open · q quit"
                })
                .cyan(),
                Line::from(clean(&self.status)),
            ])
            .wrap(Wrap { trim: false }),
            footer,
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
            Input::NameTarget { value, .. } => Some(("Target URL or data:<id>", value.clone())),
            Input::RemoveName(label) => Some((
                "Remove name",
                format!("Remove {label} and its key? Published records expire later. [y/n]"),
            )),
            Input::Share(value) => Some(("Share path", value.clone())),
            Input::Ticket(value) => Some(("Download ticket", value.clone())),
            Input::Target { value, .. } => Some(("Download target directory", value.clone())),
            Input::Remove(id) => Some((
                "Remove data",
                format!("Remove data {id}? Files are kept. [y/n]"),
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
                        clean(&self.completion.hint())
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

fn name_status(state: &blobtorrent_proto::NameState) -> &'static str {
    use blobtorrent_proto::NameState::*;
    match state {
        Disabled => "Disabled",
        WaitingForJob => "Waiting for data",
        Publishing { .. } => "Publishing",
        Published { .. } => "Published",
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
        JobKind::Share { path } => path.display().to_string(),
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
        JobState::Seeding { .. } => ("Seeding", String::new()),
        JobState::Failed { error } => ("Failed", clean(&error.message)),
    }
}

fn job_details(job: &Job) -> String {
    let (state, progress) = summary(&job.state);
    let extra = match &job.state {
        JobState::Seeding { ticket } => {
            return format!(
                "https://{}.blake3.net/\n{}\nSeeding\nHash: {}\nTicket: {ticket}",
                z32::encode(ticket.hash().as_bytes()),
                description(job),
                ticket.hash(),
            );
        }
        JobState::Downloading { source, .. } => {
            format!("Hash: {}\nSource ticket: {source}", source.hash())
        }
        JobState::Exporting { root_hash, .. } => format!("Hash: {root_hash}"),
        JobState::Failed { error } => format!("Error: {}", clean(&error.message)),
        _ => String::new(),
    };
    format!("{}\n{state} {progress}\n{extra}", description(job))
}

fn default_config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|base| base.join("blobtorrent-tui"))
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
            "blobtorrent-tui requires an interactive terminal"
        );
    }
    if let Some(ticket) = args.ticket {
        blobtorrent_proto::client::pair(&config_dir, &ticket).await?;
    }
    if let Some(endpoint) = args.endpoint {
        blobtorrent_proto::client::configure_endpoint(&config_dir, Some(endpoint))?;
    }
    let key =
        blobtorrent_proto::client::load_or_create_key(&config_dir.join("control-client.key"))?;
    if args.print_id {
        println!("{}", key.public());
        return Ok(());
    }
    if blobtorrent_proto::client::configured_endpoint(&config_dir)?.is_none() {
        use std::io::Write;
        eprintln!(
            "Paste the ticket printed by the daemon, or create one with blobtorrent control pair."
        );
        loop {
            eprint!("Pairing ticket: ");
            std::io::stderr().flush()?;
            let mut input = String::new();
            anyhow::ensure!(
                std::io::stdin().read_line(&mut input)? > 0,
                "pairing input closed"
            );
            let ticket = match input.trim().parse::<blobtorrent_proto::PairingTicket>() {
                Ok(ticket) => ticket,
                Err(_) => {
                    eprintln!("Invalid ticket. Paste the complete blobtorrent ticket.");
                    continue;
                }
            };
            match blobtorrent_proto::client::pair(&config_dir, &ticket).await {
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
    let (actions_tx, actions_rx) = mpsc::channel(8);
    let (updates_tx, mut updates_rx) = mpsc::channel(256);
    let worker = tokio::spawn(network::run(config_dir.clone(), actions_rx, updates_tx));
    let mut events = EventStream::new();
    let server_id = blobtorrent_proto::client::configured_endpoint(&config_dir)?;
    let mut app = App {
        client_id: Some(
            blobtorrent_proto::client::load_or_create_key(&config_dir.join("control-client.key"))?
                .public(),
        ),
        server_id,
        ..Default::default()
    };
    let (link_tx, mut link_results) = links::worker();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let result = async {
        loop {
            tokio::select! {
                _ = tick.tick() => { terminal.draw(|frame| app.draw(frame))?;
                    if terminal.size()?.height > 1 { links::draw_link(links::selected_url(&app.model, app.names_view).filter(|_| !app.settings.open).as_ref(), terminal.size()?.width)?; } }
                Some(outcome) = link_results.recv() => { app.status = links::complete(outcome); }
                update = updates_rx.recv() => { app.update(update.context("connection worker stopped")?); }
                event = events.next() => match event.context("terminal input closed")?? {
                    Event::Key(key) => if app.key(key, &actions_tx) { return Ok(()); },
                    Event::Paste(text) => app.paste(text),
                    Event::Resize(_, _) => { terminal.draw(|frame| app.draw(frame))?;
                    if terminal.size()?.height > 1 { links::draw_link(links::selected_url(&app.model, app.names_view).filter(|_| !app.settings.open).as_ref(), terminal.size()?.width)?; } }
                    _ => {}
                }
            }
            if let Some(action) = app.link_action.take() {
                if link_tx.try_send(action).is_err() {
                    app.status = "URL action queue is busy".into();
                }
            }
        }
    }.await;
    worker.abort();
    let _ = worker.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use blobtorrent_proto::{JobError, WatchEvent};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn settings_page_cycles_and_submits_daemon_configuration() -> Result<()> {
        let mut app = App::default();
        let (tx, mut rx) = mpsc::channel(8);
        app.update(Update::Event(WatchEvent::GatewayUpdated(
            blobtorrent_proto::GatewaySnapshot {
                config: blobtorrent_proto::GatewayConfig {
                    enabled: false,
                    ..Default::default()
                },
                state: blobtorrent_proto::GatewayState::Disabled,
            },
        )));
        app.update(Update::Event(WatchEvent::SnapshotComplete));
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert!(app.names_view);
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert!(app.settings.open);
        app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &tx);
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE), &tx);
        assert!(matches!(rx.try_recv(), Ok(Action::SetGateway(config)) if config.enabled));
        let mut terminal = Terminal::new(TestBackend::new(110, 30))?;
        terminal.draw(|frame| app.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Gateway enabled: yes"));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(!app.settings.open && !app.names_view);
        Ok(())
    }

    #[test]
    fn disconnected_view_shows_enrollment() -> Result<()> {
        let root = tempfile::tempdir()?;
        let client_id =
            blobtorrent_proto::client::load_or_create_key(&root.path().join("key"))?.public();
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
        assert!(text.contains("blobtorrent control pair"));
        assert!(text.contains(&client_id.to_string()));
        Ok(())
    }

    #[test]
    fn url_keys_use_selection_and_do_not_intercept_prompts() {
        use blobtorrent_proto::{Name, NameKey, NameState, NameTarget};
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App {
            names_view: true,
            ..Default::default()
        };
        let url = NameKey([4; 32]).url();
        app.update(Update::Event(WatchEvent::NameUpdated(Box::new(Name {
            label: "site".into(),
            key: NameKey([4; 32]),
            target: NameTarget::Url("https://example.com/".parse().unwrap()),
            state: NameState::Disabled,
        }))));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), &tx);
        assert!(matches!(app.link_action.take(), Some(links::Action::Copy(value)) if value == url));
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
            result: Ok(blobtorrent_proto::PathCompletions {
                common_prefix: "/daemon/my file.txt".into(),
                candidates: vec![blobtorrent_proto::PathCandidate {
                    path: "/daemon/my file.txt".into(),
                    kind: blobtorrent_proto::PathKind::File,
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
