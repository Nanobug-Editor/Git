//! Bounded native dialogs expose repository policy without treating Git text as executable input.
use super::*;

impl State {
    /// Observed identity and remembered policy are readonly details, not permanent history rows.
    /// Clearing a remembered choice continues through the same explicit repository action.
    fn repository_information_dialog(&self) -> ui::Dialog {
        let repository = &self.repository;
        // Local observations never advance this remote-only timestamp; results remain inspectable
        // on demand, including startup errors for a directory without a supported repository.
        let timestamp = self
            .last_fetch
            .map(model::utc)
            .unwrap_or_else(|| self.label("尚未获取", "Not fetched yet").into());
        let identity = match &repository.identity {
            Some(identity) => format!(
                "{}{}",
                identity.root,
                if identity.linked() {
                    " · linked worktree"
                } else {
                    ""
                }
            ),
            None => self
                .label(
                    "未发现可用的 Git 工作区根目录",
                    "No supported Git worktree root discovered",
                )
                .into(),
        };
        let strategy = match repository.remembered {
            None => self.label(
                "拉取：仅快进；分叉时询问",
                "Pull: fast-forward only; ask on divergence",
            ),
            Some(PullStrategy::Merge) => self.label(
                "已记住此仓库分叉时合并",
                "Remembered: merge when this repository diverges",
            ),
            Some(PullStrategy::Rebase) => self.label(
                "已记住此仓库分叉时变基",
                "Remembered: rebase when this repository diverges",
            ),
        };
        let mut actions = vec![self.button("repo-close", "关闭", "Close")];
        if repository.remembered.is_some() {
            actions.push(self.button("repo-pull-forget", "清除拉取偏好", "Clear pull preference"));
        }
        ui::Dialog {
            id: "repo-info-dialog".into(),
            title: self.label("仓库信息", "Repository information").into(),
            width: 760.,
            content: ui::Node::column(
                "repo-info-content",
                vec![
                    ui::Node::text("repo-identity", identity),
                    ui::Node::text("repo-policy", strategy),
                    ui::Node::text(
                        "repo-fetch-time",
                        format!(
                            "{}: {timestamp}",
                            self.label("上次获取远程信息", "Last remote fetch")
                        ),
                    ),
                    ui::Node::text("status", &self.message),
                    ui::Node::row("repo-actions", actions).wrap(),
                ],
            ),
        }
    }

    pub(crate) fn repository_dialog(&self) -> Option<ui::Dialog> {
        match self.repository.dialog.as_ref()? {
            Dialog::Information => Some(self.repository_information_dialog()),
            Dialog::Pull => self.repository_pull_dialog(),
            Dialog::Stashes => Some(self.repository_stash_dialog()),
            Dialog::StashConfirm { stash, pop } => {
                Some(self.repository_stash_confirmation(stash, *pop))
            }
            Dialog::StashCreate => Some(self.repository_create_dialog()),
        }
    }

    fn repository_pull_dialog(&self) -> Option<ui::Dialog> {
        let snapshot = self.repository.divergence.as_ref()?;
        Some(ui::Dialog {
            id: "repo-pull-choice".into(), title: self.label("分支已分叉", "Branches diverged").into(), width: 760.,
            content: ui::Node::column("repo-pull-content", vec![
                ui::Node::text("repo-pull-source", format!("{}\nHEAD {}\nupstream {}", self.repository.pull_branch, snapshot.head, snapshot.upstream)),
                ui::Node::text("repo-pull-counts", format!("{}: {}\n{}: {}", self.label("本地独有提交", "Local-only commits"), snapshot.ahead, self.label("上游独有提交", "Upstream-only commits"), snapshot.behind)),
                ui::Node::text("repo-pull-explanation", self.label("已获取远程信息。仅快进无法整合这些提交；选择此次如何整合。", "Remote refs were fetched. Fast-forward cannot integrate these commits; choose how to proceed.")),
                ui::Node::checkbox("repo-pull-remember", self.label("记住此仓库分叉时的选择", "Remember this repository's divergence choice"), self.repository.remember),
                ui::Node::row("repo-pull-actions", vec![self.button("repo-pull-merge", "合并", "Merge"), self.button("repo-pull-rebase", "变基", "Rebase"), self.button("repo-close", "取消", "Cancel")]).wrap(),
            ]),
        })
    }

    fn repository_stash_dialog(&self) -> ui::Dialog {
        let repository = &self.repository;
        let rows = repository
            .stashes
            .iter()
            .enumerate()
            .map(|(index, stash)| {
                let selected = repository.stash_selected.as_ref().is_some_and(|selected| {
                    selected.oid == stash.oid && selected.selector == stash.selector
                });
                let title = short(&stash.title, 180);
                self.button(
                    &format!("repo-stash-select-{index}"),
                    &format!(
                        "{}{} {title}",
                        if selected { "● " } else { "" },
                        stash.selector
                    ),
                    &format!(
                        "{}{} {title}",
                        if selected { "● " } else { "" },
                        stash.selector
                    ),
                )
            })
            .collect::<Vec<_>>();
        let list = if rows.is_empty() {
            ui::Node::text(
                "repo-stash-empty",
                self.label("没有 Stash 记录", "There are no stash entries"),
            )
        } else {
            ui::Node::scroll(
                "repo-stash-list-scroll",
                ui::Node::column("repo-stash-list", rows),
            )
            .height(350.)
        };
        let selected = repository
            .stash_selected
            .as_ref()
            .map(|stash| {
                format!(
                    "{}\n{}\n{}",
                    stash.selector,
                    stash.oid,
                    short(&stash.time, 120)
                )
            })
            .unwrap_or_default();
        let can_apply = repository.stash_selected.is_some()
            && repository.stash_preview_ready
            && !self.ui_busy();
        let mut controls = vec![self.button("repo-stash-refresh", "刷新列表", "Refresh list")];
        if repository.stash_offset > 0 {
            controls.push(self.button("repo-stash-previous", "上一页", "Previous page"));
        }
        if repository.stash_more {
            controls.push(self.button("repo-stash-more", "下一页", "Next page"));
        }
        controls.extend([
            self.button("repo-stash-apply", "应用并保留", "Apply and keep")
                .disabled(!can_apply),
            self.button("repo-stash-pop", "应用并删除…", "Apply and delete…")
                .disabled(!can_apply),
            self.button("repo-stash-drop", "仅删除记录…", "Delete entry…")
                .disabled(!can_apply),
            self.button("repo-close", "关闭", "Close"),
        ]);
        let mut preview = vec![ui::Node::text("repo-stash-selected", selected)];
        if repository.stash_preview_ready {
            preview.push(preview_node(
                "repo-stash-preview",
                &repository.stash_preview,
            ));
        } else {
            preview.push(ui::Node::text(
                "repo-stash-preview-pending",
                self.label(
                    "选择记录以读取完整对象的变更预览",
                    "Select an entry to preview the immutable object's changes",
                ),
            ));
        }
        if repository.stash_summary_only {
            preview.push(ui::Node::text("repo-stash-preview-summary", self.label("显示文件变更概要；文本差异尚未完成或超过限制。应用使用完整原始对象。", "File-change summary is shown; the text patch is pending or exceeds limits. Applying uses the complete original object.")));
        } else if repository.stash_truncated {
            preview.push(ui::Node::text(
                "repo-stash-preview-limited",
                self.label(
                    "预览只显示前 48 KiB；应用使用完整原始对象。",
                    "Preview shows the first 48 KiB; applying uses the complete original object.",
                ),
            ));
        }
        ui::Dialog {
            id: "repo-stash-window".into(),
            title: self.label("Stash 记录与变更", "Stashes and changes").into(),
            width: 1080.,
            content: ui::Node::column(
                "repo-stash-content",
                vec![
                    ui::Node::row(
                        "repo-stash-body",
                        vec![
                            list.width(310.),
                            ui::Node::column("repo-stash-details", preview).grow(),
                        ],
                    )
                    .resizable(),
                    ui::Node::row("repo-stash-controls", controls).wrap(),
                    ui::Node::text("repo-stash-message", &self.message),
                ],
            ),
        }
    }

    fn repository_stash_confirmation(&self, stash: &Stash, pop: bool) -> ui::Dialog {
        ui::Dialog {
            id: "repo-stash-confirmation".into(),
            title: self
                .label("确认 Stash 操作", "Confirm stash operation")
                .into(),
            width: 760.,
            content: ui::Node::column(
                "repo-stash-confirm-body",
                vec![
                    ui::Node::text(
                        "repo-stash-confirm-identity",
                        format!(
                            "{}\n{}\n{}",
                            stash.selector,
                            stash.oid,
                            short(&stash.title, 512)
                        ),
                    ),
                    ui::Node::text(
                        "repo-stash-confirm-impact",
                        if pop {
                            self.label("先应用此记录；仅在应用成功且记录身份未变化后删除。应用冲突时保留记录。", "Apply this entry, then delete it only after success and an identity check. Conflicts retain the entry.")
                        } else {
                            self.label("删除此 Stash 记录，不应用内容。请确认这些修改已不再需要。", "Delete this stash entry without applying it. Confirm that its changes are no longer needed.")
                        },
                    ),
                    ui::Node::row(
                        "repo-stash-confirm-actions",
                        vec![
                            self.button("repo-stash-confirm", "确认执行", "Confirm"),
                            self.button("repo-stash-back", "返回预览", "Back to preview"),
                        ],
                    )
                    .wrap(),
                ],
            ),
        }
    }

    fn repository_create_dialog(&self) -> ui::Dialog {
        // Sixteen full Windows-length paths remain below the native document's text-byte quota.
        let total = self.repository.create_files.len();
        let start = self.repository.create_page.saturating_mul(16).min(total);
        let end = (start + 16).min(total);
        let rows = self.repository.create_files[start..end]
            .iter()
            .enumerate()
            .map(|(index, file)| {
                ui::Node::text(
                    format!("repo-stash-create-file-{index}"),
                    format!("{}{} {}", file.index, file.worktree, file.path),
                )
            })
            .collect();
        ui::Dialog { id: "repo-stash-create".into(), title: self.label("预览并创建 Stash", "Preview and create stash").into(), width: 800.,
            content: ui::Node::column("repo-stash-create-body", vec![
                ui::Node::text("repo-stash-create-scope", self.label("包含全部已跟踪修改与未跟踪文件；排除忽略文件。创建后这些修改会从工作区移出。", "Includes all tracked changes and untracked files; excludes ignored files. Creating the stash removes these changes from the worktree.")),
                ui::Node::text("repo-stash-create-count", format!("{}–{} / {total}", start + usize::from(total != 0), end)),
                ui::Node::scroll("repo-stash-create-files-scroll", ui::Node::column("repo-stash-create-files", rows)).height(300.),
                ui::Node::row("repo-stash-create-pages", vec![self.button("repo-stash-create-previous", "上一页", "Previous page").disabled(start == 0 || self.ui_busy()), self.button("repo-stash-create-next", "下一页", "Next page").disabled(end == total || self.ui_busy())]).wrap(),
                ui::Node::row("repo-stash-create-actions", vec![self.button("repo-stash-create-confirm", "创建 Stash", "Create stash"), self.button("repo-close", "取消", "Cancel")]).wrap(),
            ]), }
    }
}

/// Each code block stays below the public single-text limit, including multibyte UTF-8 boundaries.
fn preview_node(id: &str, text: &str) -> ui::Node {
    let mut nodes = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 8192).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        nodes.push(ui::Node::code_block(
            format!("{id}-{}", nodes.len()),
            &text[start..end],
            None,
        ));
        start = end;
    }
    ui::Node::scroll(format!("{id}-scroll"), ui::Node::column(id, nodes)).height(350.)
}

fn short(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let result = chars
        .by_ref()
        .take(limit)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full stash page and bounded multibyte preview must remain a valid public native scene.
    #[test]
    fn stash_dialog_with_a_full_page_respects_ui_text_and_node_quotas() {
        let mut state = State::default();
        state.repository.stashes = (0..128)
            .map(|index| Stash {
                selector: format!("stash@{{{index}}}"),
                oid: format!("{index:040x}"),
                title: "主题 中😀 ".repeat(100),
                time: "2026-10-15".into(),
            })
            .collect();
        state.repository.stash_selected = state.repository.stashes.first().cloned();
        state.repository.stash_preview_ready = true;
        state.repository.stash_preview = "中".repeat(16 * 1024);
        let mut document = ui::Document::new(ui::Node::column("test-root", Vec::new()));
        document.dialog = Some(state.repository_stash_dialog());
        document.validate().unwrap();
    }

    #[test]
    fn stash_creation_pages_complete_paths_without_exceeding_a_native_scene() {
        let mut state = State::default();
        state.repository.create_files = (0..40)
            .map(|index| Change {
                path: format!("{}-{index}", "a".repeat(4096)),
                ..Default::default()
            })
            .collect();
        state.repository.create_page = 1;
        let mut document = ui::Document::new(ui::Node::column("test-root", Vec::new()));
        document.dialog = Some(state.repository_create_dialog());
        document.validate().unwrap();
    }
}
