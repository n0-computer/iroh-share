use anyhow::{Context, Result};
use blobtorrent_proto::client::{self, ControlClient};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

struct Background(PathBuf);
impl Drop for Background {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_blobtorrent-background"))
            .arg("--state-dir")
            .arg(&self.0)
            .arg("stop")
            .status();
    }
}
fn setup(state: &Path, gui: &Path) -> Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_blobtorrent-background"))
        .arg("--state-dir")
        .arg(state)
        .arg("--gui-config-dir")
        .arg(gui)
        .arg("--setup-gui")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn background_setup_pairs_once_and_preserves_remote_configuration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("daemon state");
    let gui = temp.path().join("gui config");
    let background = Background(state.clone());
    setup(&state, &gui)?;
    let identity = std::fs::read(gui.join("control-client.key"))?;
    let config = std::fs::read(gui.join("client.json"))?;
    let connection = ControlClient::connect_configured(&gui).await?;
    assert!(connection.list().await?.is_empty());
    drop(connection);
    setup(&state, &gui)?;
    assert_eq!(std::fs::read(gui.join("control-client.key"))?, identity);
    assert_eq!(std::fs::read(gui.join("client.json"))?, config);
    assert!(!std::fs::read_to_string(state.join("daemon.log"))?.contains("one-client ticket"));
    let remote = client::load_or_create_key(&temp.path().join("remote.key"))?.public();
    client::configure_endpoint(&gui, Some(remote))?;
    client::configure_endpoint(&state, Some(remote))?;
    setup(&state, &gui)?;
    assert_eq!(client::configured_endpoint(&gui)?, Some(remote));
    let output = Command::new(env!("CARGO_BIN_EXE_blobtorrent-background"))
        .arg("--state-dir")
        .arg(&state)
        .arg("stop")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!state.join("control.addr").exists());
    assert_eq!(std::fs::read(gui.join("control-client.key"))?, identity);
    assert!(state
        .join("daemon.key")
        .try_exists()
        .context("state check")?);
    #[cfg(unix)]
    {
        let mut process = Command::new(env!("CARGO_BIN_EXE_blobtorrent"))
            .arg("--state-dir")
            .arg(&state)
            .arg("daemon")
            .arg("--no-announce")
            .arg("--no-pairing-ticket")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Ok(owner) = ControlClient::connect_local(&state).await {
                    if owner.list().await.is_ok() {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await?;
        assert!(Command::new("/bin/kill")
            .arg("-TERM")
            .arg(process.id().to_string())
            .status()?
            .success());
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(status) = process.try_wait()? {
                    break Ok::<_, std::io::Error>(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await??;
        assert!(status.success());
        assert!(!state.join("control.addr").exists());
    }
    drop(background);
    Ok(())
}
