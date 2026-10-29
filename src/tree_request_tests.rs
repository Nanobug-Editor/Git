//! Scene publication policy keeps concurrent native tree-page requests on their admitted revision.
use super::*;

fn handle() -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "tree-page-test".into(),
        scope: "workspace".into(),
        resource: 1,
    }
}

fn input(event: api::Notification) -> api::Input {
    api::Input::Event {
        panel: Some("staging".into()),
        event,
    }
}

/// Observe the serialized completion at the same public guest entry point used by the manager.
fn dispatch_tree(event: tree::Event) -> api::Output {
    dispatch_ui("staging", "staging", 7, ui::Action::Tree(event))
}

/// Fixture input remains at the public serialization boundary, including its current scene token.
fn dispatch_ui(panel: &str, node: &str, revision: u64, action: ui::Action) -> api::Output {
    let invocation = api::Invocation {
        id: 1,
        message: api::Input::Event {
            panel: Some(panel.into()),
            event: api::Notification::Ui(ui::UiEvent {
                revision,
                node: node.into(),
                action,
            }),
        },
    };
    let receipt: api::Completion = serde_json::from_str(
        &feedback::dispatch(serde_json::to_string(&invocation).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt.id, 1);
    receipt.result.unwrap()
}

/// Publishing a successful resource page cannot revoke a sibling tree's current scene; a failed
/// callback still needs the normal visible diagnostic instead of silently hiding that failure.
#[test]
fn tree_page_reply_preserves_sibling_scene_revision_and_publishes_callback_errors() {
    let request = input(api::Notification::Ui(ui::UiEvent {
        revision: 7,
        node: "staging".into(),
        action: ui::Action::Tree(tree::Event::Request(tree::Request {
            tree: handle(),
            epoch: 1,
            sequence: 1,
            parent: None,
            cursor: None,
            max_nodes: 128,
        })),
    }));
    assert!(
        !repaint_after(&request, false),
        "a resource-only page reply must retain the current UI scene"
    );
    assert!(
        repaint_after(&request, true),
        "callback failures must retain a visible diagnostic"
    );
}

/// Byte-stream notifications retain the existing batching policy for both native output streams.
#[test]
fn process_output_chunks_remain_batched_without_scene_publication() {
    for stream in [
        plugin_protocol::process::Stream::Stdout,
        plugin_protocol::process::Stream::Stderr,
    ] {
        let chunk = input(api::Notification::Process {
            handle: handle(),
            update: plugin_protocol::process::Update::Output {
                stream,
                bytes: b"output".to_vec(),
            },
        });
        assert!(!repaint_after(&chunk, false));
        assert!(!repaint_after(&chunk, true));
    }
}

/// Business selection and process completion redraw. Real leaves still change their selected
/// file/filter; transport effects are exercised by the package tests through the public manager.
#[test]
fn business_input_and_process_completion_keep_publishing_their_scene() {
    for event in [
        api::Notification::Focus(true),
        api::Notification::Ui(ui::UiEvent {
            revision: 7,
            node: "refresh".into(),
            action: ui::Action::Click,
        }),
        api::Notification::Ui(ui::UiEvent {
            revision: 7,
            node: "staging".into(),
            action: ui::Action::Tree(tree::Event::Selected {
                tree: handle(),
                epoch: 1,
                node: "file".into(),
            }),
        }),
        api::Notification::Process {
            handle: handle(),
            update: plugin_protocol::process::Update::Exited { code: 0 },
        },
    ] {
        let event = input(event);
        assert!(repaint_after(&event, false));
        assert!(repaint_after(&event, true));
    }
    for tree_name in ["staging", "branches"] {
        let mut state = State {
            changes: vec![Change {
                path: "src/a-review.txt".into(),
                index: 'M',
                worktree: '.',
                ..Default::default()
            }],
            refs: vec!["refs/heads/topic".into()],
            ..Default::default()
        };
        state.trees.insert(tree_name.into(), handle());
        state.tree_paths.insert(
            format!("{tree_name}:leaf"),
            trees::TreePath::Leaf(
                if tree_name == "branches" {
                    "local/topic"
                } else {
                    "staged/src/a-review.txt"
                }
                .into(),
            ),
        );
        let changed = state
            .tree_event(tree::Event::Selected {
                tree: handle(),
                epoch: 1,
                node: "leaf".into(),
            })
            .unwrap();
        assert!(
            changed,
            "{tree_name} leaf selection must publish its business state"
        );
        if tree_name == "staging" {
            assert_eq!(state.selected_path, "src/a-review.txt");
        } else {
            assert_eq!(state.filter.as_deref(), Some("refs/heads/topic"));
        }
    }
}

/// Directory selection belongs to the native tree. Its serialized guest receipt must not
/// replace the scene while a sibling or child page is already waiting at that same revision.
#[test]
fn directory_and_ignored_selection_dispatch_preserve_the_admitted_scene() {
    for selected in ["directory", "unknown", "busy-file"] {
        STATE.with(|cell| {
            let mut state = State {
                revision: 7,
                changes: vec![Change {
                    path: "src/a-review.txt".into(),
                    worktree: 'M',
                    ..Default::default()
                }],
                ..Default::default()
            };
            state.trees.insert("staging".into(), handle());
            state.tree_paths.insert(
                "staging:directory".into(),
                trees::TreePath::Directory("unstaged/src".into()),
            );
            state.tree_paths.insert(
                "staging:busy-file".into(),
                trees::TreePath::Leaf("unstaged/src/a-review.txt".into()),
            );
            if selected == "busy-file" {
                state
                    .editor_tasks
                    .insert(9, "merge-read:another.txt".into());
            }
            *cell.borrow_mut() = state;
        });
        let output = dispatch_tree(tree::Event::Selected {
            tree: handle(),
            epoch: 1,
            node: selected.into(),
        });
        assert!(
            output.views.is_empty(),
            "{selected} has no business effect and must not invalidate allocated tree pages"
        );
        STATE.with(|cell| {
            assert_eq!(cell.borrow().revision, 7);
            *cell.borrow_mut() = State::default();
        });
    }
}

/// Reactivation or a rejected deferred measurement may repeat the same native size. Repeating
/// that observation cannot revoke admitted tree pages, while real size changes still redraw.
#[test]
fn identical_canvas_resize_dispatch_preserves_scene_and_real_changes_publish() {
    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            revision: 7,
            selected: "merge".into(),
            history_selection: Some(Commit {
                oid: "merge".into(),
                parents: vec!["parent-1".into(), "parent-2".into(), "parent-3".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
    });
    for (width, height, expected_revision, changed) in [
        (800., 180., 8, true),
        (800., 180., 8, false),
        // This fixture has no rows: new geometry is retained without republishing identical artwork.
        (801., 180., 9, false),
        (801., 181., 10, false),
        (801., 181., 10, false),
    ] {
        let current = STATE.with(|cell| cell.borrow().revision);
        let output = dispatch_ui(
            "history",
            "log",
            current,
            ui::Action::Canvas(ui::CanvasEvent::Resize {
                width,
                height,
                grid: None,
            }),
        );
        assert_eq!(
            !output.views.is_empty(),
            changed,
            "identical native measurements must not replace the scene or invalidate pending tree pages"
        );
        STATE.with(|cell| {
            let state = cell.borrow();
            assert_eq!(state.revision, expected_revision);
            assert_eq!((state.canvas_width, state.canvas_height), (width, height));
        });
    }
    // A malformed parent choice must still surface its normal diagnostic after a silent
    // observation; idempotent geometry cannot suppress the dispatcher's actual error path.
    let _ = feedback::take_notices();
    let failure = dispatch_ui(
        "commit-details",
        "history-parent-0",
        10,
        ui::Action::Select("invalid-parent".into()),
    );
    assert!(
        !failure.views.is_empty(),
        "callback failures must remain visible"
    );
    assert!(
        feedback::take_notices()
            .iter()
            .any(|notice| matches!(notice,
        plugin_protocol::interaction::Operation::PublishMessage { severity: Severity::Error, message, .. }
            if message.contains("Invalid parent selection")))
    );
    assert!(
        failure
            .views
            .iter()
            .all(|view| view.document.active_node("window-status").is_none())
    );
    STATE.with(|cell| *cell.borrow_mut() = State::default());
}

/// Native wheels carry pixel deltas, unlike scrollbar receipts which carry absolute offsets.
/// The public dispatch must move the bounded history without changing its immutable selection.
#[test]
fn native_history_wheel_dispatch_scrolls_pixels_and_stops_at_loaded_page_boundaries() {
    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            revision: 7,
            canvas_height: 112.,
            commits: (0..20)
                .map(|index| Commit {
                    oid: format!("{index:040x}"),
                    title: format!("wheel row {index}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
    });
    for (delta_y, expected, changed) in [
        (-160., 160., true),
        (40., 120., true),
        (-10_000., 448., true),
        (-1., 448., false),
        (10_000., 0., true),
        (1., 0., false),
        (0., 0., false),
    ] {
        let revision = STATE.with(|cell| cell.borrow().revision);
        let output = dispatch_ui(
            "history",
            "log",
            revision,
            ui::Action::Canvas(ui::CanvasEvent::Wheel {
                x: 100.,
                y: 40.,
                delta_x: 0.,
                delta_y,
                shift: false,
            }),
        );
        STATE.with(|cell| {
            let state = cell.borrow();
            assert_eq!(state.log_scroll, expected, "native wheel delta {delta_y}");
            assert!(state.history_selection.is_none());
            assert!(state.queue.is_empty() && state.running.is_none());
        });
        assert_eq!(
            !output.views.is_empty(),
            changed,
            "a boundary wheel cannot invalidate unrelated admitted tree pages"
        );
    }
    STATE.with(|cell| *cell.borrow_mut() = State::default());
}

/// Growing the real body near its bottom must open the row now drawn under the pointer,
/// rather than retaining the smaller viewport's obsolete absolute scroll position.
#[test]
fn resized_history_pointer_opens_the_commit_drawn_at_the_same_y() {
    use plugin_protocol::Paint;

    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            revision: 7,
            canvas_width: 800.,
            canvas_height: 56.,
            commits: (0..20)
                .map(|index| Commit {
                    oid: format!("{index:040x}"),
                    title: format!("Review {index}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
    });
    let bottom = dispatch_ui(
        "history",
        "log",
        7,
        ui::Action::Canvas(ui::CanvasEvent::Scroll { offset: 504. }),
    );
    let bottom = &bottom
        .views
        .iter()
        .find(|view| view.panel == "history")
        .unwrap()
        .document;
    let resized = dispatch_ui(
        "history",
        "log",
        bottom.revision,
        ui::Action::Canvas(ui::CanvasEvent::Resize {
            width: 800.,
            height: 280.,
            grid: None,
        }),
    );
    let scene = &resized
        .views
        .iter()
        .find(|view| view.panel == "history")
        .unwrap()
        .document;
    scene.validate().expect("the resized scene is publishable");
    let ui::Kind::Canvas(canvas) = &scene.active_node("log").unwrap().kind else {
        panic!("history canvas")
    };
    assert_eq!(canvas.scroll.as_ref().unwrap().offset, 280.);
    assert!(
        canvas.paint.iter().any(|paint| matches!(paint,
            Paint::TextBox { rect, text, .. } if rect.y == 5. && text == "Review 10"
        )),
        "Review 10 is the first visible row at the pointer's y=14"
    );
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // As in the adjacent Down test, inspect the same admitted handler's real query intent
        // before pump would cross the native unit test's unavailable WASM process transport.
        assert!(state.history_canvas(ui::CanvasEvent::Pointer {
            phase: ui::PointerPhase::Down,
            x: 100.,
            y: 14.,
            button: 0,
            clicks: 1,
            shift: false,
        }));
        let expected = format!("{:040x}", 10);
        assert_eq!(
            state.selected, expected,
            "the pointer must open the commit actually painted here"
        );
        assert_eq!(state.selected_commits, [expected.clone()]);
        assert_eq!(state.history_selection.as_ref().unwrap().oid, expected);
        assert_eq!(state.log_scroll, 280.);
        assert_eq!(state.queue.len(), 1);
        assert!(matches!(&state.queue[0].purpose, Purpose::Files(oid) if oid == &expected));
        assert!(state.queue[0].args.contains(&expected));
        state.queue.clear();
    });
    STATE.with(|cell| *cell.borrow_mut() = State::default());
}

/// Native mouse-down first focuses its Canvas and then emits Down at the same admitted scene.
/// Observations with no Git behavior cannot consume that scene before the meaningful pointer.
#[test]
fn native_canvas_observations_preserve_scene_for_the_following_history_pointer() {
    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            revision: 7,
            canvas_height: 112.,
            commits: (0..20)
                .map(|index| Commit {
                    oid: format!("{index:040x}"),
                    title: format!("pointer row {index}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
    });
    let pointer = |phase| ui::CanvasEvent::Pointer {
        phase,
        x: 100.,
        y: 40.,
        button: 0,
        clicks: 1,
        shift: false,
    };
    for event in [
        ui::CanvasEvent::Focus { focused: true },
        ui::CanvasEvent::Focus { focused: false },
        pointer(ui::PointerPhase::Move),
        pointer(ui::PointerPhase::Up),
        ui::CanvasEvent::Text {
            text: "历史中文".into(),
        },
        ui::CanvasEvent::Scroll { offset: 0. },
        ui::CanvasEvent::Key {
            key: "x".into(),
            ctrl: false,
            alt: false,
            shift: false,
        },
        ui::CanvasEvent::Key {
            key: "down".into(),
            ctrl: true,
            alt: false,
            shift: false,
        },
        ui::CanvasEvent::Key {
            key: "down".into(),
            ctrl: false,
            alt: true,
            shift: false,
        },
    ] {
        let label = format!("{event:?}");
        let output = dispatch_ui("history", "log", 7, ui::Action::Canvas(event));
        assert!(
            output.views.is_empty(),
            "{label} has no history business effect and must retain the pointer's admitted scene"
        );
        STATE.with(|cell| {
            let state = cell.borrow();
            assert_eq!(state.revision, 7, "{label}");
            assert!(state.selected.is_empty() && state.selected_commits.is_empty());
            assert!(state.history_selection.is_none());
            assert!(state.queue.is_empty() && state.running.is_none());
        });
    }
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // A meaningful selection queues real Git I/O. Native unit tests have no WASM host;
        // verify that intent here, leaving dispatch/pump execution to the actual-package test.
        assert!(state.history_canvas(pointer(ui::PointerPhase::Down)));
        assert!(!state.views().is_empty());
        assert_eq!(state.revision, 8);
        assert_eq!(state.selected, format!("{:040x}", 1));
        assert_eq!(state.selected_commits, [format!("{:040x}", 1)]);
        assert_eq!(
            state.history_selection.as_ref().unwrap().oid,
            state.selected
        );
        assert_eq!(state.queue.len(), 1);
        assert!(matches!(&state.queue[0].purpose, Purpose::Files(oid) if oid == &state.selected));
        state.queue.clear();
    });
    assert_history_canvas_business_publication(pointer(ui::PointerPhase::Down));
    STATE.with(|cell| *cell.borrow_mut() = State::default());
}

/// Continue the real pointer receipt at revision 8: scrolling/navigation publish their changes
/// while updating the inline preview OID; busy/invalid pointers and boundary keys retain the scene.
fn assert_history_canvas_business_publication(pointer: ui::CanvasEvent) {
    let scroll = dispatch_ui(
        "history",
        "log",
        8,
        ui::Action::Canvas(ui::CanvasEvent::Scroll { offset: 28. }),
    );
    assert!(!scroll.views.is_empty());
    let down = ui::CanvasEvent::Key {
        key: "down".into(),
        ctrl: false,
        alt: false,
        shift: false,
    };
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // As with the pointer test above, stop before pump crosses the native WASM transport.
        assert!(state.history_canvas(down.clone()));
        assert!(!state.views().is_empty());
        assert!(
            matches!(&state.queue[0].purpose, Purpose::Files(oid) if oid == &format!("{:040x}", 2))
        );
        state.queue.clear();
        assert_eq!(state.revision, 10);
        assert_eq!(state.log_scroll, 28.);
        assert_eq!(state.history_cursor, format!("{:040x}", 2));
        assert_eq!(state.selected, format!("{:040x}", 2));
        state.editor_tasks.insert(99, "merge-read:busy.txt".into());
    });
    for event in [pointer, down] {
        assert!(
            dispatch_ui("history", "log", 10, ui::Action::Canvas(event))
                .views
                .is_empty(),
            "a busy history ignores selection without publishing an unchanged scene"
        );
    }
    STATE.with(|cell| cell.borrow_mut().editor_tasks.clear());
    let outside = dispatch_ui(
        "history",
        "log",
        10,
        ui::Action::Canvas(ui::CanvasEvent::Pointer {
            phase: ui::PointerPhase::Down,
            x: 100.,
            y: 10_000.,
            button: 0,
            clicks: 1,
            shift: false,
        }),
    );
    assert!(outside.views.is_empty());
    let home = ui::CanvasEvent::Key {
        key: "home".into(),
        ctrl: false,
        alt: false,
        shift: false,
    };
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        assert!(state.history_canvas(home.clone()));
        assert!(!state.views().is_empty());
        assert!(
            matches!(&state.queue[0].purpose, Purpose::Files(oid) if oid == &format!("{:040x}", 0))
        );
        state.queue.clear();
    });
    assert!(
        dispatch_ui("history", "log", 11, ui::Action::Canvas(home))
            .views
            .is_empty(),
        "repeating Home at its current row has no business effect"
    );
    STATE.with(|cell| {
        assert_eq!(cell.borrow().revision, 11);
        assert_eq!(cell.borrow().selected, format!("{:040x}", 0));
    });
}
