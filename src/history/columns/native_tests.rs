//! Completed native gestures update presentation widths without starting repository work.
use super::*;

#[test]
fn native_column_release_preserves_navigation_and_rejects_retired_content() {
    let mut state = State {
        canvas_width: 1200.,
        canvas_height: 300.,
        selected: "keep".into(),
        log_scroll: 84.,
        ..Default::default()
    };
    state.log_canvas();
    let epoch = state.history_drawing.epoch;
    let edges = state.history_column_edges();
    let mut widths: Vec<_> = edges.windows(2).map(|v| v[1] - v[0]).collect();
    widths[1] += 4.;
    widths[2] -= 4.;
    assert!(state.history_canvas(ui::CanvasEvent::ColumnsResized {
        epoch,
        widths: widths.clone()
    }));
    let after = state.history_column_edges();
    assert!((after[2] - edges[2] - 4.).abs() < 0.1);
    assert_eq!((state.selected.as_str(), state.log_scroll), ("keep", 84.));
    assert!(state.queue.is_empty());
    assert!(!state.history_canvas(ui::CanvasEvent::ColumnsResized {
        epoch: epoch + 1,
        widths
    }));
}
