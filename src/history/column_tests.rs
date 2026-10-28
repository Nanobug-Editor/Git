//! Column gestures exercise the same public canvas input used by native pointer capture.
use super::*;

/// Keep an admitted commit and scroll position so resizing cannot masquerade as row navigation.
fn state() -> State {
    State {
        canvas_width: 1400.,
        canvas_height: 280.,
        log_scroll: 84.,
        selected: "keep".into(),
        commits: (0..40)
            .map(|i| Commit {
                oid: format!("{i:040x}"),
                parents: vec![],
                title: "Subject".into(),
                labels: "origin/main".into(),
                author: "Tmiracle".into(),
                time: "2026-10-09T00:47:26+08:00".into(),
            })
            .collect(),
        ..Default::default()
    }
}

/// The native viewport owns subject flexing and whole-row selection while guest width replies lag.
#[test]
fn history_columns_declare_native_viewport_and_full_row_selection() {
    let mut state = state();
    state.selected_commits = vec![state.commits[4].oid.clone()];
    let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
        panic!("history canvas")
    };
    let columns = canvas.column_resize.as_ref().unwrap();
    let viewport = columns.viewport.as_ref().expect("native viewport layout");
    assert_eq!(viewport.width, state.canvas_width);
    assert_eq!(viewport.flexible, 0);
    assert_eq!(viewport.span_paints.len(), 1);
    let Paint::Fill { rect, .. } = &canvas.paint[viewport.span_paints[0]] else {
        panic!("selected row fill")
    };
    assert_eq!(rect.w, state.canvas_width);
}

/// A narrow Resize reply must retain uncompressed preferences; only native projection compresses them.
#[test]
fn history_columns_narrow_replies_preserve_reference_preferences() {
    for (dense, trailing) in [
        (false, None),
        (true, None),
        (true, Some([86.392, 93.23221, 310.36874])),
    ] {
        let mut state = state();
        state.history_columns.trailing = trailing;
        if dense {
            // Unresolved distinct parents keep many lanes alive, crossing the old viewport gutter cap.
            for (index, commit) in state.commits.iter_mut().enumerate() {
                commit.parents = vec![format!("parent-{index}")];
            }
        }
        state.selected_commits = vec![state.commits[4].oid.clone()];
        let ui::Kind::Canvas(wide) = state.log_canvas().kind else {
            panic!("history canvas")
        };
        let original = wide.column_resize.unwrap();
        for width in [500., 340., 300., 1400.] {
            state.resize_history(width, 280.);
            let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
                panic!("history canvas")
            };
            let columns = canvas.column_resize.as_ref().unwrap();
            assert_eq!(
                columns.x, original.x,
                "late widths cannot move the topology gutter"
            );
            assert_eq!(&columns.widths[1..], &original.widths[1..]);
            assert_eq!(columns.minimum, original.minimum);
            assert_eq!(columns.minimum, [104., 40., 24., 100.]);
            let viewport = columns.viewport.as_ref().unwrap();
            assert!(viewport.width >= width);
            let Paint::Fill { rect, .. } = &canvas.paint[viewport.span_paints[0]] else {
                panic!("selected row fill")
            };
            assert_eq!(
                rect.w, viewport.width,
                "background and columns share one reference"
            );
            let visible = state.history_column_edges();
            assert!((visible[4] - (width - 8.)).abs() < 0.001);
        }
    }
}

/// Send native-equivalent left-button coordinates without starting a host/Git process.
fn pointer(state: &mut State, phase: ui::PointerPhase, x: f32) -> bool {
    state.history_canvas(ui::CanvasEvent::Pointer {
        phase,
        x,
        y: 20.,
        button: 0,
        clicks: 1,
        shift: false,
    })
}

/// All three dividers resize their adjacent columns without selecting rows or changing vertical content.
#[test]
fn history_columns_resize_each_pair_and_preserve_selection_scroll_epoch() {
    for divider in 0..3 {
        let mut state = state();
        state.log_canvas();
        let epoch = state.history_drawing.epoch;
        let initial = state.history_column_edges();
        let at = initial[divider + 1];
        assert!(!pointer(&mut state, ui::PointerPhase::Down, at));
        assert!(pointer(&mut state, ui::PointerPhase::Move, at + 24.));
        let moved = state.history_column_edges();
        for i in 0..5 {
            assert!((moved[i] - initial[i] - if i == divider + 1 { 24. } else { 0. }).abs() < 0.01);
        }
        pointer(&mut state, ui::PointerPhase::Up, at + 24.);
        assert!(!pointer(&mut state, ui::PointerPhase::Move, at + 80.));
        state.log_canvas();
        assert_eq!(state.history_drawing.epoch, epoch);
        assert_eq!(state.selected, "keep");
        assert_eq!(state.log_scroll, 84.);
        assert!(state.queue.is_empty());
    }
}

/// An overdrag can return normally, and blur cancels ownership without erasing chosen widths.
#[test]
fn history_columns_clamp_reverse_and_cancel_on_blur() {
    let mut state = state();
    state.log_canvas();
    let edges = state.history_column_edges();
    pointer(&mut state, ui::PointerPhase::Down, edges[2]);
    pointer(&mut state, ui::PointerPhase::Move, 100_000.);
    let clamped = state.history_column_edges();
    assert!(clamped.windows(2).all(|pair| pair[1] > pair[0]));
    pointer(&mut state, ui::PointerPhase::Move, edges[2] - 20.);
    assert!((state.history_column_edges()[2] - edges[2] + 20.).abs() < 0.01);
    state.history_canvas(ui::CanvasEvent::Focus { focused: false });
    assert!(!pointer(&mut state, ui::PointerPhase::Move, 1.));
    state.resize_history(300., 280.);
    assert!(
        state
            .history_column_edges()
            .windows(2)
            .all(|pair| pair[1] > pair[0])
    );
}

/// Public paint retains a complete formatted timestamp, a bold author and compact metadata grouping.
#[test]
fn history_columns_publish_compact_bold_author_and_full_time() {
    let mut state = state();
    let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
        panic!("canvas");
    };
    let row: Vec<_> = canvas
        .paint
        .iter()
        .filter(|paint| match paint {
            Paint::Text { y, .. } => *y == 5.,
            Paint::TextBox { rect, .. } => rect.y == 5.,
            _ => false,
        })
        .collect();
    assert_eq!(row.len(), 4);
    let edges = state.history_column_edges();
    for (index, expected) in [
        (1, ui::Alignment::End),
        (2, ui::Alignment::Start),
        (3, ui::Alignment::Start),
    ] {
        let Paint::TextBox {
            rect,
            align,
            text,
            bold,
            ..
        } = row[index]
        else {
            panic!("metadata must use native-aligned regions")
        };
        assert_eq!(*align, expected);
        assert_eq!(rect.x, edges[index] + 4.);
        assert_eq!(rect.x + rect.w, edges[index + 1] - 4.);
        if index == 2 {
            assert_eq!(text, "Tmiracle");
            assert!(bold);
        }
        if index == 3 {
            assert_eq!(text, "2026-10-09 00:47:26");
        }
    }
    assert!(
        !canvas
            .paint
            .iter()
            .any(|paint| matches!(paint, Paint::Fill { rect, .. } if rect.w == 1.)),
        "column separators must remain invisible"
    );
    assert_eq!(
        columns::display_time("2026-10-09T00:47:26Z"),
        "2026-10-09 00:47:26"
    );
    assert_eq!(columns::display_time("unknown"), "unknown");
}

/// Snapshot geometry survives hot replacement; malformed or unbounded widths cannot break layout.
#[test]
fn history_columns_restore_only_bounded_widths() {
    let mut columns = Columns::default();
    columns.restore(Some(&serde_json::json!([180., 90., 154.])));
    assert_eq!(columns.trailing, Some([180., 90., 154.]));
    for value in [
        serde_json::json!([-1., 90., 154.]),
        serde_json::json!([1e9, 90., 154.]),
        serde_json::json!("wrong"),
    ] {
        columns.restore(Some(&value));
        assert!(columns.trailing.is_none());
    }
}

/// Public native measurements close both metadata gaps to eight pixels and stay history-local.
#[test]
fn history_columns_use_native_advances_and_ignore_late_measurements() {
    let mut state = state();
    let ui::Kind::Canvas(initial) = state.log_canvas().kind else {
        panic!("canvas")
    };
    let widths: Vec<_> = initial
        .measure_text
        .iter()
        .map(|request| ui::TextWidth {
            id: request.id.clone(),
            width: if request.bold { 48. } else { 118. },
        })
        .collect();
    assert_eq!(widths.len(), 2);
    assert!(state.history_canvas(ui::CanvasEvent::TextMeasured {
        widths: widths.clone()
    }));
    let epoch = state.history_drawing.epoch;
    let ui::Kind::Canvas(measured) = state.log_canvas().kind else {
        panic!("canvas")
    };
    assert!(measured.measure_text.is_empty());
    let rects: Vec<_> = measured
        .paint
        .iter()
        .filter_map(|paint| match paint {
            Paint::TextBox { rect, .. } if rect.y == 5. => Some(rect),
            _ => None,
        })
        .collect();
    assert_eq!(rects.len(), 4);
    // Subject is now a native text region too; the three metadata regions retain their spacing.
    assert_eq!(rects[2].w, 48.);
    assert_eq!(rects[2].x - (rects[1].x + rects[1].w), 8.);
    assert_eq!(rects[3].x - (rects[2].x + 48.), 8.);
    assert_eq!(state.history_drawing.epoch, epoch);
    assert_eq!((state.selected.as_str(), state.log_scroll), ("keep", 84.));
    assert!(state.queue.is_empty());
    assert!(!state.history_canvas(ui::CanvasEvent::TextMeasured {
        widths: widths.clone()
    }));
    // A narrow window can give spare label width to the real author/time advances before clipping.
    state.resize_history(500., 280.);
    let ui::Kind::Canvas(narrow) = state.log_canvas().kind else {
        panic!("canvas")
    };
    let rects: Vec<_> = narrow
        .paint
        .iter()
        .filter_map(|paint| match paint {
            Paint::TextBox { rect, .. } if rect.y == 5. => Some(rect),
            _ => None,
        })
        .collect();
    assert_eq!(rects[2].w, 48.);
    assert_eq!(rects[3].w, 118.);
    state.resize_history(1400., 280.);
    // Font changes ask again; a prior face's result cannot alter the new automatic layout.
    state.environment.ui_font.family = Some("Consolas".into());
    state.log_canvas();
    assert!(!state.history_canvas(ui::CanvasEvent::TextMeasured { widths }));
    assert_eq!(state.history_drawing.epoch, epoch);
    let edges = state.history_column_edges();
    pointer(&mut state, ui::PointerPhase::Down, edges[2]);
    pointer(&mut state, ui::PointerPhase::Move, edges[2] - 24.);
    pointer(&mut state, ui::PointerPhase::Up, edges[2] - 24.);
    let manual = state.history_column_edges();
    let ui::Kind::Canvas(pending) = state.log_canvas().kind else {
        panic!("canvas")
    };
    assert!(
        state.history_canvas(ui::CanvasEvent::TextMeasured {
            widths: pending
                .measure_text
                .iter()
                .map(|request| ui::TextWidth {
                    id: request.id.clone(),
                    width: 200.
                })
                .collect()
        })
    );
    assert_eq!(state.history_column_edges(), manual);
}

/// Loading many authors uses bounded batches; ordinary scrolling never measures the loaded set again.
#[test]
fn history_columns_measure_unique_authors_incrementally_without_scroll_churn() {
    let mut state = state();
    state.commits = (0..180)
        .map(|index| Commit {
            author: format!("Author {index}"),
            oid: format!("{index:040x}"),
            ..Default::default()
        })
        .collect();
    let mut measured = std::collections::BTreeSet::new();
    for maximum in [150., 100., 120.] {
        let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
            panic!("canvas")
        };
        let author = canvas
            .measure_text
            .iter()
            .find(|request| request.bold)
            .unwrap();
        assert!(author.texts.len() <= 64);
        for text in &author.texts {
            assert!(measured.insert(text.clone()));
        }
        assert!(
            state.history_canvas(ui::CanvasEvent::TextMeasured {
                widths: canvas
                    .measure_text
                    .iter()
                    .map(|request| ui::TextWidth {
                        id: request.id.clone(),
                        width: if request.bold { maximum } else { 118. },
                    })
                    .collect(),
            })
        );
    }
    assert_eq!(measured.len(), 180);
    let before = state.history_column_edges();
    assert_eq!(before[3] - before[2], 158.);
    state.wheel_history(-28.);
    let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
        panic!("canvas")
    };
    assert!(canvas.measure_text.is_empty());
    assert_eq!(state.history_column_edges(), before);
}
