//! Measure actual page admission and scroll publication without depending on machine speed.
use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Instant,
};

/// Count requested bytes only on the measured test thread; parallel tests retain the system allocator.
struct MeasuredAllocator;
thread_local! {
    static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
}
#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

/// Allocation callbacks must neither allocate nor panic, including during thread-local teardown.
fn allocated(bytes: usize) {
    let _ = ALLOCATED.try_with(|count| {
        if let Some(previous) = count.get() {
            count.set(Some(previous.saturating_add(bytes)));
        }
    });
}

// SAFETY: all pointers, sizes and alignments are delegated unchanged to the system allocator.
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Restoring the previous scope on unwind keeps a failed test from contaminating later counts.
fn measure(action: impl FnOnce()) -> usize {
    struct Reset(Option<usize>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATED.with(|count| count.set(self.0));
        }
    }
    let _reset = Reset(ALLOCATED.with(|count| count.replace(Some(0))));
    action();
    ALLOCATED.with(|count| count.get().unwrap())
}

/// Complete linear Git records include long offscreen metadata to expose accidental duplication.
fn records(first: usize, count: usize) -> Vec<Commit> {
    (first..first + count)
        .map(|index| Commit {
            oid: format!("{index:040x}"),
            parents: vec![format!("{:040x}", index + 1)],
            title: format!("Commit {index}: {}", "large repository history ".repeat(8)),
            labels: String::new(),
            author: "History Performance".into(),
            time: "2026-10-22T00:00:00Z".into(),
        })
        .collect()
}

/// Appending one bounded page must not duplicate all previous commit strings or rebuild every lane.
#[test]
fn large_history_page_and_scroll_work_stays_bounded() {
    for count in [256, 32_768] {
        let mut state = State {
            canvas_width: 1000.,
            canvas_height: 280.,
            history_cursor: format!("{:040x}", 0),
            ..Default::default()
        };
        state.history_page(records(0, count));
        state.queue.clear();
        // Model Vec growth is amortized storage, not duplicate history metadata; reserve it outside
        // the measurement so the assertion isolates work done again for each ordinary page.
        state.commits.reserve(128);
        drop(state.history_scroll_view());
        let next = records(count, 128);
        let started = Instant::now();
        let admission_bytes = measure(|| state.history_page(next));
        let drawing_bytes = measure(|| drop(state.history_scroll_view()));
        let page_bytes = admission_bytes + drawing_bytes;
        let page_time = started.elapsed();
        let started = Instant::now();
        let scroll_bytes = measure(|| {
            for _ in 0..128 {
                assert!(state.history_canvas(ui::CanvasEvent::ScrollInput {
                    epoch: state.history_drawing.epoch,
                    change: ui::ScrollChange::Delta { delta_y: -7. },
                }));
                let scene = state.history_scroll_view();
                let encoded = serde_json::to_vec(&scene).unwrap();
                assert!(
                    encoded.len() < 64 * 1024,
                    "offscreen records reached the public scene"
                );
            }
        });
        eprintln!(
            "history rows={count}, admission={admission_bytes}, drawing={drawing_bytes}, append allocated={page_bytes} bytes in {page_time:?}, 128 scrolls allocated={scroll_bytes} bytes in {:?}",
            started.elapsed()
        );
        assert!(
            page_bytes < 1024 * 1024,
            "appending 128 rows allocated {page_bytes} bytes with {count} loaded rows"
        );
    }
}

/// An unchanged periodic observation checks one real page instead of reopening all loaded pages.
#[test]
fn unchanged_background_history_reads_only_one_page() {
    let mut state = State {
        history_cursor: format!("{:040x}", 0),
        canvas_width: 1000.,
        canvas_height: 280.,
        ..Default::default()
    };
    state.history_page(records(0, 32_768));
    state.queue.clear();
    state.log_scroll = 20_000.;
    drop(state.log_canvas());
    let epoch = state.history_drawing.epoch;
    state.automatic_observation = true;
    state.refresh_log();
    let first = state.queue.pop_front().unwrap();
    assert!(first.args.iter().any(|arg| arg == "--max-count=128"));
    state.history_page(records(0, 128));
    assert!(
        state.queue.is_empty(),
        "unchanged background refresh reopened the remaining 255 pages"
    );
    assert!(state.history_refresh.is_none());
    assert_eq!(state.commits.len(), 32_768);
    assert_eq!(state.log_scroll, 20_000.);
    drop(state.log_canvas());
    assert_eq!(state.history_drawing.epoch, epoch);
}

/// Ref movement, changed records, shorter history and explicit refresh must still replace stale data.
#[test]
fn history_refresh_falls_back_when_ownership_or_data_changes() {
    for case in ["manual", "refs", "record", "short"] {
        let mut state = State {
            history_cursor: format!("{:040x}", 0),
            ..Default::default()
        };
        state.history_page(records(0, 384));
        state.queue.clear();
        state.automatic_observation = case != "manual";
        state.refresh_log();
        state.queue.pop_front().unwrap();
        if case == "refs" {
            state.history_cache.invalidate_logs();
        }
        let mut page = records(0, if case == "short" { 64 } else { 128 });
        if case == "record" {
            page[0].labels = "new tag".into();
        }
        state.history_page(page);
        if case == "short" {
            assert_eq!(state.commits.len(), 64);
            assert!(state.history_refresh.is_none());
        } else {
            assert!(
                state.history_refresh.is_some(),
                "{case} reused stale history"
            );
            assert!(
                state
                    .queue
                    .iter()
                    .any(|command| matches!(command.purpose, Purpose::Log { .. }))
            );
        }
    }
}

/// Appending fills a partial tile, overlapping pages stay unique and equal-length replacement retires input.
#[test]
fn incremental_history_keeps_tiles_identity_and_page_overlap_correct() {
    let mut state = State {
        canvas_width: 1000.,
        canvas_height: 28.,
        history_cursor: format!("{:040x}", 0),
        ..Default::default()
    };
    state.history_page(records(0, 17));
    state.queue.clear();
    state.log_scroll = 16. * 28.;
    let first = state.log_canvas();
    let first_epoch = state.history_drawing.epoch;
    state.history_page(records(16, 3));
    assert_eq!(state.commits.len(), 19);
    let next = state.log_canvas();
    let vector = |node: ui::Node| {
        let ui::Kind::Canvas(canvas) = node.kind else {
            panic!("canvas")
        };
        canvas
            .paint
            .into_iter()
            .filter_map(|paint| match paint {
                Paint::Svg { source, .. } => Some(source),
                _ => None,
            })
            .collect::<String>()
    };
    assert_eq!(
        vector(next).matches("<circle ").count(),
        vector(first).matches("<circle ").count() + 2
    );
    assert_ne!(state.history_drawing.epoch, first_epoch);
    let append_epoch = state.history_drawing.epoch;
    state.offset = 0;
    let mut replacement = records(0, 19);
    replacement[0].parents = vec![format!("{:040x}", 2)];
    replacement[0].title = "replacement metadata".into();
    state.history_page(replacement);
    drop(state.log_canvas());
    assert_ne!(state.history_drawing.epoch, append_epoch);
    assert!(!state.history_canvas(ui::CanvasEvent::ScrollInput {
        epoch: append_epoch,
        change: ui::ScrollChange::Delta { delta_y: 7. },
    }));
    state.history_page(records(18, 3));
    assert_eq!(
        state.commits.len(),
        21,
        "replacement rebuilt the OID admission index"
    );
}
