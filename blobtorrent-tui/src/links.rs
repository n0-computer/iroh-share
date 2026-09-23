use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine};
use blobtorrent_proto::{JobState, Url};
use crossterm::{
    cursor, execute,
    terminal::{Clear, ClearType},
};
use tokio::sync::mpsc;

use crate::model::Model;

pub enum Action {
    Copy(Url),
    Open(Url),
}
pub enum Outcome {
    Message(String),
    TerminalCopy(Url),
}

pub fn selected_url(model: &Model, names: bool) -> Option<Url> {
    if names {
        let name = model.names.get(model.selected_name.as_ref()?)?;
        return Some(name.key.url());
    }
    match &model.jobs.get(&model.selected?)?.state {
        JobState::Seeding { ticket } => format!(
            "https://{}.blake3.net/",
            z32::encode(ticket.hash().as_bytes())
        )
        .parse()
        .ok(),
        _ => None,
    }
}

fn remote() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

/// Keep the native clipboard owner alive, and keep desktop calls off the UI thread.
pub fn worker() -> (mpsc::Sender<Action>, mpsc::Receiver<Outcome>) {
    let (tx, mut rx) = mpsc::channel(8);
    let (results, outcomes) = mpsc::channel(8);
    std::thread::spawn(move || {
        let mut clipboard = None;
        while let Some(action) = rx.blocking_recv() {
            let outcome = match action {
                Action::Copy(url) if remote() => Outcome::TerminalCopy(url),
                Action::Copy(url) => {
                    if clipboard.is_none() {
                        clipboard = arboard::Clipboard::new().ok();
                    }
                    match clipboard.as_mut().map(|c| c.set_text(url.as_str())) {
                        Some(Ok(())) => Outcome::Message("URL copied".into()),
                        _ => Outcome::TerminalCopy(url),
                    }
                }
                Action::Open(_) if remote() => Outcome::Message(
                    "SSH session: click Open selected URL, or press c to copy".into(),
                ),
                Action::Open(url) => match webbrowser::open(url.as_str()) {
                    Ok(()) => Outcome::Message("Opened URL in browser".into()),
                    Err(error) => {
                        Outcome::Message(format!("Cannot open URL: {error}. Press c to copy"))
                    }
                },
            };
            if results.blocking_send(outcome).is_err() {
                break;
            }
        }
    });
    (tx, outcomes)
}

pub fn copy_sequence(url: &Url) -> String {
    // tmux supports OSC 52 directly when its clipboard integration is enabled.
    format!("\x1b]52;c;{}\x07", STANDARD.encode(url.as_str()))
}

pub fn complete(outcome: Outcome) -> String {
    match outcome {
        Outcome::Message(message) => message,
        Outcome::TerminalCopy(url) => {
            let result = io::stdout()
                .write_all(copy_sequence(&url).as_bytes())
                .and_then(|()| io::stdout().flush());
            match result {
                Ok(()) => "Copy requested from terminal (OSC 52)".into(),
                Err(error) => format!("Cannot copy URL: {error}"),
            }
        }
    }
}

/// Ratatui 0.30 has no hyperlink cell metadata. Render a dedicated heading row
/// after its frame, rather than putting escape sequences into widget text.
pub fn draw_link(url: Option<&Url>, width: u16) -> io::Result<()> {
    let mut out = io::stdout().lock();
    execute!(
        out,
        cursor::SavePosition,
        cursor::MoveTo(0, 1),
        Clear(ClearType::CurrentLine)
    )?;
    if let Some(url) = url {
        let label = "Open selected URL";
        let label = &label[..label.len().min(width as usize)];
        write!(out, "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, label)?;
    }
    execute!(out, cursor::RestorePosition)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use blobtorrent_proto::{Name, NameKey, NameState, NameTarget, WatchEvent};

    #[test]
    fn name_actions_use_public_name_not_redirect_target() {
        let mut model = Model::default();
        let key = NameKey([3; 32]);
        model.apply(WatchEvent::NameUpdated(Box::new(Name {
            label: "site".into(),
            key,
            target: NameTarget::Url("https://example.com/private-path".parse().unwrap()),
            state: NameState::Disabled,
        })));
        assert_eq!(selected_url(&model, true).unwrap(), key.url());
        assert!(selected_url(&model, false).is_none());
        model.reset();
        assert!(selected_url(&model, true).is_none());
    }

    #[test]
    fn osc52_encodes_url_without_raw_control_characters() {
        let url: Url = "https://example.com/a?q=1".parse().unwrap();
        let sequence = copy_sequence(&url);
        let payload = sequence
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(STANDARD.decode(payload).unwrap(), url.as_str().as_bytes());
    }
}
