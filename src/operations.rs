//! User actions build literal, bounded commands. Destructive operations first publish an explicit preview.
use super::*;
mod commands;

impl State {
    pub(super) fn ui_event(
        &mut self,
        panel: Option<&str>,
        event: ui::UiEvent,
    ) -> Result<(), api::Failure> {
        // Local composer controls have no ownership of another panel's readonly work.
        if (panel == Some("staging") || (panel.is_none() && event.node.starts_with("commit-")))
            && self.commit_panel_event(&event)?
        {
            return Ok(());
        }
        if panel != Some("staging")
            && matches!(event.action, ui::Action::Click | ui::Action::Select(_))
        {
            self.cancel_history_reads()?;
        }
        if self.branch_prompt_event(&event)? {
            return Ok(());
        }
        if self.branch_comparison_event(&event)? {
            return Ok(());
        }
        if self.edit_rebase(&event)? {
            return Ok(());
        }
        if matches!(event.action, ui::Action::Dismiss) {
            // A repository subdialog may consume Dismiss below. Revoke an accepted native
            // enumeration first so closing any confirmation cannot leave its old write live.
            self.cancel_pending_write()?;
        }
        if event.node == "cancel-process" && matches!(event.action, ui::Action::Click) {
            self.cancel_pending_write()?;
            self.queue.clear();
            self.history_refresh = None;
            if let Some(running) = self.running.take() {
                api::guest::close_resource(running.handle)?;
            }
            self.report(
                Severity::Information,
                self.label(
                    "已停止进程；正在重新检查实际 Git 状态",
                    "Process stopped; rechecking actual Git state",
                )
                .into(),
            );
            self.refresh();
            return Ok(());
        }
        if self.repository_action(&event)? {
            return Ok(());
        }
        if let ui::Action::Select(value) = &event.action
            && let Some(side) = event
                .node
                .strip_prefix("history-parent-")
                .and_then(|side| side.parse::<usize>().ok())
        {
            if self.busy() || side > 1 {
                return Ok(());
            }
            let parent = value
                .parse::<usize>()
                .map_err(|_| failure("Invalid parent selection"))?;
            let count = self
                .history_commit()
                .map_or(0, |commit| commit.parents.len());
            if parent >= count || parent == self.history_parents[1 - side] {
                return Err(failure("请选择不同的父提交 / Select distinct parents"));
            }
            self.history_parents[side] = parent;
            if !self.history_path.is_empty() {
                let path = self.history_path.clone();
                if panel == Some("commit-details") {
                    self.history_file(&path);
                } else {
                    self.open_history_diff(&path)?;
                }
            }
            return Ok(());
        }
        match event.action {
            ui::Action::Change(text) => {
                match event.node.as_str() {
                    "commit-message" => self.commit_message = text,
                    "argument" => self.argument = text,
                    _ => {}
                }
                return Ok(());
            }
            ui::Action::Dismiss => {
                if panel == Some("conflicts")
                    && let Some(file) = self.merge_files.get_mut(&self.merge_selected)
                {
                    file.show_base = false;
                }
                return Ok(());
            }
            ui::Action::Click => {}
            _ => return Ok(()),
        }
        // An explicit refresh arriving during business is retained for its idle boundary.
        if event.node == "refresh" {
            if panel == Some("staging") {
                self.refresh_worktree();
            } else {
                self.request_refresh();
            }
            return Ok(());
        }
        if event.node == "cancel-command" {
            self.cancel_pending_write()?;
            return Ok(());
        }
        if event.node == "confirm-command" {
            if self.busy() {
                return Ok(());
            }
            if self.pending_dirty {
                return Err(failure("Choose how to handle the listed changes first"));
            }
            if let Some(command) = self.pending.take() {
                if std::mem::take(&mut self.pending_recheck) {
                    // A native save can change hard-reset obstructions and other disk effects.
                    // Rebuild the review instead of treating this click as a renewed approval.
                    self.reconsider_blocked_write(command)?;
                } else {
                    self.enqueue_checked(command)?;
                }
            }
            return Ok(());
        }
        // Read-only comparisons capture the requested file/mode and wait like tree selections.
        // Native source-release receipts may still be pending when an external command arrives;
        // dropping that command would leave the previous comparison visible as if it were current.
        if matches!(event.action, ui::Action::Click)
            && matches!(event.node.as_str(), "diff" | "diff-index")
        {
            if !self.repository_available() {
                return Err(failure("请打开仓库根目录 / Open a repository root"));
            }
            self.command_for_action(&event.node, panel)?;
            return Ok(());
        }
        if matches!(event.node.as_str(), "commit" | "commit-push") {
            return self.submit_commit(event.node == "commit-push");
        }
        if self.ui_busy() {
            return Ok(());
        }
        if !self.repository_available() && event.node != "refresh" {
            return Err(failure("请打开仓库根目录 / Open a repository root"));
        }
        if panel == Some("conflicts") && self.conflict_action(&event.node)? {
            return Ok(());
        }
        if event.node == "dirty-stash" {
            if let Some(next) = self.pending.take() {
                self.pending_dirty = false;
                self.enqueue_checked(Command::new(
                    ["stash", "push", "--include-untracked"],
                    Purpose::CleanAfterWrite(Box::new(next)),
                ))?;
            }
            return Ok(());
        }
        if event.node == "dirty-commit" {
            self.deferred_operation = self.pending.take();
            self.pending_dirty = false;
            self.editor(
                api::EditorOperation::SetPanelVisibility {
                    panel: "staging".into(),
                    visible: true,
                },
                "visibility",
            )?;
            self.report(Severity::Information, self.label("勾选本次需要提交的文件并填写提交说明；提交后将重新检查工作区", "Select files for this commit and enter a message; the worktree is checked again after commit").into());
            return Ok(());
        }
        if panel == Some("conflicts")
            && matches!(event.node.as_str(), "conflict-stage-2" | "conflict-stage-3")
        {
            return self.choose_conflict_version(if event.node.ends_with('2') { 2 } else { 3 });
        }
        if let Some(index) = event
            .node
            .strip_prefix("hunk-")
            .and_then(|index| index.parse::<usize>().ok())
        {
            let bytes = self
                .patch
                .hunk(index)
                .ok_or_else(|| failure("差异块已失效 / Hunk expired"))?;
            let mut apply = Command::new(
                ["apply", "--cached", "--recount", "--whitespace=nowarn"],
                Purpose::Action,
            );
            if self.patch.staged {
                apply.args.push("--reverse".into());
            }
            apply.args.push("-".into());
            apply.input = Some(bytes);
            let guard = Command::new(
                self.patch_args(self.patch.staged),
                Purpose::Guard {
                    expected: self.patch.bytes.clone(),
                    next: Box::new(apply),
                },
            );
            if self.patch.staged {
                // Reversing a cached patch consumes only HEAD/index bytes. Unsaved worktree
                // text is unrelated and must not be saved or allowed to block this operation.
                self.queue.push_back(guard);
            } else {
                self.stage_intent = Some((self.selected_path.clone(), guard));
                self.editor(api::EditorOperation::ListDocuments, "stage-check")?;
            }
            return Ok(());
        }
        let Some(mut command) = self.command_for_action(&event.node, panel)? else {
            return Ok(());
        };
        if event.node == "stage" {
            let change = self
                .changes
                .iter()
                .find(|c| c.path == self.selected_path)
                .unwrap();
            if change.conflict {
                return Err(failure(
                    "请使用冲突窗口保存并标记解决 / Resolve in the conflict window",
                ));
            }
            let (args, expected) = if let Some(status) = &change.intent_status {
                // Porcelain intentionally suppresses an intent-to-add entry's mode and empty
                // blob OID. Reobserve that exact state, retaining stale-index rejection.
                (
                    vec![
                        "status".into(),
                        "--porcelain=v2".into(),
                        "-z".into(),
                        "--untracked-files=all".into(),
                        "--".into(),
                        self.selected_path.clone(),
                    ],
                    status.clone(),
                )
            } else {
                (
                    vec![
                        "ls-files".into(),
                        "--stage".into(),
                        "-z".into(),
                        "--".into(),
                        self.selected_path.clone(),
                    ],
                    change.index_entry.clone(),
                )
            };
            let guard = Command::new(
                args,
                Purpose::Guard {
                    expected,
                    next: Box::new(command),
                },
            );
            self.stage_intent = Some((self.selected_path.clone(), guard));
            self.editor(api::EditorOperation::ListDocuments, "stage-check")?;
        } else if matches!(
            event.node.as_str(),
            "discard"
                | "branch-rename"
                | "branch-delete"
                | "switch"
                | "checkout-remote"
                | "merge"
                | "merge-no-ff"
                | "merge-squash"
                | "rebase"
                | "cherry-pick"
                | "reset-mixed"
                | "reset-soft"
                | "reset-hard"
                | "abort"
                | "skip"
                | "push"
                | "stash"
                | "stash-pop"
                | "stash-apply"
                | "stash-drop"
                | "pull-merge"
                | "pull-rebase"
        ) {
            let pending_panel = panel.unwrap_or("history").to_owned();
            let pending_dirty = !self.changes.is_empty()
                && matches!(
                    event.node.as_str(),
                    "switch"
                        | "checkout-remote"
                        | "merge"
                        | "merge-no-ff"
                        | "merge-squash"
                        | "rebase"
                        | "cherry-pick"
                        | "pull-merge"
                        | "pull-rebase"
                );
            if !self.prepare_operation_preview(
                command.clone(),
                pending_panel.clone(),
                pending_dirty,
            )? {
                self.set_pending_review(command, pending_panel, pending_dirty);
            }
        } else {
            command.purpose = Purpose::Action;
            self.enqueue_checked(command)?;
        }
        Ok(())
    }
    fn require_path(&self) -> Result<(), api::Failure> {
        if self.changes.iter().any(|c| c.path == self.selected_path) {
            Ok(())
        } else {
            Err(failure("先选择文件 / Select a file"))
        }
    }
}
