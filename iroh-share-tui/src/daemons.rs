//! Saved daemons: switch, add with a pairing ticket, rename, and forget.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iroh_share_proto::{client::SavedDaemon, EndpointId, PairingTicket};
use ratatui::{
    layout::{Constraint, Layout},
    style::Stylize,
    text::Line,
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

#[derive(Default)]
pub struct Page {
    pub open: bool,
    pub daemons: Vec<SavedDaemon>,
    pub current: Option<EndpointId>,
    pub status: String,
    /// A pairing ticket is being redeemed in the background.
    pub pairing: bool,
    edit: Option<Edit>,
    selected: usize,
}

enum Edit {
    Ticket(String),
    Name(String),
    Forget,
}

pub enum Action {
    None,
    Back,
    Quit,
    Switch(EndpointId),
    Pair(PairingTicket),
    Rename(EndpointId, String),
    Forget(EndpointId),
}

impl Page {
    pub fn load(&mut self, daemons: Vec<SavedDaemon>, current: Option<EndpointId>) {
        self.daemons = daemons;
        self.current = current;
        self.selected = current
            .and_then(|id| self.daemons.iter().position(|d| d.id() == id))
            .unwrap_or(0);
        self.edit = None;
    }
    pub fn current_name(&self) -> Option<String> {
        self.daemons
            .iter()
            .find(|d| Some(d.id()) == self.current)
            .map(SavedDaemon::display_name)
    }
    /// Opens the page with the ticket prompt, as when no daemon is saved yet.
    pub fn add(&mut self) {
        self.open = true;
        self.edit = Some(Edit::Ticket(String::new()));
    }
    pub fn paste(&mut self, text: String) {
        if let Some(Edit::Ticket(value) | Edit::Name(value)) = &mut self.edit {
            value.extend(text.chars().filter(|c| !c.is_control()));
        }
    }
    fn selected(&self) -> Option<EndpointId> {
        self.daemons.get(self.selected).map(SavedDaemon::id)
    }
    pub fn key(&mut self, key: KeyEvent) -> Action {
        match &mut self.edit {
            Some(Edit::Forget) => {
                let id = self.selected();
                self.edit = None;
                return match (key.code, id) {
                    (KeyCode::Char('y'), Some(id)) => Action::Forget(id),
                    _ => Action::None,
                };
            }
            Some(Edit::Ticket(value) | Edit::Name(value)) => {
                match key.code {
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear()
                    }
                    KeyCode::Esc => self.edit = None,
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        value.push(c)
                    }
                    KeyCode::Enter => return self.accept(),
                    _ => {}
                }
                return Action::None;
            }
            None => {}
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('m') if self.current.is_some() => return Action::Back,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.daemons.len().saturating_sub(1))
            }
            KeyCode::Char('a') if !self.pairing => self.edit = Some(Edit::Ticket(String::new())),
            KeyCode::Char('n') => {
                if let Some(daemon) = self.daemons.get(self.selected) {
                    self.edit = Some(Edit::Name(daemon.name.clone().unwrap_or_default()));
                }
            }
            KeyCode::Char('x') if self.selected().is_some() => self.edit = Some(Edit::Forget),
            KeyCode::Enter => match self.selected() {
                Some(id) if Some(id) == self.current => return Action::Back,
                Some(id) => return Action::Switch(id),
                None => {}
            },
            _ => {}
        }
        Action::None
    }
    fn accept(&mut self) -> Action {
        match self.edit.take() {
            Some(Edit::Ticket(value)) => match value.trim().parse::<PairingTicket>() {
                Ok(ticket) => {
                    self.pairing = true;
                    self.status = "Pairing…".into();
                    Action::Pair(ticket)
                }
                Err(_) => {
                    self.status = "Invalid ticket. Paste the complete iroh-share ticket.".into();
                    self.edit = Some(Edit::Ticket(value));
                    Action::None
                }
            },
            Some(Edit::Name(value)) => match self.selected() {
                Some(id) => Action::Rename(id, value),
                None => Action::None,
            },
            _ => Action::None,
        }
    }
    pub fn draw(&self, frame: &mut Frame) {
        let [heading, list, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new("iroh-share · Daemons").bold().cyan(),
            heading,
        );
        let mut rows: Vec<Line> = self
            .daemons
            .iter()
            .enumerate()
            .map(|(index, daemon)| {
                let marker = if index == self.selected { "›" } else { " " };
                let current = if Some(daemon.id()) == self.current {
                    " · current"
                } else {
                    ""
                };
                Line::from(format!(
                    "{marker} {}  {}{current}",
                    crate::clean(&daemon.display_name()),
                    daemon.id()
                ))
            })
            .collect();
        if rows.is_empty() {
            rows.push(Line::from(
                "No daemons yet. Press a and paste a pairing ticket.",
            ));
        }
        match &self.edit {
            Some(Edit::Ticket(value)) => rows.push(Line::from(format!(
                "\nPairing ticket: {}▏ · Enter adds · Esc cancels",
                crate::clean(value)
            ))),
            Some(Edit::Name(value)) => rows.push(Line::from(format!(
                "\nName: {}▏ · Enter saves · empty clears · Esc cancels",
                crate::clean(value)
            ))),
            Some(Edit::Forget) => rows.push(Line::from(
                "\nForget this daemon? It keeps this TUI authorized; adding it again needs a new ticket. [y/n]",
            )),
            None => {}
        }
        frame.render_widget(
            Paragraph::new(rows)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" Saved daemons ")),
            list,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "↑/↓ select · Enter switch · a add ticket · n rename · x forget · Esc back · q quit\n{}",
                crate::clean(&self.status)
            ))
            .wrap(Wrap { trim: false }),
            footer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn daemon(n: u8) -> SavedDaemon {
        let id = iroh_share_proto::client::load_or_create_key(
            &tempfile::tempdir().unwrap().keep().join(format!("{n}.key")),
        )
        .unwrap()
        .public();
        SavedDaemon {
            addr: id.into(),
            name: None,
        }
    }
    #[test]
    fn switch_rename_forget_and_reject_bad_tickets() {
        let (a, b) = (daemon(1), daemon(2));
        let mut page = Page::default();
        page.load(vec![a.clone(), b.clone()], Some(a.id()));
        assert!(matches!(page.key(key(KeyCode::Enter)), Action::Back));
        page.key(key(KeyCode::Down));
        assert!(matches!(page.key(key(KeyCode::Enter)), Action::Switch(id) if id == b.id()));
        page.key(key(KeyCode::Char('n')));
        assert!(page.edit.is_some());
        page.paste("nas".into());
        assert!(
            matches!(page.key(key(KeyCode::Enter)), Action::Rename(id, name) if id == b.id() && name == "nas")
        );
        page.key(key(KeyCode::Char('x')));
        assert!(matches!(page.key(key(KeyCode::Char('n'))), Action::None));
        page.key(key(KeyCode::Char('x')));
        assert!(matches!(page.key(key(KeyCode::Char('y'))), Action::Forget(id) if id == b.id()));
        page.key(key(KeyCode::Char('a')));
        page.paste("not a ticket".into());
        assert!(matches!(page.key(key(KeyCode::Enter)), Action::None));
        assert!(page.edit.is_some() && !page.pairing);
    }
    #[test]
    fn page_cannot_be_left_without_a_daemon() {
        let mut page = Page::default();
        page.add();
        page.key(key(KeyCode::Esc));
        assert!(matches!(page.key(key(KeyCode::Esc)), Action::None));
    }
}
