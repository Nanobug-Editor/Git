//! Real input and completed business work reserve one idle interval before periodic local refresh.
use super::*;
use std::time::{Duration, Instant};

fn ready(now: Instant) -> State {
    let mut state = workbench();
    state.refresh_timer = Some(handle());
    state.document_subscription = Some(handle());
    state.revision = 7;
    state.history_refreshed_at = Some(now - Duration::from_secs(31));
    state.commits = (0..2)
        .map(|index| Commit {
            oid: format!("{index:040x}"),
            title: format!("Review {index}"),
            ..Default::default()
        })
        .collect();
    state.canvas_height = 180.;
    state.history_cursor = state.commits[0].oid.clone();
    // These quiet-interval cases start with an already loaded preview; boundary keys must
    // express activity without crossing the unavailable native WASM process transport.
    state.selected = state.history_cursor.clone();
    state.history_selection = Some(state.commits[0].clone());
    state.selected_commits = vec![state.history_cursor.clone()];
    state.history_anchor = Some(state.history_cursor.clone());
    state
}

/// Drive the actual guest dispatcher without any native call: these inputs only select or finish
/// already owned source work. Timer queue observations below intentionally stop before process launch.
fn dispatch(state: State, event: api::Notification) -> (State, api::Output) {
    STATE.with(|cell| *cell.borrow_mut() = state);
    let input = api::Invocation {
        id: 1,
        message: api::Input::Event {
            panel: Some("history".into()),
            event,
        },
    };
    let completion: api::Completion =
        serde_json::from_str(&feedback::dispatch(serde_json::to_string(&input).unwrap()).unwrap())
            .unwrap();
    let output = completion.result.unwrap();
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    (state, output)
}

fn canvas(event: ui::CanvasEvent) -> api::Notification {
    api::Notification::Ui(ui::UiEvent {
        revision: 7,
        node: "log".into(),
        action: ui::Action::Canvas(event),
    })
}

fn source(state: &mut State) {
    state.running = Some(Running {
        handle: handle(),
        command: Command::new(
            ["cat-file", "blob", "fixture"],
            Purpose::Snapshot { column: 0 },
        ),
        output: b"authoritative source\n".to_vec(),
        stderr: Vec::new(),
    });
}

fn exited() -> api::Notification {
    api::Notification::Process {
        handle: handle(),
        update: plugin_protocol::process::Update::Exited { code: 0 },
    }
}

fn explicit(kind: &str) -> api::Notification {
    match kind {
        "focus" => api::Notification::Focus(true),
        "panel" => api::Notification::PanelVisibility {
            panel: "history".into(),
            visible: true,
        },
        "save" => api::Notification::DocumentEvent {
            subscription: handle(),
            event: api::DocumentEvent {
                sequence: 1,
                resource: None,
                kind: api::DocumentEventKind::DidSave {
                    document: api::DocumentVersion {
                        id: "owned-document".into(),
                        path: "tracked.txt".into(),
                        revision: 2,
                    },
                    result: Ok(()),
                },
            },
        },
        "manual" => api::Notification::Ui(ui::UiEvent {
            revision: 7,
            node: "refresh".into(),
            action: ui::Action::Click,
        }),
        _ => unreachable!(),
    }
}

/// Boundary keys/wheels still express intent when their selection or scroll cannot move.
/// Skipped timer checks cannot perpetually extend that interval; a later idle tick must refresh.
#[test]
fn quiet_user_activity_defers_timer_until_a_complete_idle_interval() {
    for gesture in ["home", "end", "wheel", "action"] {
        let now = Instant::now();
        let mut state = ready(now);
        if gesture == "end" {
            state.history_cursor = state.commits[1].oid.clone();
            state.selected = state.history_cursor.clone();
            state.history_selection = Some(state.commits[1].clone());
            state.selected_commits = vec![state.history_cursor.clone()];
            state.history_anchor = Some(state.history_cursor.clone());
        }
        let event = match gesture {
            "home" | "end" => canvas(ui::CanvasEvent::Key {
                key: gesture.into(),
                ctrl: false,
                alt: false,
                shift: false,
            }),
            "wheel" => canvas(ui::CanvasEvent::Wheel {
                x: 10.,
                y: 10.,
                delta_x: 0.,
                delta_y: 1.,
                shift: false,
            }),
            _ => api::Notification::Ui(ui::UiEvent {
                revision: 7,
                node: "advanced-open".into(),
                action: ui::Action::Click,
            }),
        };
        let (mut state, _) = dispatch(state, event);
        let observed = Instant::now();
        assert!(state.queue.is_empty());
        assert!(
            !state.automatic_refresh_at(observed + Duration::from_secs(2)),
            "{gesture}: recent real input must defer periodic work"
        );
        assert!(state.queue.is_empty());
        assert!(
            state.automatic_refresh_at(observed + Duration::from_secs(4)),
            "{gesture}: local refresh must resume after a complete quiet interval"
        );
        assert!(
            state
                .queue
                .iter()
                .any(|command| command.args[0] == "status")
        );
        assert!(state.queue.iter().all(|command| command.args[0] != "fetch"));
    }
}

/// Finishing an authorized source query is a new idle boundary even when the log itself is older.
#[test]
fn quiet_business_completion_restarts_the_timer_idle_interval() {
    let now = Instant::now();
    let mut state = ready(now);
    source(&mut state);
    let (mut state, output) = dispatch(state, exited());
    let observed = Instant::now();
    assert!(
        !output.views.is_empty(),
        "authoritative source completion still publishes"
    );
    assert_eq!(state.columns, ["authoritative source\n"]);
    assert!(!state.busy());
    assert!(!state.automatic_refresh_at(observed + Duration::from_secs(2)));
    assert!(state.queue.is_empty());
    assert!(state.automatic_refresh_at(observed + Duration::from_secs(4)));
    assert!(
        state
            .queue
            .iter()
            .any(|command| command.args[0] == "status")
    );
}

/// A busy timer must retain both the current scene and the in-flight request, without a retry bit.
#[test]
fn quiet_busy_timer_dispatch_preserves_the_admitted_scene() {
    let mut state = ready(Instant::now());
    source(&mut state);
    let (state, output) = dispatch(
        state,
        api::Notification::Timer {
            handle: handle(),
            sequence: 1,
        },
    );
    assert!(output.views.is_empty());
    assert_eq!(state.revision, 7);
    assert!(state.running.is_some());
    assert!(state.queue.is_empty());
}

/// Layout, scrollbar and passive focus observations do not count as new explicit input.
#[test]
fn quiet_passive_canvas_observations_do_not_delay_a_due_timer() {
    for event in [
        ui::CanvasEvent::Resize {
            width: 800.,
            height: 180.,
            grid: None,
        },
        ui::CanvasEvent::Focus { focused: true },
        ui::CanvasEvent::Scroll { offset: 0. },
    ] {
        let now = Instant::now();
        let (mut state, _) = dispatch(ready(now), canvas(event));
        assert!(state.automatic_refresh_at(now + Duration::from_millis(10)));
        assert!(
            state
                .queue
                .iter()
                .any(|command| command.args[0] == "status")
        );
    }
}

/// Explicit observations bypass periodic cooldowns; their first local query remains immediate.
#[test]
fn quiet_explicit_focus_save_and_manual_refresh_are_immediate() {
    for kind in ["focus", "save", "manual", "panel"] {
        let now = Instant::now();
        let state = ready(now);
        let (mut state, _) = dispatch(
            state,
            canvas(ui::CanvasEvent::Key {
                key: "home".into(),
                ctrl: false,
                alt: false,
                shift: false,
            }),
        );
        state.event(Some("history"), explicit(kind)).unwrap();
        assert!(
            state
                .queue
                .iter()
                .any(|command| command.args[0] == "status"),
            "{kind}: an explicit refresh cannot wait for periodic quiet"
        );
    }
}

/// Repeated explicit requests during a business query coalesce into one local refresh at its end.
#[test]
fn quiet_explicit_requests_during_business_resume_once_when_it_finishes() {
    for kind in ["focus", "save", "manual", "panel"] {
        let mut state = ready(Instant::now());
        source(&mut state);
        for _ in 0..2 {
            state.event(Some("history"), explicit(kind)).unwrap();
        }
        assert!(state.queue.is_empty());
        state.event(None, exited()).unwrap();
        assert_eq!(
            state
                .queue
                .iter()
                .filter(|command| command.args[0] == "status")
                .count(),
            1,
            "{kind}: one deferred explicit observation must run when business becomes idle"
        );
    }
}
