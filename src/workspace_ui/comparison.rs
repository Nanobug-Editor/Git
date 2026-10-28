//! Explicit branch comparisons freeze their object IDs and keep the existing native text sessions.
use super::*;

/// A paged file chooser is owned by one explicit branch comparison, never by history selection.
pub(crate) struct BranchComparison {
    generation: u64,
    reference: String,
    left: String,
    right: Option<String>,
    files: BTreeMap<String, model::HistoryChange>,
    loading: bool,
    error: String,
    page: usize,
}

impl State {
    /// Read differences at captured OIDs; a worktree comparison pairs that immutable base with live files.
    pub(crate) fn start_branch_comparison(
        &mut self,
        reference: &str,
        current: bool,
    ) -> Result<(), api::Failure> {
        if self.ui_busy()
            || self.pending.is_some()
            || self.branch_prompt.is_some()
            || self.workspace_ui.comparison.is_some()
        {
            return Err(failure(
                "请先完成当前操作 / Finish the current operation first",
            ));
        }
        let left = self
            .ref_tips
            .get(reference)
            .cloned()
            .filter(|oid| !oid.is_empty())
            .ok_or_else(|| failure("分支已失效 / Branch expired"))?;
        let right = if current {
            Some(
                self.head
                    .clone()
                    .ok_or_else(|| failure("当前没有提交 / No current commit"))?,
            )
        } else {
            None
        };
        self.workspace_ui.comparison_generation =
            self.workspace_ui.comparison_generation.wrapping_add(1);
        let generation = self.workspace_ui.comparison_generation;
        let mut args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "--no-renames".into(),
            "--name-status".into(),
            "-z".into(),
            left.clone(),
        ];
        if let Some(right) = &right {
            args.push(right.clone());
        }
        args.push("--".into());
        self.workspace_ui.comparison = Some(BranchComparison {
            generation,
            reference: reference.into(),
            left,
            right,
            files: BTreeMap::new(),
            loading: true,
            error: String::new(),
            page: 0,
        });
        self.editor(
            api::EditorOperation::SetPanelVisibility {
                panel: "history".into(),
                visible: true,
            },
            "visibility",
        )?;
        self.queue
            .push_back(Command::new(args, Purpose::BranchFiles { generation }));
        Ok(())
    }
    /// A closed or superseded chooser ignores both late success and late errors.
    pub(crate) fn finish_branch_comparison(
        &mut self,
        generation: u64,
        output: &[u8],
        stderr: &[u8],
        code: u32,
    ) -> Result<(), api::Failure> {
        let Some(comparison) = self
            .workspace_ui
            .comparison
            .as_mut()
            .filter(|comparison| comparison.generation == generation)
        else {
            return Ok(());
        };
        comparison.loading = false;
        let result = if code == 0 {
            model::history_changes(output)
        } else {
            Err(safe_diagnostic(stderr))
        };
        match result {
            Ok(files) if files.len() <= 8192 => comparison.files = files,
            Ok(_) => comparison.error = "差异文件超过 8192 项，请缩小比较范围 / More than 8192 changed files; narrow the comparison".into(),
            Err(error) => comparison.error = error,
        }
        Ok(())
    }
    /// Native file selection and paging keep the original comparison and exact accepted UI revision.
    pub(crate) fn branch_comparison_event(
        &mut self,
        event: &ui::UiEvent,
    ) -> Result<bool, api::Failure> {
        let Some(comparison) = self.workspace_ui.comparison.as_mut() else {
            return Ok(false);
        };
        match (&*event.node, &event.action) {
            ("branch-compare-close", ui::Action::Click)
            | ("branch-compare-dialog", ui::Action::Dismiss) => self.workspace_ui.comparison = None,
            ("branch-compare-prev", ui::Action::Click) => {
                comparison.page = comparison.page.saturating_sub(1)
            }
            ("branch-compare-next", ui::Action::Click) => {
                if (comparison.page + 1) * 128 < comparison.files.len() {
                    comparison.page += 1;
                }
            }
            (node, ui::Action::Click) if node.starts_with("branch-compare-file-") => {
                let index = node
                    .strip_prefix("branch-compare-file-")
                    .unwrap()
                    .parse::<usize>()
                    .map_err(|_| failure("差异文件已失效 / Comparison file expired"))?;
                let (path, change) = comparison
                    .files
                    .iter()
                    .nth(index)
                    .map(|(path, change)| (path.clone(), *change))
                    .ok_or_else(|| failure("差异文件已失效 / Comparison file expired"))?;
                let comparison = self.workspace_ui.comparison.take().unwrap();
                self.open_branch_comparison_file(&comparison, &path, change)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    /// Show all changed paths in bounded pages, preserving a real empty/error/loading state.
    pub(crate) fn branch_comparison_dialog(&self) -> Option<ui::Dialog> {
        let comparison = self.workspace_ui.comparison.as_ref()?;
        let mut rows = vec![ui::Node::text(
            "branch-compare-target",
            format!(
                "{} → {}",
                branch_name(&comparison.reference),
                comparison
                    .right
                    .as_deref()
                    .unwrap_or(self.label("工作树", "Working tree"))
            ),
        )];
        if comparison.loading {
            rows.push(ui::Node::text(
                "branch-compare-loading",
                self.label("正在读取差异…", "Reading differences…"),
            ));
        } else if !comparison.error.is_empty() {
            rows.push(ui::Node::text("branch-compare-error", &comparison.error));
        } else if comparison.files.is_empty() {
            rows.push(ui::Node::text(
                "branch-compare-empty",
                self.label("没有差异", "No differences"),
            ));
        }
        for (index, (path, change)) in comparison
            .files
            .iter()
            .enumerate()
            .skip(comparison.page * 128)
            .take(128)
        {
            let status = match change {
                model::HistoryChange::Added => "A",
                model::HistoryChange::Deleted => "D",
                model::HistoryChange::Modified => "M",
            };
            rows.push(ui::Node::button(
                format!("branch-compare-file-{index}"),
                format!("{status}  {path}"),
            ));
        }
        rows.push(ui::Node::row(
            "branch-compare-controls",
            vec![
                ui::Node::button("branch-compare-prev", self.label("上一页", "Previous page"))
                    .disabled(comparison.page == 0),
                ui::Node::button("branch-compare-next", self.label("下一页", "Next page"))
                    .disabled((comparison.page + 1) * 128 >= comparison.files.len()),
                ui::Node::button("branch-compare-close", self.label("关闭", "Close")),
            ],
        ));
        Some(ui::Dialog {
            id: "branch-compare-dialog".into(),
            title: self.label("分支差异", "Branch differences").into(),
            width: 680.,
            content: ui::Node::column("branch-compare-body", rows),
        })
    }
    /// Reuse the public comparison flight and source lifetimes; no alternative mutable document copy.
    fn open_branch_comparison_file(
        &mut self,
        comparison: &BranchComparison,
        path: &str,
        change: model::HistoryChange,
    ) -> Result<(), api::Failure> {
        self.abort_diff(None);
        self.diff_navigation = None;
        self.diff_path = path.into();
        let readonly_right = comparison.right.is_some() || change == model::HistoryChange::Deleted;
        self.diff_history = readonly_right;
        self.diff_mode = Some(readonly_right);
        self.snapshot_targets = vec![
            comparison.left.clone(),
            comparison
                .right
                .clone()
                .unwrap_or_else(|| "worktree".into()),
        ];
        self.diff_title = format!(
            "{} · {} ↔ {}",
            comparisons::short_title(path),
            branch_name(&comparison.reference),
            comparison.right.as_deref().unwrap_or("worktree")
        );
        self.diff_key = format!(
            "branch-{}-{}-{}",
            comparison.left,
            comparison.right.as_deref().unwrap_or("worktree"),
            trees::node_id(path)
        );
        self.diff_columns = vec![String::new(), String::new()];
        self.conflict_mode = false;
        self.diff_source_requested = false;
        self.snapshot_ready = 0;
        self.left_version = None;
        self.right_version = None;
        self.third_version = None;
        self.patch = Default::default();
        self.begin_diff_flight(false);
        if change == model::HistoryChange::Added {
            self.snapshot_ready += 1;
        } else {
            self.queue.push_back(Command::new(
                vec!["show".into(), format!("{}:{path}", comparison.left)],
                Purpose::DiffSnapshot { column: 0 },
            ));
        }
        if change == model::HistoryChange::Deleted {
            self.snapshot_ready += 1;
        } else if let Some(right) = &comparison.right {
            self.queue.push_back(Command::new(
                vec!["show".into(), format!("{right}:{path}")],
                Purpose::DiffSnapshot { column: 1 },
            ));
        } else {
            self.editor(
                api::EditorOperation::OpenDocumentSource { path: path.into() },
                "diff-open",
            )?;
        }
        self.maybe_start_diff()
    }
}
