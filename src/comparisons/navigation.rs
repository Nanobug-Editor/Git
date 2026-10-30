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
    /// Only a confirmed boundary gesture changes the new view's initial content position.
    position: api::ComparisonPosition,
}

impl HistoryNavigation {
    /// The captured sequence, rather than the moving history selection, identifies an arrow target.
    pub(crate) fn comparison_key(&self, path: &str) -> String {
        super::history_key(&self.targets, path)
    }
    /// Only the next and previous immutable neighbors may be prepared after visible admission.
    pub(super) fn neighboring_sources(&self) -> impl Iterator<Item = (&[String], &str)> {
        self.index
            .checked_add(1)
            .into_iter()
            .chain(self.index.checked_sub(1))
            .filter_map(|index| self.files.get(index))
            .map(|path| (self.targets.as_slice(), path.as_str()))
    }

    /// Capture only a file from the accepted preview; legacy detail-only reads have no arrow sequence.
    pub(super) fn capture(targets: Vec<String>, files: Vec<String>, path: &str) -> Option<Self> {
        let index = files.iter().position(|file| file == path)?;
        Some(Self {
            targets,
            files: files.into(),
            index,
            position: api::ComparisonPosition::FirstDifference,
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
            position: api::ComparisonPosition::FirstDifference,
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
            previous_boundary: self
                .index
                .checked_sub(1)
                .and_then(action)
                .map(|mut command| {
                    command.arguments["position"] = "end".into();
                    command
                }),
            next_boundary: action(self.index + 1).map(|mut command| {
                command.arguments["position"] = "start".into();
                command
            }),
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

    /// The initial viewport belongs to the captured file intent, never a later toolbar click.
    pub(crate) fn comparison_position(&self) -> api::ComparisonPosition {
        self.diff_navigation
            .as_ref()
            .map(|context| context.position)
            .unwrap_or_default()
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
        let mut context = self
            .diff_views
            .get(key)
            .and_then(|sources| sources.navigation.as_ref())
            .ok_or_else(|| failure("比较已关闭 / Comparison is closed"))?
            .adjacent(path)?;
        context.position = match arguments.get("position").and_then(|value| value.as_str()) {
            None | Some("first_difference") => api::ComparisonPosition::FirstDifference,
            Some("start") => api::ComparisonPosition::Start,
            Some("end") => api::ComparisonPosition::End,
            _ => return Err(failure("Invalid comparison position")),
        };
        Ok((path.into(), context))
    }

    /// Navigate inside the original commit without changing the currently selected log/preview.
    pub(crate) fn adjacent_history_file(
        &mut self,
        arguments: &serde_json::Value,
    ) -> Result<(), api::Failure> {
        let (path, context) = self.adjacent_history_context(arguments)?;
        let origin = arguments["view"].as_str().unwrap().to_owned();
        // Other work keeps its authority; coalesce the latest arrow and recheck its origin at idle.
        self.request_comparison(panel_activity::ComparisonSelection::Adjacent {
            origin,
            path,
            context,
        })
    }

    /// Retain process authority if transport release fails; only a confirmed close clears ownership.
    pub(crate) fn cancel_history_source(
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

    /// Follow only a successfully opened comparison in the currently displayed commit tree.
    /// A retained preview from another commit keeps its navigation without replacing that tree.
    pub(crate) fn reveal_completed_history_file(&self) -> Result<(), api::Failure> {
        let Some(context) = &self.diff_navigation else {
            return Ok(());
        };
        if context.targets.get(1) == Some(&self.selected)
            && self
                .history_preview_commit
                .as_ref()
                .is_some_and(|commit| commit.oid == self.selected)
        {
            self.reveal_history_path(&context.files[context.index])?;
        }
        Ok(())
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
    fn adjacent_source_launch_failure_reaches_host_message_management() {
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
        state.collect_feedback("", None);
        let _ = feedback::take_notices();
        state.flush_feedback(feedback::accept).unwrap();
        assert!(feedback::take_notices().iter().any(|notice| matches!(notice,
            plugin_protocol::interaction::Operation::PublishMessage { severity: Severity::Error, message, .. }
                if message.contains("native launch rejected"))));
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
                assert_eq!(state.queue.len(), 1);
                assert!(
                    state
                        .queue
                        .iter()
                        .all(|command| matches!(command.purpose, Purpose::HistoryBatch { .. }))
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

    /// Only boundary commands carry an edge; a later ordinary arrow returns to first-difference.
    #[test]
    fn boundary_commands_keep_position_with_the_captured_neighbor() {
        let context = HistoryNavigation::capture(
            vec!["parent".into(), "commit".into()],
            vec!["a".into(), "b".into(), "c".into()],
            "b",
        )
        .unwrap();
        let descriptor = context.descriptor("view");
        assert_eq!(
            descriptor.previous_boundary.as_ref().unwrap().arguments["position"],
            "end"
        );
        assert_eq!(
            descriptor.next_boundary.as_ref().unwrap().arguments["position"],
            "start"
        );
        let mut state = State::default();
        state.diff_views.insert(
            "view".into(),
            DiffSources {
                navigation: Some(context),
                ..Default::default()
            },
        );
        let (_, end) = state
            .adjacent_history_context(&descriptor.previous_boundary.unwrap().arguments)
            .unwrap();
        assert_eq!(end.position, api::ComparisonPosition::End);
        assert!(end.descriptor("end").previous_boundary.is_none());
        assert_eq!(
            end.adjacent("b").unwrap().position,
            api::ComparisonPosition::FirstDifference
        );
        assert!(
            state
                .adjacent_history_context(
                    &serde_json::json!({"view":"view", "path":"a", "position":"arbitrary"})
                )
                .is_err()
        );
    }
}
