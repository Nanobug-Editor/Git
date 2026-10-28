//! Workbench regressions drive guest events and complete bounded Git receipts without native UI.
use super::*;

#[path = "workbench_view_history_tests.rs"]
mod views;

#[path = "workbench_quiet_tests.rs"]
mod quiet;

#[path = "workbench_refresh_tests.rs"]
mod refresh;

#[path = "workbench_fast_switch_tests.rs"]
mod fast_switch;

#[path = "workbench_pagination_tests.rs"]
mod pagination;

/// A discovered worktree supplies the same scoped identity as the production discovery receipt.
fn workbench() -> State {
    State {
        repository: Repository {
            identity: Some(Identity {
                root: "C:/history-fixture".into(),
                git_dir: "C:/history-fixture/.git".into(),
                common_dir: "C:/history-fixture/.git".into(),
            }),
            ..Default::default()
        },
        visible_panels: BTreeMap::from([("history".into(), true)]),
        ..Default::default()
    }
}

fn handle() -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "history-fixture".into(),
        scope: "workspace".into(),
        resource: 1,
    }
}

/// Inspect the published native scene rather than duplicating its view-building decisions.
fn find_node<'a>(node: &'a ui::Node, id: &str) -> Option<&'a ui::Node> {
    if node.id == id {
        return Some(node);
    }
    match &node.kind {
        ui::Kind::Column { children } | ui::Kind::Row { children } => {
            children.iter().find_map(|child| find_node(child, id))
        }
        ui::Kind::Scroll { content } => find_node(content, id),
        ui::Kind::Editor(editor) => editor
            .left_actions()
            .iter()
            .chain(editor.right_actions())
            .find_map(|action| find_node(&action.content, id)),
        _ => None,
    }
}

/// Fixed complete records represent a repository longer than three pages; OIDs remain stable.
fn commits() -> Vec<Commit> {
    (0..385)
        .map(|index| Commit {
            oid: format!("{index:040x}"),
            parents: (index < 384)
                .then(|| format!("{:040x}", index + 1))
                .into_iter()
                .collect(),
            title: format!("Commit {index}"),
            author: "History Test".into(),
            time: "2026-10-16T00:00:00Z".into(),
            ..Default::default()
        })
        .collect()
}

/// The command boundary receives complete Git bytes; every history request must stay one page.
fn complete_history(state: &mut State, repository: &[Commit]) {
    while let Some(command) = state.queue.pop_front() {
        let output = match &command.purpose {
            Purpose::Log { .. } => {
                let argument = |prefix: &str| {
                    command
                        .args
                        .iter()
                        .find_map(|arg| arg.strip_prefix(prefix))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap()
                };
                let count = argument("--max-count=");
                assert!(count <= 128, "refresh must not request the whole history");
                repository
                    .iter()
                    .skip(argument("--skip="))
                    .take(count)
                    .map(|commit| {
                        format!(
                            "{}\0{}\0{}\0{}\0{}\0{}\0",
                            commit.oid,
                            commit.parents.join(" "),
                            commit.title,
                            commit.labels,
                            commit.author,
                            commit.time
                        )
                    })
                    .collect::<String>()
                    .into_bytes()
            }
            Purpose::HistoryBlob { column } => {
                format!("100644 blob {column:040x}\tsrc/selected.rs\0").into_bytes()
            }
            Purpose::Snapshot { column } => match column {
                0 => b"parent text\n".to_vec(),
                _ => b"selected text\n".to_vec(),
            },
            // Status/ref probes do not participate in these history observations. The separate
            // actual-package tests cover their host resources and real Git process transport.
            _ => continue,
        };
        state
            .finished(
                Running {
                    handle: handle(),
                    command,
                    output,
                    stderr: Vec::new(),
                },
                0,
            )
            .unwrap();
    }
}

/// Timer, focus, save and an arriving commit retain a three-page review and its immutable details.
#[test]
fn loaded_history_survives_automatic_refresh_and_new_commits_without_duplicate_pages() {
    let mut repository = commits();
    let mut state = workbench();
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
    assert_eq!(state.commits.len(), 384);
    state.select_history_row(260, false);
    state.queue.clear();
    state.history_files = vec!["src/selected.rs".into()];
    state.history_file("src/selected.rs");
    complete_history(&mut state, &repository);
    state.log_scroll = 7200.;
    state.tree_paths.insert(
        "files:expanded-src".into(),
        trees::TreePath::Directory("src".into()),
    );
    let selected = "0000000000000000000000000000000000000104";
    assert_eq!(state.selected, selected);
    assert_eq!(state.columns, ["parent text\n", "selected text\n"]);

    for notification in 0..4 {
        let event = match notification {
            0 => api::Notification::Timer {
                handle: handle(),
                sequence: 1,
            },
            1 => api::Notification::Focus(true),
            2 => api::Notification::DocumentEvent {
                subscription: handle(),
                event: api::DocumentEvent {
                    sequence: 1,
                    resource: None,
                    kind: api::DocumentEventKind::DidSave {
                        document: api::DocumentVersion {
                            id: "saved-document".into(),
                            path: "src/unrelated.rs".into(),
                            revision: 2,
                        },
                        result: Ok(()),
                    },
                },
            },
            _ => {
                repository.insert(
                    0,
                    Commit {
                        oid: "f".repeat(40),
                        parents: vec![repository[0].oid.clone()],
                        title: "Arriving commit".into(),
                        ..Default::default()
                    },
                );
                api::Notification::Focus(true)
            }
        };
        state.event(None, event).unwrap();
        complete_history(&mut state, &repository);
        assert_eq!(
            state.commits.len(),
            384,
            "an automatic refresh shrank the loaded review"
        );
        assert_eq!(state.selected, selected);
        assert_eq!(state.history_cursor, selected);
        assert_eq!(state.selected_commits, [selected]);
        assert_eq!(state.log_scroll, 7200.);
        assert_eq!(
            state.tree_paths["files:expanded-src"],
            trees::TreePath::Directory("src".into())
        );
        state.history_file("src/selected.rs");
        complete_history(&mut state, &repository);
        assert_eq!(state.columns, ["parent text\n", "selected text\n"]);
    }
    assert_eq!(state.commits[0].title, "Arriving commit");
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
    assert_eq!(state.commits.len(), 386);
    let unique = state
        .commits
        .iter()
        .map(|commit| &commit.oid)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        386,
        "new commits cannot duplicate a later page"
    );
}

/// A detail window owns its immutable commit source even when a new row displaces the loaded tail.
#[test]
fn selected_history_details_survive_a_commit_moving_outside_the_loaded_window() {
    let mut repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
    state.select_history_row(127, false);
    state.queue.clear();
    state.history_file("src/selected.rs");
    complete_history(&mut state, &repository);
    assert_eq!(state.columns, ["parent text\n", "selected text\n"]);
    repository.insert(
        0,
        Commit {
            oid: "f".repeat(40),
            ..Default::default()
        },
    );
    state.event(None, api::Notification::Focus(true)).unwrap();
    complete_history(&mut state, &repository);
    assert!(
        !state
            .commits
            .iter()
            .any(|commit| commit.oid == state.selected)
    );
    state.history_file("src/selected.rs");
    complete_history(&mut state, &repository);
    assert_eq!(state.columns, ["parent text\n", "selected text\n"]);
    assert_eq!(
        state.snapshot_targets[1],
        "000000000000000000000000000000000000007f"
    );
}

/// A branch filter starts at its first page; the independent open detail source remains inspectable.
#[test]
fn changing_history_filter_restarts_paging_scroll_and_range_selection() {
    let repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
    state.select_history_row(80, false);
    state.queue.clear();
    state.log_scroll = 2000.;
    state.refs = vec!["refs/heads/topic".into()];
    state.trees.insert("branches".into(), handle());
    state.tree_paths.insert(
        "branches:topic".into(),
        trees::TreePath::Leaf("local/topic".into()),
    );
    state
        .tree_event(tree::Event::Selected {
            tree: handle(),
            epoch: 1,
            node: "topic".into(),
        })
        .unwrap();
    assert_eq!(state.filter.as_deref(), Some("refs/heads/topic"));
    assert_eq!(
        state.log_scroll, 0.,
        "the new branch must begin at its visible first page"
    );
    assert!(state.selected_commits.is_empty());
    assert!(state.history_anchor.is_none());
    assert!(state.history_cursor.is_empty());
    complete_history(&mut state, &repository[..2]);
    assert_eq!(state.commits.len(), 2);
    state.history_file("src/selected.rs");
    complete_history(&mut state, &repository);
    assert_eq!(state.columns, ["parent text\n", "selected text\n"]);
    state.log_scroll = 28.;
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
    assert_eq!(state.log_scroll, 0.);
    complete_history(&mut state, &repository);
    assert_eq!(state.commits.len(), 128);
    assert!(state.filter.is_none());
}

/// A coalesced timer arriving immediately after a slow refresh leaves an idle interval for input.
#[test]
fn a_completed_refresh_does_not_let_the_next_timer_starve_user_history_actions() {
    let mut state = workbench();
    let repository = commits();
    state.refresh();
    complete_history(&mut state, &repository);
    state
        .event(
            None,
            api::Notification::Timer {
                handle: handle(),
                sequence: 1,
            },
        )
        .unwrap();
    assert!(
        state.queue.is_empty(),
        "a just-completed refresh must yield before another timer round"
    );
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
    assert_eq!(
        state.commits.len(),
        256,
        "user paging remains available during timer cooldown"
    );
    state.event(None, api::Notification::Focus(true)).unwrap();
    assert!(
        !state.queue.is_empty(),
        "explicit focus/save observations remain immediate"
    );
}

/// Refs may move between bounded reads. The overlapping boundary must not shrink the loaded range.
#[test]
fn a_commit_arriving_between_refresh_pages_preserves_a_unique_loaded_window() {
    let mut repository = commits();
    let mut state = workbench();
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
    state.refresh();
    let first_page = loop {
        let command = state.queue.pop_front().unwrap();
        if matches!(command.purpose, Purpose::Log { .. }) {
            break command;
        }
    };
    let output = repository[..128]
        .iter()
        .map(|commit| {
            format!(
                "{}\0{}\0{}\0\0\0\0",
                commit.oid,
                commit.parents.join(" "),
                commit.title
            )
        })
        .collect::<String>()
        .into_bytes();
    state
        .finished(
            Running {
                handle: handle(),
                command: first_page,
                output,
                stderr: Vec::new(),
            },
            0,
        )
        .unwrap();
    repository.insert(
        0,
        Commit {
            oid: "f".repeat(40),
            ..Default::default()
        },
    );
    complete_history(&mut state, &repository);
    assert_eq!(
        state.commits.len(),
        384,
        "one duplicate boundary cannot shrink an existing review"
    );
    let unique = state
        .commits
        .iter()
        .map(|commit| &commit.oid)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 384);
}

/// Incomplete metadata is not a replacement history, and retrying a user page cannot resume it.
#[test]
fn a_failed_refresh_keeps_the_loaded_history_and_allows_normal_user_paging() {
    let repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
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
    state.refresh();
    let command = loop {
        let command = state.queue.pop_front().unwrap();
        if matches!(command.purpose, Purpose::Log { .. }) {
            break command;
        }
    };
    assert!(
        state
            .finished(
                Running {
                    handle: handle(),
                    command,
                    output: b"incomplete\0".to_vec(),
                    stderr: Vec::new(),
                },
                0
            )
            .is_err()
    );
    assert_eq!(state.commits.len(), 256);
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
    let requested = state.queue.front().unwrap();
    assert!(
        requested.args.iter().any(|arg| arg == "--skip=256"),
        "a failed refresh cannot replace the requested next page: {:?}",
        requested.args
    );
    complete_history(&mut state, &repository);
    assert_eq!(state.commits.len(), 384);
}

/// Continuously moving refs can repeat a complete page. Exhausting the read budget keeps the last
/// complete review visible, rather than publishing a much smaller sample or polling indefinitely.
#[test]
fn continuously_moving_history_keeps_the_previous_window_when_refresh_budget_is_exhausted() {
    let repository = commits();
    let mut state = workbench();
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
    state.refresh();
    let mut reads = 0;
    while let Some(command) = state.queue.pop_front() {
        if !matches!(command.purpose, Purpose::Log { .. }) {
            continue;
        }
        reads += 1;
        assert!(
            reads <= 8,
            "a changing repository cannot cause an unbounded refresh"
        );
        // Adding one page of commits before each read can move this same old page to every skip.
        let output = repository[..128]
            .iter()
            .map(|commit| {
                format!(
                    "{}\0{}\0{}\0\0\0\0",
                    commit.oid,
                    commit.parents.join(" "),
                    commit.title
                )
            })
            .collect::<String>()
            .into_bytes();
        state
            .finished(
                Running {
                    handle: handle(),
                    command,
                    output,
                    stderr: Vec::new(),
                },
                0,
            )
            .unwrap();
    }
    assert_eq!(
        state.commits.len(),
        384,
        "an incomplete moving sample cannot replace the loaded review"
    );
    assert!(
        !state.message.is_empty(),
        "explain that the previous completed review is retained"
    );
}
