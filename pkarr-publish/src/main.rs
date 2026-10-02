//! Edit and republish a directory of pkarr records.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use n0_mainline::{Dht, MutableItem};
use pkarr_publish::{check_pair, entry, read_key, read_record, records, scan, sign, write_record};

/// How often each record is republished.
const REFRESH: Duration = Duration::from_secs(600);
/// How soon a failed publication is retried.
const RETRY: Duration = Duration::from_secs(30);
/// How often the daemon looks for new or changed files.
const SCAN: Duration = Duration::from_secs(10);
/// Time limit for one DHT lookup or publication.
const DHT_TIMEOUT: Duration = Duration::from_secs(60);

/// Edit and republish a directory of pkarr records.
///
/// Each name is `<name>.pkarr`, the signed record, plus `<name>.key`, its
/// 32-byte signing seed, where the name is edited. A server that only
/// republishes needs just the `.pkarr` files.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The directory with the records.
    #[arg(long, env = "PKARR_DIR", default_value = ".")]
    dir: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the names, their public keys and current versions.
    List,
    /// Print a name's records.
    Show { name: String },
    /// Edit a name's records in $EDITOR, sign them and publish once.
    ///
    /// Needs `<name>.key`. Without `<name>.pkarr`, starts with no records. To
    /// create a name, write 32 random bytes to `<name>.key` first, for example
    /// with `head -c 32 /dev/urandom > <name>.key && chmod 600 <name>.key`.
    Edit { name: String },
    /// Republish every record in the directory until stopped.
    ///
    /// Publishes exactly the files in the directory and picks up new and
    /// changed ones. It never reads records from the DHT.
    Daemon,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Cmd::List => list(&args.dir),
        Cmd::Show { name } => show(&args.dir, &name),
        Cmd::Edit { name } => edit(&args.dir, &name).await,
        Cmd::Daemon => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            daemon(&args.dir).await
        }
    }
}

fn list(dir: &Path) -> Result<()> {
    for entry in scan(dir)? {
        let key = entry.key.as_deref().map(read_key).transpose();
        let record = entry.record.as_deref().map(read_record).transpose();
        let status = match (&key, &record) {
            (Err(e), _) | (_, Err(e)) => format!("error: {e:#}"),
            (Ok(Some(key)), Ok(Some(record))) if check_pair(key, record).is_err() => {
                "error: the key does not match the record".into()
            }
            (Ok(key), Ok(record)) => {
                let version = record
                    .as_ref()
                    .map_or("no record yet".to_owned(), |r| format_seq(r.seq()));
                let editable = if key.is_some() { "key" } else { "read-only" };
                format!("{version}, {editable}")
            }
        };
        let public = match (&key, &record) {
            (_, Ok(Some(record))) => z32::encode(record.key()),
            (Ok(Some(key)), _) => z32::encode(key.verifying_key().as_bytes()),
            _ => "?".into(),
        };
        println!("{}\t{public}\t{status}", entry.name);
    }
    Ok(())
}

fn show(dir: &Path, name: &str) -> Result<()> {
    let entry = entry(dir, name)?;
    let path = entry
        .record
        .with_context(|| format!("{name} has no record"))?;
    let item = read_record(&path)?;
    print!("{}", header(name, &item));
    print!("{}", records::text(item.key(), item.value())?);
    Ok(())
}

/// Comment lines naming the record, which the records parser ignores.
fn header(name: &str, item: &MutableItem) -> String {
    format!(
        "; name: {name}\n; key: {}\n; version: {}\n",
        z32::encode(item.key()),
        format_seq(item.seq())
    )
}

async fn edit(dir: &Path, name: &str) -> Result<()> {
    let entry = entry(dir, name)?;
    let Some(key_path) = entry.key else {
        if entry.record.is_some() {
            bail!("{name} has no key here, so it is read-only");
        }
        bail!("unknown name {name}: create {name}.key first (see `edit --help`)");
    };
    let key = read_key(&key_path)?;
    let public = *key.verifying_key().as_bytes();
    let record_path = dir.join(format!("{name}.{}", pkarr_publish::RECORD_EXTENSION));
    let previous = entry.record.as_deref().map(read_record).transpose()?;
    if let Some(previous) = &previous {
        check_pair(&key, previous)?;
    }
    let original = match &previous {
        Some(item) => records::text(&public, item.value())?,
        None => String::new(),
    };
    let mut intro = match &previous {
        Some(item) => header(name, item),
        None => format!(
            "; name: {name}\n; key: {}\n; new name\n",
            z32::encode(&public)
        ),
    };
    intro.push_str(
        "; One record per line, for example: @ 300 IN TXT \"hello\"\n\
         ; Lines starting with ; are ignored. Save an empty file to cancel.\n",
    );
    let mut text = original.clone();
    let packet = loop {
        let edited = run_editor(name, &format!("{intro}{text}"))?;
        text = edited
            .lines()
            .filter(|line| !line.trim_start().starts_with(';'))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        if text.trim().is_empty() {
            println!("Cancelled, {name} is unchanged");
            return Ok(());
        }
        if previous.is_some() && text == original {
            println!("No changes, {name} is unchanged");
            return Ok(());
        }
        match records::packet(&public, &text) {
            Ok(packet) => break packet,
            // Reopen the editor with the problem at the top.
            Err(error) => {
                intro = format!("; error: {error:#}\n{}", strip_errors(&intro));
            }
        }
    };
    let item = sign(&key, &packet, previous.as_ref().map(MutableItem::seq))?;
    write_record(&record_path, &item)?;
    println!(
        "Signed {name} ({}), version {}",
        z32::encode(&public),
        format_seq(item.seq())
    );
    println!("Publishing once…");
    let dht = dht()?;
    match tokio::time::timeout(DHT_TIMEOUT, dht.put_mutable(item, None)).await {
        Ok(Ok(_)) => println!("Published. Run `daemon` somewhere to keep it alive."),
        Ok(Err(error)) => {
            println!("Publishing failed: {error}. The record is saved; a daemon will publish it.")
        }
        Err(_) => println!("Publishing timed out. The record is saved; a daemon will publish it."),
    }
    Ok(())
}

/// Removes earlier error lines from the editor intro.
fn strip_errors(intro: &str) -> String {
    intro
        .lines()
        .filter(|line| !line.starts_with("; error:"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Opens `text` in the user's editor and returns what they saved.
fn run_editor(name: &str, text: &str) -> Result<String> {
    let file = tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .suffix(".zone")
        .tempfile()?;
    std::fs::write(file.path(), text)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.into());
    // $EDITOR may carry arguments, as in `code --wait`.
    let mut parts = editor.split_whitespace();
    let program = parts.next().context("empty $EDITOR")?;
    let status = Command::new(program)
        .args(parts)
        .arg(file.path())
        .status()
        .with_context(|| format!("cannot start editor {editor:?}"))?;
    if !status.success() {
        bail!("the editor exited with {status}");
    }
    Ok(std::fs::read_to_string(file.path())?)
}

/// What the daemon knows about one record file.
struct Watched {
    modified: Option<SystemTime>,
    item: Option<MutableItem>,
    due: Instant,
}

async fn daemon(dir: &Path) -> Result<()> {
    let dht = dht()?;
    tracing::info!(dir = %dir.display(), "republishing pkarr records");
    let mut watched = HashMap::<String, Watched>::new();
    let run = async {
        loop {
            let entries = scan(dir)?;
            watched.retain(|name, _| {
                entries
                    .iter()
                    .any(|e| &e.name == name && e.record.is_some())
            });
            for entry in entries {
                let Some(path) = entry.record else { continue };
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let state = watched.entry(entry.name.clone()).or_insert(Watched {
                    modified: None,
                    item: None,
                    due: Instant::now(),
                });
                if state.item.is_none() || state.modified != modified {
                    state.modified = modified;
                    match read_record(&path) {
                        Ok(item) => {
                            tracing::info!(name = %entry.name, version = %format_seq(item.seq()), "loaded record");
                            state.item = Some(item);
                            state.due = Instant::now();
                        }
                        Err(error) => {
                            tracing::warn!(name = %entry.name, "{error:#}");
                            state.item = None;
                            continue;
                        }
                    }
                }
                if Instant::now() < state.due {
                    continue;
                }
                let Some(item) = state.item.clone() else {
                    continue;
                };
                let ok = match tokio::time::timeout(
                    DHT_TIMEOUT,
                    dht.put_mutable(item.clone(), None),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!(name = %entry.name, version = %format_seq(item.seq()), "published");
                        true
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(name = %entry.name, %error, "publishing failed");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(name = %entry.name, "publishing timed out");
                        false
                    }
                };
                state.due = Instant::now() + if ok { REFRESH } else { RETRY };
            }
            tokio::time::sleep(SCAN).await;
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        result = run => result,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("stopping");
            Ok(())
        }
    }
}

/// Starts a DHT client on a random port, so it never collides with another
/// node on the default port.
fn dht() -> Result<Dht> {
    Ok(Dht::builder().port(0).build()?)
}

/// Formats a pkarr sequence number, microseconds since the epoch, as UTC.
fn format_seq(seq: i64) -> String {
    let secs = seq.div_euclid(1_000_000);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sequence_numbers_as_dates() {
        assert_eq!(format_seq(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_seq(1_790_095_662_296_563), "2026-09-22 16:47:42 UTC");
    }
}
