//! Serves the 0.1.13 control protocol for older clients, by translating to the current one.
use iroh_share_proto::{
    self as v6,
    v5::{self, ControlMessage},
    AnnounceSnapshot, RpcResult,
};
use irpc::{
    channel::{mpsc, none::NoReceiver, oneshot},
    Channels, Client, WithChannels,
};

pub(crate) const GATEWAY_REMOVED: &str =
    "This daemon no longer includes a gateway. Install Iroh Link Gateway to browse blake3.net and pkarr.net links.";

/// Returns a sender for 0.1.13 requests, which are answered through `actor`.
pub fn serve(actor: Client<v6::ControlProtocol>) -> irpc::LocalSender<v5::ControlProtocol> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            // The actor orders requests; a slow one must not hold up the others.
            tokio::spawn(handle(actor.clone(), message));
        }
    });
    irpc::LocalSender::from(tx)
}

async fn handle(actor: Client<v6::ControlProtocol>, message: ControlMessage) {
    match message {
        ControlMessage::ListControl(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::AllowControl(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::RevokeControl(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Share(m) => {
            let WithChannels { inner, tx, .. } = m;
            let request = v6::Share {
                path: inner.path,
                include_directory_name: inner.include_directory_name,
                announce: true,
            };
            forward(&actor, request, tx).await
        }
        ControlMessage::CreateName(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::UpdateName(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::RemoveName(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::ListNames(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Download(m) => {
            let WithChannels { inner, tx, .. } = m;
            let request = v6::Download {
                source: inner.source,
                target: inner.target,
                announce: true,
            };
            forward(&actor, request, tx).await
        }
        ControlMessage::List(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Remove(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Watch(m) => watch(&actor, m.tx).await,
        ControlMessage::CompletePath(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::CreatePairingTicket(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::GetGateway(m) => {
            let _ = m.tx.send(Ok(removed_gateway())).await;
        }
        ControlMessage::SetGateway(m) => {
            let _ = m.tx.send(Err(GATEWAY_REMOVED.to_owned())).await;
        }
        ControlMessage::Shutdown(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Refresh(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::Import(m) => {
            let WithChannels { inner, tx, .. } = m;
            let request = v6::Import {
                source: inner.source,
                id: inner.id,
                include_directory_name: None,
                announce: None,
            };
            forward(&actor, request, tx).await
        }
        ControlMessage::GetImportDirectory(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::ExportNames(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::ExportName(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::ImportNames(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::ImportWithOptions(m) => {
            let WithChannels { inner, tx, .. } = m;
            let request = v6::Import {
                source: inner.source,
                id: inner.id,
                include_directory_name: Some(inner.include_directory_name),
                announce: Some(inner.announce),
            };
            forward(&actor, request, tx).await
        }
        ControlMessage::SetAnnounce(m) => forward(&actor, m.inner, m.tx).await,
        ControlMessage::WatchAnnounce(m) => watch_announce(&actor, m.tx).await,
    }
}

/// Sends `request` to the actor and its answer back to the old client.
async fn forward<Req, Res>(
    actor: &Client<v6::ControlProtocol>,
    request: Req,
    tx: oneshot::Sender<RpcResult<Res>>,
) where
    v6::ControlProtocol: From<Req>,
    v6::ControlMessage: From<WithChannels<Req, v6::ControlProtocol>>,
    Req: Channels<v6::ControlProtocol, Tx = oneshot::Sender<RpcResult<Res>>, Rx = NoReceiver>,
    RpcResult<Res>: irpc::RpcMessage,
{
    let result = actor
        .rpc(request)
        .await
        .unwrap_or_else(|err| Err(format!("daemon stopped: {err}")));
    let _ = tx.send(result).await;
}

/// The current watch stream, without announcement events, and with the
/// gateway snapshot that 0.1.13 sent before the end of the snapshot.
async fn watch(actor: &Client<v6::ControlProtocol>, tx: mpsc::Sender<RpcResult<v5::WatchEvent>>) {
    let mut events = match actor.server_streaming(v6::Watch {}, 128).await {
        Ok(events) => events,
        Err(err) => {
            let _ = tx.send(Err(format!("daemon stopped: {err}"))).await;
            return;
        }
    };
    while let Ok(Some(event)) = events.recv().await {
        let event = match event {
            Ok(v6::WatchEvent::JobUpdated(job)) => Ok(v5::WatchEvent::JobUpdated(job)),
            Ok(v6::WatchEvent::NameUpdated(name)) => Ok(v5::WatchEvent::NameUpdated(name)),
            Ok(v6::WatchEvent::NameRemoved { label }) => Ok(v5::WatchEvent::NameRemoved { label }),
            Ok(v6::WatchEvent::JobRemoved { id }) => Ok(v5::WatchEvent::JobRemoved { id }),
            Ok(v6::WatchEvent::SnapshotComplete) => {
                let gateway = v5::WatchEvent::GatewayUpdated(removed_gateway());
                if tx.send(Ok(gateway)).await.is_err() {
                    return;
                }
                Ok(v5::WatchEvent::SnapshotComplete)
            }
            Ok(v6::WatchEvent::AnnounceUpdated(_)) => continue,
            Err(err) => Err(err),
        };
        if tx.send(event).await.is_err() {
            return;
        }
    }
}

/// The announcement events of the current watch stream.
async fn watch_announce(
    actor: &Client<v6::ControlProtocol>,
    tx: mpsc::Sender<RpcResult<AnnounceSnapshot>>,
) {
    let mut events = match actor.server_streaming(v6::Watch {}, 128).await {
        Ok(events) => events,
        Err(err) => {
            let _ = tx.send(Err(format!("daemon stopped: {err}"))).await;
            return;
        }
    };
    while let Ok(Some(event)) = events.recv().await {
        let snapshot = match event {
            Ok(v6::WatchEvent::AnnounceUpdated(snapshot)) => Ok(snapshot),
            Ok(_) => continue,
            Err(err) => Err(err),
        };
        if tx.send(snapshot).await.is_err() {
            return;
        }
    }
}

/// What the daemon reports to clients from 0.1.7 and earlier, which still
/// show gateway settings.
pub(crate) fn removed_gateway() -> v5::GatewaySnapshot {
    v5::GatewaySnapshot {
        config: v5::GatewayConfig {
            enabled: false,
            ..Default::default()
        },
        state: v5::GatewayState::Disabled,
    }
}
