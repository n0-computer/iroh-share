use iroh_share_proto::{PathCompletions, PathKind};

#[derive(Default)]
pub struct Completion {
    result: Option<PathCompletions>,
    selected: Option<usize>,
    last_value: String,
    next_id: u64,
    pending: Option<(u64, String, bool)>,
}

impl Completion {
    pub fn reset(&mut self) {
        self.result = None;
        self.selected = None;
        self.last_value.clear();
        self.pending = None;
    }

    /// Cycle cached results or return a new request ID. Repeated Tab while a
    /// request is pending does not flood the daemon.
    pub fn complete(&mut self, value: &mut String, backwards: bool) -> Option<u64> {
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, original, _)| original == value)
        {
            return None;
        }
        if self.result.is_some() && self.last_value == *value {
            self.cycle(value, backwards);
            return None;
        }
        self.reset();
        self.next_id += 1;
        self.pending = Some((self.next_id, value.clone(), backwards));
        Some(self.next_id)
    }

    /// Ignore responses for text or prompts that have changed since the request.
    pub fn receive(
        &mut self,
        id: u64,
        result: Result<PathCompletions, String>,
        value: &mut String,
    ) -> Result<(), String> {
        let Some((pending_id, original, backwards)) = &self.pending else {
            return Ok(());
        };
        if *pending_id != id || original != value {
            return Ok(());
        }
        let backwards = *backwards;
        self.pending = None;
        let result = result?;
        let common = result.common_prefix.to_string_lossy().into_owned();
        let extend =
            !backwards && !result.truncated && result.candidates.len() > 1 && common != *value;
        self.result = Some(result);
        if extend {
            *value = common;
            self.last_value = value.clone();
        } else {
            self.cycle(value, backwards);
        }
        Ok(())
    }

    fn cycle(&mut self, value: &mut String, backwards: bool) {
        let Some(result) = &self.result else {
            return;
        };
        let count = result.candidates.len();
        if count == 0 {
            self.reset();
            return;
        }
        let index = match (self.selected, backwards) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
        };
        let candidate = &result.candidates[index];
        *value = candidate.path.to_string_lossy().into_owned();
        self.selected = Some(index);
        self.last_value = value.clone();
        // Use the daemon's kind, not the client's separator conventions.
        if count == 1 && !result.truncated && candidate.kind == PathKind::Directory {
            self.reset();
        }
    }

    pub fn hint(&self) -> String {
        if self.pending.is_some() {
            return "Completing on daemon…".into();
        }
        let Some(result) = &self.result else {
            return "Tab completes daemon paths · Shift-Tab cycles backwards".into();
        };
        let preview = result
            .candidates
            .iter()
            .take(3)
            .map(|entry| entry.path.display().to_string())
            .collect::<Vec<_>>()
            .join("  |  ");
        format!(
            "{} matches{}: {preview}",
            result.candidates.len(),
            if result.truncated {
                " (more available; refine path)"
            } else {
                ""
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_share_proto::PathCandidate;
    fn suggestions() -> PathCompletions {
        PathCompletions {
            common_prefix: "/daemon/résumé ".into(),
            candidates: ["/daemon/résumé one", "/daemon/résumé two"]
                .into_iter()
                .map(|path| PathCandidate {
                    path: path.into(),
                    kind: PathKind::File,
                })
                .collect(),
            truncated: false,
        }
    }
    #[test]
    fn cycles_daemon_results_and_ignores_stale_responses() {
        let mut completion = Completion::default();
        let mut value = "~/r".to_owned();
        let id = completion.complete(&mut value, false).unwrap();
        assert!(completion.complete(&mut value, false).is_none());
        completion
            .receive(id, Ok(suggestions()), &mut value)
            .unwrap();
        assert_eq!(value, "/daemon/résumé ");
        assert!(completion.complete(&mut value, false).is_none());
        assert_eq!(value, "/daemon/résumé one");
        completion.complete(&mut value, true);
        assert_eq!(value, "/daemon/résumé two");
        completion.reset();
        value = "edited".into();
        let new_id = completion.complete(&mut value, false).unwrap();
        assert_ne!(new_id, id);
        completion
            .receive(id, Ok(suggestions()), &mut value)
            .unwrap();
        assert_eq!(value, "edited");
        completion
            .receive(new_id, Err("permission denied".into()), &mut value)
            .unwrap_err();
        assert_eq!(value, "edited");
    }
    #[test]
    fn directory_kind_controls_descent_with_foreign_separators() {
        let mut completion = Completion::default();
        let mut value = "C:\\da".to_owned();
        let id = completion.complete(&mut value, false).unwrap();
        completion
            .receive(
                id,
                Ok(PathCompletions {
                    common_prefix: "C:\\data\\".into(),
                    candidates: vec![PathCandidate {
                        path: "C:\\data\\".into(),
                        kind: PathKind::Directory,
                    }],
                    truncated: false,
                }),
                &mut value,
            )
            .unwrap();
        assert_eq!(value, "C:\\data\\");
        assert!(completion.complete(&mut value, false).is_some());
    }
}
