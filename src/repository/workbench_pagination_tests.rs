//! Ordinary native scrolling must reach the oldest commit without a separate menu command.
use super::*;

/// A scrollbar reaching each loaded tail continues through bounded pages to the repository root.
#[test]
fn scrolling_to_history_end_reaches_oldest_commit_without_load_more() {
    for count in [128, 129, 385] {
        for wheel in [false, true] {
            assert_scroll_reaches_root(count, wheel);
        }
    }
}

/// Use `count` oldest fixture records and either native wheel deltas or absolute thumb positions.
fn assert_scroll_reaches_root(count: usize, wheel: bool) {
    let all = commits();
    let repository = &all[all.len() - count..];
    let mut state = workbench();
    state.canvas_width = 1000.;
    state.canvas_height = 280.;
    state.commit_message = "Keep this draft".into();
    state.queue_log();
    complete_history(&mut state, repository);
    let selected = state.selected.clone();
    for _ in 0..8 {
        let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
            panic!("native history canvas");
        };
        let range = canvas.scroll.unwrap();
        state.history_canvas(ui::CanvasEvent::ScrollInput {
            epoch: canvas.scroll_epoch.unwrap(),
            change: if wheel {
                ui::ScrollChange::Delta { delta_y: -10_000. }
            } else {
                ui::ScrollChange::To {
                    offset: (range.content - state.canvas_height).max(0.),
                }
            },
        });
        complete_history(&mut state, repository);
    }
    assert_eq!(
        state.commits.len(),
        repository.len(),
        "ordinary scrolling stopped at a loaded page: count={count}, wheel={wheel}"
    );
    assert_eq!(state.selected, selected);
    assert_eq!(state.commit_message, "Keep this draft");
    let ui::Kind::Canvas(canvas) = state.log_canvas().kind else {
        panic!("native history canvas");
    };
    let range = canvas.scroll.unwrap();
    assert_eq!(range.offset + state.canvas_height, range.content);
    let oldest = repository.last().unwrap();
    assert!(
        canvas.paint.iter().any(|paint| matches!(paint,
            plugin_protocol::Paint::Text { text, .. }
            | plugin_protocol::Paint::TextBox { text, .. } if text == &oldest.title
        )),
        "the root commit must appear in the final native viewport"
    );
}

/// A taller real viewport fills itself even when its first page has no scrollable overflow.
#[test]
fn growing_history_viewport_loads_the_oldest_commit_without_another_scroll() {
    let repository = commits()[256..].to_vec();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    state.history_canvas(ui::CanvasEvent::Resize {
        width: 1000.,
        height: 4000.,
        grid: None,
    });
    complete_history(&mut state, &repository);
    assert_eq!(state.commits.len(), repository.len());
    assert_eq!(state.log_scroll, 0.);
}

/// Repeated input coalesces behind existing serial work and stops requesting when the user leaves.
#[test]
fn history_scroll_demand_waits_for_work_and_stays_bounded() {
    let repository = commits();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    // The sentinel represents an already queued business command; this fixture never executes it.
    state
        .queue
        .push_back(Command::new(["status"], Purpose::Action));
    state.wheel_history(-10_000.);
    assert_eq!(state.queue.len(), 1);
    state.queue.pop_front();
    assert!(state.resume_history_page());
    for _ in 0..16 {
        state.wheel_history(-1.);
    }
    assert_eq!(
        state.queue.len(),
        1,
        "one next-page request per loaded tail"
    );
    complete_history(&mut state, &repository);
    assert_eq!(
        state.commits.len(),
        256,
        "scrolling does not load the whole repository"
    );
    state
        .queue
        .push_back(Command::new(["status"], Purpose::Action));
    state.wheel_history(-10_000.);
    state.wheel_history(10_000.);
    state.queue.pop_front();
    assert!(!state.resume_history_page());
    assert!(state.queue.is_empty());
}

/// A failed page stays retryable by a new user gesture without an automatic error/retry loop.
#[test]
fn failed_history_continuation_waits_for_new_scroll_input() {
    let repository = commits()[256..].to_vec();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    state.wheel_history(-10_000.);
    let command = state.queue.pop_front().unwrap();
    assert!(matches!(command.purpose, Purpose::Log { .. }));
    state
        .finished(
            Running {
                handle: handle(),
                command,
                output: Vec::new(),
                stderr: b"read failed".to_vec(),
            },
            128,
        )
        .unwrap();
    assert!(state.history_error.contains("read failed"));
    assert!(!state.resume_history_page());
    assert!(state.queue.is_empty());
    state.wheel_history(-1.);
    complete_history(&mut state, &repository);
    assert_eq!(state.commits.len(), 129);
    for _ in 0..16 {
        state.wheel_history(-1.);
    }
    assert!(
        state.queue.is_empty(),
        "the observed root ends automatic paging"
    );
}

/// Moving refs can return only already admitted OIDs; that is not permission to poll forever.
#[test]
fn overlapping_history_page_without_progress_stops_automatic_continuation() {
    let repository = commits();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    state.wheel_history(-10_000.);
    state.queue.pop_front().unwrap();
    state.history_page(repository[..128].to_vec());
    assert_eq!(state.commits.len(), 128);
    assert!(
        state.queue.is_empty(),
        "a repeated page cannot trigger another identical request"
    );
    assert!(
        !state.history_exhausted,
        "overlap does not establish the repository's end"
    );
}

/// A later HEAD can push the old root just beyond a refreshed window that previously reached EOF.
#[test]
fn refreshed_history_keeps_the_previously_reached_root_available() {
    let mut repository = commits()[256..].to_vec();
    let mut state = workbench();
    state.canvas_height = 280.;
    state.queue_log();
    complete_history(&mut state, &repository);
    state.wheel_history(-10_000.);
    complete_history(&mut state, &repository);
    state.wheel_history(-10_000.);
    let scroll = state.log_scroll;
    repository.insert(
        0,
        Commit {
            oid: "new-head".into(),
            parents: vec![repository[0].oid.clone()],
            title: "New commit".into(),
            ..Default::default()
        },
    );
    state.refresh();
    complete_history(&mut state, &repository);
    assert_eq!(state.commits.len(), 130);
    assert_eq!(
        state.commits.last().unwrap().oid,
        repository.last().unwrap().oid
    );
    assert_eq!(state.log_scroll, scroll);
}
