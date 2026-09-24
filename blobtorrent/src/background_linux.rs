//! Per-user systemd service registration used by the Linux installer.
use super::*;

const UNIT: &str = "blobtorrent.service";
const DESKTOP_ENTRY: &str = "blobtorrent.desktop";

fn ensure_not_root() -> Result<()> {
    let output = Command::new("id").arg("-u").output()?;
    let uid: u32 = String::from_utf8(output.stdout)?.trim().parse()?;
    anyhow::ensure!(
        uid != 0,
        "install Blobtorrent for the logged-in user, without sudo"
    );
    Ok(())
}
fn ensure_user_manager() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["--user", "show", "--property=Version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("systemctl is not available")?;
    anyhow::ensure!(
        status.success(),
        "no systemd user session is running for this account; log in through a \
         systemd-managed session or run `loginctl enable-linger`, then retry"
    );
    Ok(())
}
fn unit_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("cannot determine configuration directory")?
        .join("systemd/user")
        .join(UNIT))
}
fn desktop_entry_path() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("cannot determine data directory")?
        .join("applications")
        .join(DESKTOP_ENTRY))
}
fn systemctl(arguments: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "systemctl --user {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Escapes `%` so systemd does not expand specifiers inside a path.
fn unit_value(path: &Path) -> String {
    path.to_string_lossy().replace('%', "%%")
}
/// Quotes one command-line argument for `ExecStart=`.
fn unit_argument(path: &Path) -> String {
    let escaped = unit_value(path).replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
fn unit_definition(executable: &Path, state: &Path) -> String {
    let log = unit_value(&state.join("daemon.log"));
    format!(
        "[Unit]\n\
         Description=Blobtorrent background daemon\n\
         Documentation=https://github.com/n0-computer/blobtorrent\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} --state-dir {} daemon --no-pairing-ticket\n\
         WorkingDirectory=%h\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         TimeoutStopSec=30\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        unit_argument(executable),
        unit_argument(state),
    )
}

/// Quotes one `Exec=` argument for a desktop entry.
///
/// The quoting escape runs first and the general string escape second, as
/// the specification requires.
fn desktop_argument(path: &Path) -> String {
    let mut quoted = String::from("\"");
    for character in path.to_string_lossy().chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
}
fn desktop_entry(gui: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Blobtorrent\n\
         Comment=Share and download iroh blob collections\n\
         Exec={}\n\
         Terminal=false\n\
         Categories=Network;FileTransfer;\n",
        desktop_argument(gui)
    )
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub async fn install(state: &Path, gui: Option<PathBuf>) -> Result<()> {
    ensure_not_root()?;
    let helper = std::env::current_exe()?;
    let executable = helper.with_file_name("blobtorrent");
    anyhow::ensure!(
        executable.is_file(),
        "the daemon must be installed beside its helper"
    );
    ensure_user_manager()?;
    remove(state).await?;
    std::fs::create_dir_all(state)?;
    let unit = unit_path()?;
    std::fs::create_dir_all(unit.parent().context("invalid unit path")?)?;
    std::fs::write(&unit, unit_definition(&executable, state))?;
    let gui_executable = helper.with_file_name("blobtorrent-gui");
    if gui_executable.is_file() {
        let entry = desktop_entry_path()?;
        std::fs::create_dir_all(entry.parent().context("invalid desktop entry path")?)?;
        std::fs::write(&entry, desktop_entry(&gui_executable))?;
    }
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT])?;
    let owner = super::wait_for_owner(state).await?;
    super::setup_gui(&owner, gui).await?;
    Ok(())
}
pub async fn remove(state: &Path) -> Result<()> {
    ensure_not_root()?;
    // Ask the daemon to shut down over its control API first, so a running
    // service exits cleanly and systemd does not count the stop as a failure.
    super::stop(state).await?;
    let unit = unit_path()?;
    if unit.is_file() {
        systemctl(&["disable", "--now", UNIT])?;
        remove_if_present(&unit)?;
        systemctl(&["daemon-reload"])?;
        let _ = systemctl(&["reset-failed", UNIT]);
    }
    remove_if_present(&desktop_entry_path()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_quotes_paths_and_only_restarts_failed_exits() {
        let home = Path::new("/home/a b/50%");
        let executable = home.join(".local/share/blobtorrent/bin/blobtorrent");
        let state = home.join(".local/state/blobtorrent");
        let unit = unit_definition(&executable, &state);
        assert!(unit.contains(
            "ExecStart=\"/home/a b/50%%/.local/share/blobtorrent/bin/blobtorrent\" \
             --state-dir \"/home/a b/50%%/.local/state/blobtorrent\" daemon --no-pairing-ticket\n"
        ));
        assert!(unit.contains(
            "StandardOutput=append:/home/a b/50%%/.local/state/blobtorrent/daemon.log\n"
        ));
        assert!(unit.contains("Restart=on-failure\n"));
        assert!(unit.contains("WantedBy=default.target\n"));
    }
    #[test]
    fn desktop_entry_escapes_reserved_characters() {
        let gui = Path::new("/home/a b/$x\"y\\z/blobtorrent-gui");
        let entry = desktop_entry(gui);
        assert!(entry.contains("Exec=\"/home/a b/\\\\$x\\\\\"y\\\\\\\\z/blobtorrent-gui\"\n"));
        assert!(desktop_argument(Path::new("/opt/blobtorrent-gui")) == "\"/opt/blobtorrent-gui\"");
    }
}
