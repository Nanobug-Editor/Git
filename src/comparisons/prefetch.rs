//! Optional neighboring history reads populate only the bounded immutable source cache.
use super::*;
use plugin_protocol::process;

/// One file's exact repository, owner and commit sides survive unrelated log/preview changes.
#[derive(Clone)]
pub(crate) struct HistoryPrefetch {
    workspace: String,
    origin: String,
    path: String,
    targets: Vec<String>,
    /// A failed cancellation retains the process handle, but revokes its cache authority.
    pub(crate) discarded: bool,
}

impl HistoryPrefetch {
    /// Retired views and replaced workspaces cannot authorize a deferred or late cache write.
    fn current(&self, state: &State) -> bool {
        !self.discarded
            && self.workspace == state.environment.workspace
            && state
                .diff_views
                .get(&self.origin)
                .and_then(|sources| sources.navigation.as_ref())
                .is_some_and(|navigation| {
                    navigation.neighboring_sources().any(|(targets, path)| {
                        path == self.path && self.targets.iter().all(|oid| targets.contains(oid))
                    })
                })
    }

    /// Git receives literal NUL-delimited expressions, including filenames containing newlines.
    fn command(self) -> Command {
        let input = self
            .targets
            .iter()
            .flat_map(|oid| format!("{oid}:{}\0", self.path).into_bytes())
            .collect();
        let mut command = Command::new(
            ["cat-file", "--batch", "-z"],
            Purpose::HistoryPrefetch(self),
        );
        command.input = Some(input);
        command
    }
}

impl State {
    /// Visible admission creates at most two optional reads, never a recursive prefetch chain.
    pub(crate) fn schedule_history_prefetch(&mut self) {
        self.history_prefetch.clear();
        let Some(navigation) = self
            .diff_views
            .get(&self.diff_key)
            .and_then(|sources| sources.navigation.as_ref())
        else {
            return;
        };
        for (targets, path) in navigation.neighboring_sources() {
            // Only full immutable object IDs enter speculation; HEAD/index/worktree stay fresh.
            if targets.len() > 3
                || targets.iter().any(|oid| {
                    !oid.is_empty()
                        && (!matches!(oid.len(), 40 | 64)
                            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
                })
                || path.is_empty()
                || path.len() > 4096
                || path.contains('\0')
            {
                continue;
            }
            let mut missing = Vec::new();
            for oid in targets {
                if !oid.is_empty()
                    && !missing.contains(oid)
                    && !self
                        .history_sources
                        .contains(&self.environment.workspace, oid, path)
                {
                    missing.push(oid.clone());
                }
            }
            if !missing.is_empty() {
                self.history_prefetch.push_back(HistoryPrefetch {
                    workspace: self.environment.workspace.clone(),
                    origin: self.diff_key.clone(),
                    path: path.into(),
                    targets: missing,
                    discarded: false,
                });
            }
        }
    }

    /// Business selection, writes and refreshes run first; idle receipts consume each candidate once.
    pub(crate) fn resume_history_prefetch(&mut self) {
        if self.busy()
            || !self.pending_comparisons.is_empty()
            || self.explicit_refresh_pending
            || self.passive_refresh_pending
        {
            return;
        }
        while let Some(mut request) = self.history_prefetch.pop_front() {
            if !request.current(self) {
                continue;
            }
            request.targets.retain(|oid| {
                !self
                    .history_sources
                    .contains(&request.workspace, oid, &request.path)
            });
            if !request.targets.is_empty() {
                // A completed automatic observation may have left its flags set until idle.
                // This child has its own cancellation path, especially when release fails.
                self.automatic_observation = false;
                self.background_read = false;
                self.queue.push_back(request.command());
                break;
            }
        }
    }

    /// Optional reads cannot disable controls; real queued or native work keeps its ordinary guard.
    pub(crate) fn history_prefetch_only(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| matches!(running.command.purpose, Purpose::HistoryPrefetch(_)))
            && self.queue.is_empty()
            && !self.merge_files.values().any(|file| file.read_inflight)
            && self
                .editor_tasks
                .values()
                .all(|intent| matches!(intent.as_str(), "visibility" | "write-check-cancelled"))
    }

    /// Input abandons pending neighbors and releases only the owned speculative child.
    /// A close failure keeps the handle for its terminal receipt or a later explicit retry.
    pub(crate) fn cancel_history_prefetch(
        &mut self,
        close: impl FnOnce(api::ResourceHandle) -> Result<(), api::Failure>,
    ) -> Result<(), api::Failure> {
        self.history_prefetch.clear();
        self.queue
            .retain(|command| !matches!(command.purpose, Purpose::HistoryPrefetch(_)));
        self.retire_running_history_prefetch(close)
    }

    /// Revoke cache authority before attempting release, including output-budget exhaustion.
    fn retire_running_history_prefetch(
        &mut self,
        close: impl FnOnce(api::ResourceHandle) -> Result<(), api::Failure>,
    ) -> Result<(), api::Failure> {
        if let Some(running) = &mut self.running
            && let Purpose::HistoryPrefetch(request) = &mut running.command.purpose
        {
            request.discarded = true;
            running.output.clear();
            running.stderr.clear();
            close(running.handle.clone())?;
            self.running = None;
        }
        Ok(())
    }

    /// Optional process output never reaches business failure/refresh handling or repaints a view.
    pub(crate) fn history_prefetch_update(
        &mut self,
        update: process::Update,
        close: impl FnOnce(api::ResourceHandle) -> Result<(), api::Failure>,
    ) {
        match update {
            process::Update::Output { stream, bytes } => {
                let Some(running) = self.running.as_mut() else {
                    return;
                };
                let Purpose::HistoryPrefetch(request) = &running.command.purpose else {
                    return;
                };
                if request.discarded {
                    return;
                }
                // Three supported text sides plus headers fit below 1 MiB; oversized blobs are
                // dropped before buffering the normal process boundary's much larger allowance.
                if running.output.len() + running.stderr.len() + bytes.len() > 1024 * 1024 {
                    let _ = self.retire_running_history_prefetch(close);
                } else if stream == process::Stream::Stdout {
                    running.output.extend(bytes);
                } else {
                    running.stderr.extend(bytes);
                }
            }
            process::Update::Exited { code } => {
                if let Some(running) = self.running.take()
                    && let Purpose::HistoryPrefetch(request) = &running.command.purpose
                {
                    self.finish_history_prefetch(request, &running.output, code);
                }
            }
            process::Update::Terminated => {
                self.running = None;
            }
        }
    }

    /// Validate the complete response against captured keys; failures are misses, never empty text.
    pub(crate) fn finish_history_prefetch(
        &mut self,
        request: &HistoryPrefetch,
        output: &[u8],
        code: u32,
    ) {
        if code != 0 || !request.current(self) {
            return;
        }
        let expressions = request
            .targets
            .iter()
            .map(|oid| format!("{oid}:{}", request.path))
            .collect::<Vec<_>>();
        let Ok(sides) = batch::decode(output, &expressions) else {
            return;
        };
        for (oid, text) in request.targets.iter().zip(sides) {
            self.history_sources
                .remember(&request.workspace, oid, &request.path, text);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
