//! Review-first layout combines staging, branch/history navigation and an inline immutable commit preview.
use super::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod isolation_tests;

impl State {
    /// Keep the original revision whenever a panel's semantic document has not changed.
    /// Rendering another surface must not revoke its native clicks, form drafts or lazy pages.
    pub(super) fn changed_views(&mut self, views: Vec<api::View>) -> Vec<api::View> {
        views
            .into_iter()
            .filter(|view| {
                let mut document = view.document.clone();
                document.revision = 0;
                if self.published_views.get(&view.panel) == Some(&document) {
                    return false;
                }
                self.published_views.insert(view.panel.clone(), document);
                true
            })
            .collect()
    }

    pub(super) fn tree_node(&self, name: &str) -> ui::Node {
        if let Some(handle) = self.trees.get(name) {
            ui::Node::new(
                name,
                ui::Kind::Tree {
                    handle: handle.clone(),
                },
            )
            // The branch menu owns cancellation even while a business command is running.
            .disabled(matches!(name, "staging" | "conflicts") && self.ui_busy())
            .grow()
        } else {
            ui::Node::text(name, "")
        }
    }
    pub(super) fn button(&self, id: &str, zh: &str, en: &str) -> ui::Node {
        // Cancellation dismisses owned review state without starting another repository operation.
        ui::Node::button(id, self.label(zh, en))
            .disabled(self.ui_busy() && !matches!(id, "cancel-command" | "rebase-cancel"))
    }
    /// Reuse native input epochs for inline forms and explicit operation prompts.
    pub(super) fn input(
        &self,
        id: &str,
        value: &str,
        placeholder: &str,
        multiline: bool,
    ) -> ui::Node {
        let input = ui::Input {
            value: value.into(),
            value_revision: self.input_epochs.get(id).copied().unwrap_or(0),
            placeholder: placeholder.into(),
        };
        if multiline {
            ui::Node::textarea(id, input)
        } else {
            ui::Node::input(id, input)
        }
    }
    /// Publish history-only range/filter/read changes while sibling input stays admitted.
    pub(super) fn history_scroll_view(&mut self) -> Vec<api::View> {
        self.revision += 1;
        let mut views = vec![api::View {
            panel: "history".into(),
            document: ui::Document::new(self.history_node()).revision(self.revision),
        }];
        // A late read replaces the whole panel document, including its modal surface. Keep
        // the current review/prompt rather than silently dismissing an accepted menu action.
        self.apply_dialogs(&mut views);
        views
    }

    /// One retained branch tree, viewport and commit preview share the native resizable body.
    fn history_node(&mut self) -> ui::Node {
        let row = ui::Node::row(
            "history-body",
            vec![
                self.tree_node("branches").width(190.),
                self.log_canvas(),
                self.history_preview(),
            ],
        )
        .resizable()
        .grow();
        ui::Node::column("history-root", vec![row]).grow()
    }

    pub(super) fn views(&mut self) -> Vec<api::View> {
        if let Err(error) = self.publish_workspace() {
            self.report(Severity::Error, error.to_string());
        }
        self.revision += 1;
        let stamp = self.branch_menu_stamp();
        if self
            .branch_menu_stamp
            .as_ref()
            .is_some_and(|previous| previous != &stamp)
        {
            // Busy/modal changes affect menus, not ref membership. Publishing metadata keeps
            // expanded provider pages, pending replies and native selection/scroll intact.
            if let Err(error) = self.update_branch_nodes() {
                self.report(Severity::Error, error.to_string());
            } else {
                self.branch_menu_stamp = Some(stamp);
            }
        } else {
            self.branch_menu_stamp = Some(stamp);
        }
        let staging = self.commit_panel();
        let history = self.history_node();
        let mut views = vec![
            api::View {
                panel: "staging".into(),
                document: ui::Document::new(staging).revision(self.revision),
            },
            api::View {
                panel: "history".into(),
                document: ui::Document::new(history).revision(self.revision),
            },
            api::View {
                panel: "conflicts".into(),
                document: self.comparison_document(true),
            },
            api::View {
                panel: "commit-details".into(),
                document: self.comparison_document(false),
            },
        ];
        self.apply_dialogs(&mut views);
        views
    }

    /// Apply current modal ownership to either a full publication or a history-only update.
    /// `views` contains only panels being replaced; sibling documents and their inputs stay intact.
    fn apply_dialogs(&self, views: &mut [api::View]) {
        for view in views.iter_mut().filter(|view| view.panel == "staging") {
            view.document.dialog = self.commit_hunk_dialog();
        }
        for view in views.iter_mut().filter(|view| view.panel == "history") {
            if let Some(dialog) = self.branch_dialog() {
                view.document.dialog = Some(dialog);
            }
            if let Some(dialog) = self.branch_comparison_dialog() {
                view.document.dialog = Some(dialog);
            }
            if let Some(dialog) = self.rebase_dialog() {
                view.document.dialog = Some(dialog);
            }
            if let Some(dialog) = self.repository_dialog() {
                view.document.dialog = Some(dialog);
            }
        }
        if let Some(command) = &self.pending {
            let text = command.preview();
            let args = command.effective_args();
            let impact = match args.first().map(String::as_str) {
                Some("reset") if args.iter().any(|arg| arg == "--hard") => self.label(
                    "分支、暂存区和工作区都会回退；未提交的已跟踪内容会丢失。阻挡目标路径的未跟踪文件（包括忽略文件）也可能被删除或覆盖，请检查完整清单。",
                    "Moves branch and resets index/worktree; uncommitted tracked content is lost. Obstructing untracked files, including ignored files, may also be deleted or overwritten; review the complete list.",
                ),
                Some("reset") if args.iter().any(|arg| arg == "--soft") => self.label(
                    "只移动当前分支；保留暂存区和工作区内容。",
                    "Moves only the branch; index and worktree content are retained.",
                ),
                Some("reset") => self.label(
                    "移动分支并重置暂存区；保留工作区内容。",
                    "Moves branch and resets index; worktree content is retained.",
                ),
                Some("push") => self.label(
                    "将本地引用更新发送到远程仓库。",
                    "Publishes local ref updates to the remote repository.",
                ),
                Some("checkout"|"rm") => self.label("用所选冲突版本替换整个文件并标记解决；缺失版本表示删除。", "Replaces the whole file with the selected conflict version and resolves it; an absent version means deletion."),
                _ => self.label(
                    "核对目标、下列文件和命令后执行。",
                    "Review the target, listed files and command before execution.",
                ),
            };
            let files = self
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let mut preview = vec![ui::Node::text("impact", impact)];
            // The public UI has a 64 KiB per-text quota. Preserve every reviewed message/path
            // in bounded nodes instead of invalidating a legitimate multi-message plan.
            let full = format!("{text}\n{files}");
            let mut start = 0;
            let mut part = 0;
            while start < full.len() {
                let mut end = (start + 16000).min(full.len());
                while !full.is_char_boundary(end) {
                    end -= 1;
                }
                preview.push(ui::Node::text(format!("preview-{part}"), &full[start..end]));
                start = end;
                part += 1;
            }
            let dialog = ui::Dialog {
                id: "confirm".into(),
                title: self.label("确认 Git 操作", "Confirm Git operation").into(),
                width: 600.,
                content: ui::Node::column(
                    "confirmation",
                    vec![
                        ui::Node::scroll(
                            "preview-scroll",
                            ui::Node::column("preview-parts", preview),
                        )
                        .height(300.),
                        if self.pending_dirty {
                            self.button("dirty-commit", "先提交，再继续", "Commit, then continue")
                        } else {
                            self.button("confirm-command", "确认执行", "Confirm")
                        },
                        if self.pending_dirty {
                            self.button(
                                "dirty-stash",
                                "包含新文件创建 Stash 后继续",
                                "Stash tracked + untracked, then continue",
                            )
                        } else {
                            ui::Node::text("no-dirty-choice", "")
                        },
                        self.button("cancel-command", "取消", "Cancel"),
                    ],
                ),
            };
            for view in views.iter_mut() {
                if view.panel == self.pending_panel {
                    view.document.dialog = Some(dialog.clone());
                }
            }
        }
    }
    /// History snapshots and conflict result identities are isolated even while both windows are open.
    fn comparison_document(&self, conflict: bool) -> ui::Document {
        let panel = if conflict {
            "conflicts"
        } else {
            "commit-details"
        };
        if !self.visible_panels.get(panel).copied().unwrap_or(false) {
            // Hidden secondary windows must not duplicate the native comparison's source text
            // in every tree click/document notification. Visibility reopens the complete view.
            return ui::Document::new(ui::Node::column("window-root", Vec::new()).grow())
                .revision(self.revision);
        }
        let merge = self.merge_files.get(&self.merge_selected);
        let columns = if conflict {
            merge.map(|file| file.columns.as_slice()).unwrap_or(&[])
        } else {
            &self.columns
        };
        let mut panes = Vec::new();
        for (i, text) in columns.iter().enumerate() {
            if conflict && i == 1 {
                if let Some(document) = merge.and_then(|file| file.document.as_ref()) {
                    panes.push(
                        ui::Node::column(
                            "result-column",
                            vec![
                                ui::Node::text(
                                    "result-title",
                                    self.label("合并结果 · 可编辑", "Result · Editable"),
                                ),
                                ui::Node::new(
                                    "merge-result",
                                    ui::Kind::Editor(ui::Editor {
                                        sync_group: Some("comparison".into()),
                                        line_actions: Some(ui::EditorActions {
                                            left: self.conflict_edges(merge.unwrap(), false),
                                            right: self.conflict_edges(merge.unwrap(), true),
                                        }),
                                        ..ui::Editor::document(document.clone())
                                    }),
                                )
                                .grow(),
                            ],
                        )
                        .grow(),
                    );
                    continue;
                }
            }
            let label = if conflict {
                if i == 0 {
                    format!(
                        "{} · {} · {}",
                        self.label("来源 2 · 只读", "Stage 2 · Read only"),
                        if self.current_ref.is_empty() {
                            "detached HEAD"
                        } else {
                            &self.current_ref
                        },
                        self.head.as_deref().unwrap_or("")
                    )
                } else if i == 2 {
                    format!(
                        "{} · {} · {}",
                        self.label("来源 3 · 只读", "Stage 3 · Read only"),
                        self.operation.as_deref().unwrap_or("index"),
                        self.operation_oid.as_deref().unwrap_or("")
                    )
                } else {
                    self.label("共同祖先 · 只读", "Base · Read only").into()
                }
            } else {
                self.snapshot_targets
                    .get(i)
                    .map(String::as_str)
                    .map(|target| {
                        if target.is_empty() {
                            self.label("空树 · 只读", "Empty tree · Read only")
                        } else {
                            target
                        }
                    })
                    .unwrap_or("Snapshot")
                    .to_owned()
            };
            panes.push(
                ui::Node::column(
                    format!("pane-{i}"),
                    vec![
                        ui::Node::text(format!("pane-title-{i}"), label),
                        ui::Node::new(
                            format!("snapshot-{i}"),
                            ui::Kind::Editor(ui::Editor {
                                sync_group: Some("comparison".into()),
                                changes: if conflict {
                                    merge
                                        .map(|file| {
                                            file.regions
                                                .iter()
                                                .map(|region| {
                                                    // Until the native open receipt, the center is
                                                    // the ancestor snapshot, with its own byte offsets.
                                                    match i {
                                                        0 => &region.left,
                                                        1 => &region.base,
                                                        _ => &region.right,
                                                    }
                                                })
                                                .filter(|range| !range.is_empty())
                                                .take(512)
                                                .map(|range| api::TextRange {
                                                    start: range.start,
                                                    end: range.end,
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default()
                                } else {
                                    self.history_marks(i)
                                },
                                ..ui::Editor::snapshot(text.clone())
                            }),
                        )
                        .grow(),
                    ],
                )
                .grow(),
            );
        }
        let adjustable = panes.len() >= 2;
        let mut compare = ui::Node::row("compare-columns", panes).grow();
        compare.layout.resizable = adjustable;
        let detail = ui::Node::column(
            "details-right",
            vec![
                ui::Node::text(
                    "details-path",
                    if conflict {
                        &self.merge_selected
                    } else {
                        &self.history_path
                    },
                ),
                if !conflict {
                    self.history_parent_controls()
                } else {
                    ui::Node::text("no-parent-controls", "")
                },
                compare,
            ],
        )
        .grow();
        let files = self
            .tree_node(if conflict { "conflicts" } else { "files" })
            .width(220.);
        let mut actions = Vec::new();
        if conflict {
            let proposals_current = self.conflict_lifecycle.ready()
                && merge.is_some_and(|file| !file.resolved && !file.proposals_suspended);
            if merge.is_some_and(|file| file.opaque) {
                actions.push(ui::Node::text("opaque-conflict",self.label("二进制、非 UTF-8 或超限文件：请选择完整版本；不展示截断文本", "Binary, non UTF-8 or large file: choose a complete version; no truncated text is shown")));
            }
            actions.push(
                self.button(
                    "merge-nonconflicting",
                    "应用非冲突更改",
                    "Apply nonconflicting changes",
                )
                .disabled(
                    self.ui_busy()
                        || !proposals_current
                        || merge
                            .is_none_or(|file| file.nonconflicting.is_empty() || !file.read_valid),
                ),
            );
            actions.push(
                self.button("merge-show-base", "查看共同祖先", "View base")
                    .disabled(self.ui_busy() || merge.is_none_or(|file| file.opaque)),
            );
            actions.push(
                self.button(
                    "merge-from-base",
                    "从共同祖先重新开始（可撤销）",
                    "Restart from base (Undo available)",
                )
                .disabled(
                    self.ui_busy()
                        || !proposals_current
                        || merge.is_none_or(|file| file.opaque || !file.read_valid),
                ),
            );
            actions.extend([
                self.button(
                    "conflict-stage-2",
                    "采用完整来源 2…",
                    "Use complete stage 2…",
                )
                .disabled(self.ui_busy() || !proposals_current),
                self.button(
                    "conflict-stage-3",
                    "采用完整来源 3…",
                    "Use complete stage 3…",
                )
                .disabled(self.ui_busy() || !proposals_current),
                self.button("resolved", "保存并标记解决", "Save and resolve")
                    .disabled(
                        self.ui_busy()
                            || !proposals_current
                            || merge.is_none_or(|file| {
                                file.opaque
                                    || !file.read_valid
                                    || file.read_inflight
                                    || file.has_markers
                                    || !file.all_reviewed()
                                    || file.resolved
                            }),
                    ),
                self.button("continue", "继续", "Continue").disabled(
                    self.ui_busy()
                        || self.operation.is_none()
                        || self.changes.iter().any(|c| c.conflict),
                ),
                self.button("skip", "跳过…", "Skip…").disabled(
                    self.ui_busy()
                        || !matches!(self.operation.as_deref(), Some("rebase" | "cherry-pick")),
                ),
                self.button("abort", "中止…", "Abort…")
                    .disabled(self.ui_busy() || self.operation.is_none()),
            ]);
        }
        let mut document = ui::Document::new(
            ui::Node::column(
                "window-root",
                vec![
                    ui::Node::text(
                        "commit-identity",
                        if conflict {
                            self.label("冲突处理", "Resolve conflicts")
                        } else {
                            &self.selected
                        },
                    ),
                    ui::Node::row("window-body", vec![files, detail])
                        .resizable()
                        .grow(),
                    ui::Node::row("merge-actions", actions).wrap(),
                ],
            )
            .grow()
            .padding(8.),
        )
        .revision(self.revision)
        // Use the host's zero-byte display alignment; snapshots and the result retain native text.
        .code_alignment(true);
        if conflict {
            document.source = merge.and_then(|file| file.document.clone());
            document.editor_layout = document.source.is_some();
            if let Some(file) = merge.filter(|file| file.show_base && !file.opaque) {
                document.dialog = Some(ui::Dialog {
                    id: "base-dialog".into(),
                    title: self.label("共同祖先 · 只读", "Base · Read only").into(),
                    width: 800.,
                    content: ui::Node::column(
                        "base-content",
                        vec![
                            ui::Node::new(
                                "base-source",
                                ui::Kind::Editor(ui::Editor {
                                    changes: Vec::new(),
                                    sync_group: None,
                                    ..ui::Editor::snapshot(file.columns[1].clone())
                                }),
                            )
                            .height(420.),
                            self.button("merge-show-base", "关闭", "Close"),
                        ],
                    ),
                });
            }
        }
        document
    }
    /// Edge controls are ordinary native buttons; the host places them at the exact source row.
    fn conflict_edges(&self, file: &MergeFile, right: bool) -> Vec<ui::EditorAction> {
        // A retained resolved result is reviewable, but never carries old proposal authority.
        if !self.conflict_lifecycle.ready() || file.resolved || file.proposals_suspended {
            return Vec::new();
        }
        file.blocks
            .iter()
            .take(64)
            .map(|block| {
                let side = if right { "right" } else { "left" };
                let key = block.key;
                let reviewed = file.reviewed.get(&key) == Some(&block.fingerprint);
                let ignored = file
                    .decisions
                    .get(&key)
                    .is_some_and(|(_, choices)| choices[usize::from(right)] == 2);
                let mut controls = vec![
                    self.button(
                        &format!("merge-block-{key}-{side}"),
                        if right { "←" } else { "→" },
                        if right { "←" } else { "→" },
                    )
                    .disabled(self.ui_busy() || block.ambiguous || reviewed || !file.read_valid)
                    .tooltip(self.label("接受此来源的冲突块", "Accept this side of the conflict")),
                    self.button(&format!("merge-block-{key}-ignore-{side}"), "×", "×")
                        .disabled(
                            self.ui_busy()
                                || block.ambiguous
                                || reviewed
                                || ignored
                                || !file.read_valid,
                        )
                        .tooltip(self.label(
                            "忽略此来源的冲突块；两侧都忽略时保留共同祖先",
                            "Ignore this side; ignoring both retains the base",
                        )),
                ];
                if right && !block.markers && !reviewed {
                    controls.push(
                        self.button(&format!("merge-block-{key}-confirm"), "✓", "✓")
                            .disabled(self.ui_busy() || block.ambiguous || !file.read_valid)
                            .tooltip(self.label(
                                "确认此块的人工编辑结果",
                                "Confirm the manually edited result of this block",
                            )),
                    );
                }
                ui::EditorAction {
                    offset: block.range.start,
                    content: Box::new(ui::Node::row(format!("edge-{side}-{key}"), controls)),
                }
            })
            .collect()
    }
    /// Readonly history colors are derived only from immutable parent/result bytes.
    fn history_marks(&self, column: usize) -> Vec<api::TextRange> {
        let Some(text) = self.columns.get(column) else {
            return Vec::new();
        };
        let other = if column == 0 || column == 2 {
            self.columns.get(1)
        } else {
            self.columns.first()
        };
        other
            .and_then(|other| merge::plan::edits(other, text).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|edit| !edit.other.is_empty())
            .take(512)
            .map(|edit| api::TextRange {
                start: edit.other.start,
                end: edit.other.end,
            })
            .collect()
    }
    /// Octopus merges expose all parents explicitly; choosing two changes only the readonly view.
    pub(super) fn history_parent_controls(&self) -> ui::Node {
        let Some(commit) = self
            .history_commit()
            .filter(|commit| commit.parents.len() > 2)
        else {
            return ui::Node::text("no-extra-parents", "");
        };
        ui::Node::row(
            "history-parent-controls",
            (0..2)
                .map(|side| {
                    ui::Node::new(
                        format!("history-parent-{side}"),
                        ui::Kind::Choice {
                            options: commit
                                .parents
                                .iter()
                                .enumerate()
                                .map(|(index, oid)| ui::OptionItem {
                                    id: index.to_string(),
                                    label: format!(
                                        "{} {} · {oid}",
                                        self.label("父提交", "Parent"),
                                        index + 1
                                    ),
                                    disabled: index == self.history_parents[1 - side],
                                })
                                .collect(),
                            selected: Some(self.history_parents[side].to_string()),
                        },
                    )
                    .disabled(self.ui_busy())
                })
                .collect(),
        )
    }
}
