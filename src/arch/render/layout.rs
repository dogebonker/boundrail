//! Pure geometry for the architecture view: a layered layout inside each container, containers
//! sized to enclose their parts, edge routes between box sides around the boxes in between,
//! and label places where no two labels, and no label and box, meet.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use super::svg::Point;

/// Space around a container's parts, and the band above them that holds its label.
pub(super) const PAD: f64 = 16.0;
pub(super) const HEADER: f64 = 56.0;
const H_GAP: f64 = 48.0;
const V_GAP: f64 = 72.0;
/// Barycenter sweeps, each one down and one up.
const SWEEPS: usize = 4;
/// How far a fallback edge between nested boxes runs out to the right before it turns.
const LOOP_OUT: f64 = 24.0;
/// How far routes keep from the boxes they pass, and how far they leave a port straight.
const CLEAR: f64 = 10.0;
/// The widest spacing of the lines routes run on.
const STEP: f64 = 12.0;
/// What a route pays for a bend and for crossing another route, in picture units of length.
const BEND: f64 = 40.0;
const CROSS: f64 = 30.0;
/// How many times its length a segment costs more for each route already on it.
const SHARED: f64 = 4.0;
/// Space kept around a label, and the spacing of the places tried for it.
const LABEL_GAP: f64 = 2.0;
const LABEL_STEP: f64 = 8.0;
/// How many rings of places around its route a label tries before giving up.
const RINGS: i32 = 400;
/// How close two coordinates lie to count as the same grid line.
const NEAR: f64 = 0.05;
/// How thick a route segment counts as when a label is tested against it.
const HAIRLINE: f64 = 0.1;

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

impl Side {
    /// The direction leaving the box by this side.
    fn out(self) -> Dir {
        match self {
            Side::Top => Dir::Up,
            Side::Bottom => Dir::Down,
            Side::Left => Dir::Left,
            Side::Right => Dir::Right,
        }
    }
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

/// `at` moved `by` in the direction `dir`.
fn step(at: Point, dir: Dir, by: f64) -> Point {
    match dir {
        Dir::Up => (at.0, at.1 - by),
        Dir::Down => (at.0, at.1 + by),
        Dir::Left => (at.0 - by, at.1),
        Dir::Right => (at.0 + by, at.1),
    }
}

/// The edge ends on each side of each box: where the other end is, the edge, and which end.
type Ports = BTreeMap<(usize, Side), Vec<(f64, usize, usize)>>;

/// What routes and labels steer around: the boxes, the box holding each, each container's
/// title, and areas drawn apart from the boxes, such as the picture's title.
pub(super) struct Scene<'a> {
    pub(super) rects: &'a [Rect],
    pub(super) parents: Vec<Option<usize>>,
    pub(super) titles: Vec<Option<Rect>>,
    pub(super) fixed: Vec<Rect>,
}

impl Scene<'_> {
    /// The boxes holding box `i`, nearest first.
    fn holders(&self, mut i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(parent) = self.parents[i] {
            out.push(parent);
            i = parent;
        }
        out
    }

    fn is_container(&self, i: usize) -> bool {
        self.parents.contains(&Some(i))
    }

    /// What a route between boxes `a` and `b` may not enter: every box but those holding an
    /// end, kept `CLEAR` away; and, when `strict`, the titles of those holders and the fixed
    /// areas.
    fn obstacles(&self, a: usize, b: usize, strict: bool) -> Vec<Rect> {
        let holders: BTreeSet<usize> = (self.holders(a).into_iter())
            .chain(self.holders(b))
            .collect();
        let mut out: Vec<Rect> = (0..self.rects.len())
            .filter(|i| !holders.contains(i))
            .map(|i| inflate(&self.rects[i], CLEAR))
            .collect();
        if strict {
            out.extend(holders.iter().filter_map(|&h| self.titles[h].clone()));
            out.extend(self.fixed.iter().cloned());
        }
        out
    }
}

fn inflate(r: &Rect, by: f64) -> Rect {
    Rect {
        x: r.x - by,
        y: r.y - by,
        w: r.w + 2.0 * by,
        h: r.h + 2.0 * by,
    }
}

/// Whether `a` and `b` overlap by more than an edge.
fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// A coordinate rounded to a tenth, the precision the picture is written in, so equal
/// coordinates compare equal.
fn snap(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// An orthogonal polyline for each edge from a side of its source to a side of its target,
/// around every box it does not connect. Edges sharing a side spread evenly along it, ordered
/// by where their other end is. Each route leaves its port straight for `CLEAR`, then follows
/// the cheapest path through the gaps between boxes: shortest, fewest bends, and away from the
/// routes drawn before it, shorter edges first.
pub(super) fn routes(scene: &Scene<'_>, edges: &[(usize, usize)]) -> Vec<Vec<Point>> {
    let rects = scene.rects;
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
            let at = port(&rects[node], side, (k as f64 + 1.0) / (n + 1.0));
            ends[e][end] = (snap(at.0), snap(at.1));
        }
    }
    let stubs: Vec<[Point; 2]> = (ends.iter().zip(&sides))
        .map(|(&[a, b], &(sa, sb))| [step(a, sa.out(), CLEAR), step(b, sb.out(), CLEAR)])
        .collect();
    let grid = Grid::new(scene, &stubs);
    let mut used = Used::new(&grid);
    let mut scratch = grid.scratch();
    let mut order: Vec<usize> = (0..edges.len()).collect();
    let span = |e: usize| {
        let [a, b] = stubs[e];
        (a.0 - b.0).abs() + (a.1 - b.1).abs()
    };
    order.sort_by(|&x, &y| span(x).total_cmp(&span(y)).then(x.cmp(&y)));
    let mut out = vec![Vec::new(); edges.len()];
    for e in order {
        let (a, b) = edges[e];
        let (sa, sb) = sides[e];
        let [pa, pb] = ends[e];
        let found = [true, false].into_iter().find_map(|strict| {
            grid.block(&scene.obstacles(a, b, strict), &mut scratch.blocked);
            let ends = (stubs[e][0], sa.out(), stubs[e][1], sb.out().back());
            grid.search(&mut scratch, ends, &used)
        });
        out[e] = match found {
            Some(path) => {
                used.add(&grid, &path);
                let mut points = vec![pa];
                points.extend(path.iter().map(|&n| grid.point(n)));
                points.push(pb);
                simplify(points)
            }
            None => bend(pa, pb, sa, sb),
        };
    }
    out
}

/// `points` without repeats and without points in the middle of a straight run.
fn simplify(points: Vec<Point>) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        if out.last() == Some(&p) {
            continue;
        }
        if let [.., a, b] = out[..] {
            let straight = (a.0 == b.0 && b.0 == p.0) || (a.1 == b.1 && b.1 == p.1);
            if straight {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// An orthogonal polyline from `a`, leaving by side `sa`, to `b`, entering by side `sb`, turning
/// halfway: the route when no path around the boxes exists.
fn bend(a: Point, b: Point, sa: Side, sb: Side) -> Vec<Point> {
    match (sa, sb) {
        (Side::Top | Side::Bottom, _) => {
            let my = (a.1 + b.1) / 2.0;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];

    fn back(self) -> Dir {
        match self {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
        }
    }

    fn across(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
}

/// The lines routes run on: every edge of every box kept `CLEAR` away, of every title and fixed
/// area, the routes' first and last points, two rings around everything, and lines between
/// those no more than `STEP` apart, so parallel routes find room.
// ponytail: one grid point per crossing of lines, searched per edge; fine for tens of boxes,
// use a sparse visibility graph if pictures of hundreds of boxes get slow.
struct Grid {
    xs: Vec<f64>,
    ys: Vec<f64>,
}

/// Which grid points, and which grid segments between neighbouring points, lie inside an
/// obstacle; a segment's index is its lower or left point's.
struct Blocked {
    node: Vec<bool>,
    across: Vec<bool>,
    down: Vec<bool>,
}

/// What a search reuses from edge to edge: the blocked grid, and each state's cost and
/// predecessor.
struct Scratch {
    blocked: Blocked,
    cost: Vec<f64>,
    from: Vec<usize>,
}

/// How many routes use each grid segment, and whether one runs across or down each point.
struct Used {
    across: Vec<u32>,
    down: Vec<u32>,
    node_across: Vec<bool>,
    node_down: Vec<bool>,
}

/// A search state's priority: lowest estimate first, then lowest state.
#[derive(PartialEq)]
struct Key(f64, usize);

impl Eq for Key {}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0).then(other.1.cmp(&self.1))
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Grid {
    fn new(scene: &Scene<'_>, stubs: &[[Point; 2]]) -> Self {
        let mut areas: Vec<Rect> = scene.rects.iter().map(|r| inflate(r, CLEAR)).collect();
        areas.extend(scene.titles.iter().flatten().cloned());
        areas.extend(scene.fixed.iter().cloned());
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for r in &areas {
            xs.extend([r.x, r.right()]);
            ys.extend([r.y, r.bottom()]);
        }
        for p in stubs.iter().flatten() {
            xs.push(p.0);
            ys.push(p.1);
        }
        let ring = |values: &mut Vec<f64>| {
            let low = values.iter().copied().fold(f64::INFINITY, f64::min);
            let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if low.is_finite() {
                values.extend([low - STEP, low - 2.0 * STEP, high + STEP, high + 2.0 * STEP]);
            }
        };
        ring(&mut xs);
        ring(&mut ys);
        Self {
            xs: lines(xs),
            ys: lines(ys),
        }
    }

    fn index(&self, p: Point) -> Option<usize> {
        let find = |values: &[f64], v: f64| {
            let k = values.partition_point(|&x| x < v - NEAR);
            (values.get(k).is_some_and(|&x| (x - v).abs() < NEAR)).then_some(k)
        };
        Some(find(&self.ys, p.1)? * self.xs.len() + find(&self.xs, p.0)?)
    }

    fn point(&self, node: usize) -> Point {
        let nx = self.xs.len();
        (self.xs[node % nx], self.ys[node / nx])
    }

    /// Room for one search at a time, refilled by each.
    fn scratch(&self) -> Scratch {
        let n = self.xs.len() * self.ys.len();
        Scratch {
            blocked: Blocked {
                node: vec![false; n],
                across: vec![false; n],
                down: vec![false; n],
            },
            cost: vec![f64::INFINITY; n * Dir::ALL.len()],
            from: vec![usize::MAX; n * Dir::ALL.len()],
        }
    }

    /// Marks in `b` the grid points and segments inside `obstacles`, and only those. Every
    /// obstacle's edges are grid lines, so a segment lies wholly inside or outside each.
    fn block(&self, obstacles: &[Rect], b: &mut Blocked) {
        let nx = self.xs.len();
        for marks in [&mut b.node, &mut b.across, &mut b.down] {
            marks.fill(false);
        }
        // Indices strictly inside (low, high), and segments within [low, high].
        let inside = |values: &[f64], low: f64, high: f64| {
            values.partition_point(|&v| v <= low + NEAR)
                ..values.partition_point(|&v| v < high - NEAR)
        };
        let within = |values: &[f64], low: f64, high: f64| {
            let start = values.partition_point(|&v| v < low - NEAR);
            let end = values.partition_point(|&v| v <= high + NEAR);
            start..end.saturating_sub(1).max(start)
        };
        for r in obstacles {
            let (ix, iy) = (
                inside(&self.xs, r.x, r.right()),
                inside(&self.ys, r.y, r.bottom()),
            );
            for j in iy.clone() {
                for i in ix.clone() {
                    b.node[j * nx + i] = true;
                }
                for i in within(&self.xs, r.x, r.right()) {
                    b.across[j * nx + i] = true;
                }
            }
            for j in within(&self.ys, r.y, r.bottom()) {
                for i in ix.clone() {
                    b.down[j * nx + i] = true;
                }
            }
        }
    }

    /// The neighbour of `node` in direction `dir`, the segment between them, and its length.
    fn next(&self, node: usize, dir: Dir) -> Option<(usize, usize, f64)> {
        let nx = self.xs.len();
        let (i, j) = (node % nx, node / nx);
        let (to, segment) = match dir {
            Dir::Left => (node.checked_sub(1).filter(|_| i > 0)?, node - 1),
            Dir::Right => ((i + 1 < nx).then_some(node + 1)?, node),
            Dir::Up => (node.checked_sub(nx).filter(|_| j > 0)?, node - nx),
            Dir::Down => ((j + 1 < self.ys.len()).then_some(node + nx)?, node),
        };
        let (a, b) = (self.point(node), self.point(to));
        Some((to, segment, (a.0 - b.0).abs() + (a.1 - b.1).abs()))
    }

    /// The cheapest grid path from `start`, leaving in direction `out`, to `goal`, arriving in
    /// direction `inward`, as grid points; `None` when an end lies inside an obstacle or no
    /// path exists. A* over (point, direction) with Manhattan distance as the estimate.
    fn search(
        &self,
        scratch: &mut Scratch,
        (start, out, goal, inward): (Point, Dir, Point, Dir),
        used: &Used,
    ) -> Option<Vec<usize>> {
        let Scratch {
            blocked,
            cost,
            from,
        } = scratch;
        let (s, g) = (self.index(start)?, self.index(goal)?);
        if blocked.node[s] || blocked.node[g] {
            return None;
        }
        let target = self.point(g);
        let estimate = |node: usize| {
            let p = self.point(node);
            (p.0 - target.0).abs() + (p.1 - target.1).abs()
        };
        let dirs = Dir::ALL.len();
        let slot = |node: usize, dir: Dir| node * dirs + dir as usize;
        cost.fill(f64::INFINITY);
        from.fill(usize::MAX);
        let mut queue = BinaryHeap::new();
        cost[slot(s, out)] = 0.0;
        queue.push(Key(estimate(s), slot(s, out)));
        while let Some(Key(priority, state)) = queue.pop() {
            let (node, dir) = (state / dirs, Dir::ALL[state % dirs]);
            // A state reached more cheaply after this entry was queued is already expanded.
            if priority > cost[state] + estimate(node) {
                continue;
            }
            if node == g {
                let mut path = vec![node];
                let mut at = state;
                while from[at] != usize::MAX {
                    at = from[at];
                    path.push(at / dirs);
                }
                path.reverse();
                return Some(path);
            }
            for next in Dir::ALL {
                if next == dir.back() {
                    continue;
                }
                let Some((to, segment, length)) = self.next(node, next) else {
                    continue;
                };
                let (wall, shared, crossed) = if next.across() {
                    (
                        blocked.across[segment],
                        used.across[segment],
                        used.node_down[to],
                    )
                } else {
                    (
                        blocked.down[segment],
                        used.down[segment],
                        used.node_across[to],
                    )
                };
                if wall || blocked.node[to] {
                    continue;
                }
                let mut step = length * (1.0 + SHARED * f64::from(shared));
                if next != dir {
                    step += BEND;
                }
                if crossed {
                    step += CROSS;
                }
                if to == g && next != inward {
                    step += BEND;
                }
                let total = cost[state] + step;
                if total < cost[slot(to, next)] {
                    cost[slot(to, next)] = total;
                    from[slot(to, next)] = state;
                    queue.push(Key(total + estimate(to), slot(to, next)));
                }
            }
        }
        None
    }
}

/// `values` sorted, snapped and without repeats, with lines added between neighbours more than
/// `STEP` apart.
fn lines(values: Vec<f64>) -> Vec<f64> {
    let mut values: Vec<f64> = values.into_iter().map(snap).collect();
    values.sort_by(f64::total_cmp);
    values.dedup();
    let mut out = Vec::with_capacity(values.len() * 2);
    for pair in values.windows(2) {
        let (low, high) = (pair[0], pair[1]);
        out.push(low);
        let parts = ((high - low) / STEP).ceil();
        for k in 1..parts as usize {
            out.push(snap(low + (high - low) * k as f64 / parts));
        }
    }
    out.extend(values.last());
    out.dedup();
    out
}

impl Used {
    fn new(grid: &Grid) -> Self {
        let n = grid.xs.len() * grid.ys.len();
        Self {
            across: vec![0; n],
            down: vec![0; n],
            node_across: vec![false; n],
            node_down: vec![false; n],
        }
    }

    /// Marks the segments and points of `path` as used.
    fn add(&mut self, grid: &Grid, path: &[usize]) {
        let nx = grid.xs.len();
        for pair in path.windows(2) {
            let (a, b) = (pair[0].min(pair[1]), pair[0].max(pair[1]));
            let across = b - a == 1 && a / nx == b / nx;
            if across {
                self.across[a] += 1;
            } else {
                self.down[a] += 1;
            }
            for node in [a, b] {
                if across {
                    self.node_across[node] = true;
                } else {
                    self.node_down[node] = true;
                }
            }
        }
    }
}

/// The center of each label, `sizes` wide and high, beside its route: on the route where the
/// label meets no other label, no box or container title, and no other route; else where it
/// meets only other routes; else just beside the route; else at the nearest free spot. A label
/// may sit inside a container, below its title band, but never across its border. Labels are
/// placed in edge order, so the same picture places them the same way.
pub(super) fn labels(scene: &Scene<'_>, routes: &[Vec<Point>], sizes: &[Point]) -> Vec<Point> {
    let mut placed: Vec<Rect> = Vec::new();
    let mut centers = Vec::with_capacity(routes.len());
    for (k, (route, &(w, h))) in routes.iter().zip(sizes).enumerate() {
        let rect = |c: Point| Rect {
            x: c.0 - w / 2.0,
            y: c.1 - h / 2.0,
            w,
            h,
        };
        let free = |c: Point, soft: bool| {
            let r = inflate(&rect(c), LABEL_GAP);
            !(blocks_label(scene, &placed, &r) || (soft && crosses_route(routes, k, &r)))
        };
        let mut segments: Vec<usize> = (0..route.len().saturating_sub(1)).collect();
        let middle = segments.len().saturating_sub(1) / 2;
        segments.sort_by_key(|&s| (s.abs_diff(middle), s));
        // The middle of the middle segment: where rings start, and the place of last resort.
        let origin = segments.first().map_or((0.0, 0.0), |&s| {
            let (a, b) = (route[s], route[s + 1]);
            ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
        });
        let on = |s: usize| along(route[s], route[s + 1]);
        let beside = |s: usize| {
            let (a, b) = (route[s], route[s + 1]);
            let (dx, dy) = if a.1 == b.1 {
                (0.0, h / 2.0 + LABEL_GAP)
            } else {
                (w / 2.0 + LABEL_GAP, 0.0)
            };
            along(a, b)
                .into_iter()
                .flat_map(move |c| [(c.0 + dx, c.1 + dy), (c.0 - dx, c.1 - dy)])
        };
        let found = (segments.iter().flat_map(|&s| on(s)))
            .find(|&c| free(c, true))
            .or_else(|| {
                segments
                    .iter()
                    .flat_map(|&s| on(s))
                    .find(|&c| free(c, false))
            })
            .or_else(|| {
                segments
                    .iter()
                    .flat_map(|&s| beside(s))
                    .find(|&c| free(c, false))
            })
            .or_else(|| rings(origin).find(|&c| free(c, false)));
        let center = found.unwrap_or(origin);
        placed.push(rect(center));
        centers.push(center);
    }
    centers
}

/// Points on the segment from `a` to `b`: its middle, then `LABEL_STEP` apart outward.
fn along(a: Point, b: Point) -> Vec<Point> {
    let length = (b.0 - a.0).abs() + (b.1 - a.1).abs();
    let middle = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    if length == 0.0 {
        return vec![middle];
    }
    let (ux, uy) = ((b.0 - a.0) / length, (b.1 - a.1) / length);
    let mut out = vec![middle];
    let mut d = LABEL_STEP;
    while d <= length / 2.0 {
        for s in [d, -d] {
            out.push((middle.0 + ux * s, middle.1 + uy * s));
        }
        d += LABEL_STEP;
    }
    out
}

/// Points on square rings around `origin`, `LABEL_STEP` apart, nearest rings first; the last
/// ring lies far outside any picture.
fn rings(origin: Point) -> impl Iterator<Item = Point> {
    (1..=RINGS).flat_map(move |ring: i32| {
        let r = f64::from(ring) * LABEL_STEP;
        let side = (-ring..ring).map(f64::from);
        let top = side
            .clone()
            .map(move |k| (origin.0 + k * LABEL_STEP, origin.1 - r));
        let right = side
            .clone()
            .map(move |k| (origin.0 + r, origin.1 + k * LABEL_STEP));
        let bottom = side
            .clone()
            .map(move |k| (origin.0 - k * LABEL_STEP, origin.1 + r));
        let left = side.map(move |k| (origin.0 - r, origin.1 - k * LABEL_STEP));
        top.chain(right).chain(bottom).chain(left)
    })
}

/// Whether a label at `r` meets a placed label, a box, a container's title band or border, or
/// a fixed area.
fn blocks_label(scene: &Scene<'_>, placed: &[Rect], r: &Rect) -> bool {
    let boxes = scene.rects.iter().enumerate().any(|(i, b)| {
        if !overlaps(r, b) {
            return false;
        }
        if !scene.is_container(i) {
            return true;
        }
        let content = Rect {
            x: b.x,
            y: b.y + HEADER,
            w: b.w,
            h: b.h - HEADER,
        };
        !(r.x >= content.x
            && r.right() <= content.right()
            && r.y >= content.y
            && r.bottom() <= content.bottom())
    });
    boxes || placed.iter().chain(&scene.fixed).any(|p| overlaps(r, p))
}

/// Whether `r` meets a segment of any route but route `own`.
fn crosses_route(routes: &[Vec<Point>], own: usize, r: &Rect) -> bool {
    (routes.iter().enumerate())
        .filter(|(k, _)| *k != own)
        .flat_map(|(_, route)| route.windows(2))
        .any(|pair| {
            let (a, b) = (pair[0], pair[1]);
            let line = Rect {
                x: a.0.min(b.0),
                y: a.1.min(b.1),
                w: (a.0 - b.0).abs().max(HAIRLINE),
                h: (a.1 - b.1).abs().max(HAIRLINE),
            };
            overlaps(r, &line)
        })
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
        let routes = routes(&scene(&rects), &[(0, 1), (1, 0)]);
        assert_eq!(routes[0].first().unwrap().1, 40.0);
        assert_eq!(routes[0].last().unwrap().1, 100.0);
        // The two edges between the same boxes take different ports.
        assert_ne!(routes[0][0].0, routes[1].last().unwrap().0);
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn scene(rects: &[Rect]) -> Scene<'_> {
        Scene {
            rects,
            parents: vec![None; rects.len()],
            titles: vec![None; rects.len()],
            fixed: Vec::new(),
        }
    }

    /// Whether the segment from `a` to `b` runs through the inside of `r`.
    fn cuts(a: Point, b: Point, r: &Rect) -> bool {
        let line = rect(
            a.0.min(b.0),
            a.1.min(b.1),
            (a.0 - b.0).abs(),
            (a.1 - b.1).abs(),
        );
        line.x < r.right() && r.x < line.right() && line.y < r.bottom() && r.y < line.bottom()
    }

    #[test]
    fn a_route_goes_around_the_box_between_its_ends() {
        let rects = [
            rect(0.0, 0.0, 100.0, 40.0),
            rect(0.0, 200.0, 100.0, 40.0),
            rect(-20.0, 100.0, 140.0, 40.0),
        ];
        let route = &routes(&scene(&rects), &[(0, 1)])[0];
        assert_eq!((route[0].1, route.last().unwrap().1), (40.0, 200.0));
        for pair in route.windows(2) {
            assert!(
                pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1,
                "{route:?}"
            );
            assert!(!cuts(pair[0], pair[1], &rects[2]), "{route:?}");
        }
        // The same boxes route the same way.
        assert_eq!(&routes(&scene(&rects), &[(0, 1)])[0], route);
    }

    #[test]
    fn labels_on_a_shared_segment_do_not_meet_each_other_or_a_box() {
        let line = vec![(0.0, 0.0), (300.0, 0.0)];
        let boxes = [rect(130.0, -20.0, 40.0, 40.0)];
        let sizes = [(80.0, 16.0); 3];
        let centers = labels(&scene(&boxes), &[line.clone(), line.clone(), line], &sizes);
        let placed: Vec<Rect> = (centers.iter())
            .map(|c| rect(c.0 - 40.0, c.1 - 8.0, 80.0, 16.0))
            .collect();
        for (k, a) in placed.iter().enumerate() {
            assert!(!overlaps(a, &boxes[0]), "{centers:?}");
            for b in &placed[k + 1..] {
                assert!(!overlaps(a, b), "{centers:?}");
            }
        }
    }
}
