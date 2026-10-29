//! Commit topology and table share one native viewport and stable commit identities.
use super::*;
use plugin_protocol::{Paint, Rect};
mod cache;
#[cfg(test)]
mod column_tests;
mod columns;
mod drawing;
mod palette;
#[cfg(test)]
mod performance_tests;
pub(super) use cache::Cache;
pub(super) use columns::Columns;
pub(super) use drawing::HistoryDrawing;
pub(super) use palette::Palette;

/// Keep the visible log stable until one bounded replacement window has been decoded completely.
#[derive(Default)]
pub(super) struct Refresh {
    pub limit: usize,
    pub read: usize,
    /// Periodic observations may retain an unchanged, still-owned graph; explicit refreshes reread it.
    reuse_unchanged: bool,
    commits: Vec<Commit>,
    seen: std::collections::BTreeSet<String>,
}
impl Refresh {
    /// Refill overlapping page boundaries, with at most one extra page of observations per round.
    pub(super) fn next_count(&self) -> usize {
        (self.limit - self.commits.len())
            .min(128)
            .min(self.limit.saturating_add(128).saturating_sub(self.read))
    }
}

impl State {
    /// Native Down first emits Focus at the same scene. Publish only actual history changes,
    /// so ignored focus/text/pointer observations cannot revoke that following business input.
    pub(super) fn history_canvas(&mut self, event: ui::CanvasEvent) -> bool {
        match event {
            ui::CanvasEvent::ColumnsResized { epoch, widths }
                if epoch == self.history_drawing.epoch
                    && widths.len() == 4
                    && widths.iter().all(|w| w.is_finite() && *w >= 0.) =>
            {
                self.history_columns.finish();
                self.history_columns.trailing = Some([widths[1], widths[2], widths[3]]);
                true
            }
            ui::CanvasEvent::TextMeasured { widths } => self.history_columns.content.accept(widths),
            ui::CanvasEvent::Resize { width, height, .. } => self.resize_history(width, height),
            ui::CanvasEvent::Wheel { delta_y, .. } => self.wheel_history(delta_y),
            ui::CanvasEvent::ScrollInput { epoch, change }
                if epoch == self.history_drawing.epoch =>
            {
                match change {
                    ui::ScrollChange::Delta { delta_y } => self.wheel_history(delta_y),
                    ui::ScrollChange::To { offset } => self.scroll_history_to(offset),
                }
            }
            ui::CanvasEvent::Scroll { offset } => self.scroll_history_to(offset),
            ui::CanvasEvent::Pointer {
                phase: ui::PointerPhase::Down,
                x,
                y,
                button,
                shift,
                ..
            } if !self.history_navigation_busy() => {
                let edges = self.history_column_edges();
                self.history_columns.finish();
                if button == 0 && self.history_columns.begin(x, edges) {
                    return false; // A separator press never selects a commit or starts Git queries.
                }
                let index = ((y + self.log_scroll) / 28.) as usize;
                self.select_history_row(index, shift)
            }
            ui::CanvasEvent::Pointer {
                phase,
                x,
                button: 0,
                ..
            } if self.history_columns.dragging() => {
                let changed = self.history_columns.move_to(x);
                if matches!(phase, ui::PointerPhase::Up) {
                    self.history_columns.finish();
                }
                changed
            }
            ui::CanvasEvent::Focus { focused: false } => {
                self.history_columns.finish();
                false
            }
            ui::CanvasEvent::Key {
                key,
                ctrl: false,
                alt: false,
                shift,
            } => self.history_key(&key, shift),
            _ => false,
        }
    }

    /// Reactivation and rejected deferred callbacks can repeat identical native measurements.
    /// Only changed dimensions alter drawing/scroll geometry and require a replacement scene.
    pub(super) fn resize_history(&mut self, width: f32, height: f32) -> bool {
        if (self.canvas_width, self.canvas_height) == (width, height) {
            return false;
        }
        self.canvas_width = width;
        self.canvas_height = height;
        self.history_columns.finish();
        // Growing the body can reduce its maximum offset. Persist the same clamped position
        // used for painting so subsequent pointer/keyboard input addresses the visible rows.
        self.log_scroll = self.history_scroll();
        self.request_history_page();
        true
    }

    /// Native wheels supply pixel deltas; moving near the loaded tail requests the next page.
    /// At a completed boundary an unchanged wheel preserves scene-bound callbacks.
    pub(super) fn wheel_history(&mut self, delta_y: f32) -> bool {
        let previous = self.log_scroll;
        self.log_scroll -= delta_y;
        self.log_scroll = self.history_scroll();
        let queued = self.request_history_page();
        self.log_scroll != previous || queued
    }

    /// Apply a native thumb's `offset` within accepted rows; report a changed viewport or queued page.
    fn scroll_history_to(&mut self, offset: f32) -> bool {
        let previous = self.log_scroll;
        self.log_scroll = offset;
        self.log_scroll = self.history_scroll();
        let queued = self.request_history_page();
        self.log_scroll != previous || queued
    }

    /// Record viewport demand and report whether a page was queued, without cancelling another task.
    fn request_history_page(&mut self) -> bool {
        self.history_extend_pending = true;
        self.resume_history_page()
    }

    /// Fill one extra viewport near the tail. The serial queue coalesces repeated native input;
    /// completion retries deferred demand using the current filter and position, never an old event.
    /// Return whether a new bounded Git read was queued.
    pub(super) fn resume_history_page(&mut self) -> bool {
        if !self.history_extend_pending {
            return false;
        }
        if self.history_exhausted
            || !self.canvas_height.is_finite()
            || self.canvas_height <= 0.
            || self.visible_panels.get("history") == Some(&false)
            || self.log_scroll + self.canvas_height * 2. < self.commits.len() as f32 * 28.
        {
            self.history_extend_pending = false;
            return false;
        }
        if self.busy() || self.history_refresh.is_some() || self.history_cache.pending {
            return false;
        }
        if !self.history_cache.owns_page(&self.filter) {
            self.history_extend_pending = false;
            return false;
        }
        self.offset = self.commits.len();
        self.queue_log();
        true
    }

    /// A shortened history ends at its last visible row; stale native scroll callbacks stay bounded.
    fn history_scroll(&self) -> f32 {
        let maximum = (self.commits.len() as f32 * 28. - self.canvas_height.max(0.)).max(0.);
        if self.log_scroll.is_finite() {
            self.log_scroll.clamp(0., maximum)
        } else {
            0.
        }
    }

    /// Prefer the detail window's immutable selection over the current paged/filterable log.
    pub(super) fn history_commit(&self) -> Option<&Commit> {
        self.history_selection
            .as_ref()
            .filter(|commit| commit.oid == self.selected)
            .or_else(|| {
                self.commits
                    .iter()
                    .find(|commit| commit.oid == self.selected)
            })
    }

    /// Local observations revisit the loaded range, never implicitly load the rest of a repository.
    pub(super) fn refresh_log(&mut self) {
        self.history_cache.advance();
        // The first authoritative page is required work even when requested by visibility.
        // A genuine user action may preempt its read, but the existing idle continuation must
        // resume it without a thirty-second timer. An accepted empty page is complete as well.
        self.history_cache.pending |=
            self.commits.is_empty() && !self.history_cache.owns_page(&self.filter);
        self.history_refresh = Some(Refresh {
            limit: self.commits.len().max(128),
            reuse_unchanged: self.automatic_observation,
            ..Default::default()
        });
        self.queue_log();
    }

    /// Publish complete pages atomically; moving refs cannot introduce duplicate selectable OIDs.
    pub(super) fn history_page(&mut self, commits: Vec<Commit>) {
        let received = commits.len();
        let requested = self
            .history_refresh
            .as_ref()
            .map_or(128, Refresh::next_count);
        let appending = self.history_refresh.is_none() && self.offset > 0;
        let previous_len = self.commits.len();
        if self.history_refresh.as_ref().is_some_and(|refresh| {
            refresh.reuse_unchanged
                && refresh.read == 0
                && commits.len() == refresh.next_count()
                // Ref/HEAD observations run first and revoke this ownership on any identity change.
                && self.history_cache.owns_page(&self.filter)
                && self.commits.starts_with(&commits)
        }) {
            self.history_refresh = None;
            self.history_refreshed_at = Some(std::time::Instant::now());
            return;
        }
        if let Some(refresh) = &mut self.history_refresh {
            let count = commits.len();
            let requested = refresh.next_count();
            refresh.read += count;
            for commit in commits {
                if refresh.seen.insert(commit.oid.clone()) {
                    refresh.commits.push(commit);
                }
            }
            if count == requested && refresh.next_count() > 0 {
                self.queue_log();
                return;
            }
            if count == requested && refresh.commits.len() < refresh.limit {
                // Moving refs exhausted the bounded retry budget. Keep the last complete view
                // rather than replacing it with an overlapping subset or polling indefinitely.
                self.history_refresh = None;
                self.history_refreshed_at = Some(std::time::Instant::now());
                self.automatic_observation = false;
                self.report(Severity::Warning, self.label(
                    "历史在刷新期间持续变化，已保留上次完整结果；请稍后刷新",
                    "History kept changing during refresh; the previous complete view is retained. Refresh again shortly.",
                ).into());
                return;
            }
            let commits = self.history_refresh.take().unwrap().commits;
            if self.commits != commits {
                self.automatic_observation = false;
            }
            self.replace_history_window(commits);
            self.history_refreshed_at = Some(std::time::Instant::now());
        } else if self.offset == 0 {
            self.replace_history_window(commits);
        } else {
            // A restored window may not have an admission index yet; seed it once. Ordinary page
            // arrivals then inspect only their own OIDs instead of cloning every previously loaded ID.
            if self.history_oids.len() != self.commits.len() {
                self.history_oids = self
                    .commits
                    .iter()
                    .map(|commit| commit.oid.clone())
                    .collect();
            }
            self.commits.extend(
                commits
                    .into_iter()
                    .filter(|commit| self.history_oids.insert(commit.oid.clone())),
            );
        }
        self.history_exhausted = received < requested;
        // A moving ref can yield only overlapping OIDs. Keep the accepted rows and stop this
        // demand instead of requesting the same offset forever; a later gesture can retry.
        let progressed = !appending || self.commits.len() > previous_len;
        if !progressed {
            self.history_extend_pending = false;
        }
        self.offset = self.commits.len();
        self.log_scroll = self.history_scroll();
        // The first complete page opens an inline preview only. Background refreshes retain the
        // user's OID/cursor; an explicit new branch filter clears the cursor to seed its first row.
        if (self.history_cursor.is_empty()
            || (self.history_cache.pending
                && !self.commits.iter().any(|c| c.oid == self.history_cursor)))
            && !self.commits.is_empty()
        {
            self.select_history_row(0, false);
        }
        self.history_cache
            .remember(self.filter.clone(), &self.commits);
        self.history_cache.pending = false;
        if progressed {
            // A refreshed HEAD can move a previously visible root beyond the retained window.
            // Recheck actual viewport demand after admission, even if the old page was exhausted.
            self.request_history_page();
        }
    }

    /// Replace only the changed interval of the loaded log. Equal prefix/suffix commit records
    /// keep their allocations and stable OIDs; an identical refresh leaves the window untouched.
    fn replace_history_window(&mut self, mut next: Vec<Commit>) {
        let mut seen = std::collections::BTreeSet::new();
        next.retain(|commit| seen.insert(commit.oid.clone()));
        self.history_oids = seen;
        let prefix = self
            .commits
            .iter()
            .zip(&next)
            .take_while(|(old, new)| old == new)
            .count();
        let suffix = self.commits[prefix..]
            .iter()
            .rev()
            .zip(next[prefix..].iter().rev())
            .take_while(|(old, new)| old == new)
            .count();
        let changed = next.len() - prefix - suffix;
        if prefix == self.commits.len() && changed == 0 {
            return;
        }
        self.history_drawing.invalidate();
        let end = self.commits.len() - suffix;
        self.commits
            .splice(prefix..end, next.into_iter().skip(prefix).take(changed));
    }

    /// Pointer and keyboard selection share stable OIDs and the same oldest-first batch order.
    /// New rows supersede queued metadata reads; repeated input retains its in-flight preview.
    pub(super) fn select_history_row(&mut self, index: usize, extend: bool) -> bool {
        let selection_changed = self.mark_history_row(index, extend);
        let Some(commit) = self.commits.get(index) else {
            return false;
        };
        // Repeated selection must not queue duplicate immutable reads or reset parent choices.
        let metadata_pending = self
            .running
            .iter()
            .map(|running| &running.command)
            .chain(self.queue.iter())
            .any(|command| {
                matches!(&command.purpose,
                Purpose::Files(oid) | Purpose::CommitMessage(oid) if oid == &commit.oid)
            });
        if self.selected == commit.oid
            && ((self.history_loading && metadata_pending)
                || (!self.history_loading
                    && self
                        .history_preview_commit
                        .as_ref()
                        .is_some_and(|preview| preview.oid == commit.oid)))
        {
            return selection_changed;
        }
        let oid = commit.oid.clone();
        let next = commit.clone();
        // A running readonly child may finish naturally, but obsolete work that has not started
        // must not delay the newest selection. Its receipt is already scoped to the captured OID.
        self.queue.retain(|command| {
            !matches!(
                command.purpose,
                Purpose::Files(_) | Purpose::CommitMessage(_)
            )
        });
        if self.history_preview_commit.is_none() && !self.history_loading {
            self.history_preview_commit = self.history_commit().cloned();
        }
        self.history_selection = Some(next);
        self.selected = oid.clone();
        self.pending_history_files = None;
        self.history_error.clear();
        self.history_loading = true;
        self.history_parents = [0, 1];
        self.history_path.clear();
        self.conflict_mode = false;
        self.columns.clear();
        if self.restore_history_preview() {
            return true;
        }
        self.queue.push_front(Command::new(
            vec![
                "diff-tree".into(),
                "--root".into(),
                "--no-commit-id".into(),
                "--name-status".into(),
                "--no-renames".into(),
                "-r".into(),
                "-m".into(),
                "-z".into(),
                oid.clone(),
                "--".into(),
            ],
            Purpose::Files(oid),
        ));
        true
    }
    /// Selection alone never opens a window, starts a Git child or steals the log's native focus.
    fn mark_history_row(&mut self, index: usize, extend: bool) -> bool {
        let Some(commit) = self.commits.get(index) else {
            return false;
        };
        let oid = commit.oid.clone();
        let previous_anchor = self.history_anchor.clone();
        let selected = if extend {
            let anchor_oid = self
                .history_anchor
                .get_or_insert_with(|| self.history_cursor.clone());
            let anchor = self
                .commits
                .iter()
                .position(|commit| commit.oid == *anchor_oid)
                .unwrap_or(index);
            self.commits[anchor.min(index)..=anchor.max(index)]
                .iter()
                .rev()
                .map(|commit| commit.oid.clone())
                .collect()
        } else {
            self.history_anchor = Some(oid.clone());
            vec![oid.clone()]
        };
        // Compare only this selection's domain fields; unrelated process/UI state does not
        // decide whether a boundary key replaced an otherwise unchanged admitted scene.
        let changed = self.selected_commits != selected
            || self.history_cursor != oid
            || self.history_anchor != previous_anchor;
        self.selected_commits = selected;
        self.history_cursor = oid;
        changed
    }
    /// Base owns focus and key routing. Only recognized navigation keys change this loaded page;
    /// reveal its selected row without synthesizing a pointer or modifying any editor selection.
    /// The result reports selection/scroll changes; repeating an unchanged boundary returns false.
    pub(super) fn history_key(&mut self, key: &str, extend: bool) -> bool {
        if self.history_navigation_busy()
            || self.commits.is_empty()
            || !matches!(
                key,
                "up" | "down" | "home" | "end" | "pageup" | "pagedown" | "enter"
            )
        {
            return false;
        }
        let current = self
            .commits
            .iter()
            .position(|commit| commit.oid == self.history_cursor);
        let last = self.commits.len() - 1;
        let page = ((self.canvas_height / 28.).floor() as usize).max(1);
        let index = match key {
            "up" => current.map_or(0, |index| index.saturating_sub(1)),
            "down" => current.map_or(0, |index| (index + 1).min(last)),
            "home" => 0,
            "end" => last,
            "pageup" => current.unwrap_or(0).saturating_sub(page),
            "pagedown" => (current.unwrap_or(0) + page).min(last),
            "enter" => current.unwrap_or(0),
            _ => return false,
        };
        let previous_scroll = self.log_scroll;
        let top = index as f32 * 28.;
        if top < self.log_scroll {
            self.log_scroll = top;
        } else if top + 28. > self.log_scroll + self.canvas_height {
            self.log_scroll = (top + 28. - self.canvas_height).max(0.);
        }
        // Every actual cursor change previews the same commit as pointer selection. Repeated
        // boundary keys retain the admitted scene and do not refetch an unchanged commit.
        let selected = if self.commits[index].oid != self.selected || key == "enter" {
            self.select_history_row(index, extend)
        } else {
            self.mark_history_row(index, extend)
        };
        let queued = self.request_history_page();
        selected || self.log_scroll != previous_scroll || queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeated Shift navigation keeps its original anchor, batches oldest first and reveals the
    /// selected row. No guest host call or editable document participates in history navigation.
    #[test]
    fn keyboard_history_selection_reveals_rows_and_keeps_the_range_anchor() {
        let mut state = State {
            canvas_height: 56.,
            commits: (0..6)
                .map(|i| model::Commit {
                    oid: i.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        state.history_key("down", false);
        state.queue.clear();
        state.history_key("down", true);
        state.queue.clear();
        state.history_key("down", true);
        assert_eq!(state.selected_commits, ["2", "1", "0"]);
        assert_eq!(state.log_scroll, 28.);
        state.queue.clear();
        state.history_key("end", false);
        assert_eq!(state.selected_commits, ["5"]);
        assert_eq!(state.log_scroll, 112.);
        state.queue.clear();
        state.history_key("pageup", false);
        assert_eq!(state.history_cursor, "3");
        assert_eq!(
            state.selected, "3",
            "keyboard navigation previews its cursor commit"
        );
        assert_eq!(state.log_scroll, 84.);
        state.queue.clear();
        // Model the completed receipt; clearing its queue alone leaves a cancelled load that
        // Enter is now expected to recover rather than treating as an active preview.
        state.history_loading = false;
        state.history_preview_commit = state.history_commit().cloned();
        state.history_key("x", false);
        assert!(state.queue.is_empty());
        state.history_key("enter", false);
        assert_eq!(state.selected, "3");
        assert!(state.queue.is_empty(), "Enter preserves the active preview");
    }
}
