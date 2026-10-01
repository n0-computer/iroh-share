//! Persist and supervise the embedded content-addressed web gateway.
use anyhow::{Context, Result};
use iroh::{endpoint::presets, Endpoint};
use iroh_local_gateway::Gateway;
use iroh_mainline_endpoint_discovery::{AddrIndex, Resolver};
use iroh_share_proto::{GatewayConfig, GatewaySnapshot, GatewayState};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone)]
pub struct Controller(mpsc::Sender<Command>);
pub struct Manager {
    pub updates: watch::Receiver<GatewaySnapshot>,
    controller: Controller,
    task: JoinHandle<()>,
}
enum Command {
    Set(GatewayConfig, oneshot::Sender<Result<GatewaySnapshot>>),
    Shutdown,
}
struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<()>>,
}
impl Manager {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("gateway.json");
        let config = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid gateway settings")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => GatewayConfig::default(),
            Err(error) => return Err(error.into()),
        };
        validate(&config)?;
        let (status, updates) = watch::channel(initial(&config));
        let (tx, rx) = mpsc::channel(8);
        let task = tokio::spawn(supervise(path, config, rx, status));
        Ok(Self {
            updates,
            controller: Controller(tx),
            task,
        })
    }
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }
    pub fn snapshot(&self) -> GatewaySnapshot {
        self.updates.borrow().clone()
    }
    pub async fn shutdown(self) {
        let _ = self.controller.0.send(Command::Shutdown).await;
        let _ = self.task.await;
    }
}
impl Controller {
    pub async fn set(&self, config: GatewayConfig) -> Result<GatewaySnapshot> {
        let (tx, rx) = oneshot::channel();
        self.0
            .send(Command::Set(config, tx))
            .await
            .context("gateway manager stopped")?;
        rx.await.context("gateway manager stopped")?
    }
}
fn initial(config: &GatewayConfig) -> GatewaySnapshot {
    GatewaySnapshot {
        config: config.clone(),
        state: if config.enabled {
            GatewayState::Starting
        } else {
            GatewayState::Disabled
        },
    }
}
fn validate(config: &GatewayConfig) -> Result<()> {
    iroh_local_gateway::validate_listen_addr(config.listen)?;
    if let Some(server) = config.index_server {
        anyhow::ensure!(
            server.port() != 0
                && !server.ip().is_unspecified()
                && !server.ip().is_multicast()
                && !server.ip().is_broadcast(),
            "invalid index server address"
        );
    }
    Ok(())
}
fn save(path: &Path, config: &GatewayConfig) -> Result<()> {
    validate(config)?;
    let temporary = path.with_extension(format!("{}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(config)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
fn start(config: &GatewayConfig, status: &watch::Sender<GatewaySnapshot>) -> Option<Running> {
    let next = initial(config);
    status.send_if_modified(|current| {
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
    if !config.enabled {
        return None;
    }
    let (stop, shutdown) = watch::channel(false);
    Some(Running {
        stop,
        task: tokio::spawn(serve(config.clone(), status.clone(), shutdown)),
    })
}
async fn stop(running: &mut Option<Running>) {
    if let Some(mut running) = running.take() {
        running.stop.send_replace(true);
        if tokio::time::timeout(Duration::from_secs(4), &mut running.task)
            .await
            .is_err()
        {
            running.task.abort();
            let _ = running.task.await;
        }
    }
}
async fn supervise(
    path: PathBuf,
    mut config: GatewayConfig,
    mut rx: mpsc::Receiver<Command>,
    status: watch::Sender<GatewaySnapshot>,
) {
    let mut running = start(&config, &status);
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                Some(Command::Set(next, reply)) => {
                    if let Err(error) = save(&path, &next) {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                    stop(&mut running).await;
                    config = next;
                    running = start(&config, &status);
                    let _ = reply.send(Ok(status.borrow().clone()));
                }
                Some(Command::Shutdown) | None => break,
            },
            result = async { (&mut running.as_mut().expect("guarded").task).await }, if running.is_some() => {
                running = None;
                let error = match result {
                    Ok(Ok(())) => "gateway stopped unexpectedly".to_owned(),
                    Ok(Err(error)) => format!("{error:#}"),
                    Err(error) => error.to_string(),
                };
                tracing::warn!(%error, "Gateway failed; retrying in 30 seconds");
                status.send_replace(GatewaySnapshot { config: config.clone(), state: GatewayState::Failed { error } });
            }
            _ = tokio::time::sleep(Duration::from_secs(30)), if running.is_none() && config.enabled => {
                running = start(&config, &status);
            }
        }
    }
    stop(&mut running).await;
}

async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

async fn serve(
    config: GatewayConfig,
    status: watch::Sender<GatewaySnapshot>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    // Each gateway instance owns its endpoint and can dial the blob daemon normally.
    // The gateway only initiates blob connections; it accepts no iroh protocols.
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(Vec::new())
        .bind()
        .await?;
    let prepare = async {
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .context("cannot bind gateway HTTP address")?;
        let dht = n0_mainline::Dht::client()?;
        let index = match config.index_server {
            Some(server) => AddrIndex::udp(dht.clone(), server).await?,
            None => AddrIndex::discover(dht.clone()).await?,
        };
        let resolver = Resolver::new(dht, index);
        Ok::<_, anyhow::Error>((Gateway::new(endpoint.clone(), resolver), listener))
    };
    let result = async {
        let (gateway, listener) = tokio::select! {
            _ = wait_shutdown(&mut shutdown) => return Ok(()),
            result = prepare => result?,
        };
        let listen = listener.local_addr()?;
        tracing::info!(%listen, "Embedded gateway listening");
        status.send_replace(GatewaySnapshot {
            config: config.clone(),
            state: GatewayState::Running {
                listen,
                endpoint: endpoint.id(),
            },
        });
        let mut graceful = shutdown.clone();
        let serve = gateway.serve(listener, async move {
            let _ = wait_shutdown(&mut graceful).await;
        });
        tokio::pin!(serve);
        tokio::select! {
            result = &mut serve => Ok(result?),
            _ = wait_shutdown(&mut shutdown) => {
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut serve).await;
                Ok(())
            }
        }
    }
    .await;
    endpoint.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn running(manager: &mut Manager) -> Result<std::net::SocketAddr> {
        loop {
            let state = manager.updates.borrow_and_update().state.clone();
            match state {
                GatewayState::Running { listen, .. } => return Ok(listen),
                GatewayState::Failed { error } => anyhow::bail!(error),
                _ => manager.updates.changed().await?,
            }
        }
    }
    #[tokio::test]
    async fn embedded_http_persists_config_and_stops_without_a_blob_store() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(20), async {
            let root = tempfile::tempdir()?;
            let mut manager = Manager::load(root.path())?;
            assert_eq!(manager.snapshot().state, GatewayState::Starting);
            assert!(manager.snapshot().config.enabled);
            let config = GatewayConfig { enabled: true, listen: "127.0.0.1:0".parse()?, index_server: Some("127.0.0.1:9".parse()?) };
            manager.controller().set(config.clone()).await?;
            let address = running(&mut manager).await?;
            let mut stream = tokio::net::TcpStream::connect(address).await?;
            stream.write_all(b"GET /blake3/not-a-hash HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await?;
            let mut response = String::new(); stream.read_to_string(&mut response).await?;
            assert!(response.starts_with("HTTP/1.1 400"), "{response}");
            let invalid = GatewayConfig { listen: "0.0.0.0:8080".parse()?, ..config.clone() };
            assert!(manager.controller().set(invalid).await.is_err());
            assert!(matches!(manager.snapshot().state, GatewayState::Running { .. }));
            manager.shutdown().await;
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
            let mut manager = Manager::load(root.path())?;
            assert_eq!(manager.snapshot().config, config);
            let address = running(&mut manager).await?;
            manager.controller().set(GatewayConfig { enabled: false, ..config }).await?;
            assert_eq!(manager.snapshot().state, GatewayState::Disabled);
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
            manager.shutdown().await;
            assert!(!Manager::load(root.path())?.snapshot().config.enabled);
            assert!(!root.path().join("blobs").exists());
            Ok::<_, anyhow::Error>(())
        }).await?
    }
    #[tokio::test]
    async fn failed_save_keeps_config_and_busy_port_reports_failure() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let root = tempfile::tempdir()?;
            save(
                &root.path().join("gateway.json"),
                &GatewayConfig {
                    enabled: false,
                    ..GatewayConfig::default()
                },
            )?;
            let mut manager = Manager::load(root.path())?;
            std::fs::remove_file(root.path().join("gateway.json"))?;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let config = GatewayConfig {
                enabled: true,
                listen: listener.local_addr()?,
                index_server: Some("127.0.0.1:9".parse()?),
            };
            std::fs::create_dir(root.path().join("gateway.json"))?;
            assert!(manager.controller().set(config.clone()).await.is_err());
            assert_eq!(manager.snapshot().state, GatewayState::Disabled);
            std::fs::remove_dir(root.path().join("gateway.json"))?;
            manager.controller().set(config).await?;
            assert!(running(&mut manager).await.is_err());
            manager.shutdown().await;
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }
}
