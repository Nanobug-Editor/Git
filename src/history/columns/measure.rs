//! Native width batches size shared metadata columns without rescanning history on scroll frames.
use super::*;

/// Unique loaded authors are sampled once per font/history identity, in bounded native batches.
#[derive(Default)]
pub(in super::super) struct Content {
    /// Explicit canvas family; changing it invalidates every earlier font-dependent measurement.
    family: Option<String>,
    /// Seen bounded captions avoid duplicate native shaping across pages of the same history.
    seen: std::collections::BTreeSet<String>,
    /// Unmeasured unique captions; a reply consumes only its own leading batch.
    pending: std::collections::VecDeque<String>,
    /// Current scene sample groups; IDs rotate on every meaning change to reject late replies.
    requests: Vec<ui::TextMeasure>,
    /// Monotonic measurement identity independent of commit selection and scroll position.
    generation: u64,
    /// Maximum actual author advance observed so far in this history, before eight-pixel padding.
    author: Option<f32>,
    /// Actual advance of the full fixed-format timestamp in the current face.
    time: Option<f32>,
}

impl Content {
    /// Rebuild font-dependent samples from `commits` only when the inherited `family` changes.
    pub(in super::super) fn sync_font(&mut self, family: Option<String>, commits: &[Commit]) {
        if self.family != family {
            self.family = family;
            self.observe(commits, true);
        }
    }

    /// Append only new `commits`; `reset` starts an unrelated/reordered history without old widths.
    pub(in super::super) fn observe(&mut self, commits: &[Commit], reset: bool) {
        if reset {
            self.seen.clear();
            self.pending.clear();
            self.author = None;
            self.time = None;
        }
        let mut changed = reset;
        for commit in commits {
            let caption = author_text(&commit.author);
            if self.seen.insert(caption.clone()) {
                self.pending.push_back(caption);
                changed = true;
            }
        }
        if changed {
            self.refresh();
        }
    }

    /// Shared author/time column defaults use real advances plus four pixels on each side.
    /// Initial fallbacks last only until the first exact-scene native reply arrives.
    pub(in super::super) fn widths(&self) -> [f32; 2] {
        [
            self.author.map_or(104., |width| width + 8.),
            self.time.map_or(154., |width| width + 8.),
        ]
    }

    /// Clone only the bounded current batch for the public canvas, independent of history length.
    pub(in super::super) fn requests(&self) -> Vec<ui::TextMeasure> {
        self.requests.clone()
    }

    /// Accept `widths` only for the current batch; update natural widths and request any next batch.
    /// Dragged column widths are owned separately and remain authoritative over these defaults.
    pub(in super::super) fn accept(&mut self, widths: Vec<ui::TextWidth>) -> bool {
        if self.requests.is_empty()
            || widths.len() != self.requests.len()
            || !widths.iter().zip(&self.requests).all(|(width, request)| {
                width.id == request.id
                    && width.width.is_finite()
                    && (0. ..=1_000_000.).contains(&width.width)
            })
        {
            return false;
        }
        for (width, request) in widths.iter().zip(&self.requests) {
            if request.bold {
                self.author = Some(self.author.unwrap_or(0.).max(width.width.min(240.)));
                self.pending.drain(..request.texts.len());
            } else {
                self.time = Some(width.width.min(400.));
            }
        }
        self.refresh();
        true
    }

    /// At most 64 bounded authors and one timestamp enter any native shaping round.
    fn refresh(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("text measurement identity exhausted");
        self.requests.clear();
        if !self.pending.is_empty() {
            self.requests.push(ui::TextMeasure {
                id: format!("authors:{}", self.generation),
                texts: self.pending.iter().take(64).cloned().collect(),
                size: 12.,
                bold: true,
                font: None,
            });
        }
        if self.time.is_none() {
            self.requests.push(ui::TextMeasure { id: format!("time:{}", self.generation),
                texts: ('0'..='9').map(|digit| format!("{digit}{digit}{digit}{digit}-{digit}{digit}-{digit}{digit} {digit}{digit}:{digit}{digit}:{digit}{digit}")).collect(),
                size: 12., bold: false, font: None });
        }
    }
}
