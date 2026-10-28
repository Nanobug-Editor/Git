//! Adjacent history files retain their commit, parent choices and order across log selection changes.
use super::*;
use std::sync::Arc;

/// Immutable Git sources plus one position; neighboring views share the bounded changed-file list.
#[derive(Clone)]
pub(crate) struct HistoryNavigation {
    /// Exact parent/result/optional-parent OIDs, including the empty root parent.
    targets: Vec<String>,
    /// Complete changed-file order published by this commit's preview, not the moving log selection.
    files: Arc<[String]>,
    /// Index of the source comparison; absence of an adjacent index disables that native arrow.
    index: usize,
}

impl HistoryNavigation {
    /// Capture only a file from the accepted preview; legacy detail-only reads have no arrow sequence.
    pub(super) fn capture(targets: Vec<String>, files: Vec<String>, path: &str) -> Option<Self> {
        let index = files.iter().position(|file| file == path)?;
        Some(Self {
            targets,
            files: files.into(),
            index,
        })
    }

    /// Reject stale keys and non-neighbors instead of accepting an arbitrary caller-supplied path/OID.
    fn adjacent(&self, path: &str) -> Result<Self, api::Failure> {
        let index = self
            .files
            .iter()
            .position(|file| file == path)
            .filter(|index| index.abs_diff(self.index) == 1)
            .ok_or_else(|| failure("相邻文件已失效 / Adjacent file is no longer available"))?;
        Ok(Self {
            index,
            ..self.clone()
        })
    }

    /// Freeze the originating view key and neighbor path; the host knows no Git ordering or OIDs.
    fn descriptor(&self, key: &str) -> api::ComparisonNavigation {
        let action = |index: usize| {
            self.files.get(index).map(|path| api::ComparisonCommand {
                command: "history-adjacent".into(),
                arguments: serde_json::json!({"view": key, "path": path}),
            })
        };
        api::ComparisonNavigation {
            previous: self.index.checked_sub(1).and_then(action),
            next: action(self.index + 1),
        }
    }
}

impl State {
    /// Publish only the active intent's frozen sequence; worktree/index comparisons omit neighbors.
    pub(crate) fn comparison_navigation(&self) -> Option<api::ComparisonNavigation> {
        self.diff_navigation
            .as_ref()
            .map(|context| context.descriptor(&self.diff_key))
    }

    /// Resolve an owned retained comparison before starting any asynchronous Git read.
    fn adjacent_history_context(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<(String, HistoryNavigation), api::Failure> {
        let key = arguments
            .get("view")
            .and_then(|value| value.as_str())
            .ok_or_else(|| failure("Missing comparison view"))?;
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| failure("Missing adjacent path"))?;
        let context = self
            .diff_views
            .get(key)
            .and_then(|sources| sources.navigation.as_ref())
            .ok_or_else(|| failure("比较已关闭 / Comparison is closed"))?
            .adjacent(path)?;
        Ok((path.into(), context))
    }

    /// Navigate inside the original commit without changing the currently selected log/preview.
    pub(crate) fn adjacent_history_file(
        &mut self,
        arguments: &serde_json::Value,
    ) -> Result<(), api::Failure> {
        let (path, context) = self.adjacent_history_context(arguments)?;
        let origin = arguments["view"].as_str().unwrap().to_owned();
        if self.history_comparison_only() {
            // Only this readonly flight may yield. Retiring its exact capability/correlations
            // prevents late Git or native receipts from opening an older neighboring file.
            if self.running.as_ref().is_some_and(|running| {
                matches!(
                    running.command.purpose,
                    Purpose::HistoryBlob { .. } | Purpose::Snapshot { .. }
                )
            }) {
                self.cancel_history_source(api::guest::close_resource)?;
            }
            self.abort_diff(None);
        }
        // Other work keeps its authority; coalesce the latest arrow and recheck its origin at idle.
        self.request_comparison(panel_activity::ComparisonSelection::Adjacent {
            origin,
            path,
            context,
        })
    }

    /// Retain process authority if transport release fails; only a confirmed close clears ownership.
    fn cancel_history_source(
        &mut self,
        close: impl FnOnce(api::ResourceHandle) -> Result<(), api::Failure>,
    ) -> Result<(), api::Failure> {
        if let Some(running) = &self.running {
            close(running.handle.clone())?;
            self.running = None;
        }
        Ok(())
    }

    /// Start the admitted immutable neighbor after the existing native/Git flight releases ownership.
    pub(crate) fn open_adjacent_history_file(
        &mut self,
        path: &str,
        context: HistoryNavigation,
    ) -> Result<(), api::Failure> {
        self.abort_diff(None);
        self.snapshot_targets = context.targets.clone();
        self.diff_navigation = Some(context);
        self.queue_history_sources(path);
        self.start_history_diff(path)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A failed native release keeps the running capability owned until completion or a retry.
    #[test]
    fn adjacent_cancellation_failure_keeps_the_running_source_owned() {
        let mut state = State::default();
        state.diff_views.insert(
            "origin".into(),
            DiffSources {
                navigation: HistoryNavigation::capture(
                    vec!["parent".into(), "commit".into()],
                    vec!["a".into(), "b".into()],
                    "a",
                ),
                ..Default::default()
            },
        );
        state.diff_history = true;
        state.begin_diff_flight(true);
        let handle = api::ResourceHandle {
            instance: "fixture".into(),
            scope: "workspace".into(),
            resource: 1,
        };
        state.running = Some(Running {
            handle: handle.clone(),
            command: Command::new(["cat-file"], Purpose::Snapshot { column: 0 }),
            output: Vec::new(),
            stderr: Vec::new(),
        });
        assert!(
            state
                .cancel_history_source(|_| Err(failure("release failed")))
                .is_err()
        );
        assert_eq!(
            state.running.as_ref().map(|running| &running.handle),
            Some(&handle)
        );
        assert!(state.diff_flight.is_some());
    }

    /// Synchronous launch failures after a successful source receipt still publish inline feedback.
    #[test]
    fn adjacent_source_launch_failure_is_visible_in_history() {
        let handle = api::ResourceHandle {
            instance: "fixture".into(),
            scope: "workspace".into(),
            resource: 1,
        };
        let mut state = State {
            diff_history: true,
            diff_mode: Some(true),
            snapshot_targets: vec!["parent".into(), "commit".into()],
            diff_path: "file".into(),
            diff_key: "diff".into(),
            columns: vec![String::new(); 2],
            diff_columns: vec![String::new(); 2],
            running: Some(Running {
                handle: handle.clone(),
                command: Command::new(["cat-file"], Purpose::Snapshot { column: 0 }),
                output: b"left\n".to_vec(),
                stderr: Vec::new(),
            }),
            queue: VecDeque::from([Command::new(
                ["ls-tree"],
                Purpose::HistoryBlob { column: 1 },
            )]),
            ..Default::default()
        };
        state.begin_diff_flight(true);
        let views = state.views();
        state.changed_views(views);
        let completed = state.running.take().unwrap();
        state.finished(completed, 0).unwrap();
        // The readonly receipt suppresses ordinary repaint, but starting its next child failed.
        let publish = state.pump_with(|_, _| Err(failure("native launch rejected")));
        assert!(!state.history_error.is_empty());
        assert!(
            publish,
            "launch error was hidden by the successful comparison fast path"
        );
        let views = state.views();
        assert!(
            state
                .changed_views(views)
                .iter()
                .any(|view| view.panel == "history")
        );
    }

    /// Arrows received during an unrelated write coalesce by origin and cannot disturb that write.
    #[test]
    fn adjacent_arrows_keep_latest_intent_and_drop_closed_origins() {
        for closed in [false, true] {
            let mut state = State::default();
            state.diff_views.insert(
                "origin".into(),
                DiffSources {
                    navigation: HistoryNavigation::capture(
                        vec!["parent".into(), "commit".into()],
                        vec!["a".into(), "b".into(), "c".into()],
                        "b",
                    ),
                    ..Default::default()
                },
            );
            state
                .queue
                .push_back(Command::new(["add", "--", "untouched"], Purpose::Action));
            for path in ["a", "c"] {
                state
                    .adjacent_history_file(&serde_json::json!({"view":"origin", "path":path}))
                    .unwrap();
            }
            assert_eq!(state.pending_comparisons.len(), 1);
            assert_eq!(state.queue.len(), 1);
            assert_eq!(
                state.queue.front().unwrap().args,
                ["add", "--", "untouched"]
            );
            state.queue.clear();
            if closed {
                state.diff_views.clear();
            }
            state.resume_comparison_selection().unwrap();
            if closed {
                assert!(
                    state.queue.is_empty(),
                    "a closed origin must not reopen from an old arrow"
                );
                assert!(state.diff_flight.is_none());
            } else {
                assert_eq!(state.diff_path, "c");
                assert_eq!(state.queue.len(), 2);
                assert!(
                    state
                        .queue
                        .iter()
                        .all(|command| matches!(command.purpose, Purpose::HistoryBlob { .. }))
                );
            }
        }
    }
    /// Adjacent-file arrows open immutable sources without disabling or replacing the history dock.
    #[test]
    fn adjacent_history_file_keeps_log_and_changed_file_scene_admitted() {
        let files = vec!["a.rs".into(), "b.rs".into()];
        let mut state = State {
            selected: "commit".into(),
            history_files: files.clone(),
            history_selection: Some(Commit {
                oid: "commit".into(),
                parents: vec!["parent".into()],
                ..Default::default()
            }),
            canvas_width: 1000.,
            canvas_height: 300.,
            ..Default::default()
        };
        state.diff_views.insert(
            "first".into(),
            DiffSources {
                navigation: HistoryNavigation::capture(
                    vec!["parent".into(), "commit".into()],
                    files,
                    "a.rs",
                ),
                ..Default::default()
            },
        );
        let views = state.views();
        state.changed_views(views);
        let admitted = state.published_views["history"].clone();
        state
            .adjacent_history_file(&serde_json::json!({"view":"first", "path":"b.rs"}))
            .unwrap();
        let views = state.views();
        let changed = state.changed_views(views);
        assert!(
            !changed.iter().any(|view| view.panel == "history"),
            "opening a neighboring immutable file replaced the history document"
        );
        assert_eq!(state.published_views["history"], admitted);
        assert_eq!(state.selected, "commit");
        assert_eq!(state.history_files, ["a.rs", "b.rs"]);
    }

    /// Old pinned sources keep merge parent choices and neighbors after an unrelated commit is selected.
    #[test]
    fn adjacent_context_keeps_original_commit_parent_order_and_file_boundaries() {
        let context = HistoryNavigation::capture(
            vec!["parent-b".into(), "original".into(), "parent-a".into()],
            vec!["a.txt".into(), "中间.md".into(), "z.txt".into()],
            "中间.md",
        )
        .unwrap();
        let mut state = State {
            selected: "other".into(),
            history_files: vec!["unrelated.rs".into()],
            history_parents: [0, 1],
            ..Default::default()
        };
        state.diff_views.insert(
            "pinned".into(),
            DiffSources {
                navigation: Some(context),
                ..Default::default()
            },
        );
        let (_, first) = state
            .adjacent_history_context(&serde_json::json!({"view":"pinned", "path":"a.txt"}))
            .unwrap();
        assert_eq!(first.targets, ["parent-b", "original", "parent-a"]);
        assert!(first.descriptor("first").previous.is_none());
        assert_eq!(
            first.descriptor("first").next.unwrap().arguments["path"],
            "中间.md"
        );
        let (_, last) = state
            .adjacent_history_context(&serde_json::json!({"view":"pinned", "path":"z.txt"}))
            .unwrap();
        assert!(last.descriptor("last").next.is_none());
        assert!(
            last.adjacent("a.txt").is_err(),
            "non-neighbors cannot skip within a sequence"
        );
        for arguments in [
            serde_json::json!({"view":"closed", "path":"a.txt"}),
            serde_json::json!({"view":"pinned", "path":"unrelated.rs"}),
            serde_json::json!({"view":"pinned", "path":"中间.md"}),
        ] {
            assert!(state.adjacent_history_context(&arguments).is_err());
        }
        assert_eq!(state.selected, "other");
    }
}
