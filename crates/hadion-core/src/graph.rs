//! Lane layout for the commit graph.
//!
//! Commits arrive in display order (children before parents). Each row is
//! laid out independently as two halves so the renderer can virtualise:
//!
//! * `top`    – lines from the row's top edge down to the node centre line
//! * `bottom` – lines from the node centre line down to the row's bottom edge
//!
//! A lane keeps its column for its whole life, so the bottom half of row `r`
//! and the top half of row `r + 1` always meet at the same x position.

pub type Col = u16;

/// One line inside a half-row. `from` is the column at the upper end, `to`
/// the column at the lower end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub from: Col,
    pub to: Col,
    /// Lane whose colour this line takes.
    pub color: Col,
    /// Lines that belong to the uncommitted-changes pseudo row.
    pub dashed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// Column of this row's node.
    pub col: Col,
    pub top: Vec<Segment>,
    pub bottom: Vec<Segment>,
}

#[derive(Clone, Debug, Default)]
pub struct GraphLayout {
    pub rows: Vec<GraphRow>,
    /// Number of columns needed to draw every row.
    pub width: Col,
}

#[derive(Clone, Copy)]
struct Lane {
    /// Row index of the commit this lane is waiting for.
    target: u32,
    dashed: bool,
}

/// Lay out a graph. `parents[r]` lists the row indices of row `r`'s parents,
/// first parent first. Parents that are not part of the list (history cut
/// off by a commit limit) must simply be omitted. `dashed_rows` marks rows
/// whose outgoing lines should be dashed (the WIP row).
pub fn layout(parents: &[Vec<u32>], dashed_row: Option<u32>) -> GraphLayout {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut rows = Vec::with_capacity(parents.len());
    let mut width: usize = 0;

    for (r, row_parents) in parents.iter().enumerate() {
        let r = r as u32;

        // The node takes the leftmost lane that was waiting for it, or the
        // first free slot when it is a branch tip nobody pointed at yet.
        let col = lanes
            .iter()
            .position(|l| matches!(l, Some(l) if l.target == r))
            .unwrap_or_else(|| alloc(&mut lanes));

        let mut top = Vec::new();
        for (i, slot) in lanes.iter_mut().enumerate() {
            let Some(lane) = *slot else { continue };
            let i = i as Col;
            if lane.target == r {
                top.push(Segment {
                    from: i,
                    to: col as Col,
                    color: i,
                    dashed: lane.dashed,
                });
                *slot = None;
            } else {
                top.push(Segment {
                    from: i,
                    to: i,
                    color: i,
                    dashed: lane.dashed,
                });
            }
        }

        let dashed = dashed_row == Some(r);
        let mut merge_targets: Vec<Col> = Vec::new();
        // Lanes that start at this node: they have no line above the centre.
        let mut opened: Vec<Col> = Vec::new();
        match row_parents.split_first() {
            None => lanes[col] = None,
            Some((first, rest)) => {
                lanes[col] = Some(Lane {
                    target: *first,
                    dashed,
                });
                for &p in rest {
                    let j = match lanes
                        .iter()
                        .position(|l| matches!(l, Some(l) if l.target == p))
                    {
                        Some(j) => j,
                        None => {
                            let j = alloc(&mut lanes);
                            lanes[j] = Some(Lane { target: p, dashed });
                            opened.push(j as Col);
                            j
                        }
                    };
                    merge_targets.push(j as Col);
                }
            }
        }

        let mut bottom = Vec::new();
        for (i, slot) in lanes.iter().enumerate() {
            let Some(lane) = slot else { continue };
            let i = i as Col;
            if !opened.contains(&i) {
                bottom.push(Segment {
                    from: i,
                    to: i,
                    color: i,
                    dashed: lane.dashed,
                });
            }
        }
        for &j in &merge_targets {
            bottom.push(Segment {
                from: col as Col,
                to: j,
                color: j,
                dashed,
            });
        }

        width = width.max(col + 1).max(lanes.len());
        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
        rows.push(GraphRow {
            col: col as Col,
            top,
            bottom,
        });
    }

    GraphLayout {
        rows,
        width: width as Col,
    }
}

fn alloc(lanes: &mut Vec<Option<Lane>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(from: Col, to: Col, color: Col) -> Segment {
        Segment {
            from,
            to,
            color,
            dashed: false,
        }
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let g = layout(&[vec![1], vec![2], vec![]], None);
        assert_eq!(g.width, 1);
        assert!(g.rows.iter().all(|r| r.col == 0));
        assert!(g.rows[0].top.is_empty(), "tip has nothing above it");
        assert_eq!(g.rows[0].bottom, vec![seg(0, 0, 0)]);
        assert_eq!(g.rows[1].top, vec![seg(0, 0, 0)]);
        assert!(g.rows[2].bottom.is_empty(), "root has nothing below it");
    }

    #[test]
    fn fork_converges_into_parent() {
        // 0: feature tip -> 2, 1: main tip -> 2, 2: fork point
        let g = layout(&[vec![2], vec![2], vec![]], None);
        assert_eq!(g.rows[0].col, 0);
        assert_eq!(g.rows[1].col, 1);
        assert_eq!(g.rows[2].col, 0);
        assert_eq!(g.rows[2].top, vec![seg(0, 0, 0), seg(1, 0, 1)]);
        assert_eq!(g.width, 2);
    }

    #[test]
    fn merge_opens_a_lane_for_second_parent() {
        // 0: merge(1, 2), 1: main, 2: feature, both -> 3
        let g = layout(&[vec![1, 2], vec![3], vec![3], vec![]], None);
        assert_eq!(g.rows[0].col, 0);
        assert_eq!(g.rows[0].bottom, vec![seg(0, 0, 0), seg(0, 1, 1)]);
        assert_eq!(g.rows[1].col, 0);
        assert_eq!(g.rows[2].col, 1);
        assert_eq!(g.rows[3].top, vec![seg(0, 0, 0), seg(1, 0, 1)]);
    }

    #[test]
    fn merge_into_existing_lane_reuses_it() {
        // 0: tip of b -> 3, 1: merge(2, 3) on a, 2 -> 3, 3 root
        let g = layout(&[vec![3], vec![2, 3], vec![3], vec![]], None);
        assert_eq!(g.rows[1].col, 1);
        // second parent 3 is already awaited by lane 0
        assert!(g.rows[1].bottom.contains(&seg(1, 0, 0)));
        assert_eq!(g.width, 2);
    }

    #[test]
    fn freed_lanes_are_reused_and_trimmed() {
        // two independent histories one after another
        let g = layout(&[vec![1], vec![], vec![3], vec![]], None);
        assert!(g.rows.iter().all(|r| r.col == 0));
        assert_eq!(g.width, 1);
    }

    #[test]
    fn dashed_row_marks_its_lane_until_it_lands() {
        // 0: WIP -> 2, 1: other tip -> 2, 2: HEAD
        let g = layout(&[vec![2], vec![2], vec![]], Some(0));
        assert!(g.rows[0].bottom.iter().all(|s| s.dashed));
        let lane0_through_row1 = g.rows[1].top.iter().find(|s| s.from == 0).unwrap();
        assert!(lane0_through_row1.dashed);
        assert!(
            !g.rows[1]
                .bottom
                .iter()
                .find(|s| s.from == 1)
                .unwrap()
                .dashed
        );
    }

    #[test]
    fn missing_parents_end_the_lane() {
        let g = layout(&[vec![], vec![]], None);
        assert!(g.rows.iter().all(|r| r.bottom.is_empty()));
    }
}
