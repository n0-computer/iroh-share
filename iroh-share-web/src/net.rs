//! Runs [`Action`]s against the daemon and turns replies into [`Update`]s.

use std::{future::Future, time::Duration};

use anyhow::{Context as _, Result};
use iroh::{Endpoint, EndpointAddr};
use iroh_share_proto::{
    ControlMessage, ControlProtocol, CreateName, Download, ExportName, ExportNames,
    GetImportDirectory, Import, ImportNames, ImportOutcome, PairingStatus, PairingTicket,
    RemoveName, Share, UpdateName, Watch, WatchEvent, PAIRING_ALPN,
};
use irpc::Client;
use n0_future::time::{sleep, timeout};

use crate::app::{Action, Update};

/// Same bounds as the native client: requests and the first snapshot must not
/// leave the page waiting forever for a daemon that is gone.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(60);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_DELAY: Duration = Duration::from_secs(2);

pub async fn execute(client: Client<ControlProtocol>, action: Action) -> Update {
    let client = &client;
    match action {
        Action::CompletePath { id, path } => {
            let request = iroh_share_proto::CompletePath { path };
            let result = rpc(client, request, "path completion timed out").await;
            Update::Completion {
                id,
                result: result.map_err(text),
            }
        }
        Action::Share {
            path,
            include_directory_name,
        } => {
            let request = Share {
                path,
                include_directory_name,
            };
            let result = rpc(
                client,
                request,
                "share request timed out; check data before retrying",
            );
            Update::Published(result.await.map_err(text))
        }
        Action::Import { source, id } => {
            let request = Import { source, id };
            let result = rpc(
                client,
                request,
                "import request timed out; check data before retrying",
            );
            Update::Imported(result.await.map_err(text))
        }
        Action::Download { source, target } => {
            let request = Download { source, target };
            let result = rpc(
                client,
                request,
                "download request timed out; check data before retrying",
            );
            Update::Downloaded(result.await.map_err(text))
        }
        Action::Remove(id) => {
            let request = iroh_share_proto::Remove { id };
            let result = rpc(
                client,
                request,
                "remove request timed out; check data before retrying",
            );
            Update::ActionResult(result.await.map(|()| "Removed data".into()).map_err(text))
        }
        Action::CreateName { label, target } => {
            let request = CreateName { label, target };
            let result = rpc(
                client,
                request,
                "create name timed out; check names before retrying",
            );
            Update::NameSaved(result.await.map_err(text))
        }
        Action::UpdateName { label, target } => {
            let request = UpdateName { label, target };
            let result = rpc(
                client,
                request,
                "update name timed out; check names before retrying",
            );
            Update::NameSaved(result.await.map_err(text))
        }
        Action::RemoveName(label) => {
            let request = RemoveName {
                label: label.clone(),
            };
            let result = rpc(
                client,
                request,
                "remove name timed out; check names before retrying",
            );
            let message = format!("Removed name {label}");
            Update::ActionResult(result.await.map(|()| message).map_err(text))
        }
        Action::ExportNames => {
            let result = archive(client.rpc(ExportNames {})).await;
            Update::Exported(result.map(|b| ("pkarr-names.zip".into(), b)).map_err(text))
        }
        Action::ExportName { label, file_name } => {
            let result = archive(client.rpc(ExportName { label })).await;
            Update::Exported(result.map(|b| (file_name, b)).map_err(text))
        }
        Action::ImportNames(archive) => {
            let result = async {
                timeout(ARCHIVE_TIMEOUT, client.rpc(ImportNames { archive }))
                    .await
                    .context("name import timed out; check names before retrying")??
                    .map_err(anyhow::Error::msg)
            };
            Update::ActionResult(
                result
                    .await
                    .map(|outcomes| {
                        let imported = outcomes
                            .iter()
                            .filter(|o| matches!(o.outcome, ImportOutcome::Imported { .. }))
                            .count();
                        match outcomes.len() - imported {
                            0 => format!("Imported {imported} pkarr names"),
                            skipped => format!(
                        "Imported {imported} pkarr names; skipped {skipped} already managed"
                    ),
                        }
                    })
                    .map_err(text),
            )
        }
    }
}

/// Watch the daemon forever, reconnecting after errors. The caller cancels it
/// by dropping the future.
pub async fn watch(client: Client<ControlProtocol>, emit: impl Fn(Update)) {
    loop {
        emit(Update::Connecting);
        if let Err(error) = watch_once(&client, &emit).await {
            emit(Update::Disconnected(text(error)));
        }
        sleep(RETRY_DELAY).await;
    }
}

async fn watch_once(client: &Client<ControlProtocol>, emit: &impl Fn(Update)) -> Result<()> {
    let mut events = timeout(RPC_TIMEOUT, client.server_streaming(Watch {}, 128))
        .await
        .context("watch request timed out")??;
    let directory = rpc(
        client,
        GetImportDirectory {},
        "import directory request timed out",
    )
    .await?;
    emit(Update::ImportDirectory(directory));
    let mut snapshot_done = false;
    let deadline = sleep(SNAPSHOT_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = &mut deadline, if !snapshot_done => anyhow::bail!("initial snapshot timed out"),
        };
        let event = event?
            .context("daemon disconnected")?
            .map_err(anyhow::Error::msg)?;
        snapshot_done |= matches!(event, WatchEvent::SnapshotComplete);
        emit(Update::Event(event));
    }
}

/// Redeem over an authenticated connection; the daemon authorizes this endpoint.
pub async fn pair(endpoint: Endpoint, ticket: PairingTicket) -> Update {
    let result = async {
        let connection = endpoint.connect(ticket.addr.clone(), PAIRING_ALPN).await?;
        let result = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(ticket.secret.as_bytes()).await?;
            send.finish()?;
            let response = recv.read_to_end(1).await?;
            match response.as_slice() {
                [value] if *value == PairingStatus::Accepted as u8 => Ok(()),
                [value] if *value == PairingStatus::StorageFailure as u8 => {
                    anyhow::bail!("daemon could not save authorization; retry the ticket")
                }
                _ => anyhow::bail!(
                    "pairing ticket is invalid or already claimed; run iroh-share control pair for a new ticket"
                ),
            }
        }
        .await;
        connection.close(0u32.into(), b"pairing complete");
        result
    };
    let result: Result<EndpointAddr> = timeout(Duration::from_secs(15), result)
        .await
        .context("pairing timed out; retry with the same ticket")
        .and_then(|r| r)
        .map(|()| ticket.addr.clone());
    Update::Paired(result.map_err(text))
}

/// One bounded request whose daemon reply is an `RpcResult`.
async fn rpc<M, T>(client: &Client<ControlProtocol>, request: M, context: &'static str) -> Result<T>
where
    M: irpc::Channels<
            ControlProtocol,
            Tx = irpc::channel::oneshot::Sender<Result<T, String>>,
            Rx = irpc::channel::none::NoReceiver,
        > + Send
        + 'static,
    ControlProtocol: From<M>,
    ControlMessage: From<irpc::WithChannels<M, ControlProtocol>>,
    T: irpc::RpcMessage,
{
    timeout(RPC_TIMEOUT, client.rpc(request))
        .await
        .context(context)??
        .map_err(anyhow::Error::msg)
}

async fn archive(
    request: impl Future<Output = irpc::Result<Result<Vec<u8>, String>>>,
) -> Result<Vec<u8>> {
    timeout(ARCHIVE_TIMEOUT, request)
        .await
        .context("name export timed out")??
        .map_err(anyhow::Error::msg)
}

fn text(error: anyhow::Error) -> String {
    format!("{error:#}")
}
