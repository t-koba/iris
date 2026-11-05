use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::SystemTime;

const DEFAULT_UNDO_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoOpKind {
    Copy,
    Move,
    Rename,
    CreateFolder,
    TrashDelete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoItem {
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub trashed_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoableOp {
    pub kind: UndoOpKind,
    pub items: Vec<UndoItem>,
    pub performed_at: SystemTime,
}

#[derive(Clone, Debug)]
pub struct UndoJournal {
    undo_stack: VecDeque<UndoableOp>,
    redo_stack: VecDeque<UndoableOp>,
    limit: usize,
}

impl Default for UndoJournal {
    fn default() -> Self {
        Self::with_limit(DEFAULT_UNDO_LIMIT)
    }
}

impl UndoJournal {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            limit: limit.max(1),
        }
    }

    pub fn record(&mut self, op: UndoableOp) {
        self.undo_stack.push_back(op);
        while self.undo_stack.len() > self.limit {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
    }

    pub fn pop_undo(&mut self) -> Option<UndoableOp> {
        self.undo_stack.pop_back()
    }

    pub fn peek_undo(&self) -> Option<&UndoableOp> {
        self.undo_stack.back()
    }

    pub fn push_redo(&mut self, op: UndoableOp) {
        self.redo_stack.push_back(op);
        while self.redo_stack.len() > self.limit {
            self.redo_stack.pop_front();
        }
    }

    pub fn push_undo_from_redo(&mut self, op: UndoableOp) {
        self.undo_stack.push_back(op);
        while self.undo_stack.len() > self.limit {
            self.undo_stack.pop_front();
        }
    }

    pub fn pop_redo(&mut self) -> Option<UndoableOp> {
        self.redo_stack.pop_back()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_enforces_limit_and_drops_oldest() {
        let mut journal = UndoJournal::with_limit(2);
        journal.record(test_op("a"));
        journal.record(test_op("b"));
        journal.record(test_op("c"));

        assert_eq!(
            journal.pop_undo().unwrap().items[0].source_path,
            PathBuf::from("c")
        );
        assert_eq!(
            journal.pop_undo().unwrap().items[0].source_path,
            PathBuf::from("b")
        );
        assert!(journal.pop_undo().is_none());
    }

    #[test]
    fn recording_new_operation_clears_redo_stack() {
        let mut journal = UndoJournal::default();
        journal.record(test_op("a"));
        let op = journal.pop_undo().unwrap();
        journal.push_redo(op);
        assert!(journal.can_redo());

        journal.record(test_op("b"));

        assert!(!journal.can_redo());
        assert_eq!(
            journal.pop_undo().unwrap().items[0].source_path,
            PathBuf::from("b")
        );
        assert!(journal.pop_undo().is_none());
    }

    #[test]
    fn redo_push_preserves_remaining_redo_stack() {
        let mut journal = UndoJournal::default();
        journal.push_redo(test_op("a"));
        journal.push_redo(test_op("b"));

        let op = journal.pop_redo().unwrap();
        journal.push_undo_from_redo(op);

        assert!(journal.can_undo());
        assert!(journal.can_redo());
        assert_eq!(
            journal.pop_redo().unwrap().items[0].source_path,
            PathBuf::from("a")
        );
    }

    fn test_op(path: &str) -> UndoableOp {
        UndoableOp {
            kind: UndoOpKind::Move,
            items: vec![UndoItem {
                source_path: PathBuf::from(path),
                destination_path: PathBuf::from(format!("{path}.dst")),
                size: None,
                mtime: None,
                trashed_path: None,
            }],
            performed_at: SystemTime::UNIX_EPOCH,
        }
    }
}
