//! Restored native layout must not abandon the first log before its complete page arrives.
use super::*;

/// Exercise the dispatch cancellation boundary between queued local reads, before launching the
/// next child. No timer or synthetic Git success can hide a first-page request that was lost.
#[test]
fn startup_history_survives_passive_canvas_receipts() {
    for event in [
        ui::CanvasEvent::Resize {
            width: 1000.,
            height: 280.,
            grid: None,
        },
        ui::CanvasEvent::TextMeasured {
            widths: vec![ui::TextWidth {
                id: "history-author".into(),
                width: 80.,
            }],
        },
        ui::CanvasEvent::Focus { focused: true },
        ui::CanvasEvent::Scroll { offset: 0. },
    ] {
        let mut state = workbench();
        state.request_observation();
        assert!(
            state
                .queue
                .iter()
                .any(|c| matches!(c.purpose, Purpose::Log { .. }))
        );
        let original_queries: Vec<_> = state.queue.iter().map(|c| c.args.clone()).collect();
        let input = api::Input::Event {
            panel: Some("history".into()),
            event: api::Notification::Ui(ui::UiEvent {
                revision: state.revision,
                node: "log".into(),
                action: ui::Action::Canvas(event.clone()),
            }),
        };
        if observation::interrupts(&input) {
            state.cancel_automatic_observation().unwrap();
        }
        state.history_canvas(event.clone());
        state.resume_pending_history_filter();
        assert_eq!(
            state
                .queue
                .iter()
                .map(|c| c.args.clone())
                .collect::<Vec<_>>(),
            original_queries,
            "{event:?}: passive presentation must retain the original reads without restarting them"
        );
        assert!(
            state
                .queue
                .iter()
                .any(|c| matches!(c.purpose, Purpose::Log { .. })),
            "{event:?}: restored layout left an empty history with no queued first page; it would wait for the 30-second observer"
        );
        complete_history(&mut state, &commits());
        assert_eq!(state.commits.len(), 128);
    }
}

/// Actual input can preempt a read; completing its serialized business work must immediately
/// restore an unadmitted first page rather than depend on another focus event or timer tick.
#[test]
fn startup_history_resumes_after_a_preempting_user_operation() {
    let mut state = workbench();
    state.request_observation();
    state.cancel_automatic_observation().unwrap();
    state
        .queue
        .push_back(Command::new(["commit"], Purpose::Action));
    state.resume_pending_history_filter();
    assert_eq!(
        state.queue.len(),
        1,
        "first-page demand must not bypass a write"
    );
    state.queue.pop_front();
    state.resume_pending_history_filter();
    let command = state
        .queue
        .front()
        .expect("first page must resume immediately after the operation");
    assert!(matches!(command.purpose, Purpose::Log { .. }));
    assert!(command.args.iter().any(|arg| arg == "--skip=0"));
    complete_history(&mut state, &commits());
    assert_eq!(state.commits.len(), 128);
}

/// A complete empty result is authoritative, and a failed log waits for a deliberate retry.
/// Neither case may turn the new first-page demand into an unbounded process-launch loop.
#[test]
fn startup_history_empty_and_failed_pages_stop_retrying() {
    for success in [true, false] {
        let mut state = workbench();
        state.refresh_log();
        let command = state.queue.pop_front().unwrap();
        state
            .finished(
                Running {
                    handle: handle(),
                    command,
                    output: Vec::new(),
                    stderr: b"fixture failure".to_vec(),
                },
                if success { 0 } else { 128 },
            )
            .unwrap();
        state.resume_pending_history_filter();
        assert!(state.queue.is_empty());
        assert!(!state.history_cache.pending);
    }
}

/// Restoring several surfaces can report focus and visibility repeatedly while the first read is
/// running. Their existing coalesced observation must preserve the queue instead of restarting it.
#[test]
fn startup_history_coalesces_restored_surface_observations() {
    let mut state = workbench();
    state.request_observation();
    let original_queries: Vec<_> = state.queue.iter().map(|c| c.args.clone()).collect();
    for event in [
        api::Notification::Focus(true),
        api::Notification::PanelVisibility {
            panel: "history".into(),
            visible: true,
        },
        api::Notification::Theme(state.environment.clone()),
    ] {
        let input = api::Input::Event {
            panel: None,
            event: event.clone(),
        };
        assert!(!observation::interrupts(&input));
        state.event(None, event).unwrap();
    }
    assert!(state.passive_refresh_pending);
    assert_eq!(
        state
            .queue
            .iter()
            .map(|c| c.args.clone())
            .collect::<Vec<_>>(),
        original_queries
    );
}
