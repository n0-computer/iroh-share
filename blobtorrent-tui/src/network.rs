use anyhow::{Context, Result};
use blobtorrent_proto::{client::ControlClient, BlobTicket, WatchEvent};
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;

pub enum Action {
    CompletePath {
        id: u64,
        path: PathBuf,
    },
    Share(PathBuf),
    Download {
        ticket: BlobTicket,
        target: PathBuf,
    },
    Remove(u64),
    CreateName {
        label: String,
        target: blobtorrent_proto::NameTarget,
    },
    UpdateName {
        label: String,
        target: blobtorrent_proto::NameTarget,
    },
    RemoveName(String),
}

pub enum Update {
    Completion {
        id: u64,
        result: Result<blobtorrent_proto::PathCompletions, String>,
    },
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
    loop {
        tokio::select! {
            _ = tx.closed() => return Ok(()),
            event = events.recv() => {
                let event = event?.context("daemon disconnected")?.map_err(anyhow::Error::msg)?;
                tx.send(Update::Event(event)).await?;
            }
            Some(result) = completions.join_next() => {
                if let Ok(update) = result { tx.send(update).await?; }
            }
            action = actions.recv() => {
                let Some(action) = action else { return Ok(()); };
                let result = match action {
                    Action::CompletePath { id, path } => {
                        completions.abort_all();
                        let client = client.clone();
                        completions.spawn(async move {
                            Update::Completion { id, result: client.complete_path(path).await.map_err(|error| format!("{error:#}")) }
                        });
                        continue;
                    }
                    Action::CreateName { label, target } => client.create_name(label, target).await.map(|name| format!("Created {}: {}", name.label, name.key.url())),
                    Action::UpdateName { label, target } => client.update_name(label, target).await.map(|name| format!("Updated {}", name.label)),
                    Action::RemoveName(label) => client.remove_name(label.clone()).await.map(|()| format!("Removed name {label}")),
                    Action::Share(path) => client.share(path).await.map(|job| format!("Added data {}", job.id)),
                    Action::Download { ticket, target } => client.download(ticket, target).await.map(|job| format!("Added data {}", job.id)),
                    Action::Remove(id) => client.remove(id).await.map(|()| format!("Removed data {id}")),
                };
                tx.send(Update::ActionResult(match result { Ok(message) => message, Err(error) => format!("{error:#}") })).await?;
            }
        }
    }
}
