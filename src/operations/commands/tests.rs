//! Comparison command construction must preserve the latest intent behind native receipts.
use super::*;

/// No WIT call starts while the existing comparison owns the serialized native request chain.
#[test]
fn comparison_commands_queue_the_latest_captured_file_and_mode_while_busy() {
    let mut state = State {
        selected_path: "first.txt".into(),
        changes: ["first.txt", "second.txt"]
            .into_iter()
            .map(|path| Change {
                path: path.into(),
                index: 'M',
                worktree: 'M',
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    state.editor_tasks.insert(7, "compare".into());
    state.command_for_action("diff", None).unwrap();
    state.selected_path = "second.txt".into();
    state.command_for_action("diff-index", None).unwrap();
    assert!(matches!(state.pending_comparisons.back(),
        Some(panel_activity::ComparisonSelection::Worktree { path, staged: true }) if path == "second.txt"));
    assert_eq!(state.pending_comparisons.len(), 1);
    assert!(state.queue.is_empty());
}
