//! Per-user background launcher and installer lifecycle helper.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
#[path = "../background_macos.rs"]
mod macos;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iroh_share_proto::client::{self, ControlClient};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Parser)]
#[command(about = "Start or stop the current user's background iroh-share daemon")]
struct Args {
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Pair a new local GUI configuration after starting the daemon.
    #[arg(long)]
    setup_gui: bool,
    #[arg(long)]
    gui_config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    Stop,
    /// Install and start the current user's macOS LaunchAgent.
    #[cfg(target_os = "macos")]
    InstallAgent,
    /// Stop and unregister the current user's macOS LaunchAgent.
    #[cfg(target_os = "macos")]
    RemoveAgent,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let state = args
        .state_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(client::default_state_dir)?;
    let result = run(&state, args).await;
    if let Err(error) = &result {
        let _ = std::fs::create_dir_all(&state);
        if let Ok(mut log) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(state.join("launcher.log"))
        {
            let _ = writeln!(log, "{:?}: {error:#}", std::time::SystemTime::now());
        }
    }
    result
}

async fn run(state: &Path, args: Args) -> Result<()> {
    #[cfg(target_os = "macos")]
    match args.command {
        Some(Action::InstallAgent) => return macos::install(state, args.gui_config_dir).await,
        Some(Action::RemoveAgent) => return macos::remove(state).await,
        _ => {}
    }
    if matches!(args.command, Some(Action::Stop)) {
        anyhow::ensure!(!args.setup_gui, "--setup-gui cannot be used with stop");
        return stop(state).await;
    }
    std::fs::create_dir_all(state)?;
    // Serialize launcher invocations through startup and pairing.
    let _launcher = lock_launcher(state).await?;
    if !running(state)? {
        spawn(state)?;
    }
    let owner = wait_for_owner(state).await?;
    if args.setup_gui {
        setup_gui(&owner, args.gui_config_dir).await?;
    }
    Ok(())
}
async fn wait_for_owner(state: &Path) -> Result<ControlClient> {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let attempt = async {
                let owner = ControlClient::connect_local(state).await?;
                owner.list().await?;
                Ok::<_, anyhow::Error>(owner)
            };
            if let Ok(Ok(owner)) = tokio::time::timeout(Duration::from_secs(2), attempt).await {
                break owner;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .context("daemon did not become ready; see daemon.log")
}
async fn setup_gui(owner: &ControlClient, config: Option<PathBuf>) -> Result<()> {
    let config = config
        .or_else(|| dirs::config_dir().map(|p| p.join("iroh-share-gui")))
        .context("cannot determine GUI configuration directory")?;
    // An existing choice, including a remote daemon, belongs to the user.
    if client::configured_endpoint(&config)?.is_none() {
        let ticket = owner.create_pairing_ticket().await?;
        client::pair(&config, &ticket).await?;
        std::fs::write(config.join("local-endpoint"), ticket.addr.id.to_string())?;
    }
    Ok(())
}

async fn lock_launcher(state: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("launcher.lock"))?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match file.try_lock() {
                Ok(()) => return Ok::<_, anyhow::Error>(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .context("another launcher is still running")??;
    Ok(file)
}
fn running(state: &Path) -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.join("daemon.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(error) => Err(error.into()),
    }
}
fn spawn(state: &Path) -> Result<()> {
    let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "iroh-share.exe"
    } else {
        "iroh-share"
    });
    let log_path = state.join("daemon.log");
    if log_path.metadata().is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let previous = state.join("daemon.previous.log");
        if previous.exists() {
            std::fs::remove_file(&previous)?;
        }
        std::fs::rename(&log_path, previous)?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut command = Command::new(executable);
    command
        .arg("--state-dir")
        .arg(state)
        .arg("daemon")
        .arg("--no-pairing-ticket")
        .current_dir(dirs::home_dir().context("cannot determine home directory")?)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.spawn().context("cannot launch iroh-share daemon")?;
    Ok(())
}
async fn stop(state: &Path) -> Result<()> {
    if !state.exists() {
        return Ok(());
    }
    let _launcher = lock_launcher(state).await?;
    if !running(state)? {
        return Ok(());
    }
    let owner = ControlClient::connect_local(state).await?;
    // Closing the endpoint can race delivery of the acknowledgement; the lock
    // is the authoritative indication that shutdown and store cleanup finished.
    let result = owner.shutdown().await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while running(state)? {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| format!("daemon did not stop: {result:?}"))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn liveness_requires_a_held_lock_not_a_stale_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        assert!(!running(temp.path())?);
        let lock = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(temp.path().join("daemon.lock"))?;
        assert!(!running(temp.path())?);
        lock.lock()?;
        assert!(running(temp.path())?);
        drop(lock);
        assert!(!running(temp.path())?);
        Ok(())
    }
}
