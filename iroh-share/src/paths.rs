//! Resolve and complete paths in the daemon's filesystem context.
use anyhow::{Context, Result};
use iroh_share_proto::{PathCandidate, PathCompletions, PathKind};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MAX_CANDIDATES: usize = 256;

pub fn resolve(path: &Path) -> Result<PathBuf> {
    resolve_with_home(path, dirs::home_dir().as_deref())
}

fn resolve_with_home(path: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let path = match path.strip_prefix("~") {
        Ok(rest) => home
            .context("cannot determine daemon home directory")?
            .join(rest),
        Err(_) => path.to_owned(),
    };
    Ok(std::path::absolute(path)?)
}

pub fn complete(path: &Path) -> Result<PathCompletions> {
    complete_with_home(path, dirs::home_dir().as_deref())
}

fn complete_with_home(path: &Path, home: Option<&Path>) -> Result<PathCompletions> {
    let text = path.to_str().context("path is not UTF-8")?;
    let (directory, partial) = if text == "~" {
        (resolve_with_home(path, home)?, String::new())
    } else {
        let (prefix, partial) = match text.rfind(std::path::is_separator) {
            Some(index) => text.split_at(index + 1),
            None => (".", text),
        };
        (
            resolve_with_home(Path::new(prefix), home)?,
            partial.to_owned(),
        )
    };
    let mut entries = BTreeMap::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("cannot list {}", directory.display()))?
    {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(&partial)
            || (name.starts_with('.') && !partial.starts_with('.'))
            || name.chars().any(char::is_control)
        {
            continue;
        }
        let kind = if entry.path().is_dir() {
            PathKind::Directory
        } else {
            PathKind::File
        };
        let mut path = entry.path();
        if kind == PathKind::Directory {
            path.push("");
        }
        entries.insert(path, kind);
        if entries.len() > MAX_CANDIDATES {
            entries.pop_last();
            truncated = true;
        }
    }
    let candidates: Vec<_> = entries
        .into_iter()
        .map(|(path, kind)| PathCandidate { path, kind })
        .collect();
    // Never infer a common prefix from an incomplete candidate set.
    let common_prefix = if truncated || candidates.is_empty() {
        expanded_prefix(&directory, &partial)
    } else {
        let mut common = candidates[0]
            .path
            .to_str()
            .context("invalid path")?
            .to_owned();
        for candidate in &candidates[1..] {
            let count = common
                .chars()
                .zip(candidate.path.to_str().context("invalid path")?.chars())
                .take_while(|(a, b)| a == b)
                .map(|(c, _)| c.len_utf8())
                .sum();
            common.truncate(count);
        }
        common.into()
    };
    Ok(PathCompletions {
        common_prefix,
        candidates,
        truncated,
    })
}

fn expanded_prefix(directory: &Path, partial: &str) -> PathBuf {
    let mut value = directory.to_path_buf();
    if partial.is_empty() {
        value.push("");
    } else {
        value.push(partial);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daemon_home_completion_and_submission_agree() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("folder"))?;
        std::fs::write(root.path().join("folder/résumé one"), b"")?;
        std::fs::write(root.path().join("folder/résumé two"), b"")?;
        std::fs::write(root.path().join(".hidden"), b"")?;
        let found = complete_with_home(Path::new("~"), Some(root.path()))?;
        assert_eq!(found.candidates.len(), 1);
        assert_eq!(found.candidates[0].kind, PathKind::Directory);
        assert!(found.candidates[0]
            .path
            .to_str()
            .unwrap()
            .ends_with(std::path::MAIN_SEPARATOR));
        let found = complete_with_home(&found.candidates[0].path, None)?;
        assert_eq!(found.candidates.len(), 2);
        assert_eq!(found.common_prefix, root.path().join("folder/résumé "));
        assert_eq!(
            resolve_with_home(Path::new("~/folder/résumé one"), Some(root.path()))?,
            found.candidates[0].path
        );
        assert_eq!(
            complete_with_home(Path::new("~/."), Some(root.path()))?
                .candidates
                .len(),
            1
        );
        assert!(complete_with_home(Path::new("~/missing/"), Some(root.path())).is_err());
        assert_eq!(
            resolve(Path::new("relative"))?,
            std::env::current_dir()?.join("relative")
        );
        Ok(())
    }
    #[test]
    fn large_results_are_bounded_without_extending_partial_prefix() -> Result<()> {
        let root = tempfile::tempdir()?;
        for i in 0..=MAX_CANDIDATES {
            std::fs::write(root.path().join(format!("file{i:04}")), b"")?;
        }
        let prefix = root.path().join("f");
        let found = complete(&prefix)?;
        assert!(found.truncated);
        assert_eq!(found.candidates.len(), MAX_CANDIDATES);
        assert_eq!(found.common_prefix, prefix);
        Ok(())
    }
}
