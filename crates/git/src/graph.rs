//! Lane layout for drawing the commit history as a graph.
//!
//! Commits arrive newest first in topological order (children before
//! parents). Each lane is a column waiting for one commit; a commit takes
//! the lane that waits for it and hands it to its first parent. Every row
//! records only the segments drawn inside it, so a list can paint any row on
//! its own.

/// Where a segment runs within its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    /// From the top edge (a child above) into the commit's dot.
    Upper,
    /// From the commit's dot to the bottom edge (towards a parent below).
    Lower,
    /// From the top edge straight to the bottom edge, past this commit.
    Through,
}

/// A line inside one row, between two lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: u16,
    pub to: u16,
    /// Index into the caller's lane palette.
    pub color: u16,
    pub span: Span,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// Lane of the commit's dot.
    pub lane: u16,
    pub color: u16,
    pub edges: Vec<Edge>,
}

impl GraphRow {
    /// Lanes this row reaches into.
    pub fn width(&self) -> u16 {
        self.edges
            .iter()
            .map(|edge| edge.from.max(edge.to))
            .fold(self.lane, u16::max)
            + 1
    }
}

/// Lane state carried from one commit to the next, so history can be laid
/// out a page at a time.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    /// The commit each lane waits for, and the lane's color.
    lanes: Vec<Option<(String, u16)>>,
    next_color: u16,
}

impl Graph {
    pub fn push(&mut self, oid: &str, parents: &[String]) -> GraphRow {
        let mut edges = Vec::new();
        let waiting = self.find(oid);
        let (lane, color) = match waiting {
            Some(lane) => (
                lane,
                self.lanes[lane].as_ref().map_or(0, |(_, color)| *color),
            ),
            // A branch tip: nothing above leads here.
            None => (self.free_lane(), self.new_color()),
        };
        for (ix, slot) in self.lanes.iter().enumerate() {
            if let Some((_, lane_color)) = slot {
                edges.push(Edge {
                    from: ix as u16,
                    to: ix as u16,
                    color: *lane_color,
                    span: if ix == lane {
                        Span::Upper
                    } else {
                        Span::Through
                    },
                });
            }
        }
        if lane == self.lanes.len() {
            self.lanes.push(None);
        }
        self.lanes[lane] = None;

        for (ix, parent) in parents.iter().enumerate() {
            let (to, edge_color) = match self.find(parent) {
                // Another child already leads to this parent: join its lane.
                Some(to) => {
                    let lane_color = self.lanes[to].as_ref().map_or(0, |(_, color)| *color);
                    (to, if ix == 0 { color } else { lane_color })
                }
                None => {
                    let (to, lane_color) = if ix == 0 {
                        (lane, color)
                    } else {
                        (self.free_lane(), self.new_color())
                    };
                    if to == self.lanes.len() {
                        self.lanes.push(None);
                    }
                    self.lanes[to] = Some((parent.clone(), lane_color));
                    (to, lane_color)
                }
            };
            edges.push(Edge {
                from: lane as u16,
                to: to as u16,
                color: edge_color,
                span: Span::Lower,
            });
        }
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        GraphRow {
            lane: lane as u16,
            color,
            edges,
        }
    }

    fn find(&self, oid: &str) -> Option<usize> {
        self.lanes
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|(waiting, _)| waiting == oid))
    }

    fn free_lane(&self) -> usize {
        self.lanes
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.lanes.len())
    }

    fn new_color(&mut self) -> u16 {
        let color = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(commits: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let mut graph = Graph::default();
        commits
            .iter()
            .map(|(oid, parents)| {
                let parents: Vec<String> = parents.iter().map(|p| p.to_string()).collect();
                graph.push(oid, &parents)
            })
            .collect()
    }

    fn lower(row: &GraphRow) -> Vec<(u16, u16)> {
        row.edges
            .iter()
            .filter(|edge| edge.span == Span::Lower)
            .map(|edge| (edge.from, edge.to))
            .collect()
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let rows = layout(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert!(rows.iter().all(|row| row.lane == 0 && row.width() == 1));
        assert_eq!(lower(&rows[0]), [(0, 0)]);
        assert!(lower(&rows[2]).is_empty());
        assert_eq!(rows[1].edges[0].span, Span::Upper);
    }

    #[test]
    fn merge_opens_a_lane_that_rejoins() {
        // m merges side branch s into main; both fork from a.
        let rows = layout(&[("m", &["b", "s"]), ("b", &["a"]), ("s", &["a"]), ("a", &[])]);
        assert_eq!(lower(&rows[0]), [(0, 0), (0, 1)]);
        assert_eq!(rows[1].lane, 0);
        // s passes b on the right.
        assert!(rows[1].edges.contains(&Edge {
            from: 1,
            to: 1,
            color: rows[2].color,
            span: Span::Through,
        }));
        assert_eq!(rows[2].lane, 1);
        // s's parent a already waits in lane 0: s joins it, lane 1 closes.
        assert_eq!(lower(&rows[2]), [(1, 0)]);
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].width(), 1);
        assert_ne!(rows[0].color, rows[2].color);
    }

    #[test]
    fn separate_tips_get_their_own_lanes() {
        // Upstream u is one commit ahead of local h.
        let rows = layout(&[("u", &["x"]), ("h", &["x"]), ("x", &[])]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 1);
        assert_eq!(lower(&rows[1]), [(1, 0)]);
        assert_eq!(rows[2].width(), 1);
    }

    #[test]
    fn layout_carries_across_pages() {
        let mut graph = Graph::default();
        graph.push("m", &["b".into(), "s".into()]);
        let mut next_page = graph.clone();
        let row = next_page.push("b", &["a".into()]);
        assert_eq!(row.width(), 2);
    }
}
