//! Correlated lifecycle, process and document notifications never own another mutable text model.
use super::*;

impl State {
    /// Return whether this notification needs a replacement scene. Skipped timers and successful
    /// visibility acknowledgements change no business presentation; errors retain normal publication.
    pub(super) fn event(
        &mut self,
        panel: Option<&str>,
        event: api::Notification,
    ) -> Result<bool, api::Failure> {
        self.background_resources.observe(&event);
        if !self.matches_background_resource(&event) {
            return Ok(false);
        }
        let mut publish = true;
        match event {
            api::Notification::CommandInvocation(call) if call.id == "workspace-action" => {
                let result = self.workspace_action(&call.arguments);
                plugin_protocol::commands::reply(
                    call.reply,
                    result.clone().map(|_| serde_json::Value::Null),
                )?;
                if let Err(error) = result {
                    self.message = error.to_string();
                }
            }
            api::Notification::CommandInvocation(call) if call.id == "history-adjacent" => {
                let result = self.adjacent_history_file(&call.arguments);
                plugin_protocol::commands::reply(
                    call.reply,
                    result.clone().map(|_| serde_json::Value::Null),
                )?;
                // Busy/stale arrow callbacks must not clear another operation's owned queue.
                if let Err(error) = result {
                    self.history_error = error.to_string();
                }
            }
            api::Notification::CommandInvocation(call) if call.id == "branch-context" => {
                // The method delegates existing process/editor/panel/storage grants; typed
                // commands must not rely on the stronger ambient permissions of later UI turns.
                let result = self.branch_context(&call.arguments);
                plugin_protocol::commands::reply(
                    call.reply,
                    result.clone().map(|_| serde_json::Value::Null),
                )?;
                // A queued native action can become unavailable before its typed turn runs.
                // Return that failure without clearing another already-running operation's queue.
                if let Err(error) = result {
                    self.message = error.to_string();
                }
            }
            api::Notification::Command { id, arguments, .. } => {
                if let Some(target) = arguments
                    .as_ref()
                    .and_then(|args| args.get("target"))
                    .and_then(|value| value.as_str())
                {
                    if self.argument != target {
                        *self.input_epochs.entry("argument".into()).or_default() += 1;
                        self.argument = target.into();
                    }
                }
                if let Some(path) = arguments
                    .as_ref()
                    .and_then(|args| args.get("path"))
                    .and_then(|path| path.as_str())
                {
                    self.selected_path = path.into();
                }
                if let Some(message) = arguments
                    .as_ref()
                    .and_then(|args| args.get("message"))
                    .and_then(|value| value.as_str())
                {
                    if self.commit_message != message {
                        *self
                            .input_epochs
                            .entry("commit-message".into())
                            .or_default() += 1;
                        self.commit_message = message.into();
                    }
                }
                self.ui_event(
                    None,
                    ui::UiEvent {
                        revision: self.revision,
                        node: id,
                        action: ui::Action::Click,
                    },
                )?;
            }
            api::Notification::Timer { .. } => {
                publish = (self.workspace_ui.resource.is_some()
                    || self.visible_panels.values().any(|visible| *visible))
                    && self.automatic_refresh();
            }
            api::Notification::PanelVisibility { panel, visible } => {
                self.visible_panels.insert(panel, visible);
                if visible {
                    self.request_observation();
                }
            }
            api::Notification::Focus(true) => self.request_observation(),
            api::Notification::Theme(environment) => self.environment = environment,
            api::Notification::SubscriptionFailed {
                subscription: _,
                error,
            } => {
                // The host retired this subscription before the terminal receipt. Do not
                // restart an unbounded stream; explicit reactivation can rebuild the pair.
                self.document_subscription = None;
                self.message = error.to_string();
                return Ok(true);
            }
            api::Notification::DocumentEvent { event, .. } => {
                if let api::DocumentEventKind::Closed(document) = &event.kind {
                    self.forget_closed_diff_source(document);
                    self.diff_views.retain(|_, sources| {
                        !sources
                            .left
                            .as_ref()
                            .is_some_and(|current| current.id == document.id)
                            && !sources
                                .right
                                .as_ref()
                                .is_some_and(|current| current.id == document.id)
                            && !sources
                                .third
                                .as_ref()
                                .is_some_and(|current| current.id == document.id)
                    });
                    for file in self.merge_files.values_mut().filter(|file| {
                        file.document
                            .as_ref()
                            .is_some_and(|current| current.id == document.id)
                    }) {
                        file.document = None;
                        file.blocks.clear();
                        file.read_valid = false;
                        file.read_inflight = false;
                    }
                }
                if matches!(event.kind, api::DocumentEventKind::DidSave { .. }) {
                    self.request_observation();
                }
                if let api::DocumentEventKind::ContentChanged(info)
                | api::DocumentEventKind::Opened(info) = event.kind
                {
                    let mut read = None;
                    for (path, file) in &mut self.merge_files {
                        if file
                            .document
                            .as_ref()
                            .is_some_and(|old| old.id == info.document.id)
                        {
                            file.document = Some(info.document.clone());
                            file.blocks.clear();
                            file.read_valid = false;
                            if !file.read_inflight {
                                file.read_inflight = true;
                                read = Some((path.clone(), info.document.clone()));
                            }
                        }
                    }
                    if let Some((path, document)) = read {
                        self.editor(
                            api::EditorOperation::ReadDocument {
                                document,
                                range: None,
                            },
                            &format!("merge-read:{path}"),
                        )?;
                    }
                }
            }
            api::Notification::Ui(event) => {
                self.ui_event(panel, event)?;
            }
            api::Notification::Process { handle, update } => {
                let Some(running) = self.running.as_mut().filter(|r| r.handle == handle) else {
                    // A retired optional read cannot repaint the newer admitted user action.
                    return Ok(false);
                };
                match update {
                    plugin_protocol::process::Update::Output { stream, bytes } => {
                        let output = if stream == plugin_protocol::process::Stream::Stdout {
                            &mut running.output
                        } else {
                            &mut running.stderr
                        };
                        if output.len() + bytes.len() > 8 * 1024 * 1024 {
                            self.automatic_observation = false;
                            let _ = api::guest::close_resource(handle);
                            self.running = None;
                            self.queue.clear();
                            self.history_refresh = None;
                            self.cancel_operation_preview();
                            self.message =
                                "输出超过本次查询限制，请缩小范围 / Output limit exceeded".into();
                            self.abort_diff(Some(&failure(self.message.clone())));
                            self.fail_history_preview();
                        } else {
                            output.extend(bytes)
                        }
                    }
                    plugin_protocol::process::Update::Exited { code } => {
                        let running = self.running.take().unwrap();
                        self.finished(running, code)?;
                    }
                    plugin_protocol::process::Update::Terminated => {
                        self.automatic_observation = false;
                        self.running = None;
                        self.queue.clear();
                        self.history_refresh = None;
                        self.abort_diff(Some(&failure(
                            "比较查询已停止，请重新选择文件 / Comparison query stopped. Select the file again",
                        )));
                        self.cancel_operation_preview();
                        self.refresh();
                    }
                }
            }
            api::Notification::Request {
                handle,
                update: api::RequestUpdate::Cancelled { reason, .. },
            } if self
                .editor_tasks
                .get(&handle.resource)
                .is_some_and(|intent| {
                    matches!(intent.as_str(), "write-check" | "write-check-cancelled")
                }) =>
            {
                // Deadlines and stop-waiting requests also have a terminal receipt. Both forms
                // must release their correlation; only the still-active one can revoke an intent.
                let intent = self.editor_tasks.remove(&handle.resource).unwrap();
                let _ = api::guest::close_resource(handle);
                if intent == "write-check" {
                    self.write_intent = None;
                    return Err(api::Failure::new(
                        reason,
                        self.label(
                            "文档检查已取消，请重新选择操作并审查",
                            "Buffer check cancelled. Select the operation again and review it",
                        ),
                    ));
                }
            }
            api::Notification::Request {
                handle,
                update: api::RequestUpdate::Cancelled { reason, .. },
            } if self
                .editor_tasks
                .get(&handle.resource)
                .is_some_and(|intent| comparisons::is_diff_intent(intent)) =>
            {
                self.editor_tasks.remove(&handle.resource);
                self.take_diff_request(handle.resource);
                let _ = api::guest::close_resource(handle);
                let error = api::Failure::new(
                    reason,
                    self.label(
                        "比较已取消或超时，请重新选择文件",
                        "Comparison cancelled or timed out. Select the file again",
                    ),
                );
                self.abort_diff(Some(&error));
                return Err(error);
            }
            api::Notification::Request {
                handle,
                update: api::RequestUpdate::Completed { result },
            } => {
                let Some(intent) = self.editor_tasks.remove(&handle.resource) else {
                    return Ok(true);
                };
                let new_source = self.take_diff_request(handle.resource);
                let _ = api::guest::close_resource(handle.clone());
                // Cancellation changes the intent's correlation, not a native document. Always
                // release its terminal resource, then ignore both late success and late failure.
                if intent == "write-check-cancelled" {
                    return Ok(true);
                }
                if let Err(error) = &result {
                    if comparisons::is_diff_intent(&intent) {
                        self.abort_diff(Some(error));
                    }
                    if let Some(path) = intent.strip_prefix("merge-open:") {
                        if let Some(file) = self.merge_files.get_mut(path) {
                            file.opaque = true;
                        }
                        self.show_window("conflicts")?;
                        self.message = error.to_string();
                        return Ok(true);
                    }
                    if let Some(path) = intent.strip_prefix("merge-read:") {
                        if let Some(file) = self.merge_files.get_mut(path) {
                            file.read_inflight = false;
                            file.read_valid = false;
                            if error.code == api::ErrorCode::StaleRevision
                                && file.document.is_some()
                            {
                                // Typing may advance several revisions before an earlier read is
                                // served. Re-enumerate metadata instead of waiting for an event
                                // that may already have passed while that read was in flight.
                                file.read_inflight = true;
                                self.editor(
                                    api::EditorOperation::ListDocuments,
                                    &format!("merge-resync:{path}"),
                                )?;
                                return Ok(true);
                            }
                        }
                    }
                    return Err(error.clone());
                }
                match result? {
                    api::EditorValue::PanelVisibility { .. } if intent == "visibility" => {
                        publish = false;
                    }
                    api::EditorValue::DocumentOpened(info) => {
                        if matches!(
                            intent.as_str(),
                            "diff-open" | "diff-left" | "diff-right" | "diff-third"
                        ) {
                            self.observe_diff_source(&info, new_source);
                        }
                        match intent.as_str() {
                            "diff-open" => self.right_version = Some(info.document),
                            "diff-left" => {
                                self.diff_views
                                    .entry(self.diff_key.clone())
                                    .or_default()
                                    .left = Some(info.document.clone());
                                self.left_version = Some(info.document);
                            }
                            "diff-right" => {
                                self.diff_views
                                    .entry(self.diff_key.clone())
                                    .or_default()
                                    .right = Some(info.document.clone());
                                self.right_version = Some(info.document);
                            }
                            "diff-third" => {
                                self.diff_views
                                    .entry(self.diff_key.clone())
                                    .or_default()
                                    .third = Some(info.document.clone());
                                self.third_version = Some(info.document);
                            }
                            _ if intent.starts_with("merge-open:") => {
                                let path = &intent[11..];
                                if let Some(file) = self.merge_files.get_mut(path) {
                                    file.document = Some(info.document.clone());
                                    file.read_inflight = true;
                                }
                                self.editor(
                                    api::EditorOperation::ReadDocument {
                                        document: info.document,
                                        range: None,
                                    },
                                    &format!("merge-read:{path}"),
                                )?;
                                self.show_window("conflicts")?;
                            }
                            _ => {}
                        }
                    }
                    api::EditorValue::DocumentsCompared { .. } if intent == "compare" => {
                        self.complete_diff_flight();
                    }
                    api::EditorValue::Edited { document, .. } => {
                        let mut read = None;
                        for (path, file) in &mut self.merge_files {
                            if file
                                .document
                                .as_ref()
                                .is_some_and(|old| old.id == document.id)
                            {
                                if file
                                    .document
                                    .as_ref()
                                    .is_some_and(|current| current.revision < document.revision)
                                {
                                    file.document = Some(document.clone());
                                    // A write receipt grants the new version, not authority for
                                    // the old ranges. Hide them until that exact value is read.
                                    file.blocks.clear();
                                    file.read_valid = false;
                                }
                                if !file.read_valid && !file.read_inflight {
                                    file.read_inflight = true;
                                    read = Some((path.clone(), file.document.clone().unwrap()));
                                }
                            }
                        }
                        if let Some((path, document)) = read {
                            self.editor(
                                api::EditorOperation::ReadDocument {
                                    document,
                                    range: None,
                                },
                                &format!("merge-read:{path}"),
                            )?;
                        }
                    }
                    api::EditorValue::Saved { document } => {
                        self.result = Some(document);
                        if let Some(path) = intent.strip_prefix("save-resolve:") {
                            let expected = self
                                .merge_files
                                .get(path)
                                .ok_or_else(|| failure("Conflict session expired"))?
                                .index_entry
                                .clone();
                            let next = Command::new(["add", "--", path], Purpose::Action);
                            self.queue.push_back(Command::new(
                                ["ls-files", "--stage", "-z", "--", path],
                                Purpose::Guard {
                                    expected,
                                    next: Box::new(next),
                                },
                            ));
                        }
                    }
                    api::EditorValue::Documents { documents, .. } if intent == "stage-check" => {
                        if let Some((path, command)) = self.stage_intent.take() {
                            if documents
                                .iter()
                                .any(|document| document.document.path == path && document.dirty)
                            {
                                return Err(failure(
                                    "所选文件有未保存修改，请保存并检查差异后重试 / Selected files have unsaved changes; save and review before retrying",
                                ));
                            }
                            self.queue.push_back(command);
                        }
                    }
                    api::EditorValue::Documents { documents, .. }
                        if intent == "commit-stage-check" =>
                    {
                        if let Some((paths, command)) = self.commit_stage_intent.take() {
                            if documents.iter().any(|document| {
                                document.dirty && paths.contains(&document.document.path)
                            }) {
                                return Err(failure(
                                    "先保存并刷新差异，再重新暂存 / Save, refresh and select the change again",
                                ));
                            }
                            self.queue.push_back(command);
                        }
                    }
                    api::EditorValue::Documents { documents, .. } if intent == "write-check" => {
                        self.complete_write_check(&documents)?;
                    }
                    api::EditorValue::Documents { documents, .. }
                        if intent.starts_with("merge-resync:") =>
                    {
                        let path = &intent[13..];
                        let mut read = None;
                        if let Some(file) = self.merge_files.get_mut(path) {
                            file.read_inflight = false;
                            if let Some(current) = &file.document {
                                if let Some(info) = documents.iter().find(|info| {
                                    info.document.id == current.id
                                        && info.document.path == current.path
                                }) {
                                    let document = if current.revision > info.document.revision {
                                        current.clone()
                                    } else {
                                        info.document.clone()
                                    };
                                    file.document = Some(document.clone());
                                    file.read_inflight = true;
                                    read = Some(document);
                                } else {
                                    // Close/reopen is a new source identity, never a retry target.
                                    file.document = None;
                                    file.blocks.clear();
                                }
                            }
                        }
                        if let Some(document) = read {
                            self.editor(
                                api::EditorOperation::ReadDocument {
                                    document,
                                    range: None,
                                },
                                &format!("merge-read:{path}"),
                            )?;
                        }
                    }
                    api::EditorValue::DocumentSnapshot(snapshot)
                        if intent.starts_with("merge-read:") =>
                    {
                        let path = &intent[11..];
                        if let Some(file) = self.merge_files.get_mut(path) {
                            file.read_inflight = false;
                            if file.document.as_ref() == Some(&snapshot.info.document) {
                                file.observe(&snapshot.text)?;
                            } else if let Some(document) = file.document.clone() {
                                file.read_inflight = true;
                                self.editor(
                                    api::EditorOperation::ReadDocument {
                                        document,
                                        range: None,
                                    },
                                    &format!("merge-read:{path}"),
                                )?;
                            }
                        }
                    }
                    _ => {}
                }
                if (!self.diff_history
                    || self.snapshot_targets.len() == 2
                    || self.third_version.is_some())
                    && let (Some(left), Some(right)) =
                        (self.left_version.clone(), self.right_version.clone())
                {
                    publish = true;
                    self.left_version = None;
                    self.right_version = None;
                    self.diff_mode = None;
                    let third = self.third_version.take();
                    self.editor(
                        api::EditorOperation::OpenDocumentComparison {
                            key: self.diff_key.clone(),
                            title: self.diff_title.clone(),
                            left,
                            right,
                            third,
                            navigation: self.comparison_navigation(),
                            // Artwork is packaged here; the public host mechanism owns only display folding.
                            folding: Some(api::ComparisonFolding {
                                collapse: ui::ToolIcon::new(
                                    "icons/explorer-collapse-all.svg",
                                    "icons/explorer-collapse-all.svg",
                                ),
                                expand: ui::ToolIcon::new(
                                    "icons/explorer-expand-all.svg",
                                    "icons/explorer-expand-all.svg",
                                ),
                                context_lines: 3,
                            }),
                            preview: true,
                        },
                        "compare",
                    )?;
                }
            }
            _ => {}
        }
        publish |= self.resume_explicit_refresh();
        Ok(publish)
    }
}
