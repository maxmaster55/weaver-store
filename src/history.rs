use std::any::Any;

pub(crate) type CloneFn = fn(&dyn Any) -> Box<dyn Any>;

pub(crate) struct Change {
    pub(crate) id: usize,
    pub(crate) old: Box<dyn Any>,
    pub(crate) new: Box<dyn Any>,
    pub(crate) clone: CloneFn,
}

pub(crate) struct Step {
    pub(crate) label: String,
    pub(crate) changes: Vec<Change>,
}

impl Step {
    /// Add a change, merging with an earlier change to the same signal.
    pub(crate) fn record(&mut self, change: Change) {
        match self.changes.iter_mut().find(|c| c.id == change.id) {
            Some(existing) => existing.new = change.new,
            None => self.changes.push(change),
        }
    }
}

pub(crate) struct History {
    pub(crate) undo: Vec<Step>,
    pub(crate) redo: Vec<Step>,
    pub(crate) open: Option<Step>,
    pub(crate) depth: usize,
    pub(crate) limit: usize,
}

impl Default for History {
    fn default() -> Self {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            depth: 0,
            limit: 200,
        }
    }
}

impl History {
    pub(crate) fn push(&mut self, step: Step) {
        if step.changes.is_empty() {
            return;
        }
        self.undo.push(step);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
}

/// The values of every signal at one moment. Restore it with
/// [`crate::Store::restore`] (which is itself undoable).
pub struct Snapshot {
    pub(crate) values: Vec<(usize, Box<dyn Any>, CloneFn)>,
}

impl Clone for Snapshot {
    fn clone(&self) -> Self {
        Snapshot {
            values: self
                .values
                .iter()
                .map(|(id, v, c)| (*id, c(v.as_ref()), *c))
                .collect(),
        }
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Snapshot({} signals)", self.values.len())
    }
}
