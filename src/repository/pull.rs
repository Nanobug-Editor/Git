//! Pull fetches only on user action, then integrates a frozen upstream object under explicit policy.
use super::*;

impl State {
    /// The configured branch remote is resolved before fetch, so a non-origin tracking setup works.
    pub(super) fn repository_pull(
        &mut self,
        strategy: Option<PullStrategy>,
    ) -> Result<(), api::Failure> {
        if self.operation.is_some() {
            return Err(failure(self.label(
                "先完成或中止当前 Git 操作",
                "Finish or abort the current Git operation first",
            )));
        }
        self.repository.dialog = None;
        self.repository.pull_failure.clear();
        self.repository.divergence = None;
        self.queue.push_back(Command::new(
            ["symbolic-ref", "--quiet", "HEAD"],
            Purpose::Repository(Query::PullBranch { strategy }),
        ));
        Ok(())
    }

    /// Remembering is optional and happens only after an explicit merge/rebase choice.
    pub(super) fn repository_choose_pull(
        &mut self,
        strategy: PullStrategy,
    ) -> Result<(), api::Failure> {
        let snapshot = self
            .repository
            .divergence
            .clone()
            .ok_or_else(|| failure("The pull preview expired; pull again"))?;
        if self.repository.remember {
            self.repository.remembered = Some(strategy);
            self.repository_persist()?;
        }
        self.repository.dialog = None;
        self.repository_prepare_integration(snapshot, Some(strategy), false)
    }

    fn repository_prepare_integration(
        &mut self,
        snapshot: Divergence,
        strategy: Option<PullStrategy>,
        confirm: bool,
    ) -> Result<(), api::Failure> {
        let name = strategy.map(PullStrategy::name).unwrap_or("ff-only");
        let mut command = Command::new(
            ["symbolic-ref", "--quiet", "HEAD"],
            Purpose::Repository(Query::PullVerifyBranch {
                snapshot: snapshot.clone(),
                strategy,
            }),
        );
        command.display = Some(format!(
            "Pull {name}\n{}\nHEAD {}\nupstream {}\n本地独有 / Local-only: {}; 上游独有 / Upstream-only: {}",
            self.repository.pull_branch,
            snapshot.head,
            snapshot.upstream,
            snapshot.ahead,
            snapshot.behind
        ));
        if confirm || !self.changes.is_empty() {
            self.set_pending_review(command, "history", !self.changes.is_empty());
        } else {
            self.queue.push_back(command);
        }
        Ok(())
    }

    /// No network operation is retried automatically. An auth/transport failure stops before integration.
    pub(super) fn repository_pull_finished(
        &mut self,
        query: Query,
        output: Vec<u8>,
        stderr: Vec<u8>,
        code: u32,
    ) -> Result<(), api::Failure> {
        if code != 0 {
            if matches!(query, Query::PullBranch { .. }) {
                self.report(Severity::Warning, self.label("Detached HEAD 不自动拉取；请切到有上游的本地分支", "Pull requires a local branch with an upstream; detached HEAD is not pulled automatically").into());
            } else if matches!(query, Query::PullRemote { .. } | Query::PullUpstream { .. }) {
                self.report(
                    Severity::Warning,
                    self.label(
                        "当前分支没有可用的上游；请先配置跟踪分支",
                        "The current branch has no usable upstream; configure tracking first",
                    )
                    .into(),
                );
            } else {
                self.report(Severity::Error, self.repository_failure_message(&stderr));
                if matches!(query, Query::PullApplied) {
                    // Git may already have entered merge/rebase conflict state. Refresh does not claim rollback.
                    self.refresh();
                }
            }
            return Ok(());
        }
        match query {
            Query::PullBranch { strategy } => {
                let branch = std::str::from_utf8(&output)
                    .map_err(|_| failure("Invalid branch identity"))?
                    .trim();
                let name = branch
                    .strip_prefix("refs/heads/")
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| failure("Pull requires a local branch"))?;
                self.repository.pull_branch = branch.into();
                self.queue.push_back(Command::new(
                    vec![
                        "config".into(),
                        "--get".into(),
                        format!("branch.{name}.remote"),
                    ],
                    Purpose::Repository(Query::PullRemote {
                        branch: branch.into(),
                        strategy,
                    }),
                ));
            }
            Query::PullRemote { branch, strategy } => {
                if branch != self.repository.pull_branch {
                    return Err(failure("The pull branch changed"));
                }
                let remote = std::str::from_utf8(&output)
                    .map_err(|_| failure("Invalid upstream remote"))?
                    .trim_end_matches(['\r', '\n']);
                if remote.is_empty()
                    || remote.len() > 4096
                    || remote.starts_with('-')
                    || remote.contains(['\0', '\r', '\n'])
                {
                    return Err(failure("Invalid upstream remote"));
                }
                self.queue.push_back(Command::new(
                    ["fetch", "--prune", "--", remote],
                    Purpose::Repository(Query::PullFetch { strategy }),
                ));
            }
            Query::PullFetch { strategy } => {
                self.repository_record_fetch()?;
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "--quiet", "@{upstream}^{commit}"],
                    Purpose::Repository(Query::PullUpstream { strategy }),
                ));
            }
            Query::PullUpstream { strategy } => {
                let upstream = model::oid(&output).map_err(failure)?;
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "HEAD"],
                    Purpose::Repository(Query::PullHead { upstream, strategy }),
                ));
            }
            Query::PullHead { upstream, strategy } => {
                let head = model::oid(&output).map_err(failure)?;
                self.queue.push_back(Command::new(
                    vec![
                        "rev-list".into(),
                        "--left-right".into(),
                        "--count".into(),
                        format!("{head}...{upstream}"),
                        "--".into(),
                    ],
                    Purpose::Repository(Query::PullCounts {
                        head,
                        upstream,
                        strategy,
                    }),
                ));
            }
            Query::PullCounts {
                head,
                upstream,
                strategy,
            } => {
                let (ahead, behind) = model::divergence(&output).map_err(failure)?;
                let snapshot = Divergence {
                    head,
                    upstream,
                    ahead,
                    behind,
                };
                self.queue.push_back(Command::new(
                    ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                    Purpose::Repository(Query::PullStatus { snapshot, strategy }),
                ));
            }
            Query::PullStatus { snapshot, strategy } => {
                self.changes = crate::model::status(&output).map_err(failure)?;
                if snapshot.ahead > 0 && snapshot.behind > 0 {
                    self.repository.divergence = Some(snapshot.clone());
                    let explicit = strategy.is_some();
                    if let Some(strategy) = strategy.or(self.repository.remembered) {
                        self.repository_prepare_integration(snapshot, Some(strategy), explicit)?;
                    } else {
                        self.repository.remember = false;
                        self.repository.dialog = Some(Dialog::Pull);
                    }
                } else {
                    self.repository_prepare_integration(snapshot, strategy, strategy.is_some())?;
                }
            }
            Query::PullVerifyBranch { snapshot, strategy } => {
                let branch = std::str::from_utf8(&output)
                    .map_err(|_| failure("Invalid branch identity"))?
                    .trim();
                if branch != self.repository.pull_branch {
                    return Err(failure(self.label(
                        "当前分支已变化；请重新拉取并预览",
                        "The current branch changed; pull and review again",
                    )));
                }
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "HEAD"],
                    Purpose::Repository(Query::PullVerifyHead { snapshot, strategy }),
                ));
            }
            Query::PullVerifyHead { snapshot, strategy } => {
                let head = model::oid(&output).map_err(failure)?;
                if head != snapshot.head {
                    // Commit-then-continue intentionally advances HEAD. Recompute the local preview
                    // and ask again for any history-changing strategy; never execute the stale plan.
                    self.report(
                        Severity::Information,
                        self.label(
                            "本地提交已变化，重新核对拉取预览",
                            "Local commits changed; the pull preview is being rebuilt",
                        )
                        .into(),
                    );
                    self.queue.push_back(Command::new(
                        vec![
                            "rev-list".into(),
                            "--left-right".into(),
                            "--count".into(),
                            format!("{head}...{}", snapshot.upstream),
                            "--".into(),
                        ],
                        Purpose::Repository(Query::PullCounts {
                            head,
                            upstream: snapshot.upstream,
                            strategy,
                        }),
                    ));
                    return Ok(());
                }
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "--quiet", "@{upstream}^{commit}"],
                    Purpose::Repository(Query::PullVerifyUpstream { snapshot, strategy }),
                ));
            }
            Query::PullVerifyUpstream { snapshot, strategy } => {
                if model::oid(&output).map_err(failure)? != snapshot.upstream {
                    return Err(failure(self.label(
                        "上游引用已变化；请重新拉取并预览",
                        "The upstream ref changed; pull and review again",
                    )));
                }
                let args = match strategy {
                    None => vec![
                        "merge",
                        "--ff-only",
                        "--no-edit",
                        "--no-autostash",
                        &snapshot.upstream,
                    ],
                    Some(PullStrategy::Merge) => vec![
                        "merge",
                        "--ff",
                        "--no-edit",
                        "--no-autostash",
                        &snapshot.upstream,
                    ],
                    Some(PullStrategy::Rebase) => vec![
                        "rebase",
                        "--no-autostash",
                        "--no-update-refs",
                        &snapshot.upstream,
                    ],
                };
                self.enqueue_checked(Command::new(args, Purpose::Repository(Query::PullApplied)))?;
            }
            Query::PullApplied => {
                self.repository.dialog = None;
                self.repository.divergence = None;
                self.report(
                    Severity::Information,
                    self.label("拉取完成", "Pull completed").into(),
                );
                self.refresh();
            }
            _ => unreachable!("non-pull query is routed by repository_finished"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Divergence {
        Divergence {
            head: "a".repeat(40),
            upstream: "b".repeat(40),
            ahead: 2,
            behind: 3,
        }
    }

    /// An auth failure must not look like divergence just because tracking refs were already stale.
    #[test]
    fn failed_manual_fetch_never_queues_integration_or_a_divergence_choice() {
        let mut state = State::default();
        state
            .repository_pull_finished(
                Query::PullFetch { strategy: None },
                Vec::new(),
                b"fatal: authentication failed".to_vec(),
                128,
            )
            .unwrap();
        assert!(state.queue.is_empty());
        assert!(state.repository.dialog.is_none());
        assert!(state.last_fetch.is_none());
    }

    #[test]
    fn divergent_default_pull_asks_and_does_not_silently_merge_or_rebase() {
        let mut state = State::default();
        state
            .repository_pull_finished(
                Query::PullStatus {
                    snapshot: snapshot(),
                    strategy: None,
                },
                Vec::new(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert!(matches!(state.repository.dialog, Some(Dialog::Pull)));
        assert!(state.queue.is_empty());
        assert!(state.pending.is_none());
    }

    #[test]
    fn fast_forward_default_and_remembered_divergence_still_revalidate_the_branch() {
        let mut state = State::default();
        let mut ff = snapshot();
        ff.ahead = 0;
        state
            .repository_pull_finished(
                Query::PullStatus {
                    snapshot: ff,
                    strategy: None,
                },
                Vec::new(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert!(matches!(
            state.queue.front().unwrap().purpose,
            Purpose::Repository(Query::PullVerifyBranch { strategy: None, .. })
        ));
        state.queue.clear();
        state.repository.remembered = Some(PullStrategy::Rebase);
        state
            .repository_pull_finished(
                Query::PullStatus {
                    snapshot: snapshot(),
                    strategy: None,
                },
                Vec::new(),
                Vec::new(),
                0,
            )
            .unwrap();
        assert!(matches!(
            state.queue.front().unwrap().purpose,
            Purpose::Repository(Query::PullVerifyBranch {
                strategy: Some(PullStrategy::Rebase),
                ..
            })
        ));
    }

    #[test]
    fn commit_then_continue_rebuilds_the_local_preview_without_fetching_again() {
        let mut state = State::default();
        let new_head = "c".repeat(40);
        state
            .repository_pull_finished(
                Query::PullVerifyHead {
                    snapshot: snapshot(),
                    strategy: Some(PullStrategy::Merge),
                },
                new_head.as_bytes().to_vec(),
                Vec::new(),
                0,
            )
            .unwrap();
        let command = state.queue.front().unwrap();
        assert_eq!(command.args[0], "rev-list");
        assert!(
            matches!(&command.purpose, Purpose::Repository(Query::PullCounts { head, .. }) if *head == new_head)
        );
        assert!(state.pending.is_none());
    }
}
