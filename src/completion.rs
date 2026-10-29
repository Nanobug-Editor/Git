//! Complete Git query decoding is separate from process ownership, UI state and native document receipts.
use super::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod history_blob_tests;

/// One complete `ls-tree -z` record identifies a blob or an absent file side. A replacement
/// directory is not that file's content; gitlinks and malformed records cannot become blank text.
fn history_blob_identity(output: &[u8]) -> Result<Option<&str>, api::Failure> {
    if output.is_empty() {
        return Ok(None);
    }
    let record = output
        .strip_suffix(b"\0")
        .ok_or_else(|| failure("Invalid tree record"))?;
    let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .filter(|index| index + 1 < record.len())
        .ok_or_else(|| failure("Invalid tree record"))?;
    if record.contains(&0) {
        return Err(failure("Invalid tree record"));
    }
    let header = std::str::from_utf8(&record[..tab]).map_err(|_| failure("Invalid tree record"))?;
    let mut fields = header.split(' ');
    let mode = fields.next().unwrap_or_default();
    let kind = fields.next().unwrap_or_default();
    let oid = fields
        .next()
        .filter(|oid| !oid.is_empty() && oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| failure("Missing blob identity"))?;
    if fields.next().is_some() {
        return Err(failure("Invalid tree record"));
    }
    match (mode, kind) {
        ("100644" | "100755" | "120000", "blob") => Ok(Some(oid)),
        ("040000", "tree") => Ok(None),
        ("160000", "commit") => Err(failure(
            "子模块不是文件文本，无法比较 / A submodule is not file text and cannot be compared",
        )),
        _ => Err(failure("Invalid or unsupported tree object")),
    }
}

impl State {
    /// Decode only successful complete outputs. A failed write invalidates the queued view refresh.
    pub(super) fn finished(&mut self, running: Running, code: u32) -> Result<(), api::Failure> {
        if let Purpose::HistoryPrefetch(request) = &running.command.purpose {
            self.finish_history_prefetch(request, &running.output, code);
            return Ok(());
        }
        if let Purpose::BranchFiles { generation } = running.command.purpose {
            return self.finish_branch_comparison(
                generation,
                &running.output,
                &running.stderr,
                code,
            );
        }
        if let Purpose::Log { generation, filter } = &running.command.purpose
            && (*generation != self.history_cache.generation || *filter != self.filter)
        {
            return Ok(());
        }
        if let Purpose::Files(oid) | Purpose::CommitMessage(oid) = &running.command.purpose
            && self.selected != *oid
        {
            // Even an error from a retired selection has no authority over the current preview.
            return Ok(());
        }
        if let Purpose::Repository(query) = running.command.purpose.clone() {
            return self.repository_finished(query, running.output, running.stderr, code);
        }
        if let Purpose::OperationPreview(query) = running.command.purpose.clone() {
            return self.operation_preview_finished(query, &running.output, &running.stderr, code);
        }
        if matches!(running.command.purpose, Purpose::Head) {
            let head = if code == 0 {
                Some(
                    String::from_utf8(running.output)
                        .map_err(|_| failure("Invalid HEAD identity"))?
                        .trim()
                        .into(),
                )
            } else {
                None
            };
            if self.head != head {
                self.automatic_observation = false;
                self.history_cache.invalidate_logs();
            }
            self.head = head;
            return Ok(());
        }
        if let Purpose::OperationProbe { round, marker } = running.command.purpose {
            let publication = self
                .conflict_lifecycle
                .probe(round, marker, &running.output, code);
            return self.publish_conflict_observation(publication);
        }
        if let Purpose::OperationBatch { round } = running.command.purpose {
            if code == operation_observation::UNSUPPORTED_STORAGE_EXIT {
                self.queue_operation_probes(round, false);
                return Ok(());
            }
            let publication = self
                .conflict_lifecycle
                .probes(round, "", &running.output, code);
            return self.publish_conflict_observation(publication);
        }
        if let Purpose::Status { round } = running.command.purpose {
            let status = (code == 0)
                .then(|| model::status(&running.output).ok())
                .flatten();
            let publication = self.conflict_lifecycle.status(round, status);
            return self.publish_conflict_observation(publication);
        }
        if code != 0 {
            self.automatic_observation = false;
            self.message = safe_diagnostic(&running.stderr);
            if matches!(running.command.purpose, Purpose::PushAfterCommit { .. }) {
                self.message = format!(
                    "提交已成功，推送失败；本地提交已保留 / Commit succeeded, push failed; local commit retained: {}",
                    self.message
                );
            }
            if matches!(running.command.purpose, Purpose::PushAfterCommit { .. })
                || running
                    .command
                    .args
                    .first()
                    .is_some_and(|arg| arg == "commit")
            {
                self.commit_submit_error = Some(self.message.clone());
            }
            if matches!(running.command.purpose, Purpose::Log { .. }) {
                self.history_error = self.message.clone();
                self.history_extend_pending = false;
            }
            self.fail_panel_activity(
                running.command.feedback_owner(),
                &failure(self.message.clone()),
            );
            // A failed native write can already have changed refs/index or entered a conflict.
            // Reobserve actual Git state without claiming failure rolled it back.
            if matches!(
                running.command.purpose,
                Purpose::Action
                    | Purpose::CleanAfterWrite(_)
                    | Purpose::Then(_)
                    | Purpose::PushAfterCommit { .. }
            ) {
                if running.command.changes_index_only() {
                    self.refresh_worktree();
                } else {
                    self.refresh();
                }
            }
            return Ok(());
        }
        match running.command.purpose {
            Purpose::Status { .. } => unreachable!(),
            Purpose::Refs => {
                let old_upstreams = std::mem::take(&mut self.ref_upstreams);
                let old_refs = std::mem::take(&mut self.refs);
                let old_current = std::mem::take(&mut self.current_ref);
                let old_tips = std::mem::take(&mut self.ref_tips);
                for line in String::from_utf8(running.output)
                    .map_err(|_| failure("Invalid ref encoding"))?
                    .lines()
                {
                    if let Some((head, reference)) = line.split_once('\t') {
                        let (reference, tip) =
                            reference.split_once('\t').unwrap_or((reference, ""));
                        let (tip, upstream) = tip.split_once('\t').unwrap_or((tip, ""));
                        if !upstream.is_empty() {
                            self.ref_upstreams.insert(reference.into(), upstream.into());
                        }
                        self.ref_tips.insert(reference.into(), tip.into());
                        self.refs.push(reference.into());
                        if head == "*" {
                            self.current_ref = reference.into();
                        }
                    }
                }
                if self.ref_tips != old_tips || self.current_ref != old_current {
                    self.history_cache.invalidate_logs();
                }
                // Branch pages own ref membership and the current-branch marker, independently
                // of status and commit-detail files. An identical local poll keeps them loaded.
                if self.current_ref != old_current || self.ref_upstreams != old_upstreams {
                    self.automatic_observation = false;
                    if self.workspace_ui.resource.is_some() {
                        if let Some(tree) = self.trees.get("branches") {
                            tree::refresh(tree.clone())?;
                        }
                        // The host discarded its cached forest; provider baselines must restart
                        // with fresh lazy replies rather than patching pages no longer present.
                        self.tree_pages.remove("branches");
                    }
                    self.tree_paths
                        .retain(|key, _| !key.starts_with("branches:"));
                    self.branch_menu_stamp = None;
                }
                if self.refs != old_refs || self.current_ref != old_current {
                    self.automatic_observation = false;
                    // Cached metadata belongs to this ref incarnation. New root/prefix replies
                    // repopulate only published IDs; old removed refs must not grow update work.
                    self.reconcile_tree("branches")?;
                }
                if !self.branch_initialized
                    && let Some(path) = trees::branch_initial_path(&self.current_ref)
                    && let Some(handle) = self.trees.get("branches")
                {
                    tree::initialize(handle.clone(), path)?;
                    self.branch_initialized = true;
                }
            }
            Purpose::Log { .. } => {
                let commits = model::log(&running.output).map_err(|error| {
                    // An incomplete page has no publication authority and cannot hijack More.
                    self.history_refresh = None;
                    self.history_extend_pending = false;
                    failure(error)
                })?;
                self.history_page(commits);
            }
            Purpose::Files(oid) => {
                // A replaced selection owns neither this page nor a later message receipt.
                if self.selected != oid {
                    return Ok(());
                }
                self.pending_history_files =
                    Some(model::history_changes(&running.output).map_err(failure)?);
                self.queue.push_back(Command::new(
                    ["show", "--no-patch", "--format=%B", &oid, "--"],
                    Purpose::CommitMessage(oid.clone()),
                ));
            }
            Purpose::CommitMessage(oid) => {
                if self.selected != oid {
                    return Ok(());
                }
                // Returning to the same immutable OID can make an older message receipt match
                // again after its paired files were cancelled. Only a complete current file map
                // may enable the tree; the newly queued Files query will supply its own message.
                let Some(files) = self.pending_history_files.take() else {
                    return Ok(());
                };
                let message = history_preview::decode_message(running.output)?;
                let paths = files.keys().cloned().collect();
                let previous_status = std::mem::replace(&mut self.history_file_status, files);
                let previous_files = std::mem::replace(&mut self.history_files, paths);
                // Admission can reject an oversized/invalid delta. Keep the last complete
                // preview in that case, matching the host's atomic preservation of its tree.
                if let Err(error) = self.reconcile_tree("files") {
                    self.history_file_status = previous_status;
                    self.history_files = previous_files;
                    return Err(error);
                }
                self.history_message = message;
                self.history_preview_commit = self.history_commit().cloned();
                self.history_loading = false;
                // Startup may have discovered this selected commit during a silent observation.
                // Its complete tree is already admitted: publish the matching enabled scene now,
                // rather than waiting for another native event that a fully cached tree never sends.
                self.automatic_observation = false;
                self.cache_history_preview();
            }
            purpose @ (Purpose::Snapshot { .. } | Purpose::DiffSnapshot { .. }) => {
                let (column, diff) = match purpose {
                    Purpose::Snapshot { column } => (column, false),
                    Purpose::DiffSnapshot { column } => (column, true),
                    _ => unreachable!(),
                };
                let text = String::from_utf8(running.output).map_err(|_| {
                    failure("二进制或非 UTF-8：不进行文本比较 / Binary or non UTF-8")
                })?;
                if text.len() > 256 * 1024 || text.contains('\0') {
                    return Err(failure(
                        "二进制或文本过大 / Binary or text comparison limit",
                    ));
                }
                if !diff {
                    self.remember_history_source(column, Some(&text));
                }
                let columns = if diff {
                    &mut self.diff_columns
                } else {
                    &mut self.columns
                };
                if columns.len() <= column {
                    columns.resize(column + 1, String::new());
                }
                columns[column] = text;
                if !diff {
                    if self.diff_history {
                        self.diff_columns[column] = self.columns[column].clone();
                        self.snapshot_ready += 1;
                        self.maybe_start_diff()?;
                    }
                    return Ok(());
                }
                self.snapshot_ready += 1;
                self.maybe_start_diff()?;
            }
            Purpose::HistoryBatch { columns, path } => {
                self.finish_history_batch(&columns, &path, &running.output)?;
            }
            Purpose::HistoryBlob { column } => {
                if let Some(oid) = history_blob_identity(&running.output)? {
                    self.queue.push_front(Command::new(
                        ["cat-file", "blob", oid],
                        Purpose::Snapshot { column },
                    ));
                } else {
                    self.remember_history_source(column, None);
                    if self.columns.len() <= column {
                        self.columns.resize(column + 1, String::new());
                    }
                    if self.diff_history {
                        self.snapshot_ready += 1;
                        self.maybe_start_diff()?;
                    }
                }
            }
            Purpose::MergeSize {
                path,
                column,
                stage,
            } => {
                let size = std::str::from_utf8(&running.output)
                    .ok()
                    .and_then(|text| text.trim().parse::<usize>().ok())
                    .ok_or_else(|| failure("Invalid conflict blob size"))?;
                if size > 256 * 1024 {
                    self.merge_files.get_mut(&path).unwrap().opaque = true;
                    self.conflict_source_ready(&path)?;
                } else {
                    self.queue.push_front(Command::new(
                        vec!["show".into(), format!(":{stage}:{path}")],
                        Purpose::MergeSnapshot { path, column },
                    ));
                }
            }
            Purpose::MergeSnapshot { path, column } => {
                let file = self
                    .merge_files
                    .get_mut(&path)
                    .ok_or_else(|| failure("Conflict session expired"))?;
                match String::from_utf8(running.output) {
                    Ok(text) if !text.contains('\0') => file.columns[column] = text,
                    _ => file.opaque = true,
                }
                self.conflict_source_ready(&path)?;
            }
            Purpose::Patch { staged } => {
                self.patch = patch::Patch::read(running.output, staged).map_err(failure)?
            }
            Purpose::Action => {
                if running
                    .command
                    .args
                    .first()
                    .is_some_and(|arg| arg == "commit")
                {
                    self.commit_submitting = false;
                }
                self.follow_branch_metadata_change(&running.command.args)?;
                let checkout = if running
                    .command
                    .args
                    .first()
                    .is_some_and(|arg| arg == "switch")
                {
                    if running.command.args.len() == 2 {
                        running.command.args.get(1).cloned()
                    } else {
                        running
                            .command
                            .args
                            .windows(2)
                            .find(|pair| pair[0] == "-c")
                            .map(|pair| pair[1].clone())
                    }
                } else {
                    None
                };
                let preference_error = checkout
                    .as_deref()
                    .and_then(|branch| self.record_checkout(branch).err());
                if running.command.service.as_deref() == Some("rebase") {
                    self.clear_rebase_plan()?;
                }
                if running
                    .command
                    .args
                    .first()
                    .is_some_and(|arg| arg == "fetch")
                {
                    self.last_fetch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|time| time.as_secs());
                }
                self.message = preference_error.map(|error| format!("Git 操作已完成，但最近分支保存失败 / Git operation completed, but recent branches could not be saved: {error}")).unwrap_or_else(|| "Git 操作完成 / Operation completed".into());
                if running.command.changes_index_only() {
                    self.refresh_worktree();
                } else {
                    self.queue.clear();
                    self.refresh();
                }
            }
            Purpose::CleanAfterWrite(next) => {
                self.commit_submitting = false;
                self.queue.push_front(Command::new(
                    ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                    Purpose::CheckClean(next),
                ));
            }
            Purpose::Then(next) => {
                if matches!(next.purpose, Purpose::PushAfterCommit { .. }) {
                    self.message = "提交已成功，正在推送… / Commit succeeded, pushing…".into();
                }
                self.enqueue_checked(*next)?;
            }
            Purpose::PushAfterCommit { next } => {
                self.commit_submitting = false;
                self.message = "提交并推送完成 / Commit and push completed".into();
                if let Some(next) = next {
                    // Resume a previously requested operation only after the usual fresh clean-tree review.
                    self.queue.push_front(Command::new(
                        ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                        Purpose::CheckClean(next),
                    ));
                } else {
                    self.queue.clear();
                    self.refresh();
                }
            }
            Purpose::ReviewAfterCommit(next) => self.reconsider_operation(*next)?,
            Purpose::CheckClean(next) => {
                self.changes = model::status(&running.output).map_err(failure)?;
                if self.changes.is_empty() {
                    // Commit and stash can change an operation's frozen HEAD or obstruction list.
                    // Re-read its impact and request review again rather than executing an old plan.
                    self.reconsider_operation(*next)?;
                } else {
                    // This fresh dirty-worktree review replaces any previously revoked approval.
                    self.set_pending_review(*next, self.pending_panel.clone(), true);
                    self.message = self
                        .label(
                            "仍有未提交修改，请处理后继续",
                            "Uncommitted changes remain; review before continuing",
                        )
                        .into();
                }
            }
            Purpose::LeaseSource { remote, reference } => {
                let local = String::from_utf8(running.output)
                    .map_err(|_| failure("Invalid local commit"))?
                    .trim()
                    .to_owned();
                if !matches!(local.len(), 40 | 64)
                    || !local.bytes().all(|ch| ch.is_ascii_hexdigit())
                {
                    return Err(failure("Invalid local commit identity"));
                }
                self.queue.push_front(Command::new(
                    [
                        "ls-remote",
                        "--exit-code",
                        "--refs",
                        "--",
                        &remote,
                        &reference,
                    ],
                    Purpose::Lease {
                        remote: remote.clone(),
                        reference: reference.clone(),
                        local,
                    },
                ));
            }
            Purpose::RebaseBase => {
                let base = String::from_utf8(running.output)
                    .map_err(|_| failure("Invalid rebase base"))?
                    .trim()
                    .to_owned();
                self.queue.push_front(Command::new(
                    ["rev-parse", "--verify", "HEAD"],
                    Purpose::RebaseHead { base },
                ));
            }
            Purpose::RebaseHead { base } => {
                let head = String::from_utf8(running.output)
                    .map_err(|_| failure("Invalid rebase HEAD"))?
                    .trim()
                    .to_owned();
                self.queue.push_front(Command::new(
                    vec![
                        "log".into(),
                        "-z".into(),
                        "--reverse".into(),
                        "--topo-order".into(),
                        "--max-count=65".into(),
                        "--format=%H%x00%P%x00%s%x00%B".into(),
                        format!("{base}..{head}"),
                        "--".into(),
                    ],
                    Purpose::RebaseRange { base, head },
                ));
            }
            Purpose::RebaseRange { base, head } => {
                self.prepare_rebase_plan(base, head, &running.output)?
            }
            Purpose::Lease {
                remote,
                reference,
                local,
            } => {
                let output = String::from_utf8(running.output)
                    .map_err(|_| failure("Invalid remote identity"))?;
                let oid = output
                    .lines()
                    .find_map(|line| {
                        line.split_once('\t')
                            .filter(|(_, name)| *name == reference)
                            .map(|(oid, _)| oid)
                    })
                    .filter(|oid| {
                        matches!(oid.len(), 40 | 64) && oid.bytes().all(|ch| ch.is_ascii_hexdigit())
                    })
                    .ok_or_else(|| {
                        failure("Remote ref is absent or ambiguous; fetch and review first")
                    })?;
                // The remote OID was sampled for this new manual confirmation only.
                self.set_pending_review(
                    Command::new(
                        vec![
                            "push".into(),
                            format!("--force-with-lease={reference}:{oid}"),
                            remote,
                            format!("{local}:{reference}"),
                        ],
                        Purpose::Action,
                    ),
                    "history",
                    false,
                );
            }
            Purpose::OperationProbe { .. }
            | Purpose::OperationBatch { .. }
            | Purpose::Head
            | Purpose::BranchFiles { .. }
            | Purpose::HistoryPrefetch(_) => {
                unreachable!()
            }
            Purpose::Repository(_) => unreachable!(),
            Purpose::OperationPreview(_) => unreachable!(),

            Purpose::Guard { expected, next } => {
                if running.output != expected {
                    self.patch = Default::default();
                    self.message="Git 前置状态已变化，请重新检查操作 / Git preconditions changed; review the operation again".into();
                    self.queue.clear();
                    self.refresh();
                    return Ok(());
                }
                self.enqueue_checked(*next)?;
            }
            Purpose::CommitSelection {
                expected,
                head,
                branch,
                next,
            } => {
                self.finish_commit_selection(expected, head, branch, *next, &running.output)?;
            }
        }
        Ok(())
    }
}
