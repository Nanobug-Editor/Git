//! Guest-owned history columns reuse the public canvas pointer capture without changing scroll identity.
use super::*;
mod measure;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod native_tests;

/// Trailing widths remain compact when the viewport grows; the subject receives the remaining space.
#[derive(Default)]
pub(crate) struct Columns {
    /// User-selected label/author/time widths in logical pixels; absent means the compact defaults.
    pub(crate) trailing: Option<[f32; 3]>,
    /// Incremental native sample widths belong to automatic layout, never overwrite dragged widths.
    pub(super) content: measure::Content,
    /// One left-button gesture owns a fixed pair of adjacent columns until release or focus loss.
    drag: Option<Drag>,
}

/// Capturing the initial geometry avoids cumulative error and permits reversing a clamped drag.
struct Drag {
    /// Divider index: subject/labels, labels/author, or author/time.
    divider: usize,
    /// Initial canvas-local horizontal pointer position.
    anchor: f32,
    /// Four initial column widths, excluding the graph and outside padding.
    widths: [f32; 4],
    /// Minimum widths scaled to the available viewport at gesture start.
    minimum: [f32; 4],
}

impl Columns {
    /// Publish uncompressed intent, even when its reference viewport is wider than the real canvas.
    /// Native projection is the only compression step; late narrow replies cannot erase preferences.
    pub(super) fn reference_widths(&self, left: f32, right: f32) -> [f32; 4] {
        let minimum = Self::minimum(f32::MAX);
        let natural = self.content.widths();
        let override_widths = self.trailing.or_else(|| {
            self.drag
                .as_ref()
                .map(|drag| [drag.widths[1], drag.widths[2], drag.widths[3]])
        });
        let trailing = override_widths.unwrap_or([220., natural[0], natural[1]]);
        let desired: [f32; 3] = std::array::from_fn(|i| trailing[i].max(minimum[i + 1]));
        [
            (right - left - desired.iter().sum::<f32>()).max(minimum[0]),
            desired[0],
            desired[1],
            desired[2],
        ]
    }

    /// Legacy pointer hit testing mirrors the public native fit rule, never an alternate priority order.
    pub(super) fn edges(&self, left: f32, right: f32) -> [f32; 5] {
        let mut widths = self.reference_widths(left, right);
        let reference_right = left + widths.iter().sum::<f32>();
        let available = (right - left).max(0.);
        let minimum = Self::minimum(f32::MAX);
        let floor = minimum.iter().sum::<f32>();
        if available < floor {
            widths = minimum.map(|value| value * available / floor);
        } else {
            widths[0] = (widths[0] + right - reference_right).max(minimum[0]);
            let excess = (widths.iter().sum::<f32>() - available).max(0.);
            let spare: f32 = widths
                .iter()
                .zip(minimum)
                .map(|(w, m)| (w - m).max(0.))
                .sum();
            if spare > 0. {
                for (width, minimum) in widths.iter_mut().zip(minimum) {
                    *width -= (*width - minimum).max(0.) * (excess / spare).min(1.);
                }
            }
        }
        let mut edges = [left; 5];
        for (index, width) in widths.into_iter().enumerate() {
            edges[index + 1] = edges[index] + width;
        }
        edges
    }

    /// Enforce readable lower bounds while still admitting a viewport narrower than their sum.
    pub(super) fn minimum(available: f32) -> [f32; 4] {
        let minimum = [104., 40., 24., 100.];
        let scale = (available / minimum.iter().sum::<f32>()).clamp(0., 1.);
        minimum.map(|width| width * scale)
    }

    /// Hit a five-pixel target around any separator; a new press always retires an earlier gesture.
    pub(super) fn begin(&mut self, x: f32, edges: [f32; 5]) -> bool {
        self.drag = None;
        if let Some(divider) = (0..3).find(|i| (x - edges[i + 1]).abs() <= 5.) {
            self.drag = Some(Drag {
                divider,
                anchor: x,
                widths: std::array::from_fn(|i| edges[i + 1] - edges[i]),
                minimum: Self::minimum(edges[4] - edges[0]),
            });
        }
        self.drag.is_some()
    }

    /// Apply an absolute pointer position to the captured pair, leaving all other dividers fixed.
    pub(super) fn move_to(&mut self, x: f32) -> bool {
        let Some(drag) = &self.drag else {
            return false;
        };
        if !x.is_finite() {
            return false;
        }
        let i = drag.divider;
        let lower = drag.minimum[i] - drag.widths[i];
        let upper = (drag.widths[i + 1] - drag.minimum[i + 1]).max(lower);
        let delta = (x - drag.anchor).clamp(lower, upper);
        let mut widths = drag.widths;
        widths[i] += delta;
        widths[i + 1] -= delta;
        let next = [widths[1], widths[2], widths[3]];
        let changed = self.trailing != Some(next);
        self.trailing = Some(next);
        changed
    }

    /// Release/cancellation leaves the last accepted widths, but cannot resize on later pointer motion.
    pub(super) fn finish(&mut self) {
        self.drag = None;
    }

    /// Only an owned drag can route pointer motion as a history-only presentation change.
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Restore bounded logical widths from a hot-update snapshot without retaining pointer ownership.
    pub(crate) fn restore(&mut self, value: Option<&serde_json::Value>) {
        self.trailing = value
            .and_then(|value| serde_json::from_value::<[f32; 3]>(value.clone()).ok())
            .filter(|widths| {
                widths
                    .iter()
                    .all(|w| w.is_finite() && (1. ..=100_000.).contains(w))
            });
        self.finish();
    }
}

/// Bound the displayed/measurement author caption consistently; the immutable commit preview
/// retains the complete Git value, while overly long names visibly end with an ellipsis.
pub(super) fn author_text(value: &str) -> String {
    let mut characters = value.chars();
    let mut text: String = characters.by_ref().take(64).collect();
    if characters.next().is_some() {
        text.push('…');
    }
    text
}

impl State {
    /// Share graph and text geometry between painting and separator hit testing.
    pub(super) fn history_column_edges(&self) -> [f32; 5] {
        self.history_columns.edges(
            self.history_drawing.graph_width() + 16.,
            self.canvas_width.max(300.) - 8.,
        )
    }
}

/// Display the original Git wall-clock fields without reinterpreting their timezone or accepting a partial date.
pub(super) fn display_time(value: &str) -> String {
    let Some(prefix) = value.get(..19) else {
        return value.into();
    };
    let valid = prefix.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        10 => b == b'T' || b == b' ',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    });
    if valid {
        prefix.replace('T', " ")
    } else {
        value.into()
    }
}
