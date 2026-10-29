//! File-bound comparisons keep readonly Git snapshots separate from native mutable document identities.
use super::*;
mod batch;
mod cache;
mod lifecycle;
mod navigation;
mod prefetch;
pub(super) use cache::HistorySources;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod cache_tests;
pub(super) use lifecycle::{Flight, is_diff_intent};
pub(super) use navigation::HistoryNavigation;
pub(super) use prefetch::HistoryPrefetch;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod lifecycle_tests;

impl State {
    /// A readonly snapshot is paired with the existing worktree session; no mutable text is copied.
    pub(super) fn open_diff(&mut self, path: &str, staged: bool) -> Result<(), api::Failure> {
        self.abort_diff(None);
        self.diff_navigation = None;
        self.selected_path = path.into();
        self.diff_path = path.into();
        self.diff_history = false;
        self.diff_title = format!(
            "{}: {}",
            self.label("仓库差异", "Repository Diff"),
            short_title(path)
        );
        self.diff_key = format!(
            "{}-{}",
            if staged { "index" } else { "worktree" },
            trees::node_id(path)
        );
        self.diff_columns.clear();
        self.conflict_mode = false;
        let change = self
            .changes
            .iter()
            .find(|change| change.path == path)
            .ok_or_else(|| failure("File status expired"))?;
        let left_empty = if staged {
            change.index == 'A'
        } else {
            change.worktree == '?' || change.index == 'D'
        };
        let right_empty = if staged {
            change.index == 'D'
        } else {
            change.worktree == 'D'
        };
        let original = change.original.clone().unwrap_or_else(|| path.into());
        self.begin_diff_flight(false);
        self.diff_mode = Some(staged || right_empty);
        self.diff_source_requested = false;
        self.snapshot_ready = 0;
        self.left_version = None;
        self.right_version = None;
        self.third_version = None;
        self.patch = Default::default();
        self.queue.push_back(Command::new(
            self.patch_args(staged),
            Purpose::Patch { staged },
        ));
        if left_empty {
            self.diff_columns.resize(2, String::new());
            self.snapshot_ready += 1;
        } else {
            self.queue.push_back(Command::new(
                vec![
                    "show".into(),
                    if staged {
                        format!("HEAD:{original}")
                    } else {
                        format!(":{path}")
                    },
                ],
                Purpose::DiffSnapshot { column: 0 },
            ));
        }
        if right_empty {
            self.diff_columns.resize(2, String::new());
            self.snapshot_ready += 1;
        } else if staged {
            self.queue.push_back(Command::new(
                vec!["show".into(), format!(":{path}")],
                Purpose::DiffSnapshot { column: 1 },
            ));
        } else {
            self.editor(
                api::EditorOperation::OpenDocumentSource { path: path.into() },
                "diff-open",
            )?;
        }
        self.maybe_start_diff()?;
        Ok(())
    }
    /// A new/deleted side is an explicit empty snapshot, never a failed Git query treated as empty.
    pub(super) fn maybe_start_diff(&mut self) -> Result<(), api::Failure> {
        if self.diff_mode.is_some()
            && !self.diff_source_requested
            && self.snapshot_ready
                == if self.diff_history {
                    self.snapshot_targets.len()
                } else if self.diff_mode == Some(true) {
                    2
                } else {
                    1
                }
        {
            self.diff_source_requested = true;
            self.open_diff_snapshot(0)?;
            // Independent source preparations share one dispatch, instead of waiting one native
            // receipt per pane. The flight owns every request and cleans all peers on failure.
            if self.diff_mode == Some(true) {
                self.open_diff_snapshot(1)?;
                if self.diff_history && self.snapshot_targets.len() == 3 {
                    self.open_diff_snapshot(2)?;
                }
            }
        }
        Ok(())
    }
    /// Refresh a still-live source; a close notification can trail the click that tries to reuse it.
    /// A rejected cached refresh gets one ordinary fresh open, preserving host ownership checks.
    pub(super) fn open_diff_snapshot(&mut self, column: usize) -> Result<(), api::Failure> {
        self.ensure_diff_observation()?;
        let intent = match column {
            0 => "diff-left",
            1 => "diff-right",
            _ => "diff-third",
        };
        let source = self
            .diff_views
            .get(&self.diff_key)
            .and_then(|sources| match column {
                0 => sources.left.clone(),
                1 => sources.right.clone(),
                _ => sources.third.clone(),
            });
        if let Some(document) = source {
            let text = self.diff_columns.get(column).cloned().unwrap_or_default();
            match self.editor(
                api::EditorOperation::RefreshVirtualDocument { document, text },
                intent,
            ) {
                Ok(()) => return Ok(()),
                Err(error) if lifecycle::expired_source(&error) => {
                    self.forget_rejected_diff_source(column)
                }
                Err(error) => return Err(error),
            }
        }
        let text = self.diff_columns.get(column).cloned().unwrap_or_default();
        self.editor(
            api::EditorOperation::OpenVirtualSource {
                title: format!(
                    "{} · {}",
                    short_title(&self.diff_path),
                    if self.diff_history {
                        self.snapshot_targets
                            .get(column)
                            .map(String::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(self.label("空树", "Empty tree"))
                    } else if column == 1 {
                        "index"
                    } else {
                        "base"
                    }
                ),
                language: None,
                language_path: Some(self.diff_path.clone()),
                text,
            },
            intent,
        )
    }
    /// Disable user-configured diff helpers and colors so Git emits a complete machine patch.
    pub(super) fn patch_args(&self, staged: bool) -> Vec<String> {
        let mut args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "--full-index".into(),
            "--unified=3".into(),
        ];
        if staged {
            args.push("--cached".into())
        }
        args.extend(["--".into(), self.selected_path.clone()]);
        args
    }
    /// Each history source is an immutable blob at an explicit OID, including an empty root parent.
    pub(super) fn history_file(&mut self, path: &str) {
        self.history_path = path.into();
        self.columns.clear();
        self.snapshot_targets.clear();
        let Some(commit) = self.history_commit() else {
            return;
        };
        self.snapshot_targets = if commit.parents.len() > 1 {
            vec![
                commit.parents[self.history_parents[0].min(commit.parents.len() - 1)].clone(),
                commit.oid.clone(),
                commit.parents[self.history_parents[1].min(commit.parents.len() - 1)].clone(),
            ]
        } else {
            vec![
                commit.parents.first().cloned().unwrap_or_default(),
                commit.oid.clone(),
            ]
        };
        self.queue_history_sources(path);
    }

    /// Read only captured OIDs; later log/parent selection cannot retarget an in-flight file.
    fn queue_history_sources(&mut self, path: &str) {
        self.columns = vec![String::new(); self.snapshot_targets.len()];
        self.snapshot_ready = 0;
        let mut missing = Vec::new();
        let mut input = Vec::new();
        for (column, oid) in self.snapshot_targets.iter().enumerate() {
            if oid.is_empty() {
                self.snapshot_ready += 1;
                continue;
            }
            if let Some(text) = self
                .history_sources
                .get(&self.environment.workspace, oid, path)
            {
                self.columns[column] = text.unwrap_or_default();
                self.snapshot_ready += 1;
                continue;
            }
            missing.push(column);
            input.extend_from_slice(format!("{oid}:{path}\0").as_bytes());
        }
        if !missing.is_empty() {
            // NUL-delimited input preserves filenames containing newlines. Output uses Git's
            // declared byte sizes, never line splitting of file contents or missing path names.
            let mut command = Command::new(
                ["cat-file", "--batch", "-z"],
                Purpose::HistoryBatch {
                    columns: missing,
                    path: path.into(),
                },
            );
            command.input = Some(input);
            self.queue.push_back(command);
        }
    }

    /// Capture the selected OID, parents and path before any asynchronous query. Its readonly
    /// editor tab has a distinct key from another commit of the same file and from staged edits.
    pub(super) fn open_history_diff(&mut self, path: &str) -> Result<(), api::Failure> {
        self.abort_diff(None);
        self.history_file(path);
        if self.snapshot_targets.is_empty() {
            return Err(failure("提交选择已失效 / Commit selection expired"));
        }
        self.diff_navigation = HistoryNavigation::capture(
            self.snapshot_targets.clone(),
            self.history_navigation_files(),
            path,
        );
        self.start_history_diff(path)
    }

    /// The original file click and adjacent-file arrows share one source/receipt lifecycle.
    fn start_history_diff(&mut self, path: &str) -> Result<(), api::Failure> {
        self.diff_key = format!(
            "history-{}",
            trees::node_id(&format!("{}\0{}", self.snapshot_targets.join("\0"), path))
        );
        self.diff_title = format!(
            "{}: {}",
            self.label("仓库差异", "Repository Diff"),
            short_title(path)
        );
        self.diff_path = path.into();
        self.begin_diff_flight(true);
        self.diff_history = true;
        self.diff_mode = Some(true);
        self.diff_columns = self.columns.clone();
        self.diff_source_requested = false;
        self.left_version = None;
        self.right_version = None;
        self.third_version = None;
        self.history_error.clear();
        self.maybe_start_diff()
    }
}

/// Bound display titles by UTF-8 bytes as well as characters; paths and Git arguments retain all bytes.
pub(super) fn short_title(path: &str) -> String {
    let mut title = String::new();
    for character in path.rsplit('/').next().unwrap_or(path).chars().take(70) {
        let character = if character.is_control() {
            '�'
        } else {
            character
        };
        if title.len() + character.len_utf8() > 160 {
            break;
        }
        title.push(character);
    }
    title
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A historical OID plus Unicode basename still fits the public 256-byte title quota.
    #[test]
    fn historical_titles_keep_unicode_boundaries_and_room_for_source_identity() {
        let path = format!("folder/{}", "文件😀".repeat(40));
        let title = short_title(&path);
        assert!(title.len() <= 160 && title.chars().count() <= 70);
        assert!(path.rsplit('/').next().unwrap().starts_with(&title));
        assert!(format!("{title} · {}", "a".repeat(64)).len() <= 256);
    }
}
