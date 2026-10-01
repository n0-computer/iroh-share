//! Active root-hash requests; individual child blobs are not attributed to collections.
use std::collections::BTreeMap;

use iroh_blobs::{
    provider::events::{EventMask, EventSender, ProviderMessage, RequestMode},
    Hash,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

pub fn track() -> (EventSender, watch::Receiver<BTreeMap<Hash, u32>>) {
    let (tx, mut rx) = mpsc::channel(64);
    let events = EventSender::new(
        tx,
        EventMask {
            get: RequestMode::Notify,
            ..EventMask::DEFAULT
        },
    );
    let (counts, snapshot) = watch::channel(BTreeMap::<Hash, u32>::new());
    tokio::spawn(async move {
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else { break };
                    if let ProviderMessage::GetRequestReceivedNotify(message) = message {
                        let hash = message.inner.request.hash;
                        counts.send_modify(|counts| *counts.entry(hash).or_default() += 1);
                        requests.spawn(async move {
                            let mut updates = message.rx;
                            // Notify holds this channel open for the request's lifetime,
                            // including cancellation and failure, without progress traffic.
                            while let Ok(Some(_)) = updates.recv().await {}
                            hash
                        });
                    }
                }
                Some(Ok(hash)) = requests.join_next(), if !requests.is_empty() => {
                    counts.send_modify(|counts| {
                        if let Some(count) = counts.get_mut(&hash) {
                            *count -= 1;
                            if *count == 0 { counts.remove(&hash); }
                        }
                    });
                }
            }
        }
        counts.send_modify(|counts| counts.clear());
    });
    (events, snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{endpoint::presets, Endpoint};
    use iroh_blobs::{protocol::GetRequest, provider::StreamPair};

    #[tokio::test]
    async fn concurrent_requests_clear_on_completion_or_cancellation() -> anyhow::Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let server = Endpoint::builder(presets::Minimal)
                .alpns(vec![iroh_blobs::ALPN.to_vec()])
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let client = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let (client_conn, server_conn) = tokio::try_join!(
                async {
                    Ok::<_, anyhow::Error>(client.connect(server.addr(), iroh_blobs::ALPN).await?)
                },
                async { Ok::<_, anyhow::Error>(server.accept().await.unwrap().await?) },
            )?;
            let (mut send, _recv) = client_conn.open_bi().await?;
            send.write_all(&[0]).await?;
            let (writer, reader) = server_conn.accept_bi().await?;
            let (events, mut counts) = track();
            let pair = StreamPair::new(1, reader, writer, events);
            let hash = Hash::new(b"collection");
            let other = Hash::new(b"other collection");
            let first = pair.get_request(|| GetRequest::all(hash)).await?;
            let second = pair.get_request(|| GetRequest::all(hash)).await?;
            let unrelated = pair.get_request(|| GetRequest::all(other)).await?;
            counts
                .wait_for(|counts| counts.get(&hash) == Some(&2) && counts.get(&other) == Some(&1))
                .await?;
            drop(first);
            counts
                .wait_for(|counts| counts.get(&hash) == Some(&1))
                .await?;
            let cancelled = tokio::spawn(async move {
                let _request = second;
                std::future::pending::<()>().await;
            });
            cancelled.abort();
            let _ = cancelled.await;
            counts
                .wait_for(|counts| !counts.contains_key(&hash) && counts.get(&other) == Some(&1))
                .await?;
            drop(unrelated);
            counts.wait_for(|counts| counts.is_empty()).await?;
            client.close().await;
            server.close().await;
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }
}
