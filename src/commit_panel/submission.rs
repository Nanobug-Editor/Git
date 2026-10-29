//! A commit freezes local inclusion, validates disk/index identity, then submits only literal file paths.
use super::*;

impl State {
    /// Button admission depends on the local range and draft, never on checkbox-driven Git work.
    pub(crate) fn can_submit_commit(&self) -> bool {
        self.repository_available()
            && self.conflict_lifecycle.ready()
            && !self.ui_busy()
            && !self.commit_submitting
            && !self.commit_message.trim().is_empty()
            && !self.changes.iter().any(|change| change.conflict)
            && self
                .changes
                .iter()
                .any(|change| self.commit_selected.contains(&change.path))
    }

    /// Capture one accepted click. Only selected files are checked for unsaved native buffers.
    /// Later range/draft edits cannot change this command or enqueue a duplicate submission.
    pub(crate) fn submit_commit(&mut self, push: bool) -> Result<(), api::Failure> {
        if !self.can_submit_commit() {
            return Ok(());
        }
        let (paths, command) = match self.commit_submission_command(push) {
            Ok(submission) => submission,
            Err(error) => {
                self.commit_submit_error = Some(error.to_string());
                return Err(error);
            }
        };
        self.commit_submit_error = None;
        self.commit_submitting = true;
        self.commit_stage_intent = Some((paths, command));
        self.editor(api::EditorOperation::ListDocuments, "commit-stage-check")?;
        Ok(())
    }

    /// Build an exact file-range commit; Git's --only preserves staged content outside that range.
    /// Whole-file inclusion commits that file's disk contents, matching the changelist workflow.
    /// NUL pathspec stdin avoids argv limits, wildcards and quoting for large directory selections.
    pub(crate) fn commit_submission_command(
        &mut self,
        push: bool,
    ) -> Result<(Vec<String>, Command), api::Failure> {
        let selected = self
            .changes
            .iter()
            .filter(|change| self.commit_selected.contains(&change.path) && !change.conflict)
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() || self.commit_message.trim().is_empty() {
            return Err(failure(
                "请选择提交文件并填写说明 / Select files and enter a commit message",
            ));
        }
        // stdin belongs to the NUL path list. Bound the separate literal message argument below
        // Windows' command-line limit, including escaping, before admitting any Git write.
        let quoted_units = self.commit_message.encode_utf16().count()
            + self
                .commit_message
                .bytes()
                .filter(|byte| matches!(byte, b'"' | b'\\'))
                .count();
        if self.commit_message.len() > 16384
            || quoted_units > 24000
            || self.commit_message.contains('\0')
        {
            return Err(failure(
                "提交说明须不超过 16 KiB 且不含空字符 / Commit message must be at most 16 KiB without NUL",
            ));
        }
        let mut paths = std::collections::BTreeSet::new();
        for change in &selected {
            paths.insert(change.path.clone());
            if let Some(original) = &change.original {
                paths.insert(original.clone());
            }
        }
        let input = paths
            .iter()
            .flat_map(|path| path.as_bytes().iter().copied().chain([0]))
            .collect::<Vec<_>>();
        if input.len() > 64 * 1024 {
            return Err(failure(
                "所选路径过多，请按目录分批提交 / Select fewer directories per commit",
            ));
        }
        let continuation = self.commit_continuation(push);
        let mut commit = Command::new(
            [
                "commit",
                "--only",
                "--message",
                &self.commit_message,
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            continuation,
        );
        commit.input = Some(input);
        commit.display = Some(
            if push {
                "提交所选文件并推送 / Commit selected files and push"
            } else {
                "提交所选文件 / Commit selected files"
            }
            .into(),
        );

        // Unversioned files need an index entry before --only can name them. Stage just those
        // selected additions; tracked files and excluded index entries stay under Git's own transaction.
        let untracked = selected
            .iter()
            .filter(|change| change.worktree == '?')
            .flat_map(|change| change.path.as_bytes().iter().copied().chain([0]))
            .collect::<Vec<_>>();
        let next = if untracked.is_empty() {
            commit
        } else {
            let mut add = Command::new(
                ["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
                Purpose::Then(Box::new(commit)),
            );
            add.input = Some(untracked);
            add
        };
        let command = Command::new(
            [
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
            ],
            Purpose::CommitSelection {
                expected: selected,
                head: self.head.clone(),
                branch: if self.head.is_none() && self.current_ref.is_empty() {
                    String::new()
                } else {
                    self.current_ref
                        .strip_prefix("refs/heads/")
                        .unwrap_or("(detached)")
                        .into()
                },
                next: Box::new(next),
            },
        );
        Ok((paths.into_iter().collect(), command))
    }

    /// Preserve the existing clean-tree recheck/review when committing before another operation.
    /// A push is chained only after a successful local commit receipt.
    fn commit_continuation(&mut self, push: bool) -> Purpose {
        let next = self.deferred_operation.take().map(|next| {
            Box::new(if next.service.as_deref() == Some("rebase") {
                Command::new(
                    [
                        "rev-parse",
                        "--verify",
                        self.interactive_plan
                            .as_ref()
                            .map(|plan| plan.base.as_str())
                            .unwrap_or("HEAD"),
                    ],
                    Purpose::RebaseBase,
                )
            } else if matches!(next.purpose, Purpose::Guard { .. }) {
                Command::new(
                    ["rev-parse", "--verify", "HEAD"],
                    Purpose::ReviewAfterCommit(Box::new(next)),
                )
            } else {
                next
            })
        });
        if push {
            Purpose::Then(Box::new(Command::new(
                ["push"],
                Purpose::PushAfterCommit { next },
            )))
        } else {
            next.map(Purpose::CleanAfterWrite)
                .unwrap_or(Purpose::Action)
        }
    }

    /// Revalidate HEAD, branch and every selected index observation after native-buffer inspection.
    /// Changed prerequisites cancel the frozen submission; local inclusion and the draft survive retry.
    pub(crate) fn finish_commit_selection(
        &mut self,
        expected: Vec<Change>,
        head: Option<String>,
        branch: String,
        next: Command,
        output: &[u8],
    ) -> Result<(), api::Failure> {
        let current = model::status(output).map_err(failure)?;
        let header = |prefix: &[u8]| {
            output
                .split(|byte| *byte == 0)
                .find_map(|record| record.strip_prefix(prefix))
        };
        let same_head =
            header(b"# branch.oid ") == Some(head.as_deref().unwrap_or("(initial)").as_bytes());
        // for-each-ref cannot name an unborn branch. HEAD must still be initial in that case;
        // established branches and detached HEAD retain an exact branch-name guard.
        let same_branch = if branch.is_empty() && head.is_none() {
            header(b"# branch.head ").is_some()
        } else {
            header(b"# branch.head ") == Some(branch.as_bytes())
        };
        let same_selection = expected.iter().all(|before| {
            current.iter().any(|after| {
                before.path == after.path
                    && before.index_entry == after.index_entry
                    && before.intent_status == after.intent_status
                    && before.index == after.index
                    && before.worktree == after.worktree
                    && before.original == after.original
                    && !after.conflict
            })
        });
        if !same_head
            || !same_branch
            || !same_selection
            || current.iter().any(|change| change.conflict)
        {
            self.report(Severity::Warning, "提交范围或 Git 状态已变化，请检查后重试 / Commit selection or Git state changed; review and retry".into());
            self.commit_submit_error = Some(self.message.clone());
            self.commit_submitting = false;
            self.queue.clear();
            self.refresh_worktree();
            return Ok(());
        }
        self.enqueue_checked(next)
    }
}
