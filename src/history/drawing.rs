//! Viewport-sized history drawing reuses immutable topology and source-relative SVG tiles.
use super::*;

/// Derived presentation belongs to the guest; host canvases never interpret commits or lanes.
#[derive(Default)]
pub(crate) struct HistoryDrawing {
    /// A content replacement rotates the public scroll identity; repainting keeps it unchanged.
    pub(super) epoch: u64,
    /// Admission marks replacements explicitly; scrolling never compares offscreen commit metadata.
    dirty: bool,
    /// Incremental, chunked lane geometry and the unresolved parent frontier.
    graph: graph::Layout,
    /// Geometry/color changes invalidate tiles without invalidating the user's scroll content.
    style: Option<(u32, u32, [u32; graph::COLOR_COUNT], usize)>,
    /// Only tiles intersecting the current viewport are retained, bounding derived SVG storage.
    tiles: BTreeMap<usize, String>,
}

impl HistoryDrawing {
    /// Keep the topology gutter independent of viewport replies; otherwise shrinking its origin
    /// would change native column allocation after the frame has already fitted the old drawing.
    /// Dense graphs compress their lane pitch within this bounded gutter, and the canvas clips it.
    pub(super) fn graph_width(&self) -> f32 {
        (self.graph.width().max(1) as f32 * 14. + 16.).clamp(56., 192.)
    }
    /// Replacement/reordering invalidates geometry even when the new sequence has the same length.
    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Append only new rows; stable scroll frames do constant-time cache checks with no history clone.
    fn synchronize(&mut self, commits: &[Commit], columns: &mut Columns) {
        let reset = self.dirty || self.epoch == 0 || self.graph.len() > commits.len();
        if !reset && self.graph.len() == commits.len() {
            return;
        }
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("history content epoch exhausted");
        columns.content.observe(
            if reset {
                commits
            } else {
                &commits[self.graph.len()..]
            },
            reset,
        );
        if reset {
            self.graph = graph::Layout::default();
            self.tiles.clear();
            self.dirty = false;
        } else {
            // A previously partial tail tile must include its newly appended rows. Fully populated
            // tiles remain valid; a wider frontier is handled by the ordinary style cache below.
            let previous_len = self.graph.len();
            let rows = self.style.map_or(0, |style| style.3);
            self.tiles.retain(|first, _| *first + rows <= previous_len);
        }
        self.graph.append(&commits[self.graph.len()..]);
    }

    /// Tile source coordinates never contain the current scroll offset, so host pixels stay reusable.
    fn tile(
        &mut self,
        first: usize,
        rows: usize,
        width: f32,
        pitch: f32,
        palette: [u32; graph::COLOR_COUNT],
    ) -> String {
        let style = (width.to_bits(), pitch.to_bits(), palette, rows);
        if self.style != Some(style) {
            self.style = Some(style);
            self.tiles.clear();
        }
        self.tiles.entry(first).or_insert_with(|| {
            let height = rows * 28;
            let mut source = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">");
            for i in first..(first + rows).min(self.graph.len()) {
                let row = self.graph.row(i);
                let y = (i - first) * 28;
                let center = 10. + row.lane as f32 * pitch;
                source.push_str(&format!("<g transform=\"translate(0 {y})\">"));
                for edge in &row.edges {
                    let a = 10. + edge.from as f32 * pitch;
                    let b = 10. + edge.to as f32 * pitch;
                    let color = palette[edge.color % graph::COLOR_COUNT];
                    // A hollow merge center must reveal the actual selected/unselected surface.
                    // End incident lines at the ring instead of painting a background-colored disk.
                    let inset = if row.merge && edge.from == row.lane { 4. / (b - a).hypot(14.) } else { 0. };
                    let x = a + (b - a) * inset;
                    let y = 14. + 14. * inset;
                    source.push_str(&format!("<path d=\"M {x} {y} L {b} 28\" fill=\"none\" stroke=\"#{color:06x}\" stroke-width=\"1\"/>"));
                }
                if i > 0 {
                    for edge in &self.graph.row(i - 1).edges {
                        let x = 10. + edge.to as f32 * pitch;
                        let color = palette[edge.color % graph::COLOR_COUNT];
                        let end = if row.merge && edge.to == row.lane { 10 } else { 14 };
                        source.push_str(&format!("<path d=\"M {x} 0 L {x} {end}\" stroke=\"#{color:06x}\" stroke-width=\"1\"/>"));
                    }
                }
                let color = palette[row.color % graph::COLOR_COUNT];
                if row.merge {
                    source.push_str(&format!("<circle cx=\"{center}\" cy=\"14\" r=\"4\" fill=\"none\" stroke=\"#{color:06x}\" stroke-width=\"1.5\"/></g>"));
                } else {
                    source.push_str(&format!("<circle cx=\"{center}\" cy=\"14\" r=\"3\" fill=\"#{color:06x}\"/></g>"));
                }
            }
            source.push_str("</svg>");
            source
        }).clone()
    }
}

impl State {
    /// Retain one viewport of overscan on each side so native frames need not await WASM artwork.
    pub(crate) fn log_canvas(&mut self) -> ui::Node {
        self.history_columns
            .content
            .sync_font(self.environment.ui_font.family.clone(), &self.commits);
        self.history_drawing
            .synchronize(&self.commits, &mut self.history_columns);
        let width = self.canvas_width.max(300.);
        let height = self.canvas_height.max(28.);
        let scroll = self.history_scroll();
        let graph_inset = 16.;
        let lanes = self.history_drawing.graph.width().max(1);
        let graph_width = self.history_drawing.graph_width();
        let pitch = ((graph_width - 16.) / lanes as f32).min(14.);
        // Keep preferred widths in the drawing even below their minimum total. Native frames fit
        // this immutable reference into actual bounds; a Resize reply must not compress them twice.
        let column_widths = self
            .history_columns
            .reference_widths(graph_width + 16., width - 8.);
        let mut column_edges = [graph_width + 16.; 5];
        for (index, width) in column_widths.iter().enumerate() {
            column_edges[index + 1] = column_edges[index] + width;
        }
        let column_reference_width = column_edges[4] + 8.;
        // Declare exact widths/minima instead of subtracting rounded coordinates. A one-ULP
        // minimum change on a late reply would otherwise retire an active native divider gesture.
        let column_minimum = columns::Columns::minimum(f32::MAX).to_vec();
        let first = (scroll / 28.).floor() as usize;
        // The drawing quota remains bounded even for a hostile, oversized geometry notification.
        let guard_rows = ((height / 28.).ceil() as usize + 1).min(1000);
        let start = first.saturating_sub(guard_rows).min(self.commits.len());
        let end = (((scroll + height * 2.) / 28.).ceil() as usize)
            .saturating_add(1)
            .min(start.saturating_add(6000))
            .min(self.commits.len());
        let mut paint = Vec::new();
        let mut column_bindings = Vec::new();
        let mut viewport_paints = Vec::new();
        for (i, commit) in self.commits.iter().enumerate().take(end).skip(start) {
            let y = i as f32 * 28. - scroll;
            if self.selected_commits.contains(&commit.oid) {
                viewport_paints.push(paint.len());
                paint.push(Paint::Fill {
                    rect: Rect {
                        x: 0.,
                        y,
                        w: column_reference_width,
                        h: 28.,
                    },
                    color: self.environment.selection,
                    extend_to_bottom: false,
                });
            }
            let time = super::columns::display_time(&commit.time);
            for (column, (edges, text)) in column_edges
                .windows(2)
                .zip([&commit.title, &commit.labels, &commit.author, &time])
                .enumerate()
            {
                // Bound metadata payloads as before; only native glyph metrics may determine alignment.
                let text = if column == 2 {
                    columns::author_text(text)
                } else if column == 3 {
                    text.clone()
                } else {
                    // Native resizing needs the caption rather than a guest-truncated row.
                    text.chars().take(512).collect()
                };
                column_bindings.push(ui::ColumnBinding {
                    paint: paint.len(),
                    column,
                });
                paint.push(Paint::TextBox {
                    rect: Rect {
                        x: edges[0] + if column == 0 { 0. } else { 4. },
                        y: y + 5.,
                        w: (edges[1] - edges[0] - 8.).max(0.),
                        h: 18.,
                    },
                    text,
                    color: self.environment.foreground,
                    size: 12.,
                    bold: column == 2,
                    font: None,
                    align: match column {
                        1 => ui::Alignment::End,
                        _ => ui::Alignment::Start,
                    },
                });
            }
        }
        // Normal windows use sixteen-row tiles. Very tall native viewports still use at most
        // sixteen SVG operations, retaining the public vector budget and stable page boundaries.
        let tile_rows = (end - start).div_ceil(14).max(16);
        let first_tile = start / tile_rows * tile_rows;
        self.history_drawing
            .tiles
            .retain(|first, _| *first >= first_tile && *first < end);
        for first in (first_tile..end).step_by(tile_rows) {
            let source = self.history_drawing.tile(
                first,
                tile_rows,
                graph_width,
                pitch,
                self.history_palette.colors(self.environment.dark),
            );
            let rect = Rect {
                x: graph_inset,
                y: first as f32 * 28. - scroll,
                w: graph_width,
                h: (tile_rows * 28) as f32,
            };
            paint.push(Paint::Svg {
                rect,
                clip: rect,
                source,
            });
        }
        // Column boundaries remain hit-testable without painting separators over the selected row.
        ui::Node::new(
            "log",
            ui::Kind::Canvas(ui::Canvas {
                column_resize: (!self.commits.is_empty()).then(|| ui::ColumnResize {
                    epoch: self.history_drawing.epoch,
                    x: column_edges[0],
                    widths: column_widths.to_vec(),
                    minimum: column_minimum,
                    bindings: column_bindings,
                    // Native resizing absorbs width in the subject column before the guest can
                    // reply; trailing metadata and selection backgrounds follow the same viewport.
                    viewport: Some(ui::ColumnViewport {
                        width: column_reference_width,
                        flexible: 0,
                        span_paints: viewport_paints,
                    }),
                }),
                font: self.environment.ui_font.clone(),
                measure_text: self.history_columns.content.requests(),
                scroll: Some(ui::ScrollRange {
                    content: self.commits.len() as f32 * 28.,
                    offset: scroll,
                }),
                scroll_epoch: Some(self.history_drawing.epoch),
                scroll_buffer: Some(ui::ScrollBuffer {
                    start: start as f32 * 28.,
                    end: end as f32 * 28.,
                }),
                paint,
                focusable: true,
                ..Default::default()
            }),
        )
        .disabled(self.history_navigation_busy())
        .grow()
    }
}

#[cfg(test)]
mod color_tests;

/// Bound drawing payloads using conservative approximate advances; native text regions align their glyphs.
/// Complete Unicode text remains in the commit preview; a fitting timestamp is never abbreviated.
fn fit(text: &str, width: f32, bold: bool) -> (String, f32) {
    let mut result = String::new();
    let mut used = 0.;
    let advance = |ch: char| {
        if ch.is_ascii() {
            if bold { 7.3 } else { 7. }
        } else {
            14.
        }
    };
    for ch in text.chars() {
        if used + advance(ch) > width {
            while used + 10. > width {
                let Some(last) = result.pop() else {
                    break;
                };
                used -= advance(last);
            }
            if width >= 10. {
                result.push('…');
                used += 10.;
            }
            break;
        }
        used += advance(ch);
        result.push(ch);
    }
    (result, used)
}
