//! Published native scenes cover readonly source labels, extra parents and command availability.
use super::*;

/// A root commit has an intentionally empty parent, with a readable source label in both locales.
#[test]
fn root_commit_comparison_labels_its_empty_tree_in_chinese_and_english() {
    let mut state = workbench();
    state.commits = vec![Commit {
        oid: "a".repeat(40),
        ..Default::default()
    }];
    state.select_history_row(0, false);
    state.queue.clear();
    state.history_file("src/selected.rs");
    assert_eq!(state.snapshot_targets, ["", &"a".repeat(40)]);
    assert!(
        state
            .queue
            .iter()
            .all(|command| !command.args.iter().any(|arg| arg.is_empty()))
    );
    complete_history(&mut state, &[]);
    assert_eq!(state.columns, ["", "selected text\n"]);
    for (locale, expected) in [
        ("zh-CN", "空树 · 只读"),
        ("en-US", "Empty tree · Read only"),
    ] {
        state.environment.locale = locale.into();
        let views = state.views();
        let details = views
            .iter()
            .find(|view| view.panel == "commit-details")
            .unwrap();
        let label = find_node(&details.document.root, "pane-title-0").unwrap();
        assert!(
            matches!(&label.kind, ui::Kind::Text { text } if text == expected),
            "{locale}: {:?}",
            label.kind
        );
    }
}

/// An octopus merge exposes every parent, rejects a duplicate pair and retains the choice on refresh.
#[test]
fn octopus_history_parent_choices_are_explicit_distinct_localized_and_stable() {
    let parents = ['a', 'b', 'c'].map(|value| value.to_string().repeat(40));
    let merge = "f".repeat(40);
    let mut state = workbench();
    state.commits = vec![Commit {
        oid: merge.clone(),
        parents: parents.to_vec(),
        ..Default::default()
    }];
    state.select_history_row(0, false);
    state.queue.clear();
    state.history_file("src/selected.rs");
    complete_history(&mut state, &[]);
    assert_eq!(
        state.snapshot_targets,
        [parents[0].clone(), merge.clone(), parents[1].clone()]
    );
    let views = state.views();
    let details = views
        .iter()
        .find(|view| view.panel == "commit-details")
        .unwrap();
    let choices = find_node(&details.document.root, "history-parent-0").unwrap();
    let ui::Kind::Choice { options, selected } = &choices.kind else {
        panic!("parent choice")
    };
    assert_eq!(options.len(), 3);
    assert_eq!(selected.as_deref(), Some("0"));
    assert!(options[1].disabled);
    assert_eq!(options[2].label, format!("父提交 3 · {}", parents[2]));
    state
        .ui_event(
            Some("commit-details"),
            ui::UiEvent {
                revision: state.revision,
                node: "history-parent-0".into(),
                action: ui::Action::Select("2".into()),
            },
        )
        .unwrap();
    complete_history(&mut state, &[]);
    assert_eq!(
        state.snapshot_targets,
        [parents[2].clone(), merge.clone(), parents[1].clone()]
    );
    assert!(
        state
            .ui_event(
                Some("commit-details"),
                ui::UiEvent {
                    revision: state.revision,
                    node: "history-parent-1".into(),
                    action: ui::Action::Select("2".into()),
                }
            )
            .is_err()
    );
    assert_eq!(state.history_parents, [2, 1]);
    state.event(None, api::Notification::Focus(true)).unwrap();
    complete_history(&mut state, &commits());
    assert_eq!(state.history_parents, [2, 1]);
    state
        .ui_event(
            Some("commit-details"),
            ui::UiEvent {
                revision: state.revision,
                node: "history-parent-1".into(),
                action: ui::Action::Select("0".into()),
            },
        )
        .unwrap();
    complete_history(&mut state, &[]);
    assert_eq!(
        state.snapshot_targets,
        [parents[2].clone(), merge, parents[0].clone()]
    );
    state.environment.locale = "en-US".into();
    let views = state.views();
    let details = views
        .iter()
        .find(|view| view.panel == "commit-details")
        .unwrap();
    let choices = find_node(&details.document.root, "history-parent-1").unwrap();
    let ui::Kind::Choice { options, .. } = &choices.kind else {
        panic!("parent choice")
    };
    assert_eq!(options[2].label, format!("Parent 3 · {}", parents[2]));
}

/// Busy queries and native result reads cannot advertise controls whose callbacks would be ignored.
/// Explicit cancellation remains reachable while the same current operation is still running.
#[test]
fn workbench_busy_controls_match_queue_process_and_native_read_authority() {
    for busy_source in 0..4 {
        let mut state = workbench();
        for name in ["staging", "branches", "files", "conflicts"] {
            state.trees.insert(name.into(), handle());
        }
        let round = state.conflict_lifecycle.begin();
        state.conflict_lifecycle.status(round, Some(Vec::new()));
        for marker in conflicts::lifecycle::Marker::ALL {
            state.conflict_lifecycle.probe(round, marker, &[], 1);
        }
        state.operation = Some("rebase".into());
        state.merge_selected = "src/conflict.rs".into();
        state.merge_files.insert(
            state.merge_selected.clone(),
            MergeFile {
                document: Some(api::DocumentVersion {
                    id: "result".into(),
                    path: "src/conflict.rs".into(),
                    revision: 1,
                }),
                columns: vec!["left".into(), "base".into(), "right".into()],
                read_valid: true,
                read_inflight: busy_source == 3,
                blocks: vec![merge::Block {
                    key: 0,
                    range: 0..1,
                    left: "left".into(),
                    right: "right".into(),
                    base: "base".into(),
                    markers: false,
                    ambiguous: false,
                    fingerprint: 1,
                }],
                nonconflicting: vec![api::TextEdit {
                    range: api::TextRange { start: 0, end: 0 },
                    text: "change".into(),
                }],
                ..Default::default()
            },
        );
        state.pending = Some(Command::new(["reset", "--soft", "HEAD"], Purpose::Action));
        state.pending_panel = "conflicts".into();
        match busy_source {
            0 => state
                .queue
                .push_back(Command::new(["status"], Purpose::Refs)),
            1 => {
                state.running = Some(Running {
                    handle: handle(),
                    command: Command::new(["status"], Purpose::Refs),
                    output: Vec::new(),
                    stderr: Vec::new(),
                })
            }
            2 => {
                state
                    .editor_tasks
                    .insert(2, "merge-read:src/conflict.rs".into());
            }
            _ => {}
        }
        let views = state.views();
        for id in [
            "conflicts",
            "merge-nonconflicting",
            "merge-from-base",
            "merge-show-base",
            "conflict-stage-2",
            "conflict-stage-3",
            "continue",
            "skip",
            "abort",
            "merge-block-0-left",
            "merge-block-0-ignore-left",
            "merge-block-0-confirm",
        ] {
            let node = views
                .iter()
                .find_map(|view| find_node(&view.document.root, id))
                .unwrap();
            assert!(
                node.disabled,
                "{id} remained enabled during busy source {busy_source}"
            );
        }
        // Busy operations still gate writes and conflict actions. The commit tree keeps
        // native browsing available without fading the whole file viewport on each checkbox.
        let staging = views.iter().find(|view| view.panel == "staging").unwrap();
        assert!(staging.document.active_node("staging").is_some());
        let conflicts = views.iter().find(|view| view.panel == "conflicts").unwrap();
        let cancel = find_node(
            &conflicts.document.dialog.as_ref().unwrap().content,
            "cancel-command",
        )
        .unwrap();
        assert!(!cancel.disabled, "dismissal cannot be blocked by a read");
        let branches = views
            .iter()
            .find_map(|view| find_node(&view.document.root, "branches"))
            .unwrap();
        assert!(
            !branches.disabled,
            "branch context keeps cancellation reachable; individual commands own admission"
        );
    }
}

/// Ordinary commit details are exactly parent-to-result and expose no conflict mutation surface.
#[test]
fn ordinary_history_details_are_a_readonly_parent_to_commit_comparison() {
    let mut state = workbench();
    state.commits = vec![Commit {
        oid: "b".repeat(40),
        parents: vec!["a".repeat(40)],
        ..Default::default()
    }];
    state.select_history_row(0, false);
    state.queue.clear();
    state.history_file("src/selected.rs");
    complete_history(&mut state, &[]);
    assert_eq!(state.snapshot_targets, ["a".repeat(40), "b".repeat(40)]);
    let views = state.views();
    let details = views
        .iter()
        .find(|view| view.panel == "commit-details")
        .unwrap();
    for id in ["snapshot-0", "snapshot-1"] {
        assert!(matches!(
            find_node(&details.document.root, id).unwrap().kind,
            ui::Kind::Editor(_)
        ));
    }
    for absent in [
        "snapshot-2",
        "merge-result",
        "resolved",
        "continue",
        "history-parent-0",
    ] {
        assert!(
            find_node(&details.document.root, absent).is_none(),
            "unexpected detail control {absent}"
        );
    }
}

/// Ref deletion can genuinely shorten history. Every published canvas stays within the public
/// geometry contract, including a now-empty repository and a filter awaiting its first page.
#[test]
fn shrinking_and_empty_history_keep_the_native_canvas_scroll_range_valid() {
    let repository = commits();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    for _ in 0..2 {
        state
            .ui_event(
                Some("history"),
                ui::UiEvent {
                    revision: state.revision,
                    node: "more".into(),
                    action: ui::Action::Click,
                },
            )
            .unwrap();
        complete_history(&mut state, &repository);
    }
    state.log_scroll = 7200.;
    for (count, expected_scroll) in [(100, 2520.), (0, 0.)] {
        state.refresh();
        complete_history(&mut state, &repository[..count]);
        let views = state.views();
        let history = views.iter().find(|view| view.panel == "history").unwrap();
        let ui::Kind::Canvas(canvas) = &find_node(&history.document.root, "log").unwrap().kind
        else {
            panic!("history canvas")
        };
        canvas
            .validate()
            .expect("a shorter repository must still publish a valid native scene");
        assert_eq!(state.log_scroll, expected_scroll);
        assert_eq!(canvas.scroll.as_ref().unwrap().offset, expected_scroll);
    }
    state.log_scroll = 7200.;
    state
        .ui_event(
            Some("history"),
            ui::UiEvent {
                revision: state.revision,
                node: "all".into(),
                action: ui::Action::Click,
            },
        )
        .unwrap();
    ui::Document::new(state.log_canvas())
        .validate()
        .expect("a pending filter has a valid empty viewport");
}

/// Exercise the serialized guest boundary without a native transport; these presentation actions
/// must not issue Git, preference or document requests merely to inspect already observed metadata.
fn dispatch_history(event: api::Notification) -> ui::Document {
    let invocation = api::Invocation {
        id: 1,
        message: api::Input::Event {
            panel: Some("history".into()),
            event,
        },
    };
    let completion: api::Completion = serde_json::from_str(
        &GitPlugin::dispatch(serde_json::to_string(&invocation).unwrap()).unwrap(),
    )
    .unwrap();
    completion
        .result
        .unwrap()
        .views
        .into_iter()
        .find(|view| view.panel == "history")
        .unwrap()
        .document
}

/// Headerless history retains a padded graph and bounded text columns across native widths,
/// locales and scroll offsets; the first row starts directly at the top of its input viewport.
#[test]
fn headerless_history_keeps_graph_inset_columns_and_scroll_aligned() {
    use plugin_protocol::Paint;

    for locale in ["zh-CN", "en-US"] {
        for width in [700., 997., 1500.] {
            for font_size in [12., 20.] {
                let mut state = workbench();
                state.environment.locale = locale.into();
                state.environment.ui_font.size_px = Some(font_size);
                state.commits = (0..20)
                    .map(|index| Commit {
                        oid: format!("{index:040x}"),
                        title: format!("Review {index}"),
                        labels: "分支标签".repeat(40),
                        author: "Ada".into(),
                        time: "10:30".into(),
                        ..Default::default()
                    })
                    .collect();
                STATE.with(|cell| *cell.borrow_mut() = state);
                let before = dispatch_history(api::Notification::Ui(ui::UiEvent {
                    revision: 0,
                    node: "log".into(),
                    action: ui::Action::Canvas(ui::CanvasEvent::Resize {
                        width,
                        height: 84.,
                        grid: None,
                    }),
                }));
                before
                    .validate()
                    .expect("aligned history is a public scene");
                assert!(find_node(&before.root, "log-headers").is_none());
                let ui::Kind::Canvas(body) = &find_node(&before.root, "log").unwrap().kind else {
                    panic!("history body")
                };
                let graph = body
                    .paint
                    .iter()
                    .find_map(|paint| match paint {
                        Paint::Svg { rect, clip, .. } => Some((*rect, *clip)),
                        _ => None,
                    })
                    .expect("one shared topology painting");
                assert_eq!(graph.0.x, 16.);
                assert_eq!(graph.0, graph.1);
                let cells = body
                    .paint
                    .iter()
                    .filter_map(|paint| match paint {
                        Paint::Text { x, y, text, .. } if *y == 5. => Some((*x, text, 0.)),
                        Paint::TextBox { rect, text, .. } if rect.y == 5. => {
                            Some((rect.x, text, rect.w))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(cells.len(), 4);
                assert_eq!(cells[0].0, graph.0.x + graph.0.w);
                assert!(cells.windows(2).all(|pair| pair[0].0 < pair[1].0));
                assert_eq!(
                    cells[1].1,
                    &"分支标签".repeat(40),
                    "native clipping retains the caption for a subsequent drag"
                );
                // The host shapes and clips inside these non-overlapping native regions;
                // guest tests must not infer native glyph widths from character counts.
                assert!(cells[1].0 + cells[1].2 < cells[2].0);
                assert_eq!(body.scroll.as_ref().unwrap().content, 560.);
                let after = dispatch_history(api::Notification::Ui(ui::UiEvent {
                    revision: before.revision,
                    node: "log".into(),
                    action: ui::Action::Canvas(ui::CanvasEvent::Scroll { offset: 56. }),
                }));
                after
                    .validate()
                    .expect("scrolling preserves the public scene");
                assert!(find_node(&after.root, "log-headers").is_none());
                let ui::Kind::Canvas(scrolled) = &find_node(&after.root, "log").unwrap().kind
                else {
                    panic!("history body after scroll")
                };
                assert_eq!(scrolled.scroll.as_ref().unwrap().offset, 56.);
                assert!(
                    scrolled.paint.iter().any(|paint| matches!(paint,
                        Paint::TextBox { rect, text, .. } if rect.y == 5. && text == "Review 2"
                    )),
                    "no removed heading can offset the first body row"
                );
                STATE.with(|cell| *cell.borrow_mut() = State::default());
            }
        }
    }
}

/// The public history root has no footer even after errors; its explicit information dialog
/// preserves repository policy, fetch time and operation results without starting another query.
#[test]
fn history_without_footer_keeps_results_and_repository_details_in_the_information_dialog() {
    for locale in ["zh-CN", "en-US"] {
        let mut state = workbench();
        state.environment.locale = locale.into();
        state.repository.identity.as_mut().unwrap().git_dir =
            "C:/common/.git/worktrees/linked".into();
        state.repository.remembered = Some(PullStrategy::Merge);
        state.last_fetch = Some(1_760_000_000);
        state.message = "Visible operation result / 操作结果".into();
        STATE.with(|cell| *cell.borrow_mut() = state);
        let history = dispatch_history(api::Notification::Focus(false));
        history
            .validate()
            .expect("compact history remains a valid native scene");
        for id in ["branches", "log"] {
            assert!(
                find_node(&history.root, id).is_some(),
                "{id} remains in the workbench"
            );
        }
        for id in [
            "status",
            "repo-controls",
            "repo-fetch-time",
            "repo-info",
            "more",
            "repo-stashes",
            "log-headers",
            "repo-identity",
            "repo-policy",
            "toolbar",
            "argument",
            "advanced-open",
        ] {
            assert!(
                find_node(&history.root, id).is_none(),
                "{id} cannot occupy an idle history row"
            );
        }
        let info = dispatch_history(api::Notification::Ui(ui::UiEvent {
            revision: history.revision,
            node: "repo-info".into(),
            action: ui::Action::Click,
        }));
        info.validate()
            .expect("information dialog shares the public scene quotas");
        let dialog = info
            .dialog
            .as_ref()
            .expect("the information button opens repository details");
        assert_eq!(dialog.id, "repo-info-dialog");
        assert!(
            matches!(&find_node(&dialog.content, "repo-fetch-time").unwrap().kind,
            ui::Kind::Text { text } if text.contains("UTC"))
        );
        assert!(
            matches!(&find_node(&dialog.content, "status").unwrap().kind,
            ui::Kind::Text { text } if text == "Visible operation result / 操作结果")
        );
        assert!(
            matches!(&find_node(&dialog.content, "repo-identity").unwrap().kind,
            ui::Kind::Text { text } if text.contains("C:/history-fixture") && text.contains("linked worktree"))
        );
        assert!(
            matches!(&find_node(&dialog.content, "repo-policy").unwrap().kind,
            ui::Kind::Text { text } if text.contains(if locale == "zh-CN" { "已记住" } else { "Remembered: merge" }))
        );
        assert!(
            !find_node(&dialog.content, "repo-pull-forget")
                .unwrap()
                .disabled
        );
        let closed = dispatch_history(api::Notification::Ui(ui::UiEvent {
            revision: info.revision,
            node: "repo-close".into(),
            action: ui::Action::Click,
        }));
        assert!(closed.dialog.is_none());
        STATE.with(|cell| {
            assert!(
                cell.borrow().queue.is_empty(),
                "inspection must not initiate network or Git work"
            );
            *cell.borrow_mut() = State::default();
        });
    }
}
