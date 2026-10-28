//! Bounded immutable Git sources avoid process launches on historical back/forward navigation.
use super::*;

/// Only validated complete text, including a confirmed absent side, enters this per-instance cache.
#[derive(Default)]
pub(crate) struct HistorySources {
    /// Most recently accessed entries are last; both entry count and total retained bytes are bounded.
    entries: VecDeque<Entry>,
    /// Includes repository/OID/path keys so very long filenames cannot evade the byte budget.
    bytes: usize,
}

/// An immutable tree and exact file path identify one text side within one repository.
struct Entry {
    /// Logical workspace identity supplied by the owning host instance.
    workspace: String,
    /// Frozen commit/tree object, never a moving branch expression.
    oid: String,
    /// Exact Git-relative selected file path.
    path: String,
    /// `None` records confirmed absence; `Some("")` is a real, valid empty blob.
    text: Option<String>,
}

impl Entry {
    /// Account for retained key and text allocations, excluding fixed per-entry bookkeeping.
    fn bytes(&self) -> usize {
        self.workspace.len()
            + self.oid.len()
            + self.path.len()
            + self.text.as_ref().map_or(0, String::len)
    }

    /// Paths remain literal, and equal object spellings in different repositories stay separate.
    fn matches(&self, workspace: &str, oid: &str, path: &str) -> bool {
        self.workspace == workspace && self.oid == oid && self.path == path
    }
}

impl HistorySources {
    /// Outer `None` is a miss; a hit retains whether Git observed absence or a complete text blob.
    pub(super) fn get(&mut self, workspace: &str, oid: &str, path: &str) -> Option<Option<String>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.matches(workspace, oid, path))?;
        let entry = self.entries.remove(index)?;
        let text = entry.text.clone();
        self.entries.push_back(entry);
        Some(text)
    }

    /// Publish only a successful complete Git read. Worktree and index reads never call this method.
    pub(super) fn remember(&mut self, workspace: &str, oid: &str, path: &str, text: Option<&str>) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.matches(workspace, oid, path))
        {
            self.bytes -= self.entries.remove(index).unwrap().bytes();
        }
        let entry = Entry {
            workspace: workspace.into(),
            oid: oid.into(),
            path: path.into(),
            text: text.map(str::to_owned),
        };
        self.bytes += entry.bytes();
        self.entries.push_back(entry);
        while self.entries.len() > 64 || self.bytes > 8 * 1024 * 1024 {
            self.bytes -= self.entries.pop_front().unwrap().bytes();
        }
    }
}

impl State {
    /// Completed legacy/history blobs share immutable keys, independently of the moving log selection.
    pub(crate) fn remember_history_source(&mut self, column: usize, text: Option<&str>) {
        if let Some(oid) = self
            .snapshot_targets
            .get(column)
            .filter(|oid| !oid.is_empty())
        {
            let path = if self.diff_history {
                &self.diff_path
            } else {
                &self.history_path
            };
            if !path.is_empty() {
                self.history_sources
                    .remember(&self.environment.workspace, oid, path, text);
            }
        }
    }
}
