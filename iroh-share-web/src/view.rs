//! The whole page as one JSON object. The page renders it as is: every label,
//! tooltip, enabled flag and abbreviation is decided here, using the same rules
//! and wording as the desktop GUI.

use iroh_share_proto::{BlobTicket, Job, JobState, Name, NameState, NameTarget};
use serde_json::{json, Value};

use crate::app::{job_path, updatable, AddMode, EditorTarget, Removal, State};

const MANAGED: &str = "Belongs to content; manage it under Content";

impl State {
    pub fn view(&self) -> Value {
        json!({
            "busy": self.busy,
            "status": self.status,
            "pairing": self.pairing.then(|| self.pairing_view()),
            "page": self.page,
            "daemonTitle": format!(
                "Daemon: {}",
                self.current_daemon().map_or_else(|| "No daemon".into(), |d| d.display_name())
            ),
            "daemons": self.daemons.iter().map(|d| json!({
                "id": d.id().to_string(),
                "label": d.display_name(),
                "selected": Some(d.id()) == self.current,
            })).collect::<Vec<_>>(),
            "jobs": self.jobs.values().map(|job| self.job_view(job)).collect::<Vec<_>>(),
            "add": self.add_view(),
            "otherNames": self.other_content_names(),
            "names": self.names_view(self.names.values().collect(), false),
            "canNamesIo": self.can_act(),
            "settings": self.settings_view(),
            "confirm": self.confirm_view(),
        })
    }

    fn pairing_view(&self) -> Value {
        json!({
            "title": if self.daemons.is_empty() { "Connect to Iroh Share" } else { "Add a daemon" },
            "ticket": self.pair_ticket,
            "name": self.pair_name,
            "busy": self.busy,
            "canCancel": self.current.is_some(),
        })
    }

    fn job_view(&self, job: &Job) -> Value {
        let path = job_path(job).display().to_string();
        let names: Vec<_> = self
            .names
            .values()
            .filter(|n| n.target == NameTarget::Job(job.id))
            .map(|n| name_link(n, Some(self.can_act())))
            .collect();
        let seeding = match &job.state {
            JobState::Seeding { ticket, .. } => {
                let url = content_url(ticket);
                json!({
                    "url": url,
                    "compact": compact_url(&url),
                    "ticket": ticket.to_string(),
                    "hash": ticket.hash().to_hex().to_string(),
                })
            }
            _ => Value::Null,
        };
        json!({
            "id": job.id,
            "path": path,
            "selected": self.selected == Some(job.id),
            "status": job_status(&job.state),
            "names": names,
            "seeding": seeding,
            "canAddName": self.can_act(),
            "canUpdate": self.can_act() && updatable(job),
            "canRemove": self.can_act(),
        })
    }

    fn add_view(&self) -> Value {
        let action = self.add_action();
        let updating = self
            .import_id
            .and_then(|id| self.jobs.get(&id))
            .map(|job| format!("Updating {}", job_path(job).display()));
        json!({
            "mode": self.add_mode,
            "path": self.add_path,
            "pathPlaceholder": if self.add_mode == AddMode::Download {
                "Destination on the daemon · Tab to complete"
            } else {
                "Path on the daemon · Tab to complete"
            },
            "ticket": self.add_ticket,
            "source": self.add_source,
            "includeDirectoryName": self.include_directory_name,
            "discover": self.discover,
            "updating": updating,
            "submitLabel": if self.import_id.is_some() { "Update" } else { "Add" },
            "canSubmit": self.can_act() && action.is_ok(),
            "reason": action.err(),
            "canCancel": self.import_id.is_some(),
            "candidates": self.candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        })
    }

    /// Content names without visible data: fixed content URLs and names whose
    /// linked data is unavailable. Names on visible data stay in their row.
    fn other_content_names(&self) -> Value {
        let names: Vec<_> = self
            .names
            .values()
            .filter(|n| {
                n.target.is_content()
                    && !matches!(&n.target, NameTarget::Job(id) if self.jobs.contains_key(id))
            })
            .collect();
        if names.is_empty() {
            return Value::Null;
        }
        self.names_view(names, true)
    }

    /// The Names table (`content == false`) lists every name, with content names
    /// read-only; they are managed from Content.
    fn names_view(&self, names: Vec<&Name>, content: bool) -> Value {
        let rows: Vec<_> = names
            .into_iter()
            .map(|name| {
                let editing = self
                    .editor
                    .as_ref()
                    .filter(|e| e.label == name.label && e.content == content);
                if let Some(editor) = editing {
                    return self.editor_view(name, &editor.target);
                }
                let managed = !content && name.target.is_content();
                let records = match &name.target {
                    NameTarget::Url(url) => url.to_string(),
                    NameTarget::Records(text) => text.clone(),
                    NameTarget::Job(id) => self
                        .jobs
                        .get(id)
                        .map(|j| format!("Following {}", job_path(j).display()))
                        .unwrap_or_else(|| "Linked data is unavailable".into()),
                };
                json!({
                    "label": name.label,
                    "link": name_link(name, None),
                    "records": records,
                    "managed": managed,
                    "canEdit": self.can_act() && !managed,
                    "editTitle": if managed { MANAGED } else { "" },
                    "canExport": self.can_act(),
                    "canRemove": self.can_act() && !managed,
                    "removeTitle": if managed { MANAGED } else { "Remove name…" },
                    "editor": null,
                })
            })
            .collect();
        let records = self.new_records.trim();
        json!({
            "content": content,
            "rows": rows,
            "newRecords": self.new_records,
            "canCreate": self.can_act() && self.editor.is_none() && !records.is_empty(),
        })
    }

    fn editor_view(&self, name: &Name, target: &EditorTarget) -> Value {
        let editor = match target {
            EditorTarget::Records(text) => json!({
                "kind": "records",
                "text": text,
                "canSave": self.can_act() && !text.trim().is_empty(),
            }),
            EditorTarget::Job(selected) => json!({
                "kind": "job",
                "selected": selected.filter(|id| self.jobs.contains_key(id)),
                "options": self.jobs.values().map(|job| json!({
                    "id": job.id,
                    "path": job_path(job).display().to_string(),
                })).collect::<Vec<_>>(),
                "canSave": self.can_act() && selected.is_some_and(|id| self.jobs.contains_key(&id)),
            }),
        };
        json!({ "label": name.label, "link": name_link(name, None), "editor": editor })
    }

    fn settings_view(&self) -> Value {
        json!({
            "importDirectory": self.import_directory.as_ref().map(|p| format!("Ticket import folder: {}", p.display())),
            "daemon": self.current_daemon().map(|d| json!({
                "id": d.id().to_string(),
                "short": d.id().fmt_short().to_string(),
                "name": self.daemon_name,
            })),
            "clientId": self.client_id,
        })
    }

    fn confirm_view(&self) -> Value {
        let Some(removal) = &self.removal else {
            return Value::Null;
        };
        let (text, local) = match removal {
            Removal::Data(_) => ("Stop sharing this data? Files and names are kept. Names that follow this data will no longer receive updates.", false),
            Removal::Name(_) => ("Remove this name and its signing key? Cached records may remain resolvable.", false),
            Removal::Daemon(_) => ("Forget this daemon? It is removed from this browser's list only and keeps this browser authorized. Adding it again needs a new pairing ticket.", true),
        };
        // Forgetting is local and stays possible while the daemon is unreachable.
        json!({ "text": text, "canConfirm": local || self.can_act() })
    }
}

/// A name shown as its public URL with copy/open actions and inline status.
/// `removable` adds an inline remove action, enabled or not.
fn name_link(name: &Name, removable: Option<bool>) -> Value {
    let url = name.key.url().to_string();
    json!({
        "label": name.label,
        "url": url,
        "compact": compact_url(&url),
        "status": name_status(&name.state),
        "canRemove": removable,
    })
}

pub fn job_status(state: &JobState) -> String {
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

pub fn name_status(state: &NameState) -> String {
    match state {
        NameState::Disabled => "Disabled".into(),
        NameState::WaitingForJob => "Waiting for data".into(),
        NameState::NoRecords => "No records yet".into(),
        NameState::Publishing { .. } | NameState::PublishingRecords => "Publishing…".into(),
        NameState::Published { .. } | NameState::PublishedRecords { .. } => "Published".into(),
        NameState::Failed { error } => format!("Failed: {}", error.message),
    }
}

pub fn content_url(ticket: &BlobTicket) -> String {
    format!(
        "https://{}.blake3.net/",
        z32::encode(ticket.hash().as_bytes())
    )
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

/// Abbreviated label for a URL; copy actions always use the full URL.
pub fn compact_url(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_urls_abbreviate_keys_but_not_short_values() {
        let url = format!("https://{}.blake3.net/", "a".repeat(52));
        let compact = compact_url(&url);
        assert!(compact.contains('…') && compact.ends_with(".blake3.net"));
        assert_eq!(compact_url("https://example.com/"), "https://example.com/");
    }
}
