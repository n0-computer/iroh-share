use blobtorrent_proto::{Job, WatchEvent};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Model {
    pub jobs: BTreeMap<u64, Job>,
    pub names: BTreeMap<String, blobtorrent_proto::Name>,
    pub selected_name: Option<String>,
    pub selected: Option<u64>,
    pub ready: bool,
}

impl Model {
    pub fn reset(&mut self) {
        self.jobs.clear();
        self.names.clear();
        self.selected_name = None;
        self.selected = None;
        self.ready = false;
    }

    pub fn apply(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::JobUpdated(job) => {
                self.jobs.insert(job.id, *job);
            }
            WatchEvent::JobRemoved { id } => {
                self.jobs.remove(&id);
            }
            WatchEvent::NameUpdated(name) => {
                self.names.insert(name.label.clone(), *name);
            }
            WatchEvent::NameRemoved { label } => {
                self.names.remove(&label);
            }
            WatchEvent::SnapshotComplete => self.ready = true,
        }
        if self
            .selected_name
            .as_ref()
            .is_none_or(|label| !self.names.contains_key(label))
        {
            self.selected_name = self.names.keys().next().cloned();
        }
        if self.selected.is_none_or(|id| !self.jobs.contains_key(&id)) {
            self.selected = self.jobs.keys().next().copied();
        }
    }

    pub fn step_name(&mut self, down: bool) {
        let labels: Vec<_> = self.names.keys().cloned().collect();
        if labels.is_empty() {
            self.selected_name = None;
            return;
        }
        let index = labels
            .iter()
            .position(|label| Some(label) == self.selected_name.as_ref())
            .unwrap_or(0);
        let index = if down {
            (index + 1).min(labels.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected_name = Some(labels[index].clone());
    }

    pub fn step(&mut self, down: bool) {
        let ids: Vec<_> = self.jobs.keys().copied().collect();
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let index = ids
            .iter()
            .position(|id| Some(*id) == self.selected)
            .unwrap_or(0);
        let index = if down {
            (index + 1).min(ids.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected = Some(ids[index]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blobtorrent_proto::{JobKind, JobState};
    fn update(id: u64) -> WatchEvent {
        WatchEvent::JobUpdated(Box::new(Job {
            id,
            kind: JobKind::Share {
                path: "file".into(),
            },
            state: JobState::Queued,
        }))
    }
    #[test]
    fn selection_survives_updates_and_reconnect_discards_stale_jobs() {
        let mut model = Model::default();
        model.apply(update(2));
        model.apply(update(5));
        model.apply(WatchEvent::SnapshotComplete);
        model.step(true);
        model.apply(update(2));
        assert_eq!(model.selected, Some(5));
        model.apply(WatchEvent::JobRemoved { id: 5 });
        assert_eq!(model.selected, Some(2));
        model.reset();
        assert!(!model.ready);
        assert!(model.jobs.is_empty());
        model.apply(WatchEvent::SnapshotComplete);
        assert!(model.ready);
        assert_eq!(model.selected, None);
    }
}
