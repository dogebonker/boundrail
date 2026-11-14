//! Pure geometry for the architecture view: a layered layout inside each container, containers
//! sized to enclose their parts, and edge routes between box sides.
//!
//! ponytail: no dummy nodes for edges that skip ranks and no crossing count, so a long edge can
//! cross the boxes between its ends; add dummy nodes when real models show it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};

use super::svg::Point;

/// Space around a container's parts, and the band above them that holds its label.
pub(super) const PAD: f64 = 16.0;
pub(super) const HEADER: f64 = 44.0;
const H_GAP: f64 = 48.0;
const V_GAP: f64 = 72.0;
/// Barycenter sweeps, each one down and one up.
const SWEEPS: usize = 4;
/// How far an edge between nested boxes runs out to the right before it turns.
const LOOP_OUT: f64 = 24.0;
/// Spacing of the lanes edges leaving one side turn in; more than a label is high.
const LANE: f64 = 20.0;

#[derive(Clone, Default)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
    fn center(&self) -> Point {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// One box to place: its container, if any, and the size its own label needs.
pub(super) struct Boxed {
    pub(super) parent: Option<usize>,
    pub(super) w: f64,
    pub(super) h: f64,
}

/// Each node's rank by the longest path over `edges`, ignoring self loops and the back edges a
/// depth-first search finds, so a cycle cannot loop.
pub(super) fn ranks(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let forward = acyclic(n, edges);
    let mut indegree = vec![0usize; n];
    let mut out = vec![Vec::new(); n];
    for &(a, b) in &forward {
        out[a].push(b);
        indegree[b] += 1;
    }
    let mut rank = vec![0; n];
    let mut queue: VecDeque<usize> = (0..n).filter(|&v| indegree[v] == 0).collect();
    while let Some(v) = queue.pop_front() {
        for &w in &out[v] {
            rank[w] = rank[w].max(rank[v] + 1);
            indegree[w] -= 1;
            if indegree[w] == 0 {
                queue.push_back(w);
            }
        }
    }
    rank
}

/// `edges` without self loops and without the back edges of a depth-first search in node order.
fn acyclic(n: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    const NEW: u8 = 0;
    const OPEN: u8 = 1;
    let mut out = vec![Vec::new(); n];
    for (e, &(a, b)) in edges.iter().enumerate() {
        if a != b {
            out[a].push(e);
        }
    }
    let mut state = vec![NEW; n];
    let mut back = vec![false; edges.len()];
    for start in 0..n {
        if state[start] != NEW {
            continue;
        }
        state[start] = OPEN;
        let mut stack = vec![(start, 0usize)];
        while let Some((v, next)) = stack.last_mut() {
            let Some(&e) = out[*v].get(*next) else {
                state[*v] = OPEN + 1;
                stack.pop();
                continue;
            };
            *next += 1;
            let w = edges[e].1;
            match state[w] {
                NEW => {
                    state[w] = OPEN;
                    stack.push((w, 0));
                }
                OPEN => back[e] = true,
                _ => {}
            }
        }
    }
    (edges.iter().zip(back))
        .filter(|((a, b), back)| a != b && !back)
        .map(|(edge, _)| *edge)
        .collect()
}

/// The nodes of each rank, ordered by a few barycenter sweeps.
pub(super) fn order(ranks: &[usize], edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let n = ranks.len();
    let rows = ranks.iter().max().map_or(0, |top| top + 1);
    let mut layers = vec![Vec::new(); rows];
    for (v, &r) in ranks.iter().enumerate() {
        layers[r].push(v);
    }
    let (mut above, mut below) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for &(a, b) in edges {
        for (v, w) in [(a, b), (b, a)] {
            match ranks[w].cmp(&ranks[v]) {
                Ordering::Less => above[v].push(w),
                Ordering::Greater => below[v].push(w),
                Ordering::Equal => {}
            }
        }
    }
    let mut position = vec![0.0; n];
    for layer in &layers {
        for (i, &v) in layer.iter().enumerate() {
            position[v] = i as f64;
        }
    }
    for _ in 0..SWEEPS {
        for layer in layers.iter_mut().skip(1) {
            sort_layer(layer, &above, &mut position);
        }
        for layer in layers.iter_mut().rev().skip(1) {
            sort_layer(layer, &below, &mut position);
        }
    }
    layers
}

/// Sorts `layer` by the mean position of each node's neighbours in `near`, a node without any
/// keeping its own, and records the new positions.
fn sort_layer(layer: &mut [usize], near: &[Vec<usize>], position: &mut [f64]) {
    let mut keyed: Vec<(f64, usize)> = (layer.iter())
        .map(|&v| match near[v].len() {
            0 => (position[v], v),
            len => (
                near[v].iter().map(|&w| position[w]).sum::<f64>() / len as f64,
                v,
            ),
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (i, (slot, (_, v))) in layer.iter_mut().zip(keyed).enumerate() {
        *slot = v;
        position[v] = i as f64;
    }
}

/// The top-left corner of each node, rows top to bottom and each row centered, with the size of
/// the whole.
fn place(layers: &[Vec<usize>], sizes: &[Point]) -> (Vec<Point>, Point) {
    let row_width = |row: &[usize]| {
        row.iter().map(|&v| sizes[v].0).sum::<f64>() + H_GAP * row.len().saturating_sub(1) as f64
    };
    let width = layers.iter().map(|r| row_width(r)).fold(0.0, f64::max);
    let mut corners = vec![(0.0, 0.0); sizes.len()];
    let mut y = 0.0;
    for row in layers.iter().filter(|r| !r.is_empty()) {
        let height = row.iter().map(|&v| sizes[v].1).fold(0.0, f64::max);
        let mut x = (width - row_width(row)) / 2.0;
        for &v in row {
            corners[v] = (x, y + (height - sizes[v].1) / 2.0);
            x += sizes[v].0 + H_GAP;
        }
        y += height + V_GAP;
    }
    (corners, (width, (y - V_GAP).max(0.0)))
}

/// A layered layout of nodes with `sizes`: corners and the size of the whole.
fn layered(sizes: &[Point], edges: &[(usize, usize)]) -> (Vec<Point>, Point) {
    let ranks = ranks(sizes.len(), edges);
    place(&order(&ranks, edges), sizes)
}

/// Places every box: the boxes of each container (and the top level) in a layered layout over
/// the edges lifted to them, each container sized to enclose its parts. Returns the boxes in
/// absolute coordinates from the origin and the size of the whole.
pub(super) fn nest(boxes: &[Boxed], edges: &[(usize, usize)]) -> (Vec<Rect>, Point) {
    let mut kids = vec![Vec::new(); boxes.len()];
    let mut roots = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        match b.parent {
            Some(p) => kids[p].push(i),
            None => roots.push(i),
        }
    }
    let mut rects = vec![Rect::default(); boxes.len()];
    let nesting = Nesting {
        boxes,
        kids,
        lifted: lift(boxes, edges),
    };
    let size = nesting.arrange(&roots, None, &mut rects);
    for &root in &roots {
        nesting.offset(root, (0.0, 0.0), &mut rects);
    }
    (rects, size)
}

/// Edges between parts of one container, by container (`None` for the top level).
type Lifted = BTreeMap<Option<usize>, Vec<(usize, usize)>>;

/// Lifts each edge to the container where its ends part: the edge between the two parts of the
/// ends' lowest common container that hold them. An edge between a box and one inside it, or a
/// box and itself, lifts nowhere.
fn lift(boxes: &[Boxed], edges: &[(usize, usize)]) -> Lifted {
    let chain = |mut at: usize| {
        let mut chain = vec![at];
        while let Some(parent) = boxes[at].parent {
            chain.push(parent);
            at = parent;
        }
        chain.reverse();
        chain
    };
    let chains: Vec<Vec<usize>> = (0..boxes.len()).map(chain).collect();
    let mut lifted = Lifted::new();
    for &(a, b) in edges {
        let (ca, cb) = (&chains[a], &chains[b]);
        let shared = ca.iter().zip(cb).take_while(|(x, y)| x == y).count();
        if shared < ca.len() && shared < cb.len() {
            let container = shared.checked_sub(1).map(|k| ca[k]);
            lifted
                .entry(container)
                .or_default()
                .push((ca[shared], cb[shared]));
        }
    }
    lifted
}

struct Nesting<'a> {
    boxes: &'a [Boxed],
    kids: Vec<Vec<usize>>,
    lifted: Lifted,
}

impl Nesting<'_> {
    /// Sizes the boxes of `group`, the parts of `parent`, and places them relative to each other;
    /// returns the size they take together.
    fn arrange(&self, group: &[usize], parent: Option<usize>, rects: &mut [Rect]) -> Point {
        for &i in group {
            let (w, h) = self.size(i, rects);
            rects[i].w = w;
            rects[i].h = h;
        }
        let local: BTreeMap<usize, usize> =
            group.iter().enumerate().map(|(k, &i)| (i, k)).collect();
        let lifted: Vec<(usize, usize)> = (self.lifted.get(&parent).into_iter().flatten())
            .map(|(a, b)| (local[a], local[b]))
            .collect();
        let sizes: Vec<Point> = group.iter().map(|&i| (rects[i].w, rects[i].h)).collect();
        let (corners, size) = layered(&sizes, &lifted);
        for (&i, corner) in group.iter().zip(corners) {
            (rects[i].x, rects[i].y) = corner;
        }
        size
    }

    /// The size of box `i`: its own for a leaf; for a container, its parts' arranged size with
    /// padding and the label band, the parts then placed relative to its corner.
    fn size(&self, i: usize, rects: &mut [Rect]) -> Point {
        let own = (self.boxes[i].w, self.boxes[i].h);
        if self.kids[i].is_empty() {
            return own;
        }
        let (cw, ch) = self.arrange(&self.kids[i], Some(i), rects);
        let w = own.0.max(cw + 2.0 * PAD);
        let shift = (w - cw) / 2.0;
        for &k in &self.kids[i] {
            rects[k].x += shift;
            rects[k].y += HEADER;
        }
        (w, HEADER + ch + PAD)
    }

    /// Moves box `i` and everything inside it by `origin`.
    fn offset(&self, i: usize, origin: Point, rects: &mut [Rect]) {
        rects[i].x += origin.0;
        rects[i].y += origin.1;
        let corner = (rects[i].x, rects[i].y);
        for &k in &self.kids[i] {
            self.offset(k, corner, rects);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// The sides an edge from `a` to `b` leaves and enters by: bottom to top when `b` is below,
/// top to bottom when above, else side to side; boxes that overlap (one inside the other) both
/// use their right side.
fn sides(a: &Rect, b: &Rect) -> (Side, Side) {
    if b.y >= a.bottom() {
        (Side::Bottom, Side::Top)
    } else if a.y >= b.bottom() {
        (Side::Top, Side::Bottom)
    } else if b.x >= a.right() {
        (Side::Right, Side::Left)
    } else if a.x >= b.right() {
        (Side::Left, Side::Right)
    } else {
        (Side::Right, Side::Right)
    }
}

fn port(r: &Rect, side: Side, t: f64) -> Point {
    match side {
        Side::Top => (r.x + r.w * t, r.y),
        Side::Bottom => (r.x + r.w * t, r.bottom()),
        Side::Left => (r.x, r.y + r.h * t),
        Side::Right => (r.right(), r.y + r.h * t),
    }
}

/// The edge ends on each side of each box: where the other end is, the edge, and which end.
type Ports = BTreeMap<(usize, Side), Vec<(f64, usize, usize)>>;

/// A polyline for each edge from a side of its source to a side of its target. Edges sharing a
/// side spread evenly along it, ordered by where their other end is.
pub(super) fn routes(rects: &[Rect], edges: &[(usize, usize)]) -> Vec<Vec<Point>> {
    let sides: Vec<(Side, Side)> = (edges.iter())
        .map(|&(a, b)| sides(&rects[a], &rects[b]))
        .collect();
    let mut groups = Ports::new();
    for (e, (&(a, b), &(sa, sb))) in edges.iter().zip(&sides).enumerate() {
        let key = |other: &Rect, side| match side {
            Side::Top | Side::Bottom => other.center().0,
            Side::Left | Side::Right => other.center().1,
        };
        groups
            .entry((a, sa))
            .or_default()
            .push((key(&rects[b], sa), e, 0));
        groups
            .entry((b, sb))
            .or_default()
            .push((key(&rects[a], sb), e, 1));
    }
    let mut ends = vec![[(0.0, 0.0); 2]; edges.len()];
    for ((node, side), mut list) in groups {
        list.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        let n = list.len() as f64;
        for (k, (_, e, end)) in list.into_iter().enumerate() {
            ends[e][end] = port(&rects[node], side, (k as f64 + 1.0) / (n + 1.0));
        }
    }
    let lanes = lanes(edges, &sides, &ends);
    (ends.iter().zip(&sides).zip(lanes))
        .map(|((&[a, b], &(sa, sb)), lane)| bend(a, b, sa, sb, lane))
        .collect()
}

/// For each edge leaving the top or bottom of its box, its lane among the edges leaving that
/// side: the shortest horizontal run turns nearest the box, so runs do not overlap.
fn lanes(edges: &[(usize, usize)], sides: &[(Side, Side)], ends: &[[Point; 2]]) -> Vec<usize> {
    let mut leaving: BTreeMap<(usize, Side), Vec<(f64, usize)>> = BTreeMap::new();
    for (e, (&(a, _), &(side, _))) in edges.iter().zip(sides).enumerate() {
        let run = (ends[e][0].0 - ends[e][1].0).abs();
        leaving.entry((a, side)).or_default().push((run, e));
    }
    let mut lanes = vec![0; edges.len()];
    for list in leaving.values_mut() {
        list.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        for (lane, &(_, e)) in list.iter().enumerate() {
            lanes[e] = lane;
        }
    }
    lanes
}

/// An orthogonal polyline from `a`, leaving by side `sa`, to `b`, entering by side `sb`. A
/// vertical edge turns in its lane of the gap next to its source, where no box is placed, so its
/// label sits in free space.
fn bend(a: Point, b: Point, sa: Side, sb: Side, lane: usize) -> Vec<Point> {
    match (sa, sb) {
        (Side::Top | Side::Bottom, _) => {
            let rise = b.1 - a.1;
            let turn = (LANE * (lane as f64 + 1.0))
                .min(rise.abs() - LANE / 2.0)
                .max(0.0);
            let my = a.1 + rise.signum() * turn;
            vec![a, (a.0, my), (b.0, my), b]
        }
        (Side::Right, Side::Right) => {
            let x = a.0.max(b.0) + LOOP_OUT;
            vec![a, (x, a.1), (x, b.1), b]
        }
        _ => {
            let mx = (a.0 + b.0) / 2.0;
            vec![a, (mx, a.1), (mx, b.1), b]
        }
    }
}

/// The midpoint of a polyline's middle segment, where its label goes.
pub(super) fn midpoint(points: &[Point]) -> Point {
    let k = points.len().saturating_sub(1) / 2;
    match (points.get(k), points.get(k + 1)) {
        (Some(a), Some(b)) => ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0),
        (Some(a), None) => *a,
        _ => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(parent: Option<usize>) -> Boxed {
        Boxed {
            parent,
            w: 100.0,
            h: 40.0,
        }
    }

    fn encloses(outer: &Rect, inner: &Rect) -> bool {
        outer.x <= inner.x
            && outer.y <= inner.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn a_chain_ranks_in_order() {
        assert_eq!(ranks(4, &[(2, 3), (0, 1), (1, 2)]), [0, 1, 2, 3]);
        // A shortcut does not pull its target up: the longest path wins.
        assert_eq!(ranks(3, &[(0, 1), (1, 2), (0, 2)]), [0, 1, 2]);
    }

    #[test]
    fn a_cycle_ranks_without_looping() {
        let r = ranks(3, &[(0, 1), (1, 2), (2, 0), (1, 1)]);
        assert_eq!(r, [0, 1, 2]);
        let layers = order(&r, &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(layers, [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn barycenters_uncross_two_chains() {
        // 0 -> 3 and 1 -> 2: the second layer follows the first.
        let edges = [(0, 3), (1, 2)];
        let r = ranks(4, &edges);
        assert_eq!(order(&r, &edges), [vec![0, 1], vec![3, 2]]);
    }

    #[test]
    fn a_container_encloses_its_parts() {
        // 0 holds 1 and 2, 2 holds 3; 4 is a second root that 3 points to.
        let boxes = [
            leaf(None),
            leaf(Some(0)),
            leaf(Some(0)),
            leaf(Some(2)),
            leaf(None),
        ];
        let edges = [(1, 3), (3, 4)];
        let (rects, size) = nest(&boxes, &edges);
        for (child, parent) in [(1, 0), (2, 0), (3, 2), (3, 0)] {
            assert!(
                encloses(&rects[parent], &rects[child]),
                "{child} in {parent}"
            );
        }
        assert!(!encloses(&rects[0], &rects[4]));
        for r in &rects {
            assert!(r.right() <= size.0 + 0.001 && r.bottom() <= size.1 + 0.001);
        }
        // The edge from inside 0 to 4 ranks 4 below 0.
        assert!(rects[4].y >= rects[0].bottom());
    }

    #[test]
    fn routes_leave_and_enter_by_facing_sides() {
        let rects = [
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 40.0,
            },
            Rect {
                x: 0.0,
                y: 100.0,
                w: 100.0,
                h: 40.0,
            },
        ];
        let routes = routes(&rects, &[(0, 1), (1, 0)]);
        assert_eq!(routes[0].first().unwrap().1, 40.0);
        assert_eq!(routes[0].last().unwrap().1, 100.0);
        // The two edges between the same boxes take different ports.
        assert_ne!(routes[0][0].0, routes[1].last().unwrap().0);
        // Each turns in the first lane next to its source.
        assert_eq!(midpoint(&routes[0]).1, 40.0 + LANE);
        assert_eq!(midpoint(&routes[1]).1, 100.0 - LANE);
    }
}
