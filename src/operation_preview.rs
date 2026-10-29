//! Read-only Git queries freeze ordinary history operations before the user can confirm them.
//! Every result carries its preparation generation; mutable UI selection is never a query input.
use super::*;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod git_tests;
mod model;
mod reset_impact;
#[cfg(test)]
mod tests;
use model::{Draft, Kind};

/// Only the current preparation may publish a confirmation. Cancelling revokes queued replies.
#[derive(Default)]
pub(crate) struct PreviewState {
    generation: u64,
    draft: Option<Draft>,
}

/// Correlation belongs to one read-only query, including the final repository identity recheck.
#[derive(Clone)]
pub(crate) struct Query {
    generation: u64,
    step: Step,
}

#[derive(Clone)]
enum Step {
    Worktree { choose_dirty: bool },
    Branch,
    Head,
    Target(usize),
    Commits,
    RemoteRefs(usize),
    ResetTree,
    ResetUntracked,
    CheckBranch,
    CheckHead,
}

impl State {
    /// Native saving revokes the old approval. Ordinary operations can rebuild their complete
    /// impact at the same immutable target; other workflows must be selected again after refresh.
    pub(super) fn reconsider_blocked_write(
        &mut self,
        command: Command,
    ) -> Result<(), api::Failure> {
        self.deferred_operation = None;
        let mut effect = &command;
        while let Purpose::Guard { next, .. } = &effect.purpose {
            effect = next;
        }
        if Kind::recognize(effect)?.is_some() {
            let choose_dirty = matches!(
                effect.args.first().map(String::as_str),
                Some("merge" | "rebase" | "cherry-pick")
            );
            return self.reconsider_operation_from(command, Step::Worktree { choose_dirty });
        }

        // A generic argv echo is not a fresh preview for stash, switch, discard or a helper's
        // persisted plan. Withdraw their old commands instead of falling through to execution.
        self.cancel_pending_write()?;
        self.refresh();
        self.report(
            Severity::Warning,
            self.label(
                "内容已变化，请重新选择操作并审查",
                "Content changed. Select the operation again and review it",
            )
            .into(),
        );
        Ok(())
    }

    /// Commit/stash and native-buffer saving invalidate HEAD-bound or worktree impact reviews.
    /// Keep the immutable target but read and display a new range/list before renewed approval.
    pub(super) fn reconsider_operation(&mut self, command: Command) -> Result<(), api::Failure> {
        self.reconsider_operation_from(command, Step::Branch)
    }

    /// Only native-buffer recovery needs another status read; explicit commit/stash recovery
    /// already reached this path through a complete clean-worktree observation.
    fn reconsider_operation_from(
        &mut self,
        mut command: Command,
        first: Step,
    ) -> Result<(), api::Failure> {
        while let Purpose::Guard { next, .. } = command.purpose {
            command = *next;
        }
        if command.args.first().is_some_and(|arg| arg == "rebase") && command.service.is_none() {
            command.args = vec![
                "rebase".into(),
                command
                    .args
                    .last()
                    .cloned()
                    .ok_or_else(|| failure("Missing rebase target"))?,
            ];
        }
        command.display = None;
        if !self.prepare_operation_preview_from(command.clone(), "history".into(), false, first)? {
            self.enqueue_checked(command)?;
        }
        Ok(())
    }
    /// Recognize only complete ordinary operations. Callers retain their command on false by
    /// passing a clone; interactive services and continue/abort/skip keep their existing policy.
    pub(super) fn prepare_operation_preview(
        &mut self,
        command: Command,
        panel: String,
        dirty: bool,
    ) -> Result<bool, api::Failure> {
        self.prepare_operation_preview_from(command, panel, dirty, Step::Branch)
    }

    /// The entry step owns the entire preparation generation, including optional status recovery.
    fn prepare_operation_preview_from(
        &mut self,
        command: Command,
        panel: String,
        dirty: bool,
        first: Step,
    ) -> Result<bool, api::Failure> {
        let Some(kind) = Kind::recognize(&command)? else {
            return Ok(false);
        };
        if self.operation_preview.draft.is_some() {
            return Err(failure(
                "正在准备操作预览 / An operation preview is already being prepared",
            ));
        }
        let draft = Draft::new(command, kind, panel, dirty)?;
        self.cancel_operation_preview();
        self.pending = None;
        self.pending_dirty = false;
        self.pending_recheck = false;
        self.operation_preview.draft = Some(draft);
        self.report(
            Severity::Information,
            self.label(
                "正在冻结目标与读取改写范围…",
                "Resolving exact targets and reading affected history…",
            )
            .into(),
        );
        self.queue_operation_preview(first);
        Ok(true)
    }

    /// Revocation changes no Git state. The owner also dismisses its pending confirmation UI.
    pub(super) fn cancel_operation_preview(&mut self) {
        self.operation_preview.generation = self.operation_preview.generation.wrapping_add(1);
        self.operation_preview.draft = None;
    }

    /// Handle this purpose before the general exit-code branch so every failure retires the draft.
    /// Stderr is returned as diagnostic data; a failed query never implies a repository rollback.
    pub(super) fn operation_preview_finished(
        &mut self,
        query: Query,
        stdout: &[u8],
        stderr: &[u8],
        code: u32,
    ) -> Result<(), api::Failure> {
        if query.generation != self.operation_preview.generation
            || self.operation_preview.draft.is_none()
        {
            return Ok(());
        }
        let result = self.advance_operation_preview(query.step, stdout, stderr, code);
        if result.is_err() {
            self.cancel_operation_preview();
            self.pending = None;
            self.pending_dirty = false;
        }
        result
    }

    /// The next command is derived exclusively from the retained draft, never current selection.
    fn queue_operation_preview(&mut self, step: Step) {
        let draft = self
            .operation_preview
            .draft
            .as_ref()
            .expect("owned preview draft");
        let args = match &step {
            Step::Worktree { .. } => vec![
                "status".into(),
                "--porcelain=v2".into(),
                "-z".into(),
                "--untracked-files=all".into(),
            ],
            Step::Branch | Step::CheckBranch => vec![
                "rev-parse".into(),
                "--symbolic-full-name".into(),
                "HEAD".into(),
            ],
            Step::Head | Step::CheckHead => {
                vec!["rev-parse".into(), "--verify".into(), "HEAD".into()]
            }
            Step::Target(index) => vec![
                "rev-parse".into(),
                "--verify".into(),
                "--end-of-options".into(),
                format!("{}^{{commit}}", draft.targets[*index]),
            ],
            Step::Commits => draft.log_args(),
            Step::ResetTree => vec![
                "ls-tree".into(),
                "-r".into(),
                "-z".into(),
                "--full-tree".into(),
                draft.resolved[0].clone(),
            ],
            // Deliberately omit exclude-standard: ignored files can also be destroyed by hard reset.
            Step::ResetUntracked => reset_impact::untracked_args(),
            Step::RemoteRefs(index) => vec![
                "for-each-ref".into(),
                "--count=129".into(),
                format!("--contains={}", draft.commits[*index].oid),
                "--format=%(refname)%09%(objectname)".into(),
                "refs/remotes/".into(),
            ],
        };
        self.queue.push_back(Command::new(
            args,
            Purpose::OperationPreview(Query {
                generation: self.operation_preview.generation,
                step,
            }),
        ));
    }

    /// All preparatory phases are read-only, bounded, and keep symbolic HEAD plus its OID together.
    fn advance_operation_preview(
        &mut self,
        step: Step,
        stdout: &[u8],
        stderr: &[u8],
        code: u32,
    ) -> Result<(), api::Failure> {
        if code != 0 {
            return Err(failure(format!(
                "Git 预览查询失败，仓库状态未被回滚 / Git preview query failed (exit {code}); repository state was not rolled back:\n{}",
                safe_diagnostic(stderr)
            )));
        }
        if stdout.len() > 262144 || stderr.len() > 262144 {
            return Err(failure(
                "预览查询超过 256 KiB，无法完整确认 / Preview exceeds 256 KiB; refusing an incomplete review",
            ));
        }
        let draft = self.operation_preview.draft.as_mut().expect("live draft");
        let next = match step {
            Step::Worktree { choose_dirty } => {
                self.changes = crate::model::status(stdout).map_err(failure)?;
                draft.dirty = choose_dirty && !self.changes.is_empty();
                Step::Branch
            }
            Step::Branch => {
                draft.branch = model::branch(stdout)?;
                draft.branch_bytes = stdout.to_vec();
                Step::Head
            }
            Step::Head => {
                draft.head = model::oid(stdout)?;
                draft.head_bytes = stdout.to_vec();
                Step::Target(0)
            }
            Step::Target(index) => {
                draft.resolved.push(model::oid(stdout)?);
                if index + 1 < draft.targets.len() {
                    Step::Target(index + 1)
                } else {
                    draft.validate_targets()?;
                    Step::Commits
                }
            }
            Step::Commits => {
                draft.read_commits(stdout)?;
                if draft.commits.is_empty() {
                    draft.after_history()
                } else {
                    Step::RemoteRefs(0)
                }
            }
            Step::RemoteRefs(index) => {
                draft.commits[index].remotes = model::remote_refs(stdout)?;
                if index + 1 < draft.commits.len() {
                    Step::RemoteRefs(index + 1)
                } else {
                    draft.after_history()
                }
            }
            Step::ResetTree => {
                draft
                    .hard_reset
                    .as_mut()
                    .expect("hard reset preview")
                    .read_tree(stdout)?;
                Step::ResetUntracked
            }
            Step::ResetUntracked => {
                draft
                    .hard_reset
                    .as_mut()
                    .expect("hard reset preview")
                    .read_untracked(stdout)?;
                Step::CheckBranch
            }
            Step::CheckBranch => {
                if stdout != draft.branch_bytes {
                    return Err(model::changed());
                }
                Step::CheckHead
            }
            Step::CheckHead => {
                if stdout != draft.head_bytes {
                    return Err(model::changed());
                }
                let draft = self
                    .operation_preview
                    .draft
                    .take()
                    .expect("completed draft");
                let command = draft.frozen_command()?;
                self.set_pending_review(command, draft.panel, draft.dirty);
                self.report(
                    Severity::Information,
                    self.label(
                        "目标与影响已冻结，请检查后确认",
                        "Exact targets and effects are ready for review",
                    )
                    .into(),
                );
                return Ok(());
            }
        };
        self.queue_operation_preview(next);
        Ok(())
    }
}
