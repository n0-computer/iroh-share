use anyhow::{Context, Result};
use iroh_share_proto::{client::ControlClient, DownloadSource, WatchEvent};
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;

pub enum Action {
    CompletePath {
        id: u64,
        path: PathBuf,
    },
    Share(PathBuf),
    ShareWithDirectoryName(PathBuf),
    Import {
        source: DownloadSource,
        id: Option<u64>,
        /// False drops the top-level directory. True keeps the paths of new
        /// data and the remembered layout of updated data.
        include_directory_name: bool,
        /// False keeps new data off Mainline from the start. True announces
        /// new data and keeps the setting of updated data.
        announce: bool,
    },
    Refresh(u64),
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
    SetAnnounce {
        id: u64,
        announce: bool,
    },
}

pub enum Update {
    Completion {
        id: u64,
        result: Result<iroh_share_proto::PathCompletions, String>,
    },
    ImportDirectory(PathBuf),
    /// None when the daemon predates per-data announcement settings.
    Announce(Option<iroh_share_proto::AnnounceSnapshot>),
    Added {
        job: iroh_share_proto::Job,
        new: bool,
    },
    Connecting,
    Event(WatchEvent),
    Disconnected(String),
    ActionResult(Result<String, String>),
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
            tx.send(update(event)).await?;
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
                tx.send(update(event)).await?;
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
                    Action::ShareWithDirectoryName(path) => match client.share_with_options(path, true, true).await {
                        Ok(job) => { tx.send(Update::Added { job, new: true }).await?; continue; }
                        Err(error) => Err(error),
                    },
                    Action::Share(path) => match client.share(path).await {
                        Ok(job) => { tx.send(Update::Added { job, new: true }).await?; continue; }
                        Err(error) => Err(error),
                    },
                    Action::Import { source, id, include_directory_name, announce } => {
                        // The plain request also works with daemons that predate the options.
                        let result = if include_directory_name && announce {
                            client.import(source, id).await
                        } else {
                            client.import_with_options(source, id, include_directory_name, announce).await
                        };
                        match result {
                            Ok(job) => { tx.send(Update::Added { job, new: id.is_none() }).await?; continue; }
                            Err(error) => Err(error),
                        }
                    }
                    Action::SetAnnounce { id, announce } => client.set_announce(id, announce).await.map(|()| {
                        if announce { format!("Data {id} is announced on Mainline") } else { format!("Data {id} is no longer announced on Mainline; direct tickets still work") }
                    }),
                    Action::Refresh(id) => client.refresh(id).await.map(|()| "Refresh requested; linked names will follow the updated content".into()),
                    Action::Download { source, target } => client.download(source, target).await.map(|job| format!("Added data {}", job.id)),
                    Action::Remove(id) => client.remove(id).await.map(|()| format!("Removed data {id}")),
                };
                tx.send(Update::ActionResult(result.map_err(|error| format!("{error:#}")))).await?;
            }
        }
    }
}

/// Announcement settings go to their own update; everything else is a model event.
fn update(event: WatchEvent) -> Update {
    match event {
        WatchEvent::AnnounceUpdated(snapshot) => Update::Announce(Some(snapshot)),
        event => Update::Event(event),
    }
}
