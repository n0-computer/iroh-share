use anyhow::{Context, Result};
use iroh_share_proto::{client::ControlClient, DownloadSource, WatchEvent};
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;

pub enum Action {
    ExportNames(PathBuf),
    ExportName {
        label: String,
        path: PathBuf,
    },
    ImportNames(PathBuf),
    SetGateway(iroh_share_proto::GatewayConfig),
    CompletePath {
        id: u64,
        path: PathBuf,
    },
    Publish {
        path: PathBuf,
        include_directory_name: bool,
        label: Option<String>,
    },
    Refresh(u64),
    Import {
        source: DownloadSource,
        id: Option<u64>,
        label: Option<String>,
    },
    ShareMany(Vec<PathBuf>),
    Download {
        source: DownloadSource,
        target: PathBuf,
    },
    Remove(u64),
    CreateName {
        label: String,
        target: iroh_share_proto::NameTarget,
    },
    UpdateName {
        label: String,
        target: iroh_share_proto::NameTarget,
    },
    RemoveName(String),
}

pub enum Update {
    GatewaySaved(Result<iroh_share_proto::GatewaySnapshot, String>),
    Completion {
        id: u64,
        result: Result<iroh_share_proto::PathCompletions, String>,
    },
    NameSaved(Result<iroh_share_proto::Name, String>),
    Imported {
        job: iroh_share_proto::Job,
        name_error: Option<String>,
    },
    Published {
        job: iroh_share_proto::Job,
        name_error: Option<String>,
    },
    ImportDirectory(PathBuf),
    Connecting,
    Event(WatchEvent),
    Disconnected(String),
    ActionResult(String),
}

pub async fn run(
    config_dir: PathBuf,
    mut actions: mpsc::Receiver<Action>,
    tx: mpsc::Sender<Update>,
) {
    loop {
        if tx.send(Update::Connecting).await.is_err() {
            return;
        }
        let result = session(&config_dir, &mut actions, &tx).await;
        if tx.is_closed() || actions.is_closed() {
            return;
        }
        if let Err(error) = result {
            if tx
                .send(Update::Disconnected(format!("{error:#}")))
                .await
                .is_err()
            {
                return;
            }
        }
        // Never replay queued commands against a new daemon whose job IDs may differ.
        while actions.try_recv().is_ok() {}
        tokio::select! {
            _ = tx.closed() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn session(
    config_dir: &std::path::Path,
    actions: &mut mpsc::Receiver<Action>,
    tx: &mpsc::Sender<Update>,
) -> Result<()> {
    let client = ControlClient::connect_configured(config_dir).await?;
    let mut events = client.watch().await?;
    tx.send(Update::ImportDirectory(client.import_directory().await?))
        .await?;
    // Bound the first snapshot as well as connecting: an unreachable endpoint must
    // not leave the interface waiting forever for a daemon that no longer exists.
    let snapshot = async {
        loop {
            let event = events
                .recv()
                .await?
                .context("daemon disconnected")?
                .map_err(anyhow::Error::msg)?;
            let done = matches!(event, WatchEvent::SnapshotComplete);
            if done {
                // Commands queued by a stale screen must not target reused IDs
                // after reconnecting. Only enable input after this drain.
                while actions.try_recv().is_ok() {}
            }
            tx.send(Update::Event(event)).await?;
            if done {
                return Ok::<_, anyhow::Error>(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(15), snapshot)
        .await
        .context("initial snapshot timed out")??;
    let mut completions = tokio::task::JoinSet::new();
    let mut settings_requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = tx.closed() => return Ok(()),
            event = events.recv() => {
                let event = event?.context("daemon disconnected")?.map_err(anyhow::Error::msg)?;
                tx.send(Update::Event(event)).await?;
            }
            Some(result) = settings_requests.join_next() => {
                if let Ok(update) = result { tx.send(update).await?; }
            }
            Some(result) = completions.join_next() => {
                if let Ok(update) = result { tx.send(update).await?; }
            }
            action = actions.recv() => {
                let Some(action) = action else { return Ok(()); };
                let result = match action {
                    Action::ExportNames(path) => client.export_names(&path).await.map(|()| format!("Exported pkarr names to {}", path.display())),
                    Action::ExportName { label, path } => client.export_name(label, &path).await.map(|()| format!("Exported pkarr name to {}", path.display())),
                    Action::ImportNames(path) => client.import_names(&path).await.map(|outcomes| import_summary(&outcomes)),
                    Action::SetGateway(config) => {
                        let client = client.clone();
                        settings_requests.spawn(async move {
                            Update::GatewaySaved(client.set_gateway(config).await.map_err(|error| format!("{error:#}")))
                        });
                        continue;
                    }
                    Action::CompletePath { id, path } => {
                        completions.abort_all();
                        let client = client.clone();
                        completions.spawn(async move {
                            Update::Completion { id, result: client.complete_path(path).await.map_err(|error| format!("{error:#}")) }
                        });
                        continue;
                    }
                    Action::CreateName { label, target } => {
                        let result = client.create_name(label, target).await.map_err(|e| format!("{e:#}"));
                        tx.send(Update::NameSaved(result)).await?;
                        continue;
                    },
                    Action::UpdateName { label, target } => {
                        let result = client.update_name(label, target).await.map_err(|e| format!("{e:#}"));
                        tx.send(Update::NameSaved(result)).await?;
                        continue;
                    },
                    Action::RemoveName(label) => client.remove_name(label.clone()).await.map(|()| format!("Removed name {label}")),
                    Action::ShareMany(paths) => {
                        let mut added = 0;
                        let mut errors = Vec::new();
                        for path in paths {
                            match client.share(path.clone()).await {
                                Ok(job) => {
                                    added += 1;
                                    tx.send(Update::Event(WatchEvent::JobUpdated(Box::new(job)))).await?;
                                },
                                Err(error) => errors.push(format!("{}: {error:#}", path.display())),
                            }
                        }
                        Ok(format!("Shared {added} paths{}", if errors.is_empty() { String::new() } else { format!("; {}", errors.join("; ")) }))
                    }
                    Action::Import { source, id, label } => {
                        match client.import(source, id).await {
                            Ok(job) => {
                                let name_error = if let Some(label) = label {
                                    client.create_name(label, iroh_share_proto::NameTarget::Job(job.id)).await.err().map(|e| format!("{e:#}"))
                                } else { None };
                                tx.send(Update::Imported { job, name_error }).await?;
                                continue;
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Action::Refresh(id) => client.refresh(id).await.map(|()| "Refreshing directory… Its name will follow the updated content.".into()),
                    Action::Publish { path, label, include_directory_name } => {
                        match client.share_with_options(path, include_directory_name).await {
                            Ok(job) => {
                                let name_error = if let Some(label) = label {
                                    client.create_name(label, iroh_share_proto::NameTarget::Job(job.id)).await.err().map(|e| format!("{e:#}"))
                                } else { None };
                                tx.send(Update::Published { job, name_error }).await?;
                                continue;
                            }
                            Err(error) => Err(error),
                        }
                    },
                    Action::Download { source, target } => client.download(source, target).await.map(|job| format!("Added data {}", job.id)),
                    Action::Remove(id) => client.remove(id).await.map(|()| format!("Removed data {id}")),
                };
                tx.send(Update::ActionResult(match result { Ok(message) => message, Err(error) => format!("{error:#}") })).await?;
            }
        }
    }
}

fn import_summary(outcomes: &[iroh_share_proto::ImportedName]) -> String {
    let imported = outcomes
        .iter()
        .filter(|o| matches!(o.outcome, iroh_share_proto::ImportOutcome::Imported { .. }))
        .count();
    let skipped = outcomes.len() - imported;
    match skipped {
        0 => format!("Imported {imported} pkarr names"),
        _ => format!("Imported {imported} pkarr names; skipped {skipped} already managed"),
    }
}
