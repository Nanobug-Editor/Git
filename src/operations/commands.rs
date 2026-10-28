//! Literal Git action construction; UI routing and confirmation state live in the parent module.
use super::*;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
impl State {
    /// A handled read/UI action returns None; only an explicit write reaches confirmation/queue policy.
    pub(super) fn command_for_action(
        &mut self,
        node: &str,
        panel: Option<&str>,
    ) -> Result<Option<Command>, api::Failure> {
        let target = self.argument.trim().to_owned();
        let target_arg = || {
            if target.is_empty() || target.starts_with('-') || target.contains(['\0', '\n', '\r']) {
                Err(failure(
                    "请输入明确的分支或提交 / Expected an explicit branch or commit",
                ))
            } else {
                Ok(target.clone())
            }
        };
        let command = match node {
            "refresh" => {
                self.request_refresh();
                return Ok(None);
            }
            "refresh-hunks" => {
                self.require_path()?;
                self.queue.push_back(Command::new(
                    self.patch_args(false),
                    Purpose::Patch { staged: false },
                ));
                return Ok(None);
            }
            "diff" | "diff-index" => {
                self.require_path()?;
                let path = self.selected_path.clone();
                self.request_comparison(panel_activity::ComparisonSelection::Worktree {
                    path,
                    staged: node == "diff-index",
                })?;
                return Ok(None);
            }
            "all" => {
                self.select_history_filter(None)?;
                return Ok(None);
            }
            "more" => {
                self.offset = if self.history_cache.owns_page(&self.filter) {
                    self.commits.len()
                } else {
                    0
                };
                self.queue_log();
                return Ok(None);
            }
            "stage" => {
                self.require_path()?;
                Command::new(
                    vec!["add".into(), "--".into(), self.selected_path.clone()],
                    Purpose::Action,
                )
            }
            "resolve-file" => {
                self.require_path()?;
                let path = self.selected_path.clone();
                self.open_conflict(&path)?;
                return Ok(None);
            }
            "unstage" => {
                self.require_path()?;
                // The empty tree has no HEAD to restore from. Removing only the cached entry
                // preserves the first file's worktree bytes and leaves it available for restaging.
                let mut args = if self.head.is_some() {
                    vec!["restore".into(), "--staged".into(), "--".into()]
                } else {
                    vec!["rm".into(), "--cached".into(), "-f".into(), "--".into()]
                };
                args.push(self.selected_path.clone());
                if let Some(original) = self
                    .changes
                    .iter()
                    .find(|change| change.path == self.selected_path)
                    .and_then(|change| change.original.clone())
                {
                    args.push(original);
                }
                Command::new(args, Purpose::Action)
            }
            "discard" => {
                self.require_path()?;
                if self
                    .changes
                    .iter()
                    .any(|change| change.path == self.selected_path && change.worktree == '?')
                {
                    Command::new(["clean", "-f", "--", &self.selected_path], Purpose::Action)
                } else {
                    Command::new(
                        vec![
                            "restore".into(),
                            "--worktree".into(),
                            "--".into(),
                            self.selected_path.clone(),
                        ],
                        Purpose::Action,
                    )
                }
            }
            "fetch" => Command::new(["fetch", "--prune"], Purpose::Action),
            "pull" => Command::new(["pull", "--ff-only"], Purpose::Action),
            "pull-merge" => Command::new(["pull", "--no-rebase"], Purpose::Action),
            "pull-rebase" => Command::new(["pull", "--rebase"], Purpose::Action),
            "push" => Command::new(["push"], Purpose::Action),
            "push-lease" => {
                let parts = target.split_whitespace().collect::<Vec<_>>();
                if parts.len() != 2
                    || !lease_remote_name(parts[0])
                    || !parts[1].starts_with("refs/heads/")
                    || parts.iter().any(|value| value.contains(['\0', '\n', '\r']))
                {
                    return Err(failure(
                        "请输入：远程名 refs/heads/分支名 / Enter: remote refs/heads/branch",
                    ));
                }
                self.queue.push_back(Command::new(
                    ["rev-parse", "--verify", "HEAD"],
                    Purpose::LeaseSource {
                        remote: parts[0].into(),
                        reference: parts[1].into(),
                    },
                ));
                return Ok(None);
            }
            "switch" => Command::new(vec!["switch".into(), target_arg()?], Purpose::Action),
            "checkout-remote" => {
                let source = self
                    .branch_prompt
                    .as_ref()
                    .map(|prompt| prompt.source.as_str())
                    .filter(|source| {
                        source.starts_with("refs/remotes/")
                            && self.refs.iter().any(|reference| reference == source)
                    })
                    .ok_or_else(|| {
                        failure("远程跟踪分支已失效 / Remote-tracking branch expired")
                    })?;
                let oid = self
                    .ref_tips
                    .get(source)
                    .cloned()
                    .filter(|oid| !oid.is_empty())
                    .ok_or_else(|| failure("分支提交已失效 / Branch tip expired"))?;
                let effect = Command::new(
                    vec![
                        "switch".into(),
                        "--track".into(),
                        "-c".into(),
                        target_arg()?,
                        source.into(),
                    ],
                    Purpose::Action,
                );
                Command::new(
                    vec![
                        String::from("for-each-ref"),
                        "--format=%(objectname)".into(),
                        source.to_owned(),
                    ],
                    Purpose::Guard {
                        expected: format!("{oid}\n").into_bytes(),
                        next: Box::new(effect),
                    },
                )
            }
            "branch" => {
                let mut args = vec!["branch".into(), "--".into(), target_arg()?];
                if let Some(prompt) = &self.branch_prompt {
                    args.push(prompt.source.clone());
                }
                Command::new(args, Purpose::Action)
            }
            "branch-rename" | "branch-delete" => {
                let source = if node == "branch-rename" {
                    self.branch_prompt
                        .as_ref()
                        .map(|prompt| prompt.source.as_str())
                        .unwrap_or_default()
                } else {
                    target.as_str()
                };
                let short = source
                    .strip_prefix("refs/heads/")
                    .filter(|_| self.refs.iter().any(|reference| reference == source))
                    .ok_or_else(|| failure("本地分支已失效 / Local branch expired"))?;
                if node == "branch-delete" && source == self.current_ref {
                    return Err(failure(
                        "不能删除当前分支 / Cannot delete the current branch",
                    ));
                }
                let oid = self
                    .ref_tips
                    .get(source)
                    .cloned()
                    .filter(|oid| !oid.is_empty())
                    .ok_or_else(|| failure("分支提交已失效 / Branch tip expired"))?;
                let args = if node == "branch-rename" {
                    vec![
                        "branch".into(),
                        "-m".into(),
                        "--".into(),
                        short.into(),
                        target_arg()?,
                    ]
                } else {
                    vec!["branch".into(), "-d".into(), "--".into(), short.into()]
                };
                let mut effect = Command::new(args, Purpose::Action);
                effect.display = Some(format!(
                    "{}\n{}\n{}",
                    if node == "branch-rename" {
                        self.label("重命名本地分支", "Rename local branch")
                    } else {
                        self.label(
                            "删除本地分支（拒绝未合并提交）",
                            "Delete local branch (reject unmerged commits)",
                        )
                    },
                    source,
                    effect.args.join(" ")
                ));
                Command::new(
                    vec![
                        String::from("for-each-ref"),
                        "--format=%(objectname)".into(),
                        source.to_owned(),
                    ],
                    Purpose::Guard {
                        expected: format!("{oid}\n").into_bytes(),
                        next: Box::new(effect),
                    },
                )
            }
            "merge" => Command::new(
                vec![
                    "merge".into(),
                    "--no-edit".into(),
                    "--".into(),
                    target_arg()?,
                ],
                Purpose::Action,
            ),
            "merge-no-ff" => Command::new(
                vec![
                    "merge".into(),
                    "--no-ff".into(),
                    "--no-edit".into(),
                    "--".into(),
                    target_arg()?,
                ],
                Purpose::Action,
            ),
            "merge-squash" => Command::new(
                vec![
                    "merge".into(),
                    "--squash".into(),
                    "--".into(),
                    target_arg()?,
                ],
                Purpose::Action,
            ),
            "rebase" => Command::new(vec!["rebase".into(), target_arg()?], Purpose::Action),
            "rebase-plan" => {
                let target = target_arg()?;
                self.queue.push_back(Command::new(
                    vec![
                        "rev-parse".into(),
                        "--verify".into(),
                        format!("{target}^{{commit}}"),
                    ],
                    Purpose::RebaseBase,
                ));
                return Ok(None);
            }
            "stash" => Command::new(["stash", "push", "--include-untracked"], Purpose::Action),
            "stash-apply" => Command::new(
                vec![
                    "stash".into(),
                    "apply".into(),
                    if target.is_empty() {
                        "stash@{0}".into()
                    } else {
                        target_arg()?
                    },
                ],
                Purpose::Action,
            ),
            "stash-pop" => Command::new(
                vec![
                    "stash".into(),
                    "pop".into(),
                    if target.is_empty() {
                        "stash@{0}".into()
                    } else {
                        target_arg()?
                    },
                ],
                Purpose::Action,
            ),
            "stash-drop" => Command::new(
                vec![
                    "stash".into(),
                    "drop".into(),
                    if target.is_empty() {
                        "stash@{0}".into()
                    } else {
                        target_arg()?
                    },
                ],
                Purpose::Action,
            ),
            "cherry-pick" => {
                let commits = if target.is_empty() {
                    self.selected_commits.clone()
                } else {
                    target.split_whitespace().map(str::to_owned).collect()
                };
                if commits.is_empty()
                    || commits.len() > 128
                    || commits
                        .iter()
                        .any(|value| value.starts_with('-') || value.contains(['\0', '\n', '\r']))
                {
                    return Err(failure(
                        "Select commits in order, or enter at most 128 explicit revisions",
                    ));
                }
                Command::new(
                    [vec!["cherry-pick".into(), "--".into()], commits].concat(),
                    Purpose::Action,
                )
            }
            "reset-mixed" => Command::new(
                vec!["reset".into(), "--mixed".into(), target_arg()?],
                Purpose::Action,
            ),
            "reset-soft" => Command::new(
                vec!["reset".into(), "--soft".into(), target_arg()?],
                Purpose::Action,
            ),
            "reset-hard" => Command::new(
                vec!["reset".into(), "--hard".into(), target_arg()?],
                Purpose::Action,
            ),
            "resolved" if panel == Some("conflicts") => {
                let path = self.merge_selected.clone();
                let file = self
                    .merge_files
                    .get(&path)
                    .ok_or_else(|| failure("No result document"))?;
                if !file.read_valid
                    || file.read_inflight
                    || !file.all_reviewed()
                    || file.has_markers
                {
                    return Err(failure(
                        "先处理全部冲突块 / Resolve every conflict block first",
                    ));
                }
                let document = file
                    .document
                    .clone()
                    .ok_or_else(|| failure("No result document"))?;
                self.editor(
                    api::EditorOperation::SaveDocument { document },
                    &format!("save-resolve:{path}"),
                )?;
                return Ok(None);
            }
            "continue" => {
                if self.changes.iter().any(|c| c.conflict) {
                    return Err(failure("尚有未解决文件 / Unresolved files remain"));
                }
                if self.operation.as_deref() == Some("rebase")
                    && let Some(plan) = &self.interactive_plan
                {
                    let command = self.rebase_command(plan, "continue")?;
                    self.enqueue_checked(command)?;
                    return Ok(None);
                }
                Command::new(
                    [
                        self.operation
                            .as_deref()
                            .ok_or_else(|| failure("No ongoing operation"))?,
                        "--continue",
                    ],
                    Purpose::Action,
                )
            }
            "skip"
                if self.operation.as_deref() == Some("rebase")
                    && self.interactive_plan.is_some() =>
            {
                self.rebase_command(self.interactive_plan.as_ref().unwrap(), "skip")?
            }
            "skip" => Command::new(
                [
                    self.operation
                        .as_deref()
                        .filter(|op| *op != "merge")
                        .ok_or_else(|| failure("Cannot skip this operation"))?,
                    "--skip",
                ],
                Purpose::Action,
            ),
            "abort" => Command::new(
                [
                    self.operation
                        .as_deref()
                        .ok_or_else(|| failure("No ongoing operation"))?,
                    "--abort",
                ],
                Purpose::Action,
            ),
            _ => return Ok(None),
        };

        Ok(Some(command))
    }
}

/// Force-lease review accepts a remote alias, never a URL/path that may embed credentials in UI.
/// Keeping the alphabet explicit also prevents scp-style user@host:path targets from entering logs.
fn lease_remote_name(value: &str) -> bool {
    value.chars().next().is_some_and(char::is_alphanumeric)
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

#[cfg(test)]
mod lease_remote_tests {
    use super::*;

    #[test]
    fn force_lease_refuses_credential_urls_before_queueing_any_query() {
        for remote in [
            "https://user:secret@example.com/repo",
            "git@host:repo",
            "../repo",
            "C:\\repo",
            "-remote",
            "user@host",
        ] {
            let mut state = State::default();
            state.argument = format!("{remote} refs/heads/main");
            assert!(state.command_for_action("push-lease", None).is_err());
            assert!(state.queue.is_empty());
        }
        for remote in ["origin", "upstream-2", "team.remote", "中文远程"] {
            assert!(lease_remote_name(remote));
        }
    }
}
