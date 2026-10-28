//! Lane transitions follow actual parent OIDs; topology and labels use the same paged row order.
use super::model::Commit;

/// Eight reusable color slots distinguish concurrent strands without storing theme colors in geometry.
pub(crate) const COLOR_COUNT: usize = 8;

/// An unresolved parent keeps its strand color when other lanes are inserted or compacted.
#[derive(Clone)]
struct Lane {
    /// Next ancestor expected in the ordered log.
    oid: String,
    /// Palette slot owned by this strand until it joins another strand or reaches a root.
    color: usize,
    /// Earlier-created strands take precedence when first-parent ancestry reaches a shared frontier.
    strand: usize,
}

/// An edge carries one color across both halves of a row and across SVG tile boundaries.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    /// Horizontal lane at the current row's center.
    pub from: usize,
    /// Horizontal lane at the following row's center.
    pub to: usize,
    /// Palette slot of the continuing strand, independent of its horizontal position.
    pub color: usize,
}

/// One row records incoming lane positions and outgoing parent edges to the next row.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    /// Horizontal position of this commit node.
    pub lane: usize,
    /// Palette slot of the commit's strand.
    pub color: usize,
    /// Multi-parent commits use a hollow ring so merge points remain recognizable without labels.
    pub merge: bool,
    /// Number of lanes entering this row.
    pub before: usize,
    /// Number of lanes leaving this row.
    pub after: usize,
    /// Pass-through strands and parent links in the next-row coordinate system.
    pub edges: Vec<Edge>,
}

/// Keep only the unresolved parent frontier and compact geometry, never duplicate commit metadata.
#[derive(Default)]
pub(crate) struct Layout {
    /// Parent identities that can continue across arbitrary Git page boundaries.
    lanes: Vec<Lane>,
    /// Round-robin starting slot; active strands get distinct colors while palette space remains.
    next_color: usize,
    /// Monotonic strand precedence preserves the primary lineage when Git visits a side branch first.
    next_strand: usize,
    /// Fixed-size chunks avoid moving the entire loaded graph when one page is appended.
    chunks: Vec<Vec<Row>>,
    /// Number of rows already admitted, used for constant-time scroll cache checks.
    len: usize,
    /// The widest accepted frontier fixes the shared graph/text columns.
    width: usize,
}

impl Layout {
    /// Return the number of already laid-out commits without inspecting their strings.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Return the widest frontier across all accepted rows, including offscreen merges.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Address cached geometry by absolute row index; callers bound it by `len()`.
    pub(crate) fn row(&self, index: usize) -> &Row {
        &self.chunks[index / 128][index % 128]
    }

    /// Extend the current topology with new ordered commits, preserving unresolved cross-page edges.
    pub(crate) fn append(&mut self, commits: &[Commit]) {
        for commit in commits {
            let lane = self
                .lanes
                .iter()
                .position(|lane| lane.oid == commit.oid)
                .unwrap_or_else(|| {
                    let lane = self.new_strand(&commit.oid, &[]);
                    self.lanes.push(lane);
                    self.lanes.len() - 1
                });
            let before = self.lanes.clone();
            let current = self.lanes.remove(lane);
            let color = current.color;
            for (index, parent) in commit.parents.iter().enumerate() {
                if let Some(next) = self.lanes.iter_mut().find(|lane| &lane.oid == parent) {
                    // Topological order may visit the side branch before the primary parent.
                    // Promote only unresolved shared ancestry, leaving cached past rows intact.
                    if index == 0 && current.strand < next.strand {
                        next.color = color;
                        next.strand = current.strand;
                    }
                } else {
                    // The first parent continues this strand. Other parents start independent
                    // strands; reserve incoming colors too so a merge cannot reuse one mid-row.
                    let parent_lane = if index == 0 {
                        Lane {
                            oid: parent.clone(),
                            color,
                            strand: current.strand,
                        }
                    } else {
                        self.new_strand(parent, &before)
                    };
                    self.lanes
                        .insert((lane + index).min(self.lanes.len()), parent_lane);
                }
            }
            let mut edges = Vec::new();
            for (index, previous) in before.iter().enumerate() {
                if index != lane {
                    if let Some(next) = self
                        .lanes
                        .iter()
                        .position(|target| target.oid == previous.oid)
                    {
                        edges.push(Edge {
                            from: index,
                            to: next,
                            color: previous.color,
                        });
                    }
                }
            }
            for (index, parent) in commit.parents.iter().enumerate() {
                if let Some(next) = self.lanes.iter().position(|lane| &lane.oid == parent) {
                    edges.push(Edge {
                        from: lane,
                        to: next,
                        color: if index == 0 {
                            color
                        } else {
                            self.lanes[next].color
                        },
                    });
                }
            }
            self.width = self.width.max(before.len()).max(self.lanes.len());
            if self.len % 128 == 0 {
                self.chunks.push(Vec::with_capacity(128));
            }
            self.chunks.last_mut().unwrap().push(Row {
                lane,
                color,
                merge: commit.parents.len() > 1,
                before: before.len(),
                after: self.lanes.len(),
                edges,
            });
            self.len += 1;
        }
    }

    /// Create a colored strand for `oid`, reserving this row's incoming colors while allocating.
    fn new_strand(&mut self, oid: &str, reserved: &[Lane]) -> Lane {
        let lane = Lane {
            oid: oid.into(),
            color: self.allocate_color(reserved),
            strand: self.next_strand,
        };
        self.next_strand = self
            .next_strand
            .checked_add(1)
            .expect("history strand identity exhausted");
        lane
    }

    /// Choose a free slot against the current frontier and this row's reserved incoming lanes.
    /// More than eight concurrent strands cycle deterministically through the finite palette.
    fn allocate_color(&mut self, reserved: &[Lane]) -> usize {
        let color = (0..COLOR_COUNT)
            .map(|offset| (self.next_color + offset) % COLOR_COUNT)
            .find(|color| {
                !self
                    .lanes
                    .iter()
                    .chain(reserved)
                    .any(|lane| lane.color == *color)
            })
            .unwrap_or(self.next_color);
        self.next_color = (color + 1) % COLOR_COUNT;
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small ordered DAG with explicit ancestry, independent of branch labels or row columns.
    fn commits(rows: &[(&str, &[&str])]) -> Vec<Commit> {
        rows.iter()
            .map(|(oid, parents)| Commit {
                oid: (*oid).into(),
                parents: parents.iter().map(|oid| (*oid).into()).collect(),
                ..Default::default()
            })
            .collect()
    }

    /// A surviving strand keeps its color when a finished neighbor shifts it into the first lane.
    #[test]
    fn strand_colors_survive_lane_compaction() {
        let mut graph = Layout::default();
        graph.append(&commits(&[
            ("merge", &["left", "right"]),
            ("left", &[]),
            ("right", &["older"]),
            ("older", &[]),
        ]));
        let main = graph.row(0).color;
        let side = graph.row(0).edges[1].color;
        assert_ne!(main, side);
        assert_eq!(graph.row(1).color, main);
        assert_eq!(
            graph.row(1).edges,
            [Edge {
                from: 1,
                to: 0,
                color: side
            }]
        );
        assert_eq!(graph.row(2).lane, 0);
        assert_eq!(graph.row(2).color, side);
        assert_eq!(graph.row(3).color, side);
    }

    /// Octopus parents remain distinct until they meet; shared ancestry uses its existing strand.
    #[test]
    fn merge_edges_keep_parent_colors_until_shared_ancestor() {
        let mut graph = Layout::default();
        graph.append(&commits(&[
            ("merge", &["a", "b", "c"]),
            ("a", &["base"]),
            ("b", &["base"]),
            ("c", &["base"]),
            ("base", &[]),
        ]));
        let colors: Vec<_> = graph.row(0).edges.iter().map(|edge| edge.color).collect();
        assert_eq!(
            colors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        for index in 1..=3 {
            assert_eq!(graph.row(index).color, colors[index - 1]);
            assert_eq!(
                graph.row(index).edges.last().unwrap().color,
                colors[index - 1]
            );
        }
        assert_eq!(graph.row(4).color, colors[0]);
    }

    /// Git commonly visits the second parent first; it must not take over older primary ancestry.
    #[test]
    fn primary_color_wins_when_side_branch_is_visited_first() {
        let records = commits(&[
            ("merge", &["primary", "side"]),
            ("side", &["base"]),
            ("primary", &["base"]),
            ("base", &["older"]),
            ("older", &[]),
        ]);
        let mut graph = Layout::default();
        for page in records.chunks(1) {
            graph.append(page);
        }
        let main = graph.row(0).color;
        assert_ne!(graph.row(1).color, main);
        assert_eq!(graph.row(2).color, main);
        assert_eq!(graph.row(3).color, main);
        assert_eq!(graph.row(4).color, main);
    }

    /// A full palette distinguishes eight active parents and deterministic reuse stays in bounds.
    #[test]
    fn palette_reuse_avoids_active_strands() {
        let names: Vec<_> = (0..COLOR_COUNT)
            .map(|index| format!("branch-{index}"))
            .collect();
        let mut records = vec![Commit {
            oid: "merge".into(),
            parents: names.clone(),
            ..Default::default()
        }];
        records.extend(names.iter().map(|oid| Commit {
            oid: oid.clone(),
            ..Default::default()
        }));
        records.extend((0..32).map(|index| Commit {
            oid: format!("other-{index}"),
            ..Default::default()
        }));
        let mut graph = Layout::default();
        graph.append(&records);
        let colors: std::collections::BTreeSet<_> =
            graph.row(0).edges.iter().map(|edge| edge.color).collect();
        assert_eq!(colors.len(), COLOR_COUNT);
        for index in 0..records.len() {
            assert!(graph.row(index).color < COLOR_COUNT);
        }
        let mut paged = Layout::default();
        for page in records.chunks(3) {
            paged.append(page);
        }
        assert_eq!(paged.chunks, graph.chunks);
    }

    /// A merge and its shared ancestor retain identical lanes on either side of a page boundary.
    #[test]
    fn paged_layout_keeps_merge_frontier_and_chunk_boundaries() {
        let mut commits: Vec<_> = (0..260)
            .map(|i| Commit {
                oid: i.to_string(),
                parents: (i < 259).then(|| (i + 1).to_string()).into_iter().collect(),
                ..Default::default()
            })
            .collect();
        commits[127].parents = vec!["128".into(), "129".into()];
        commits[128].parents = vec!["130".into()];
        let mut whole = Layout::default();
        whole.append(&commits);
        for page_size in [1, 7, 127, 128, 129] {
            let mut paged = Layout::default();
            for page in commits.chunks(page_size) {
                paged.append(page);
            }
            assert_eq!(paged.chunks, whole.chunks);
            assert_eq!(paged.width(), 2);
            let positions = |row: &Row| {
                row.edges
                    .iter()
                    .map(|edge| (edge.from, edge.to))
                    .collect::<Vec<_>>()
            };
            assert_eq!(positions(paged.row(127)), [(0, 0), (0, 1)]);
            assert_eq!(positions(paged.row(129)), [(0, 0), (1, 0)]);
            assert!(paged.row(259).edges.is_empty());
        }
    }
}
