use blobtorrent_proto::{GatewayConfig, GatewaySnapshot, GatewayState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::Stylize,
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

#[derive(Default)]
pub struct Page {
    pub open: bool,
    pub editing: Option<String>,
    pub draft: GatewayConfig,
    pub snapshot: Option<GatewaySnapshot>,
    pub dirty: bool,
    pub status: String,
    selected: usize,
}
pub enum Action {
    None,
    Save(GatewayConfig),
    Back,
    Quit,
}
impl Page {
    pub fn receive(&mut self, snapshot: GatewaySnapshot) {
        if !self.dirty {
            self.draft = snapshot.config.clone();
        }
        self.snapshot = Some(snapshot);
    }
    pub fn saved(&mut self, result: Result<GatewaySnapshot, String>) {
        match result {
            Ok(snapshot) => {
                if self.draft == snapshot.config {
                    self.dirty = false;
                }
                self.receive(snapshot);
                self.status = "Settings saved and applied".into();
            }
            Err(error) => self.status = format!("Cannot apply settings: {error}"),
        }
    }
    pub fn paste(&mut self, text: String) {
        if let Some(value) = &mut self.editing {
            value.extend(text.chars().filter(|c| !c.is_control()));
        }
    }
    pub fn key(&mut self, key: KeyEvent, ready: bool, busy: bool) -> Action {
        if let Some(value) = &mut self.editing {
            match key.code {
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear()
                }
                KeyCode::Esc => self.editing = None,
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
                KeyCode::Enter => {
                    let result = match self.selected {
                        1 => value.trim().parse().map(|listen| self.draft.listen = listen).map_err(|_| "Enter an IP address and port, such as 127.0.0.1:8080"),
                        2 if value.trim().is_empty() => { self.draft.index_server = None; Ok(()) }
                        2 => value.trim().parse().map(|server| self.draft.index_server = Some(server)).map_err(|_| "Enter an IPv4 address and port, or leave empty for automatic discovery"),
                        _ => Ok(()),
                    };
                    match result {
                        Ok(()) => {
                            self.editing = None;
                            self.dirty = true;
                            self.status = "Unsaved changes · s saves and applies".into();
                        }
                        Err(error) => self.status = error.into(),
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => return Action::Back,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(3),
            KeyCode::Char('r') => {
                if let Some(snapshot) = &self.snapshot {
                    self.draft = snapshot.config.clone();
                    self.dirty = false;
                    self.status.clear();
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.selected == 0 => {
                self.draft.enabled = !self.draft.enabled;
                self.dirty = true;
            }
            KeyCode::Enter if self.selected == 1 || self.selected == 2 => {
                self.editing = Some(if self.selected == 1 {
                    self.draft.listen.to_string()
                } else {
                    self.draft
                        .index_server
                        .map(|server| server.to_string())
                        .unwrap_or_default()
                });
            }
            KeyCode::Char('s') | KeyCode::Enter => {
                if !ready {
                    self.status = "Connect to the daemon to apply settings".into();
                } else if busy {
                    self.status = "A request is in progress".into();
                } else if !self.draft.listen.ip().is_loopback() {
                    self.status = "HTTP address must be loopback (127.0.0.1 or ::1)".into();
                } else {
                    self.status = "Applying settings…".into();
                    return Action::Save(self.draft.clone());
                }
            }
            _ => {}
        }
        Action::None
    }
    pub fn draw(&self, frame: &mut Frame, ready: bool) {
        let [heading, fields, detail, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new("blobtorrent · Settings").bold().cyan(),
            heading,
        );
        let values = [
            format!(
                "Gateway enabled: {}",
                if self.draft.enabled { "yes" } else { "no" }
            ),
            format!("HTTP listen address: {}", self.draft.listen),
            format!(
                "Index server: {}",
                self.draft
                    .index_server
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "automatic (Mainline rendezvous)".into())
            ),
            "Save and apply / retry".into(),
        ];
        let mut rows = Vec::new();
        for (index, value) in values.iter().enumerate() {
            rows.push(ratatui::text::Line::from(format!(
                "{} {value}",
                if self.selected == index { "›" } else { " " }
            )));
            if self.selected == index {
                if let Some(edit) = &self.editing {
                    rows.push(ratatui::text::Line::from(format!(
                        "  {}▏ · Ctrl-U clears",
                        crate::clean(edit)
                    )));
                }
            }
        }
        frame.render_widget(
            Paragraph::new(rows).block(Block::bordered().title(if self.dirty {
                " Gateway · unsaved "
            } else {
                " Gateway "
            })),
            fields,
        );
        let state = if !ready {
            "Disconnected · gateway status unavailable".into()
        } else {
            match self.snapshot.as_ref().map(|snapshot| &snapshot.state) {
                None => "Waiting for gateway status…".into(),
                Some(GatewayState::Disabled) => "Disabled".into(),
                Some(GatewayState::Starting) => "Starting · discovering index servers…".into(),
                Some(GatewayState::Running { listen, .. }) => format!("Running · http://{listen}"),
                Some(GatewayState::Failed { error }) => format!(
                    "Failed: {}\nRetries automatically every 30 seconds.",
                    crate::clean(error)
                ),
            }
        };
        frame.render_widget(Paragraph::new(format!("{state}\n\nThe gateway runs inside the daemon and stays running when the TUI closes.\nIt browses the content-addressed web using its own iroh endpoint.\nHTTP listens on the daemon machine's loopback interface.\nConfigure your browser extension to use that address."))
            .wrap(Wrap { trim: false }).block(Block::bordered().title(" Status ")), detail);
        frame.render_widget(Paragraph::new(format!("↑/↓ select · Enter edit · Space toggle · s save/apply · r discard · Esc back · q quit\n{}", crate::clean(&self.status))).wrap(Wrap { trim: false }), footer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn edits_are_preserved_by_status_updates_and_require_explicit_save() {
        let mut page = Page::default();
        page.receive(GatewaySnapshot {
            config: GatewayConfig {
                enabled: false,
                ..Default::default()
            },
            state: GatewayState::Disabled,
        });
        assert!(matches!(
            page.key(key(KeyCode::Char(' ')), true, false),
            Action::None
        ));
        assert!(page.draft.enabled);
        page.receive(GatewaySnapshot {
            config: GatewayConfig {
                enabled: false,
                ..Default::default()
            },
            state: GatewayState::Disabled,
        });
        assert!(page.draft.enabled);
        assert!(matches!(
            page.key(key(KeyCode::Char('s')), false, false),
            Action::None
        ));
        let Action::Save(config) = page.key(key(KeyCode::Char('s')), true, false) else {
            panic!("save expected");
        };
        page.saved(Ok(GatewaySnapshot {
            config,
            state: GatewayState::Starting,
        }));
        assert!(!page.dirty);
    }
    #[test]
    fn invalid_edit_stays_open_and_escape_discards_it() {
        let mut page = Page::default();
        page.key(key(KeyCode::Down), true, false);
        page.key(key(KeyCode::Enter), true, false);
        page.editing = Some("bad-address".into());
        page.key(key(KeyCode::Enter), true, false);
        assert!(page.editing.is_some());
        assert_eq!(page.draft.listen, GatewayConfig::default().listen);
        page.key(key(KeyCode::Esc), true, false);
        assert!(page.editing.is_none());
    }
}
