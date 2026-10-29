//! Repository discovery, explicit network policy and immutable stash previews share one scoped state.
//! Git metadata paths are identities only: linked worktrees never widen direct workspace file access.
#[cfg(test)]
mod commit_submit_tests;
mod model;
mod pull;
mod stash;
mod views;

#[cfg(test)]
#[path = "writes_dirty_tests.rs"]
mod dirty_tests;
#[cfg(test)]
mod workbench_history_tests;

use super::*;
use model::{Divergence, Identity, PullStrategy, Stash};

/// All repository-local choices belong to the current workspace and worktree incarnation.
#[derive(Default)]
pub(super) struct Repository {
    identity: Option<Identity>,
    discovering: bool,
    preference_key: Option<api::PreferenceKey>,
    preference: api::PreferenceValue,
    remembered: Option<PullStrategy>,
    remember: bool,
    divergence: Option<Divergence>,
    pull_branch: String,
    pull_failure: String,
    dialog: Option<Dialog>,
    stashes: Vec<Stash>,
    stash_selected: Option<Stash>,
    stash_preview: String,
    stash_preview_ready: bool,
    stash_truncated: bool,
    stash_summary_only: bool,
    stash_more: bool,
    stash_cursor: usize,
    stash_offset: usize,
    create_files: Vec<Change>,
    create_status: Vec<u8>,
    create_page: usize,
}

#[derive(Clone)]
enum Dialog {
    Information,
    Pull,
    Stashes,
    StashConfirm { stash: Stash, pop: bool },
    StashCreate,
}

/// Read/write completions retain immutable targets; failed reads never become successful writes.
#[derive(Clone)]
pub(super) enum Query {
    Discover,
    Fetch,
    PullBranch {
        strategy: Option<PullStrategy>,
    },
    PullRemote {
        branch: String,
        strategy: Option<PullStrategy>,
    },
    PullFetch {
        strategy: Option<PullStrategy>,
    },
    PullUpstream {
        strategy: Option<PullStrategy>,
    },
    PullHead {
        upstream: String,
        strategy: Option<PullStrategy>,
    },
    PullCounts {
        head: String,
        upstream: String,
        strategy: Option<PullStrategy>,
    },
    PullStatus {
        snapshot: Divergence,
        strategy: Option<PullStrategy>,
    },
    PullVerifyBranch {
        snapshot: Divergence,
        strategy: Option<PullStrategy>,
    },
    PullVerifyHead {
        snapshot: Divergence,
        strategy: Option<PullStrategy>,
    },
    PullVerifyUpstream {
        snapshot: Divergence,
        strategy: Option<PullStrategy>,
    },
    PullApplied,
    StashList {
        offset: usize,
    },
    StashPreview {
        oid: String,
    },
    StashPatch {
        oid: String,
    },
    StashCreatePreview,
    StashCreateVerify {
        expected: Vec<u8>,
    },
    StashCreated,
    StashApplied {
        stash: Stash,
        drop_after: bool,
    },
    StashVerifySelector {
        stash: Stash,
        applied: bool,
    },
    StashDropped,
}

impl State {
    /// A preempted optional discovery owns no identity and may be retried on the next refresh.
    pub(super) fn cancel_repository_discovery(&mut self) {
        if self.repository.identity.is_none() {
            self.repository.discovering = false;
        }
    }

    /// Called before ordinary refresh queries. A failed discovery is retryable after `git init`.
    /// Only the successful completion schedules the first normal refresh, avoiding recursive probes.
    pub(super) fn repository_discover(&mut self) -> bool {
        if self.repository.identity.is_some() {
            return false;
        }
        if !self.repository.discovering {
            self.repository.discovering = true;
            self.queue.push_back(Command::new(
                [
                    "rev-parse",
                    "--path-format=absolute",
                    "--is-bare-repository",
                    "--show-prefix",
                    "--show-toplevel",
                    "--git-dir",
                    "--git-common-dir",
                ],
                Purpose::Repository(Query::Discover),
            ));
        }
        true
    }

    /// Other guest actions must not accidentally interpret root-relative paths in a parent repository.
    pub(super) fn repository_available(&self) -> bool {
        self.repository.identity.is_some()
    }

    /// Metadata refs in linked checkouts must name that worktree explicitly: unqualified rebase
    /// paths resolve in the common gitdir. An invalid identity keeps old proposal writes sealed.
    /// `native` selects the packaged Windows reader; unsupported storage retries this round with Git.
    pub(super) fn queue_operation_probes(&mut self, round: u64, native: bool) {
        let prefix = self
            .repository
            .identity
            .as_ref()
            .ok_or("Missing repository identity")
            .and_then(|identity| {
                conflicts::lifecycle::worktree_prefix(&identity.git_dir, &identity.common_dir)
            });
        let Ok(prefix) = prefix else {
            let _ = self.publish_conflict_observation(Some(Err(())));
            return;
        };
        if native {
            let identity = self.repository.identity.as_ref().unwrap();
            let request = operation_observation::Request {
                mode: "observe-operations".into(),
                workspace: identity.root.clone(),
                git_dir: identity.git_dir.clone(),
                common_dir: identity.common_dir.clone(),
            };
            let mut command =
                Command::new(["observe-operations"], Purpose::OperationBatch { round });
            command.service = Some("rebase".into());
            command.input = Some(serde_json::to_vec(&request).expect("bounded metadata identity"));
            self.queue.push_back(command);
        } else {
            // The packaged helper is Windows-only; other platforms retain Git's own ref resolver.
            for marker in conflicts::lifecycle::Marker::ALL {
                let reference = format!("{prefix}{}", marker.reference());
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "--quiet", &reference],
                    Purpose::OperationProbe { round, marker },
                ));
            }
        }
    }

    /// Intercept repository policy events before the old direct Git action mapping.
    /// Busy callbacks are acknowledged without clearing another command's queue.
    pub(super) fn repository_action(&mut self, event: &ui::UiEvent) -> Result<bool, api::Failure> {
        let recognized = event.node.starts_with("repo-")
            || matches!(
                event.node.as_str(),
                "fetch"
                    | "pull"
                    | "pull-merge"
                    | "pull-rebase"
                    | "stash"
                    | "stash-list"
                    | "stash-apply"
                    | "stash-pop"
                    | "stash-drop"
            );
        if matches!(event.action, ui::Action::Dismiss) && self.repository.dialog.is_some() {
            self.repository.dialog = None;
            return Ok(true);
        }
        if !recognized {
            return Ok(false);
        }
        if self.busy() {
            return Ok(true);
        }
        // Inspection remains available for invalid roots so their actionable diagnostic can be
        // read without restoring a permanent status bar. Neither action starts Git or storage I/O.
        if matches!(event.action, ui::Action::Click) {
            match event.node.as_str() {
                "repo-info" => {
                    self.repository.dialog = Some(Dialog::Information);
                    return Ok(true);
                }
                "repo-close" => {
                    self.repository.dialog = None;
                    return Ok(true);
                }
                _ => {}
            }
        }
        if !self.repository_available() {
            return Err(failure(self.label(
                "请先刷新并打开 Git 仓库根目录",
                "Refresh and open the Git repository root first",
            )));
        }
        if let ui::Action::Toggle(remember) = event.action {
            if event.node == "repo-pull-remember" {
                self.repository.remember = remember;
            }
            return Ok(true);
        }
        if !matches!(event.action, ui::Action::Click) {
            return Ok(true);
        }
        match event.node.as_str() {
            "fetch" => {
                self.repository.dialog = None;
                self.queue.push_back(Command::new(
                    ["fetch", "--prune"],
                    Purpose::Repository(Query::Fetch),
                ));
            }
            "pull" => self.repository_pull(None)?,
            "pull-merge" => self.repository_pull(Some(PullStrategy::Merge))?,
            "pull-rebase" => self.repository_pull(Some(PullStrategy::Rebase))?,
            "repo-pull-merge" => self.repository_choose_pull(PullStrategy::Merge)?,
            "repo-pull-rebase" => self.repository_choose_pull(PullStrategy::Rebase)?,
            "repo-pull-forget" => {
                self.repository.remembered = None;
                self.repository_persist()?;
            }
            "stash" => {
                self.repository.dialog = None;
                self.queue.push_back(Command::new(
                    ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                    Purpose::Repository(Query::StashCreatePreview),
                ));
            }
            "repo-stashes" | "stash-list" | "stash-apply" | "stash-pop" | "stash-drop"
            | "repo-stash-refresh" => self.repository_stashes(0),
            "repo-stash-more" => self.repository_stashes(self.repository.stash_cursor),
            "repo-stash-previous" => {
                self.repository_stashes(self.repository.stash_offset.saturating_sub(128))
            }
            "repo-stash-apply" => self.repository_stash_apply(false)?,
            "repo-stash-pop" | "repo-stash-drop" => {
                let stash = self.repository_selected_stash()?;
                self.repository.dialog = Some(Dialog::StashConfirm {
                    stash,
                    pop: event.node == "repo-stash-pop",
                });
            }
            "repo-stash-confirm" => self.repository_stash_confirm()?,
            "repo-stash-back" => self.repository.dialog = Some(Dialog::Stashes),
            "repo-stash-create-confirm" => {
                let expected = self.repository.create_status.clone();
                self.repository.dialog = None;
                self.queue.push_back(Command::new(
                    ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                    Purpose::Repository(Query::StashCreateVerify { expected }),
                ));
            }
            "repo-stash-create-next" => {
                self.repository.create_page = self.repository.create_page.saturating_add(1)
            }
            "repo-stash-create-previous" => {
                self.repository.create_page = self.repository.create_page.saturating_sub(1)
            }
            _ => {
                if let Some(index) = event
                    .node
                    .strip_prefix("repo-stash-select-")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    self.repository_select_stash(index)?;
                }
            }
        }
        Ok(true)
    }

    /// Route before the parent's generic nonzero-exit handling: ff-only rejection is a typed outcome.
    pub(super) fn repository_finished(
        &mut self,
        query: Query,
        output: Vec<u8>,
        stderr: Vec<u8>,
        code: u32,
    ) -> Result<(), api::Failure> {
        match query {
            Query::Discover => {
                self.repository.discovering = false;
                if code != 0 {
                    self.report(
                        Severity::Information,
                        self.label(
                            "当前目录不是 Git 工作区；打开仓库根目录后刷新",
                            "This directory is not a Git worktree; open its root and refresh",
                        )
                        .into(),
                    );
                    return Ok(());
                }
                let identity = Identity::parse(&output).map_err(failure)?;
                // Rediscovery can establish a different worktree; no graph/preview crosses it.
                self.history_cache = history::Cache::default();
                self.automatic_observation = false;
                let key = api::PreferenceKey {
                    file_type: String::new(),
                    name: identity.preference_name(),
                };
                self.repository.identity = Some(identity);
                self.repository.preference_key = Some(key.clone());
                match api::guest::read_preference(key, false) {
                    Ok(read) => {
                        self.repository.preference = read.value;
                        self.repository_decode_preference();
                    }
                    Err(error) => self.report(Severity::Warning, error.to_string()),
                }
                self.refresh();
            }
            Query::Fetch => {
                if code == 0 {
                    self.repository_record_fetch()?;
                    self.report(
                        Severity::Information,
                        self.label("获取完成", "Fetch completed").into(),
                    );
                    self.refresh();
                } else {
                    self.report(Severity::Error, self.repository_failure_message(&stderr));
                }
            }
            query @ (Query::PullBranch { .. }
            | Query::PullRemote { .. }
            | Query::PullFetch { .. }
            | Query::PullUpstream { .. }
            | Query::PullHead { .. }
            | Query::PullCounts { .. }
            | Query::PullStatus { .. }
            | Query::PullVerifyBranch { .. }
            | Query::PullVerifyHead { .. }
            | Query::PullVerifyUpstream { .. }
            | Query::PullApplied) => self.repository_pull_finished(query, output, stderr, code)?,
            query => self.repository_stash_finished(query, output, stderr, code)?,
        }
        Ok(())
    }

    /// Sanitize one failure cause before the caller chooses its final policy/context and severity.
    fn repository_failure_message(&self, bytes: &[u8]) -> String {
        let message = safe_diagnostic(bytes);
        if message.trim().is_empty() {
            self.label(
                "Git 操作失败；刷新后检查真实状态",
                "Git operation failed; refresh to inspect its actual state",
            )
            .into()
        } else {
            message
        }
    }

    fn repository_record_fetch(&mut self) -> Result<(), api::Failure> {
        self.last_fetch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
        self.repository_persist()
    }

    fn repository_decode_preference(&mut self) {
        let Some(identity) = &self.repository.identity else {
            return;
        };
        let Some(data) = &self.repository.preference.data else {
            return;
        };
        if data.get("schema").and_then(|value| value.as_u64()) != Some(1)
            || data.get("root").and_then(|value| value.as_str()) != Some(identity.root.as_str())
            || data.get("git_dir").and_then(|value| value.as_str())
                != Some(identity.git_dir.as_str())
        {
            return;
        }
        self.repository.remembered = data
            .get("pull")
            .and_then(|value| value.as_str())
            .and_then(PullStrategy::parse);
        self.workspace_ui.recent = data
            .get("recent_branches")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|reference| {
                reference.starts_with("refs/heads/")
                    && reference.len() <= 4096
                    && !reference.chars().any(char::is_control)
            })
            .take(20)
            .map(str::to_owned)
            .collect();
        if let Some(time) = data.get("last_fetch").and_then(|value| value.as_u64()) {
            self.last_fetch = Some(self.last_fetch.unwrap_or(0).max(time));
        }
    }

    /// Compare-and-set never overwrites another winning preference by blindly retrying.
    pub(super) fn repository_persist(&mut self) -> Result<(), api::Failure> {
        let (Some(identity), Some(key)) = (
            &self.repository.identity,
            self.repository.preference_key.clone(),
        ) else {
            return Ok(());
        };
        let data = serde_json::json!({ "schema": 1, "root": identity.root, "git_dir": identity.git_dir,
            "pull": self.repository.remembered.map(PullStrategy::name), "last_fetch": self.last_fetch,
            "recent_branches": self.workspace_ui.recent });
        match api::guest::write_preference(key.clone(), self.repository.preference.revision, data) {
            Ok(value) => self.repository.preference = value,
            Err(error) if error.code == api::ErrorCode::Conflict => {
                self.repository.preference = api::guest::read_preference(key, false)?.value;
                self.repository_decode_preference();
                self.report(
                    Severity::Warning,
                    self.label(
                        "仓库偏好已在其他窗口变化；保留较新的设置",
                        "Repository preferences changed elsewhere; the newer setting was preserved",
                    )
                    .into(),
                );
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }
}
