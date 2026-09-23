use std::{
    io,
    path::{Path, PathBuf, MAIN_SEPARATOR},
};

#[derive(Default)]
pub struct Completion {
    matches: Vec<String>,
    selected: Option<usize>,
    last_value: String,
}

impl Completion {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn complete(&mut self, value: &mut String, backwards: bool) -> io::Result<()> {
        self.complete_with_home(value, backwards, dirs::home_dir().as_deref())
    }

    fn complete_with_home(
        &mut self,
        value: &mut String,
        backwards: bool,
        home: Option<&Path>,
    ) -> io::Result<()> {
        if value == "~" {
            expand_home(value, home)?;
            value.push(MAIN_SEPARATOR);
        }
        if self.matches.is_empty() || *value != self.last_value {
            self.reset();
            self.matches = candidates(value, home)?;
            if self.matches.is_empty() {
                return Ok(());
            }
            let common = common_prefix(&self.matches);
            if !backwards && self.matches.len() > 1 && common.len() > value.len() {
                *value = common;
                self.last_value = value.clone();
                return Ok(());
            }
        }
        let count = self.matches.len();
        let index = match (self.selected, backwards) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
        };
        *value = self.matches[index].clone();
        self.selected = Some(index);
        self.last_value = value.clone();
        // A unique directory can be explored further with the very next Tab.
        if count == 1 && value.ends_with(MAIN_SEPARATOR) {
            self.reset();
        }
        Ok(())
    }

    pub fn hint(&self) -> String {
        if self.matches.is_empty() {
            return "Tab completes paths · Shift-Tab cycles backwards".into();
        }
        let count = self.matches.len();
        let preview = self
            .matches
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("  |  ");
        format!(
            "{count} match{}: {preview}{}",
            if count == 1 { "" } else { "es" },
            if count > 3 { " …" } else { "" }
        )
    }
}

pub fn expand_path(value: &str) -> io::Result<PathBuf> {
    expand_home(value, dirs::home_dir().as_deref())
}

fn expand_home(value: &str, home: Option<&Path>) -> io::Result<PathBuf> {
    if value == "~" {
        return home.map(Path::to_owned).ok_or_else(missing_home);
    }
    if let Some(rest) = value
        .strip_prefix('~')
        .filter(|s| s.starts_with(std::path::is_separator))
    {
        return home
            .map(|path| path.join(rest.trim_start_matches(std::path::is_separator)))
            .ok_or_else(missing_home);
    }
    Ok(value.into())
}

fn missing_home() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "cannot determine home directory")
}

fn candidates(value: &str, home: Option<&Path>) -> io::Result<Vec<String>> {
    let (prefix, partial) = match value.rfind(std::path::is_separator) {
        Some(index) => value.split_at(index + 1),
        None => ("", value),
    };
    let directory = if prefix.is_empty() {
        PathBuf::from(".")
    } else {
        expand_home(prefix, home)?
    };
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(partial)
            || (name.starts_with('.') && !partial.starts_with('.'))
            || name.chars().any(char::is_control)
        {
            continue;
        }
        let suffix = if entry.path().is_dir() {
            MAIN_SEPARATOR.to_string()
        } else {
            String::new()
        };
        matches.push(format!("{prefix}{name}{suffix}"));
    }
    matches.sort();
    Ok(matches)
}

fn common_prefix(values: &[String]) -> String {
    let mut prefix = values[0].clone();
    for value in &values[1..] {
        let bytes = prefix
            .chars()
            .zip(value.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        prefix.truncate(bytes);
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_common_prefix_and_cycles_both_directions() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("résumé one.txt"), "")?;
        std::fs::write(dir.path().join("résumé two.txt"), "")?;
        let base = format!("{}{MAIN_SEPARATOR}", dir.path().display());
        let mut value = format!("{base}r");
        let mut completion = Completion::default();
        completion.complete_with_home(&mut value, false, None)?;
        assert_eq!(value, format!("{base}résumé "));
        completion.complete_with_home(&mut value, false, None)?;
        assert_eq!(value, format!("{base}résumé one.txt"));
        completion.complete_with_home(&mut value, false, None)?;
        assert_eq!(value, format!("{base}résumé two.txt"));
        completion.complete_with_home(&mut value, true, None)?;
        assert_eq!(value, format!("{base}résumé one.txt"));
        // An edit invalidates the previous candidate set.
        value = format!("{base}missing");
        completion.complete_with_home(&mut value, false, None)?;
        assert_eq!(value, format!("{base}missing"));
        Ok(())
    }

    #[test]
    fn descends_directories_and_expands_home_on_submission() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("folder"))?;
        std::fs::write(dir.path().join("folder/file.txt"), "")?;
        std::fs::write(dir.path().join(".hidden"), "")?;
        let mut value = format!("~{MAIN_SEPARATOR}f");
        let mut completion = Completion::default();
        completion.complete_with_home(&mut value, false, Some(dir.path()))?;
        assert_eq!(value, format!("~{MAIN_SEPARATOR}folder{MAIN_SEPARATOR}"));
        completion.complete_with_home(&mut value, false, Some(dir.path()))?;
        assert_eq!(
            expand_home(&value, Some(dir.path()))?,
            dir.path().join("folder/file.txt")
        );
        assert_eq!(
            candidates(&format!("~{MAIN_SEPARATOR}"), Some(dir.path()))?.len(),
            1
        );
        assert_eq!(
            candidates(&format!("~{MAIN_SEPARATOR}."), Some(dir.path()))?,
            vec![format!("~{MAIN_SEPARATOR}.hidden")]
        );
        assert!(candidates(&format!("~{MAIN_SEPARATOR}missing/"), Some(dir.path())).is_err());
        Ok(())
    }
}
