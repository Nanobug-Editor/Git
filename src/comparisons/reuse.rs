//! Reopen immutable comparisons using live document capabilities instead of refreshing their text.
use super::*;

impl State {
    /// An observed, completed historical view can be activated with the same exact source versions.
    /// The host still checks every capability; a late close gets one ordinary source reacquisition.
    pub(super) fn try_reuse_history_comparison(&mut self) -> Result<bool, api::Failure> {
        if self.document_subscription.is_none() {
            return Ok(false);
        }
        let Some(sources) = self
            .diff_views
            .get(&self.diff_key)
            .filter(|sources| {
                sources.navigation.is_some()
                    && sources.left.is_some()
                    && sources.right.is_some()
                    && (self.snapshot_targets.len() == 2 || sources.third.is_some())
            })
            .cloned()
        else {
            return Ok(false);
        };
        self.left_version = sources.left;
        self.right_version = sources.right;
        self.third_version = sources.third;
        self.diff_flight.as_mut().unwrap().reused_comparison = true;
        self.diff_source_requested = true;
        // history_file only queues this bounded batch; no native process has been started yet.
        self.queue.retain(|command| {
            !matches!(&command.purpose,
            Purpose::HistoryBatch { path, .. } if path == &self.diff_path)
        });
        self.publish_prepared_comparison()
    }

    /// Emit one comparison request once all exact source capabilities are ready.
    /// Both a fresh read and a retained immutable view use this same public host operation.
    pub(crate) fn publish_prepared_comparison(&mut self) -> Result<bool, api::Failure> {
        if self.diff_history && self.snapshot_targets.len() == 3 && self.third_version.is_none() {
            return Ok(false);
        }
        let (Some(left), Some(right)) = (self.left_version.clone(), self.right_version.clone())
        else {
            return Ok(false);
        };
        self.left_version = None;
        self.right_version = None;
        self.diff_mode = None;
        let third = self.third_version.take();
        self.editor(
            api::EditorOperation::OpenDocumentComparison {
                key: self.diff_key.clone(),
                title: self.diff_title.clone(),
                left,
                right,
                third,
                navigation: self.comparison_navigation(),
                initial_position: self.comparison_position(),
                folding: Some(api::ComparisonFolding {
                    collapse: ui::ToolIcon::new(
                        "icons/explorer-collapse-all.svg",
                        "icons/explorer-collapse-all.svg",
                    ),
                    expand: ui::ToolIcon::new(
                        "icons/explorer-expand-all.svg",
                        "icons/explorer-expand-all.svg",
                    ),
                    context_lines: 3,
                }),
                preview: true,
            },
            "compare",
        )?;
        Ok(true)
    }
}
