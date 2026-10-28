//! Branch menus bind immutable ref targets; input dialogs never reuse a global editable target row.
use super::*;

/// Every input to branch-node presentation, independent of history filters and tree membership.
#[derive(PartialEq, Eq)]
pub(super) struct MenuStamp {
    /// Business/modal ownership disables writes while the cached tree remains readable.
    busy: bool,
    /// Cancellation is offered only when an owned operation can actually be stopped.
    cancellable: bool,
    /// Repository availability gates operations, but never the inspect/refresh entry points.
    available: bool,
    /// Stash requires actual worktree changes even when no ref has moved.
    changed: bool,
    /// Pull/push and current-branch descriptions depend on the checked-out full ref.
    current: String,
    /// Directory labels and descriptions follow the current native locale.
    locale: String,
}

/// One explicit name/revision/lease input retains the branch captured by the context invocation.
pub(super) struct Prompt {
    /// Existing operation identity, selected from the implemented contextual actions.
    pub action: String,
    /// Full observed ref, independently of the selected history filter.
    pub source: String,
    /// Native input mirrors only this dialog's value; Git validates its literal argv later.
    pub input: String,
}

/// Menu order is also the native keyboard order; separators divide repository, branch and write actions.
const ACTIONS: &[(&str, &str, &str, bool)] = &[
    ("refresh", "刷新", "Refresh", false),
    ("all", "显示全部分支提交", "Show all refs", false),
    ("more", "更多提交", "More commits", false),
    (
        "repo-info",
        "仓库信息与操作结果…",
        "Repository info and results…",
        false,
    ),
    ("fetch", "获取远程信息", "Fetch", true),
    ("pull", "拉取当前分支", "Pull current branch", false),
    ("push", "推送当前分支…", "Push current branch…", false),
    ("switch", "切换到此分支…", "Switch to this branch…", true),
    (
        "branch",
        "从此处分出新分支…",
        "New branch from here…",
        false,
    ),
    (
        "merge",
        "合并到当前分支…",
        "Merge into current branch…",
        true,
    ),
    (
        "merge-no-ff",
        "创建合并提交…",
        "Merge with no fast-forward…",
        false,
    ),
    ("merge-squash", "压缩合并…", "Squash merge…", false),
    (
        "rebase",
        "将当前分支变基到此处…",
        "Rebase current branch onto here…",
        false,
    ),
    (
        "rebase-plan",
        "以此处为基点整理提交…",
        "Interactive rebase from here…",
        false,
    ),
    (
        "cherry-pick",
        "拣选此分支顶端提交…",
        "Cherry-pick branch tip…",
        false,
    ),
    (
        "reset-soft",
        "重置当前分支到此处（soft）…",
        "Reset current branch here (soft)…",
        true,
    ),
    (
        "reset-mixed",
        "重置当前分支到此处（mixed）…",
        "Reset current branch here (mixed)…",
        false,
    ),
    (
        "reset-hard",
        "重置当前分支到此处（hard）…",
        "Reset current branch here (hard)…",
        false,
    ),
    (
        "stash",
        "存储当前工作区修改…",
        "Stash worktree changes…",
        true,
    ),
    ("stash-list", "Stash 列表…", "Stash list…", false),
    (
        "stash-apply",
        "选择 Stash 并应用…",
        "Choose stash to apply…",
        false,
    ),
    (
        "stash-pop",
        "选择 Stash 并弹出…",
        "Choose stash to pop…",
        false,
    ),
    (
        "stash-drop",
        "选择 Stash 并删除…",
        "Choose stash to delete…",
        false,
    ),
    (
        "pull-merge",
        "拉取当前分支并合并…",
        "Pull current branch with merge…",
        true,
    ),
    (
        "pull-rebase",
        "拉取当前分支并变基…",
        "Pull current branch with rebase…",
        false,
    ),
    (
        "push-lease",
        "按远端 OID 安全强推当前分支…",
        "Push current branch with exact lease…",
        false,
    ),
    (
        "cancel-process",
        "停止当前操作",
        "Stop current operation",
        true,
    ),
    ("branch-rename", "重命名…", "Rename…", true),
    ("branch-delete", "删除…", "Delete…", false),
    (
        "checkout-remote",
        "签出为本地跟踪分支…",
        "Checkout as a local tracking branch…",
        false,
    ),
];

impl State {
    /// Sample menu inputs once per publication; source data has its own refresh authority.
    pub(super) fn branch_menu_stamp(&self) -> MenuStamp {
        MenuStamp {
            busy: self.ui_busy()
                || self.pending.is_some()
                || self.branch_prompt.is_some()
                || self.workspace_ui.comparison.is_some(),
            cancellable: self.ui_busy() && (self.running.is_some() || self.write_intent.is_some()),
            available: self.repository_available(),
            changed: !self.changes.is_empty(),
            current: self.current_ref.clone(),
            locale: self.environment.locale.clone(),
        }
    }

    /// Update published branch-node presentation in bounded batches. Collapsed/uncached nodes
    /// are ignored by the host; future pages are built from the same current policy.
    pub(super) fn update_branch_nodes(&self) -> Result<(), api::Failure> {
        let Some(handle) = self.trees.get("branches") else {
            return Ok(());
        };
        let mut batch = Vec::new();
        let mut bytes = 0;
        // Reserve space for the envelope/array separators within the public metadata budget.
        const BUDGET: usize = 512 * 1024 - 4096;
        for (key, path) in &self.tree_paths {
            let Some(id) = key.strip_prefix("branches:") else {
                continue;
            };
            let node = self.provider_node("branches", id, path);
            let size = serde_json::to_vec(&node)
                .map_err(|_| failure("Invalid branch metadata"))?
                .len();
            if size > BUDGET {
                return Err(api::Failure::new(
                    api::ErrorCode::LimitExceeded,
                    "Branch metadata exceeds its update budget",
                ));
            }
            if batch.len() >= tree::MAX_PAGE as usize || bytes + size > BUDGET {
                tree::update(handle.clone(), std::mem::take(&mut batch))?;
                bytes = 0;
            }
            batch.push(node);
            bytes += size;
        }
        if !batch.is_empty() {
            tree::update(handle.clone(), batch)?;
        }
        Ok(())
    }

    /// Produce only implemented actions, with applicability checked again when the typed command arrives.
    pub(super) fn branch_menu(&self, reference: &str) -> Vec<tree::Command> {
        let known = self.refs.iter().any(|current| current == reference);
        let current = known && reference == self.current_ref;
        let local = known && reference.starts_with("refs/heads/");
        let busy = self.ui_busy()
            || self.pending.is_some()
            || self.branch_prompt.is_some()
            || self.workspace_ui.comparison.is_some();
        ACTIONS
            .iter()
            .map(|(action, zh, en, separator)| {
                let applicable = match *action {
                    "refresh" | "all" | "more" | "repo-info" | "fetch" | "stash-list"
                    | "stash-apply" | "stash-pop" | "stash-drop" => true,
                    "cancel-process" => {
                        self.ui_busy() && (self.running.is_some() || self.write_intent.is_some())
                    }
                    "pull" | "push" | "pull-merge" | "pull-rebase" | "push-lease" => current,
                    "switch" => local && !current,
                    "checkout-remote" => known && reference.starts_with("refs/remotes/"),
                    "branch-rename" => local,
                    "branch-delete" => local && !current,
                    "branch" | "cherry-pick" | "reset-soft" | "reset-mixed" | "reset-hard" => known,
                    "merge" | "merge-no-ff" | "merge-squash" | "rebase" | "rebase-plan" => {
                        known && !self.current_ref.is_empty()
                    }
                    "stash" => !self.changes.is_empty(),
                    _ => false,
                };
                tree::Command {
                    command: "branch-context".into(),
                    label: ui::LocalizedText::new(*en, *zh),
                    arguments: serde_json::json!({"action": action, "target": reference}),
                    disabled: !applicable
                        || (*action != "cancel-process" && busy)
                        || (!matches!(*action, "refresh" | "repo-info")
                            && !self.repository_available()),
                    separator_before: *separator,
                }
            })
            .collect()
    }

    /// Resolve a stable tree path to an observed full ref; directories carry no implicit branch target.
    pub(super) fn branch_reference(&self, path: &str) -> String {
        path.strip_prefix("local/")
            .map(|tail| format!("refs/heads/{tail}"))
            .or_else(|| {
                path.strip_prefix("remote/")
                    .map(|tail| format!("refs/remotes/{tail}"))
            })
            .filter(|reference| self.refs.contains(reference))
            .unwrap_or_default()
    }

    /// Recheck live policy before reusing ordinary operation previews and native-buffer write guards.
    pub(super) fn branch_context(
        &mut self,
        arguments: &serde_json::Value,
    ) -> Result<(), api::Failure> {
        let action = arguments["action"].as_str().unwrap_or_default();
        self.cancel_history_reads()?;
        let target = arguments["target"].as_str().unwrap_or_default();
        if matches!(action, "compare-current" | "compare-worktree") {
            return self.start_branch_comparison(target, action == "compare-current");
        }
        if !self
            .branch_menu(target)
            .iter()
            .any(|item| item.arguments["action"] == action && !item.disabled)
        {
            return Err(failure(
                "该操作当前不可用 / This action is currently unavailable",
            ));
        }
        if matches!(
            action,
            "branch"
                | "branch-rename"
                | "checkout-remote"
                | "push-lease"
                | "merge"
                | "merge-no-ff"
                | "merge-squash"
                | "rebase"
                | "rebase-plan"
                | "cherry-pick"
                | "reset-soft"
                | "reset-mixed"
                | "reset-hard"
        ) {
            self.reveal_workspace_history()?;
            self.branch_prompt = Some(Prompt {
                action: action.into(),
                source: target.into(),
                input: match action {
                    "push-lease" => format!("origin {target}"),
                    "branch" => String::new(),
                    "branch-rename" => target.strip_prefix("refs/heads/").unwrap_or(target).into(),
                    "checkout-remote" => target
                        .strip_prefix("refs/remotes/")
                        .and_then(|tail| tail.split_once('/'))
                        .map_or("", |(_, name)| name)
                        .into(),
                    _ => target.into(),
                },
            });
            *self.input_epochs.entry("branch-name".into()).or_default() += 1;
            return Ok(());
        }
        // git switch expects a local short name; other revision consumers receive the unambiguous ref.
        self.argument = if action == "switch" {
            target.strip_prefix("refs/heads/").unwrap_or(target).into()
        } else {
            target.into()
        };
        if !matches!(action, "refresh" | "all" | "more" | "repo-info") {
            self.reveal_workspace_history()?;
        }
        self.ui_event(
            None,
            ui::UiEvent {
                revision: self.revision,
                node: action.into(),
                action: ui::Action::Click,
            },
        )
    }

    /// Consume only this prompt's native controls; cancellation releases its captured target.
    pub(super) fn branch_prompt_event(
        &mut self,
        event: &ui::UiEvent,
    ) -> Result<bool, api::Failure> {
        if self.branch_prompt.is_none() {
            return Ok(false);
        }
        match (&*event.node, &event.action) {
            ("branch-name", ui::Action::Change(input)) => {
                self.branch_prompt.as_mut().unwrap().input = input.clone()
            }
            ("branch-cancel", ui::Action::Click) | (_, ui::Action::Dismiss) => {
                self.branch_prompt = None
            }
            ("branch-create", ui::Action::Click) => {
                let prompt = self.branch_prompt.as_ref().unwrap();
                if self.busy() || prompt.input.trim().is_empty() {
                    return Ok(true);
                }
                if !self.refs.contains(&prompt.source) {
                    return Err(failure("来源分支已失效 / Source branch expired"));
                }
                let action = prompt.action.clone();
                self.argument = prompt.input.trim().into();
                // The ordinary dispatcher reads the source and keeps all write-preview policy.
                // Its action name differs from the prompt controls, so it cannot re-enter this arm.
                self.ui_event(
                    Some("history"),
                    ui::UiEvent {
                        revision: self.revision,
                        node: action,
                        action: ui::Action::Click,
                    },
                )?;
                self.branch_prompt = None;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Parameter input appears only after an explicit contextual action, with the source visible.
    pub(super) fn branch_dialog(&self) -> Option<ui::Dialog> {
        let prompt = self.branch_prompt.as_ref()?;
        let create = matches!(
            prompt.action.as_str(),
            "branch" | "branch-rename" | "checkout-remote"
        );
        let lease = prompt.action == "push-lease";
        let title = ACTIONS
            .iter()
            .find(|(action, ..)| *action == prompt.action)
            .map(|(_, zh, en, _)| self.label(zh, en))
            .unwrap_or_default();
        Some(ui::Dialog {
            id: "branch-dialog".into(),
            width: 680.,
            title: title.into(),
            content: ui::Node::column(
                "branch-dialog-content",
                vec![
                    ui::Node::text(
                        "branch-source",
                        format!(
                            "{}: {}",
                            self.label("所选分支", "Selected branch"),
                            prompt.source
                        ),
                    ),
                    self.input(
                        "branch-name",
                        &prompt.input,
                        self.label(
                            if create {
                                "新分支名称"
                            } else if lease {
                                "远程名 refs/heads/分支名"
                            } else {
                                "目标分支 / 提交（多个提交按执行顺序输入）"
                            },
                            if create {
                                "New branch name"
                            } else if lease {
                                "remote refs/heads/branch"
                            } else {
                                "Branch / commit (multiple commits in execution order)"
                            },
                        ),
                        false,
                    ),
                    ui::Node::row(
                        "branch-prompt-actions",
                        vec![
                            self.button(
                                "branch-create",
                                if create {
                                    "创建"
                                } else if lease {
                                    "检查远端并预览…"
                                } else {
                                    "预览操作…"
                                },
                                if create {
                                    "Create"
                                } else if lease {
                                    "Inspect remote and review…"
                                } else {
                                    "Review operation…"
                                },
                            )
                            .disabled(self.ui_busy() || prompt.input.trim().is_empty()),
                            self.button("branch-cancel", "取消", "Cancel"),
                        ],
                    ),
                ],
            ),
        })
    }
}
