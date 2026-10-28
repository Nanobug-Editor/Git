//! Optional local reads yield to current native input without replaying stale events or writes.
use super::*;

/// Only periodic/process notifications belong to the silent local observation. Public input,
/// layout, document and environment changes keep their ordinary publication semantics.
pub(super) fn is_background_event(input: &api::Input) -> bool {
    matches!(
        input,
        api::Input::Event {
            event: api::Notification::Timer { .. } | api::Notification::Process { .. },
            ..
        }
    )
}

/// Tree pages and terminal native receipts may finish alongside a read. Other native events
/// preempt it before their current, already-admitted UI action reaches any busy guard.
pub(super) fn interrupts(input: &api::Input) -> bool {
    matches!(input, api::Input::Event { .. })
        && !is_background_event(input)
        && !matches!(
            input,
            api::Input::Event {
                event: api::Notification::Request { .. }
                    | api::Notification::Ui(ui::UiEvent {
                        action: ui::Action::Tree(tree::Event::Request(_)),
                        ..
                    }),
                ..
            }
        )
}

impl State {
    /// These commands only observe local Git state. Closing their owned child cannot interrupt
    /// a user write; late process updates no longer match `running`. Completed observations stay
    /// authoritative, while an incomplete operation probe restores its last complete readiness.
    pub(super) fn cancel_automatic_observation(&mut self) -> Result<(), api::Failure> {
        if !self.background_read {
            return Ok(());
        }
        self.automatic_observation = false;
        self.background_read = false;
        self.queue.clear();
        self.history_refresh = None;
        if let Some(running) = self.running.take() {
            api::guest::close_resource(running.handle)?;
        }
        self.conflict_lifecycle.cancel_observation();
        for file in self.merge_files.values_mut() {
            file.proposals_suspended = !self.conflict_lifecycle.ready();
        }
        self.cancel_repository_discovery();
        Ok(())
    }
}
