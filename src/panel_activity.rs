//! Surface-local browsing shares a serialized Git writer without cancelling sibling queries.
use super::*;

/// Diagnostics and cancellation follow the issuing surface, not the most recently focused window.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum FeedbackOwner {
    Staging,
    History,
    Shared,
}

/// A file click retains its source identity while another native comparison finishes.
pub(super) enum ComparisonSelection {
    /// Worktree/index selection is revalidated against the latest observed file status.
    Worktree { path: String, staged: bool },
    /// Immutable history selection is discarded only when its own selected commit changed.
    History { oid: String, path: String },
    /// Adjacent arrows retain their original comparison sequence even if the selected log changes.
    Adjacent {
        origin: String,
        path: String,
        context: comparisons::HistoryNavigation,
    },
}

impl ComparisonSelection {
    /// Both history entry points share a latest pending choice, independent of the composer.
    fn history(&self) -> bool {
        !matches!(self, Self::Worktree { .. })
    }
}

impl State {
    /// Immutable comparison sources are captured separately; only the shared conflict editor blocks browsing.
    pub(super) fn history_navigation_busy(&self) -> bool {
        self.editor_tasks
            .values()
            .any(|intent| intent.starts_with("merge-"))
            || self.merge_files.values().any(|file| file.read_inflight)
    }

    /// Successful adjacent/source receipts own editor tabs, not the history dock's admitted scene.
    pub(super) fn history_comparison_event(&self, input: &api::Input) -> bool {
        match input {
            api::Input::Event {
                event: api::Notification::CommandInvocation(call),
                ..
            } => call.id == "history-adjacent",
            api::Input::Event {
                event: api::Notification::Process { handle, .. },
                ..
            } if self.diff_history => self.running.as_ref().is_some_and(|running| {
                &running.handle == handle
                    && matches!(
                        running.command.purpose,
                        Purpose::HistoryBlob { .. }
                            | Purpose::HistoryBatch { .. }
                            | Purpose::Snapshot { .. }
                    )
            }),
            api::Input::Event {
                event: api::Notification::Request { handle, .. },
                ..
            } if self.diff_history => self
                .editor_tasks
                .get(&handle.resource)
                .is_some_and(|intent| comparisons::is_diff_intent(intent)),
            _ => false,
        }
    }
    /// Resolve ownership before consuming an input or taking its correlated process/request.
    pub(super) fn feedback_owner(&self, input: &api::Input) -> FeedbackOwner {
        use FeedbackOwner::*;
        match input {
            api::Input::Event {
                panel: Some(panel), ..
            } if panel == "staging" => Staging,
            api::Input::Event {
                panel: Some(panel), ..
            } if matches!(panel.as_str(), "history" | "commit-details") => History,
            api::Input::Event {
                event: api::Notification::Process { .. },
                ..
            } => self
                .running
                .as_ref()
                .map_or(Shared, |running| running.command.feedback_owner()),
            api::Input::Event {
                event: api::Notification::CommandInvocation(_),
                ..
            } => History,
            api::Input::Event {
                event: api::Notification::Request { handle, .. },
                ..
            } => {
                let intent = self
                    .editor_tasks
                    .get(&handle.resource)
                    .map(String::as_str)
                    .unwrap_or("");
                if matches!(intent, "commit-stage-check" | "stage-check") {
                    Staging
                } else if intent == "write-check" {
                    self.write_intent
                        .as_ref()
                        .map_or(Staging, Command::feedback_owner)
                } else if self.diff_history && comparisons::is_diff_intent(intent) {
                    History
                } else {
                    Staging
                }
            }
            _ => Shared,
        }
    }

    /// Retain operation-local diagnostic state; visual messages are published by the host owner.
    pub(super) fn retain_panel_feedback(&mut self, owner: FeedbackOwner, previous: &str) {
        if self.message == previous {
            return;
        }
        match owner {
            FeedbackOwner::Staging => {}
            FeedbackOwner::History => self.history_notice = self.message.clone(),
            FeedbackOwner::Shared => {
                self.history_notice = self.message.clone();
            }
        }
    }

    /// Retire the failing chain only; another panel's immutable reads and draft keep their ownership.
    pub(super) fn fail_panel_activity(&mut self, owner: FeedbackOwner, error: &api::Failure) {
        if owner != FeedbackOwner::History {
            // Submission failure releases its frozen command; local choices remain available for retry.
            if self.commit_submitting {
                self.commit_submit_error = Some(error.to_string());
            }
            self.commit_submitting = false;
            self.commit_stage_intent = None;
            let _ = self.reconcile_tree("staging");
        }
        if owner == FeedbackOwner::Shared || self.diff_history == (owner == FeedbackOwner::History)
        {
            self.abort_diff(Some(error));
        }
        if owner != FeedbackOwner::Staging {
            self.fail_history_preview();
            self.history_refresh = None;
            self.history_cache.pending = false;
        }
        self.queue
            .retain(|command| owner != FeedbackOwner::Shared && command.feedback_owner() != owner);
    }
    /// Keep the most recent pending file click per surface; never abort the sibling's comparison.
    pub(super) fn request_comparison(
        &mut self,
        selection: ComparisonSelection,
    ) -> Result<(), api::Failure> {
        let history = selection.history();
        // Repeated input keeps the exact running source requests. It still supersedes an older
        // queued choice on this surface, while another commit/parent/index side remains distinct.
        let key = match &selection {
            ComparisonSelection::History { oid, path } if *oid == self.selected => Some(
                comparisons::history_key(&self.history_snapshot_targets(), path),
            ),
            ComparisonSelection::Worktree { path, staged } => Some(format!(
                "{}-{}",
                if *staged { "index" } else { "worktree" },
                trees::node_id(path)
            )),
            ComparisonSelection::Adjacent { path, context, .. } => {
                Some(context.comparison_key(path))
            }
            _ => None,
        };
        if self.diff_flight.is_some() && key.as_ref() == Some(&self.diff_key) {
            self.pending_comparisons
                .retain(|old| old.history() != history);
            return Ok(());
        }
        if history && self.history_comparison_only() {
            // Tree clicks and toolbar arrows share latest-selection semantics. Cancel only this
            // readonly flight, preserving unrelated writes and the sibling commit surface.
            if self.running.as_ref().is_some_and(|running| {
                matches!(
                    running.command.purpose,
                    Purpose::HistoryBlob { .. }
                        | Purpose::HistoryBatch { .. }
                        | Purpose::Snapshot { .. }
                )
            }) {
                self.cancel_history_source(api::guest::close_resource)?;
            }
            self.abort_diff(None);
        }
        self.pending_comparisons
            .retain(|old| old.history() != history);
        self.pending_comparisons.push_back(selection);
        self.resume_comparison_selection()
    }

    /// Start a captured selection only after the existing Git/native request chain releases ownership.
    pub(super) fn resume_comparison_selection(&mut self) -> Result<(), api::Failure> {
        if self.busy() {
            return Ok(());
        }
        while let Some(selection) = self.pending_comparisons.pop_front() {
            match selection {
                ComparisonSelection::Worktree { path, staged }
                    if self.changes.iter().any(|change| change.path == path) =>
                {
                    return self.open_diff(&path, staged);
                }
                ComparisonSelection::History { oid, path }
                    if self.selected == oid && self.history_files.contains(&path) =>
                {
                    return self.open_history_diff(&path);
                }
                ComparisonSelection::Adjacent {
                    origin,
                    path,
                    context,
                } if self.diff_views.contains_key(&origin) => {
                    return self.open_adjacent_history_file(&path, context);
                }
                _ => {} // Only a newer selection or changed repository data can invalidate this click.
            }
        }
        Ok(())
    }

    /// Immutable comparison reads do not own staging controls. Their native sources remain serialized.
    pub(super) fn history_comparison_only(&self) -> bool {
        self.diff_history
            && self.diff_flight.is_some()
            && self
                .running
                .iter()
                .map(|running| &running.command)
                .chain(self.queue.iter())
                .all(|command| {
                    matches!(
                        command.purpose,
                        Purpose::HistoryBlob { .. }
                            | Purpose::HistoryBatch { .. }
                            | Purpose::Snapshot { .. }
                            | Purpose::Log { .. }
                            | Purpose::Files(_)
                            | Purpose::CommitMessage(_)
                    )
                })
            && self
                .editor_tasks
                .values()
                .all(|intent| comparisons::is_diff_intent(intent) || intent == "visibility")
    }

    /// Focus/visibility/save are passive local observations, never a full-history reload or UI lock.
    pub(super) fn request_observation(&mut self) {
        if self.busy() {
            self.passive_refresh_pending = true;
        } else {
            self.passive_refresh_pending = false;
            self.automatic_observation = true;
            self.background_read = true;
            self.refresh();
        }
    }

    /// Index/worktree writes reobserve only changed files and operation markers, preserving history.
    /// This can follow existing immutable reads in the queue without cancelling those reads.
    pub(super) fn refresh_worktree(&mut self) {
        if self
            .running
            .iter()
            .map(|running| &running.command)
            .chain(self.queue.iter())
            .any(|command| matches!(command.purpose, Purpose::Status { .. }))
        {
            return;
        }
        let round = self.conflict_lifecycle.begin();
        for file in self.merge_files.values_mut() {
            file.proposals_suspended = true;
        }
        self.queue.push_back(Command::new(
            ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
            Purpose::Status { round },
        ));
        self.queue_operation_probes(round, self.environment.os == "windows");
    }
}

impl Command {
    /// Guarded commands inherit their eventual operation's diagnostic owner.
    pub(super) fn feedback_owner(&self) -> FeedbackOwner {
        use FeedbackOwner::*;
        match &self.purpose {
            Purpose::Guard { next, .. } | Purpose::CommitSelection { next, .. } => {
                next.feedback_owner()
            }
            Purpose::Action | Purpose::Then(_)
                if self.changes_index_only()
                    || self
                        .args
                        .first()
                        .is_some_and(|arg| arg == "commit" || arg == "restore") =>
            {
                Staging
            }
            Purpose::Patch { .. } | Purpose::DiffSnapshot { .. } => Staging,
            Purpose::PushAfterCommit { .. } => Staging,
            Purpose::Status { .. }
            | Purpose::OperationProbe { .. }
            | Purpose::OperationBatch { .. }
            | Purpose::Repository(_) => Shared,
            _ => History,
        }
    }
    /// These commands cannot move refs or HEAD; refreshing the log after them only reloads a sibling.
    pub(super) fn changes_index_only(&self) -> bool {
        let args = self.effective_args();
        match args.first().map(String::as_str) {
            Some("add") => true,
            Some("restore") => {
                args.iter().any(|arg| arg == "--staged")
                    && !args.iter().any(|arg| arg == "--worktree")
            }
            Some("rm" | "apply") => args.iter().any(|arg| arg == "--cached"),
            _ => false,
        }
    }
}
