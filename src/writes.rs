//! Worktree-changing commands must observe native unsaved buffers before touching disk.
use super::*;
impl State {
    /// Replace one complete pending review: `command` retains its checks/argv, `panel` owns the
    /// confirmation surface and `dirty` requires an explicit worktree choice. A new review
    /// always clears native-buffer recheck state; metadata from an old approval cannot survive.
    pub(super) fn set_pending_review(
        &mut self,
        command: Command,
        panel: impl Into<String>,
        dirty: bool,
    ) {
        self.pending = Some(command);
        self.pending_panel = panel.into();
        self.pending_dirty = dirty;
        self.pending_recheck = false;
    }

    /// Revoked approvals retain only their eventual cleanup receipt, never an executable command.
    /// Persisted unstarted plans are revoked too; actual running operations keep recovery metadata.
    pub(super) fn cancel_pending_write(&mut self) -> Result<(), api::Failure> {
        self.commit_stage_intent = None;
        self.commit_submitting = false;
        self.cancel_operation_preview();
        self.write_intent = None;
        self.pending = None;
        self.pending_dirty = false;
        self.pending_recheck = false;
        self.deferred_operation = None;
        self.rebase_draft = None;
        for intent in self.editor_tasks.values_mut() {
            if intent == "write-check" {
                *intent = "write-check-cancelled".into();
            }
        }
        // Clear all in-memory approvals before a fallible private-data write. A storage error
        // cannot leave an old pending command executable with its recheck flag already removed.
        if self.operation.is_none() && self.running.is_none() && self.interactive_plan.is_some() {
            self.clear_rebase_plan()?;
        }
        Ok(())
    }

    /// Commit/index-only operations deliberately do not save or consume unrelated editor buffers.
    /// Worktree-changing operations stop for explicit saving; no automatic stash/save is implied.
    pub(super) fn enqueue_checked(&mut self, command: Command) -> Result<(), api::Failure> {
        let args = command.effective_args();
        // Index-only forms leave both disk and unsaved native text untouched. Check options only
        // before `--`, so a selected filename cannot accidentally change this classification.
        let options = args
            .iter()
            .skip(1)
            .take_while(|argument| argument.as_str() != "--")
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mutates = match args.first().map(String::as_str) {
            Some("restore") => !options.contains(&"--staged") || options.contains(&"--worktree"),
            Some("rm") => !options.contains(&"--cached"),
            Some("reset") => options.contains(&"--hard"),
            _ => matches!(
                args.first().map(String::as_str),
                Some(
                    "switch"
                        | "merge"
                        | "rebase"
                        | "cherry-pick"
                        | "pull"
                        | "clean"
                        | "stash"
                        | "checkout"
                )
            ),
        };
        if mutates {
            self.write_intent = Some(command);
            self.editor(api::EditorOperation::ListDocuments, "write-check")
        } else {
            self.queue.push_back(command);
            Ok(())
        }
    }
    /// The receipt belongs to the current intent. A dirty buffer remains authoritative even if Git
    /// reports a clean disk; the user saves and reviews again instead of having it overwritten.
    pub(super) fn complete_write_check(
        &mut self,
        documents: &[api::DocumentInfo],
    ) -> Result<(), api::Failure> {
        let Some(command) = self.write_intent.take() else {
            return Ok(());
        };
        let affected = command.effective_args();
        let selected_only = matches!(
            affected.first().map(String::as_str),
            Some("restore" | "clean" | "checkout" | "rm")
        );
        let dirty = documents
            .iter()
            .filter(|info| {
                info.dirty
                    && matches!(info.resource, api::ResourceIdentity::Local { .. })
                    && (!selected_only || affected.iter().any(|arg| arg == &info.document.path))
            })
            .map(|info| info.document.path.as_str())
            .collect::<Vec<_>>();
        if !dirty.is_empty() {
            let paths = dirty.into_iter().take(30).collect::<Vec<_>>().join("\n");
            self.set_pending_review(command, self.pending_panel.clone(), false);
            // Saving changes the disk snapshot, so this retained command is only a review
            // request. The old confirmation can never approve its original impact again.
            self.pending_recheck = true;
            return Err(failure(format!(
                "请先保存这些未保存文件，然后重新检查操作 / Save these buffers and review the operation again:\n{paths}"
            )));
        }
        self.queue.push_back(command);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These commands may run with a dirty native document because neither reads nor writes its
    /// worktree bytes. A save request here is an observable, unintended host side effect.
    #[test]
    fn index_edges_unstage_queues_without_requesting_native_documents() {
        for args in [
            vec!["restore", "--staged", "--", "tracked.txt"],
            vec!["restore", "--staged", "--", "--worktree"],
            vec!["rm", "--cached", "-f", "--", "new.txt"],
        ] {
            let mut state = State::default();
            state
                .enqueue_checked(Command::new(args.clone(), Purpose::Action))
                .unwrap();
            assert!(state.write_intent.is_none());
            assert!(state.editor_tasks.is_empty());
            assert_eq!(state.queue.pop_front().unwrap().args, args);
        }
    }
}
