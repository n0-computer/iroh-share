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
    drop(background);
    Ok(())
}
