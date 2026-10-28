//! Bounded workspace-local snapshots make branch filtering independent of native process latency.
use super::*;
use std::collections::VecDeque;

/// Keep recent first pages and immutable commit previews within explicit guest memory budgets.
#[derive(Default)]
pub(crate) struct Cache {
    /// Only the latest filter/refresh incarnation may admit a log completion.
    pub(crate) generation: u64,
    /// A cold filter still needs its authoritative first page, including after a preempting action.
    pub(crate) pending: bool,
    /// LRU first pages keyed by the exact full ref; None denotes the all-ref filter.
    logs: VecDeque<(Option<String>, Vec<Commit>)>,
    /// Complete immutable previews share no mutable editor text or undo history.
    previews: VecDeque<Preview>,
    /// Only an authoritative page may advance pagination; retained/projected rows cannot supply skip.
    accepted_filter: Option<Option<String>>,
}

/// Files and the complete message are cached together only after native tree admission succeeds.
struct Preview {
    /// The OID identifies preview content; display metadata comes from its admitted log record.
    commit: Commit,
    /// All changed paths and tones from the same complete diff-tree receipt.
    files: BTreeMap<String, model::HistoryChange>,
    /// Complete validated UTF-8 message, never a truncated log subject.
    message: String,
}

impl Cache {
    /// Retire previous log callbacks without discarding accepted snapshots of other filters.
    pub(crate) fn advance(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("history generation exhausted");
    }

    /// Ref movement changes decorations and reachability; immutable OID previews remain valid.
    pub(crate) fn invalidate_logs(&mut self) {
        self.logs.clear();
        self.accepted_filter = None;
    }

    /// Admit at most twelve first pages and four MiB of commit metadata, never the whole graph.
    pub(in crate::history) fn remember(&mut self, filter: Option<String>, commits: &[Commit]) {
        self.accepted_filter = Some(filter.clone());
        self.logs.retain(|(key, _)| key != &filter);
        self.logs
            .push_back((filter, commits.iter().take(128).cloned().collect()));
        while self.logs.len() > 12
            || self
                .logs
                .iter()
                .map(|(_, rows)| log_bytes(rows))
                .sum::<usize>()
                > 4 * 1024 * 1024
        {
            self.logs.pop_front();
        }
    }

    /// Report whether the displayed range was admitted for this filter, rather than held provisionally.
    pub(crate) fn owns_page(&self, filter: &Option<String>) -> bool {
        self.accepted_filter.as_ref() == Some(filter)
    }

    /// Touch a matching admitted page; callers reset its viewport and select the first commit.
    fn get(&mut self, filter: &Option<String>) -> Option<Vec<Commit>> {
        let index = self.logs.iter().position(|(key, _)| key == filter)?;
        let entry = self.logs.remove(index)?;
        let rows = entry.1.clone();
        self.logs.push_back(entry);
        Some(rows)
    }

    /// Project known ancestry only when its exact current ref tip exists in the admitted graph.
    /// Missing ancestors are deliberately not treated as an exhausted branch; Git still fills it.
    fn project(&self, tip: &str, current: &[Commit]) -> Option<Vec<Commit>> {
        let rows = self
            .logs
            .iter()
            .find(|(key, _)| key.is_none())
            .map_or(current, |(_, rows)| rows);
        let lookup: BTreeMap<_, _> = rows
            .iter()
            .map(|commit| (commit.oid.as_str(), commit))
            .collect();
        if !lookup.contains_key(tip) {
            return None;
        }
        let mut pending = vec![tip];
        let mut reachable = std::collections::BTreeSet::new();
        while let Some(oid) = pending.pop() {
            if reachable.insert(oid)
                && let Some(commit) = lookup.get(oid)
            {
                pending.extend(commit.parents.iter().map(String::as_str));
            }
        }
        Some(
            rows.iter()
                .filter(|commit| reachable.contains(commit.oid.as_str()))
                .take(128)
                .cloned()
                .collect(),
        )
    }
}

/// Count owned UTF-8 metadata and parent identities so hostile long subjects cannot grow the cache.
fn log_bytes(commits: &[Commit]) -> usize {
    commits
        .iter()
        .map(|c| {
            std::mem::size_of::<Commit>()
                + c.oid.len()
                + c.title.len()
                + c.labels.len()
                + c.author.len()
                + c.time.len()
                + c.parents
                    .iter()
                    .map(|p| p.len() + std::mem::size_of::<String>())
                    .sum::<usize>()
        })
        .sum()
}

impl State {
    /// Serial reads may be preempted; writes, discovery and editor acknowledgements keep busy guards.
    pub(crate) fn history_reads_only(&self) -> bool {
        let readable = |purpose: &Purpose| {
            matches!(
                purpose,
                Purpose::Log { .. } | Purpose::Files(_) | Purpose::CommitMessage(_)
            )
        };
        (!self.queue.is_empty() || self.running.is_some())
            && self
                .running
                .as_ref()
                .is_none_or(|running| readable(&running.command.purpose))
            && self.queue.iter().all(|command| readable(&command.purpose))
            && self
                .editor_tasks
                .values()
                .all(|intent| matches!(intent.as_str(), "visibility" | "write-check-cancelled"))
            && !self.merge_files.values().any(|file| file.read_inflight)
    }

    /// A newer action closes only an owned readonly child; late receipts are ignored and an
    /// unfinished cold filter remains pending until the action yields to its replacement read.
    pub(crate) fn cancel_history_reads(&mut self) -> Result<(), api::Failure> {
        if !self.history_reads_only() {
            return Ok(());
        }
        if let Some(running) = self.running.take() {
            api::guest::close_resource(running.handle)?;
        }
        self.queue.clear();
        self.history_refresh = None;
        self.history_cache.advance();
        self.history_loading = false;
        self.pending_history_files = None;
        Ok(())
    }

    /// Finish a cold filter after a newer preview/editor action yields; cancelled reads never leave
    /// another branch's retained rows permanently presented as the requested filter's own page.
    pub(crate) fn resume_pending_history_filter(&mut self) {
        if self.history_cache.pending && !self.busy() {
            self.offset = 0;
            self.queue_log();
        }
    }

    /// Restore accepted first pages synchronously, otherwise retain/project rows and query one page.
    /// The requested filter owns callback authority; no cached row authorizes a repository write.
    pub(crate) fn select_history_filter(
        &mut self,
        filter: Option<String>,
    ) -> Result<(), api::Failure> {
        self.cancel_history_reads()?;
        self.history_cache.advance();
        self.history_extend_pending = false;
        self.history_exhausted = false;
        self.filter = filter;
        self.history_error.clear();
        self.offset = 0;
        self.log_scroll = 0.;
        self.selected_commits.clear();
        self.history_cursor.clear();
        self.history_anchor = None;
        if let Some(commits) = self.history_cache.get(&self.filter) {
            self.history_page(commits);
            return Ok(());
        }
        let projected = self
            .filter
            .as_ref()
            .and_then(|key| self.ref_tips.get(key))
            .and_then(|tip| self.history_cache.project(tip, &self.commits));
        self.queue_log();
        self.history_cache.pending = true;
        if let Some(commits) = projected {
            self.replace_history_window(commits);
            self.select_history_row(0, false);
        }
        Ok(())
    }

    /// Save a complete preview only after both Git receipts and its atomic tree delta were accepted.
    pub(crate) fn cache_history_preview(&mut self) {
        let Some(commit) = self.history_preview_commit.clone() else {
            return;
        };
        self.history_cache
            .previews
            .retain(|preview| preview.commit.oid != commit.oid);
        self.history_cache.previews.push_back(Preview {
            commit,
            files: self.history_file_status.clone(),
            message: self.history_message.clone(),
        });
        while self.history_cache.previews.len() > 12
            || self
                .history_cache
                .previews
                .iter()
                .map(|preview| {
                    preview.message.len()
                        + log_bytes(std::slice::from_ref(&preview.commit))
                        + preview
                            .files
                            .keys()
                            .map(|path| path.len() + 64)
                            .sum::<usize>()
                })
                .sum::<usize>()
                > 2 * 1024 * 1024
        {
            self.history_cache.previews.pop_front();
        }
    }

    /// Restore files/message together; a rejected delta keeps the old preview and falls back to Git.
    pub(in crate::history) fn restore_history_preview(&mut self) -> bool {
        let Some(index) = self
            .history_cache
            .previews
            .iter()
            .position(|preview| preview.commit.oid == self.selected)
        else {
            return false;
        };
        let preview = self.history_cache.previews.remove(index).unwrap();
        let old_status = std::mem::replace(&mut self.history_file_status, preview.files.clone());
        let old_files = std::mem::replace(
            &mut self.history_files,
            preview.files.keys().cloned().collect(),
        );
        if self.reconcile_tree("files").is_err() {
            self.history_file_status = old_status;
            self.history_files = old_files;
            return false;
        }
        self.history_message = preview.message.clone();
        self.history_preview_commit = Some(preview.commit.clone());
        self.history_loading = false;
        self.history_cache.previews.push_back(preview);
        true
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// LRU count, row and byte limits apply even to maliciously large valid metadata fields.
    #[test]
    fn branch_cache_is_bounded_and_evicts_the_oldest_pages() {
        let rows = vec![
            Commit {
                oid: "oid".into(),
                ..Default::default()
            };
            385
        ];
        let mut cache = Cache::default();
        for index in 0..13 {
            cache.remember(Some(format!("branch-{index}")), &rows);
        }
        assert_eq!(cache.logs.len(), 12);
        assert!(cache.get(&Some("branch-0".into())).is_none());
        assert_eq!(cache.get(&Some("branch-1".into())).unwrap().len(), 128);
        cache.remember(
            Some("oversized".into()),
            &[Commit {
                title: "x".repeat(4 * 1024 * 1024),
                ..Default::default()
            }],
        );
        assert!(cache.logs.is_empty());
    }

    /// Cache hits must distinguish an admitted empty repository from an uncached filter.
    #[test]
    fn branch_cache_preserves_empty_pages_and_invalidates_current_pagination() {
        let mut cache = Cache::default();
        cache.remember(None, &[]);
        assert_eq!(cache.get(&None), Some(Vec::new()));
        assert!(cache.owns_page(&None));
        cache.invalidate_logs();
        assert!(cache.get(&None).is_none());
        assert!(!cache.owns_page(&None));
    }
}
