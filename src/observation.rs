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

/// Only business input preempts optional reads. Layout, measurement and focus receipts describe
/// native presentation; cancelling on those receipts can abandon a restored panel's first page.
/// Focus, visibility and save observations coalesce through their ordinary event handlers.
pub(super) fn interrupts(input: &api::Input) -> bool {
    match input {
        api::Input::Event {
            event: api::Notification::Command { .. } | api::Notification::CommandInvocation(_),
            ..
        } => true,
        api::Input::Event {
            event: api::Notification::Ui(ui::UiEvent { action, .. }),
            ..
        } => !matches!(
            action,
            ui::Action::Tree(tree::Event::Request(_))
                | ui::Action::Canvas(
                    ui::CanvasEvent::Resize { .. }
                        | ui::CanvasEvent::TextMeasured { .. }
                        | ui::CanvasEvent::Focus { .. }
                        | ui::CanvasEvent::Scroll { .. }
                        | ui::CanvasEvent::ColumnsResized { .. }
                        | ui::CanvasEvent::Pointer {
                            phase: ui::PointerPhase::Move | ui::PointerPhase::Up,
                            ..
                        }
                )
        ),
        _ => false,
    }
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
        // Selected preview metadata is required work even when a background log page discovered
        // it. Keep its current chain; cancelling it would leave an already published loading tree
        // with no owner, and repeated row input must not restart a nearly completed immutable read.
        let selected = self.selected.as_str();
        let required = |purpose: &Purpose| matches!(purpose, Purpose::Files(oid) | Purpose::CommitMessage(oid) if oid == selected);
        self.queue.retain(|command| required(&command.purpose));
        self.history_refresh = None;
        if self
            .running
            .as_ref()
            .is_some_and(|running| !required(&running.command.purpose))
            && let Some(running) = self.running.take()
        {
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
