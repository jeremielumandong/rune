//! Following a scroll or a pan by moving the kept frame's pixels (AgentOps patch,
//! PARITY-PATCH.md).
//!
//! A scroll moves every instance in a viewport by the same offset, so the plain comparison
//! damages the whole viewport. Here the kept frame's viewport pixels are moved by that offset
//! instead ([`Shift`], two texture copies), and only what the move cannot account for is
//! redrawn: the strips the move exposes (one along an edge for a scroll, an L for a pan that
//! moves both ways), and anything in the viewport that did not move with the content (a
//! scrollbar, an overlay, a background that is not one flat colour).
//!
//! A moved pixel equals a redrawn one when everything that covers it moved by the same whole
//! number of device pixels: the shader computes coverage from the pixel's position relative to
//! each quad. Instances qualify when their bytes are equal but for their coordinates, moved by
//! the offset (within 1/64 px), and their clip is the viewport's or a clip that moved with them
//! inside it. The moved area stays clear of the viewport's antialiased or faded edges and
//! rounded corners.
//!
//! A move with no horizontal part compares x coordinates byte for byte, as a vertical scroll
//! always did; one with a horizontal part treats x words as it treats y words.
use super::*;

/// Fixed-point steps per device pixel for comparing moved coordinates.
const Q: f32 = 64.0;
/// The fewest moved instances worth a move.
const MIN_MOVED: usize = 8;

/// The kept frame's pixels in `to` (`[x0, y0, x1, y1]`) come from `dx` pixels left and `dy`
/// pixels above (right and below when negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shift {
    pub to: [u32; 4],
    pub dx: i32,
    pub dy: i32,
}

/// Word indices (of an instance's 32) that hold y coordinates, for a kind.
fn y_words(kind: f32) -> &'static [usize] {
    if kind == KIND_SEGMENT {
        &[1, 5, 7, 25, 27]
    } else if kind == KIND_TRIANGLE {
        &[1, 5, 7, 21, 25, 27]
    } else {
        &[1, 25, 27]
    }
}

/// Word indices that hold x coordinates, for a kind: each y word's partner.
fn x_words(kind: f32) -> &'static [usize] {
    if kind == KIND_SEGMENT {
        &[0, 4, 6, 24, 26]
    } else if kind == KIND_TRIANGLE {
        &[0, 4, 6, 20, 24, 26]
    } else {
        &[0, 24, 26]
    }
}

/// An offset in device pixels, and whether x words take part (`dx != 0`): a vertical move keeps
/// them byte for byte.
#[derive(Clone, Copy, Debug)]
pub(super) struct Move {
    dx: f32,
    dy: f32,
    xs: bool,
}

impl Move {
    fn vertical(dy: f32) -> Self { Move { dx: 0.0, dy, xs: false } }
    fn new(dx: f32, dy: f32) -> Self { Move { dx, dy, xs: dx != 0.0 } }
    /// No offset, comparing what `self` compares.
    fn still(self) -> Self { Move { dx: 0.0, dy: 0.0, xs: self.xs } }
}

/// Whether moving an instance's coordinates moves all it draws (layers and effect slots
/// composite other passes).
fn movable(i: &Instance) -> bool {
    let kind = i.params[1];
    !(kind == KIND_LAYER || kind == KIND_LAYER_END || kind == KIND_EFFECT || kind == KIND_BACKDROP_BLUR) && kind.is_finite()
}

/// Whether a layer record composites what it holds the same wherever it moves by whole
/// pixels: no blur (whose taps reach past what moved), turn or cache.
fn plain_layer(i: &Instance) -> bool {
    i.params[1] == KIND_LAYER && i.params[2] == 0.0 && i.params[3] == 0.0 && i.border == [0.0; 4]
}

/// Each outermost layer's instances, `first..past`, from its record to its end, into `out`.
fn layers(p: &Painted, out: &mut Vec<(usize, usize)>) {
    out.clear();
    for (k, &s) in p.scopes.iter().enumerate() {
        if s == NONE { continue; }
        match out.last_mut() {
            Some(last) if last.1 == k && p.scopes[last.0] == s => last.1 = k + 1,
            _ => out.push((k, k + 1)),
        }
    }
}

/// How far an instance's rounded clip bends in from its edges: continuous corners bend in
/// further than circular ones of the same radius.
fn radius(i: &Instance) -> f32 {
    unpack_clip_radii(i.fade[3]).iter().fold(0.0f32, |m, r| m.max(r.abs() * 1.6))
}

/// A layer's instances hashed but for the coordinates `m` compares.
fn layer_key(p: &Painted, a: usize, b: usize, m: Move) -> u64 {
    p.instances[a..b].iter().fold(0xcbf2_9ce4_8422_2325u64, |h, i| (h ^ hash_words(&canon(i, m.still(), false, true))).wrapping_mul(0x0000_0100_0000_01b3))
}

/// How each instance of a layer moved by `m` from `old` to `new`, into `ways`, when all of them
/// did (as `moved_near` finds, when `edges` are given) and its records are `plain_layer`s; an
/// end record counts as moved with its clip.
fn moved_layer(old: &[Instance], new: &[Instance], m: Move, edges: Option<[f32; 4]>, ways: &mut Vec<Pair>) -> bool {
    ways.clear();
    for (o, n) in old.iter().zip(new) {
        let kind = n.params[1];
        let how = if kind == KIND_LAYER_END {
            same(o, n).then_some(Pair::Moved(true))
        } else if !(movable(n) || plain_layer(n)) || (kind == KIND_LAYER && !plain_layer(o)) {
            None
        } else if let Some(edges) = edges {
            moved_near(o, n, m, edges)
        } else {
            let mine = canon(n, m.still(), false, false);
            if canon(o, m, false, false) == mine { Some(Pair::Moved(false)) }
            else if canon(o, m, true, false) == mine { Some(Pair::Moved(true)) }
            else { None }
        };
        let Some(how) = how else { return false };
        ways.push(how);
    }
    true
}

/// How `n` is `o` moved by `m`, its coordinates within a step of `1/Q` px either way (a
/// position computed another way can round to the next step): with its clip kept or moved, or
/// moved with clips the viewport's edge cuts differently, alike between `edges` (the pixels a
/// move can reach). Faded clips, whose ramps start at their edges, are not cut.
fn moved_near(o: &Instance, n: &Instance, m: Move, edges: [f32; 4]) -> Option<Pair> {
    let kind = n.params[1];
    let (ys, xs) = (y_words(kind), if m.xs { x_words(kind) } else { &[] });
    let (ow, nw): ([u32; 32], [u32; 32]) = (bytemuck::cast(*o), bytemuck::cast(*n));
    let derived = |k: usize| derived_height(kind) && (k == 3 || (k == 2 && m.xs));
    let near = |a: &[u32; 32], b: &[u32; 32]| (0..32).all(|k| {
        if (ys.contains(&k) || xs.contains(&k) || derived(k)) && a[k] != b[k] {
            let (x, y) = (f32::from_bits(ow[k]), f32::from_bits(nw[k]));
            x.is_finite() && y.is_finite() && x.abs() < 1.0e7 && y.abs() < 1.0e7 && (a[k] as i32).abs_diff(b[k] as i32) <= 1
        } else {
            a[k] == b[k]
        }
    });
    let mine = canon(n, m.still(), false, false);
    if near(&canon(o, m, false, false), &mine) { return Some(Pair::Moved(false)); }
    let mut moved = canon(o, m, true, false);
    if near(&moved, &mine) { return Some(Pair::Moved(true)); }
    if o.fade[..3] != [0; 3] || n.fade[..3] != [0; 3] { return None; }
    let q = |v: f32| ((v * Q).round() as i32) as u32;
    let mut mine = mine;
    (moved[25], moved[27]) = (q((o.clip[1] + m.dy).max(edges[1])), q((o.clip[3] + m.dy).min(edges[3])));
    (mine[25], mine[27]) = (q(n.clip[1].max(edges[1])), q(n.clip[3].min(edges[3])));
    if m.xs {
        (moved[24], moved[26]) = (q((o.clip[0] + m.dx).max(edges[0])), q((o.clip[2] + m.dx).min(edges[2])));
        (mine[24], mine[26]) = (q(n.clip[0].max(edges[0])), q(n.clip[2].min(edges[2])));
    }
    near(&moved, &mine).then_some(Pair::Cut)
}

/// An instance's words with its coordinates moved by `m` (its clip's too when `clip_moves`) and
/// fixed to `1/Q` px, or zeroed when `zero`, for hashing and comparing; x words only when the
/// move has a horizontal part (`Move::xs`).
fn canon(i: &Instance, m: Move, clip_moves: bool, zero: bool) -> [u32; 32] {
    let mut w: [u32; 32] = bytemuck::cast(*i);
    let kind = i.params[1];
    let mut fix = |words: &[usize], by: f32| for &k in words {
        let v = f32::from_bits(w[k]);
        w[k] = if zero {
            0
        } else if v.is_finite() && v.abs() < 1.0e7 {
            let moved = if k < 24 || clip_moves { v + by } else { v };
            ((moved * Q).round() as i32) as u32
        } else {
            // `NO_CLIP` and the like stay put.
            w[k]
        };
    };
    fix(y_words(kind), m.dy);
    if m.xs { fix(x_words(kind), m.dx); }
    // A segment's or triangle's rect bounds its points, its height (and width) their
    // difference, which can round differently as they move.
    if derived_height(kind) {
        let dims: &[usize] = if m.xs { &[2, 3] } else { &[3] };
        for &k in dims {
            let h = f32::from_bits(w[k]);
            w[k] = if zero { 0 } else if h.is_finite() && h.abs() < 1.0e7 { ((h * Q).round() as i32) as u32 } else { w[k] };
        }
    }
    w
}

fn derived_height(kind: f32) -> bool { kind == KIND_SEGMENT || kind == KIND_TRIANGLE }

fn hash_words(w: &[u32; 32]) -> u64 { hash32(w) }

/// The buffers `scrolled` works in, kept from frame to frame (`damage::Work`).
#[derive(Default)]
pub(in crate::render) struct Buffers {
    olds: Vec<(u64, f32)>,
    sample: Vec<(u64, f32)>,
    votes: Vec<i64>,
    runs: Vec<(usize, i64)>,
    /// Pan votes from instances at the same index (from the front and from the back).
    pan_votes: Vec<(i64, i64)>,
    pan_runs: Vec<(usize, (i64, i64))>,
    /// Old and sampled new instances under keys with x and y zeroed, with their y and x.
    olds2: Vec<(u64, f32, f32)>,
    sample2: Vec<(u64, f32, f32)>,
    exact: Vec<(u64, u32)>,
    moved: Vec<(u64, u32, bool)>,
    used: Vec<bool>,
    taken: Vec<bool>,
    pairs: Vec<(u32, u32, Pair, u32)>,
    old_layers: Vec<(usize, usize)>,
    olds_layered: Vec<(u64, usize, usize)>,
    new_layers: Vec<(usize, usize)>,
    ways: Vec<Pair>,
    clips: Vec<[u32; 4]>,
    loose: Vec<(u64, u32)>,
    order: Vec<(u32, u32)>,
    run: Run,
    broken: Vec<u32>,
    paired: Vec<Pair>,
}

/// The offset most instances moved by, whole numbers of device pixels, guessed from up to 256
/// new instances. Vertical offsets come from each against the old ones that match it but for y,
/// nearest in y first; offsets with a horizontal part (a pan) from each against the old one at
/// the same index, counted from the front and from the back (a pan keeps the painter order).
/// Of the likeliest, the one that accounts for the most sampled instances wins, if it accounts
/// for a sixteenth of them at least; pans are looked at first, and one that accounts for half
/// the sample is taken without looking for a vertical offset.
pub(super) fn guess(old: &Painted, new: &Painted, b: &mut Buffers) -> Option<Move> {
    let Buffers { olds, sample, votes, runs, pan_votes, pan_runs, olds2, sample2, .. } = b;
    let q = Q as i64;
    let whole = |d: i64| (d % q).abs() <= 1;
    let step = (new.instances.len() / 256).max(1);
    // The sampled instances, of which a move must account for a sixteenth (a zoom lines up
    // a few by chance).
    let sampled = (0..new.instances.len()).step_by(step).filter(|&j| new.scopes[j] == NONE && movable(&new.instances[j])).count();
    let enough = |count: usize| count >= 4 && count * 16 >= sampled;
    // Pans: the same instance at the same index, moved both ways.
    pan_votes.clear();
    let (lo, ln) = (old.instances.len(), new.instances.len());
    for j in (0..ln).step_by(step) {
        let tail = (j + lo).checked_sub(ln).filter(|_| lo != ln);
        for i in [Some(j).filter(|&i| i < lo), tail].into_iter().flatten() {
            let (o, n) = (&old.instances[i], &new.instances[j]);
            if old.scopes[i] != NONE || new.scopes[j] != NONE || !movable(n) || o.params[1] != n.params[1] { continue; }
            let both = Move::new(1.0, 0.0);
            if canon(o, both, false, true) != canon(n, both, false, true) { continue; }
            let (dx, dy) = (n.rect[0] - o.rect[0], n.rect[1] - o.rect[1]);
            if dx != 0.0 && dx.is_finite() && dy.is_finite() { pan_votes.push(((dx * Q).round() as i64, (dy * Q).round() as i64)); }
        }
    }
    let mut pan = None;
    if pan_votes.len() >= 4 {
        pan_votes.sort_unstable();
        pan_runs.clear();
        for (k, v) in pan_votes.iter().enumerate() {
            if k > 0 && pan_votes[k - 1] == *v { pan_runs.last_mut().unwrap().0 += 1 } else { pan_runs.push((1, *v)) }
        }
        pan_runs.sort_unstable_by(|a, b| b.cmp(a));
        let candidates = pan_runs.iter().take(4).filter(|r| r.0 >= 4 && whole(r.1.0) && whole(r.1.1) && r.1.0.abs() >= q / 2);
        let mut candidates = candidates.peekable();
        if candidates.peek().is_some() {
            // Keys with x and y zeroed, sorted by y then x, to check each candidate against.
            let both = Move::new(1.0, 0.0);
            olds2.clear();
            olds2.extend((0..lo).filter(|&i| old.scopes[i] == NONE && movable(&old.instances[i]))
                .map(|i| (hash_words(&canon(&old.instances[i], both, false, true)), old.instances[i].rect[1], old.instances[i].rect[0])));
            olds2.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.total_cmp(&b.2)));
            sample2.clear();
            sample2.extend((0..ln).step_by(step).filter(|&j| new.scopes[j] == NONE && movable(&new.instances[j]))
                .map(|j| (hash_words(&canon(&new.instances[j], both, false, true)), new.instances[j].rect[1], new.instances[j].rect[0])));
            let olds2 = &olds2[..];
            let accounted2 = |(dx, dy): (i64, i64)| sample2.iter().filter(|&&(key, y, x)| {
                let (wy, wx) = (y - dy as f32 / Q, x - dx as f32 / Q);
                let same = &olds2[olds2.partition_point(|e| e.0 < key)..olds2.partition_point(|e| e.0 <= key)];
                let at = same.partition_point(|e| e.1 < wy - 1.0 / Q);
                same[at..].iter().take_while(|e| e.1 <= wy + 1.0 / Q).any(|e| (e.2 - wx).abs() <= 1.0 / Q)
            }).count();
            pan = candidates.map(|r| (accounted2(r.1), r.1.0, r.1.1)).max();
        }
    }
    // Most of the sample panned: no need to look for a vertical offset.
    if let Some((count, dx, dy)) = pan && count * 2 >= sampled {
        return Some(Move::new(((dx as f32) / Q).round(), ((dy as f32) / Q).round()));
    }
    let vertical = Move::vertical(0.0);
    olds.clear();
    olds.extend((0..old.instances.len())
        .filter(|&i| old.scopes[i] == NONE && movable(&old.instances[i]))
        .map(|i| (hash_words(&canon(&old.instances[i], vertical, false, true)), old.instances[i].rect[1])));
    olds.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    sample.clear();
    sample.extend((0..new.instances.len()).step_by(step)
        .filter(|&j| new.scopes[j] == NONE && movable(&new.instances[j]))
        .map(|j| (hash_words(&canon(&new.instances[j], vertical, false, true)), new.instances[j].rect[1])));
    let olds = &olds[..];
    let range = |key: u64| &olds[olds.partition_point(|e| e.0 < key)..olds.partition_point(|e| e.0 <= key)];
    votes.clear();
    for &(key, y) in sample.iter() {
        let same = range(key);
        let near = same.partition_point(|e| e.1 < y);
        for e in &same[near.saturating_sub(4)..(near + 4).min(same.len())] {
            let d = y - e.1;
            if d != 0.0 && d.is_finite() { votes.push((d * Q).round() as i64); }
        }
    }
    votes.sort_unstable();
    runs.clear();
    for (k, v) in votes.iter().enumerate() {
        if k > 0 && votes[k - 1] == *v { runs.last_mut().unwrap().0 += 1 } else { runs.push((1, *v)) }
    }
    runs.sort_unstable_by(|a, b| b.cmp(a));
    // Repeating content (the same glyph in each row) votes for its pitch too: of the likeliest
    // offsets, take the one that accounts for the most sampled instances.
    let accounted = |d: i64| sample.iter().filter(|&&(key, y)| {
        let want = y - d as f32 / Q;
        let same = range(key);
        let at = same.partition_point(|e| e.1 < want - 1.0 / Q);
        same.get(at).is_some_and(|e| (e.1 - want).abs() <= 1.0 / Q)
    }).count();
    let best = runs.iter().take(4).filter(|r| r.0 >= 4 && whole(r.1) && r.1.abs() >= q / 2)
        .map(|r| (accounted(r.1), 0, r.1)).max();
    let (count, dx, dy) = best.max(pan)?;
    let (dx, dy) = (((dx as f32) / Q).round(), ((dy as f32) / Q).round());
    (enough(count) && (dx != 0.0 || dy != 0.0)).then(|| Move::new(dx, dy))
}

/// The window pixel edges a clip `c` covers fully (`inner`) and touches at all (`outer`):
/// a whole-pixel edge is exact, a fractional one gives a pixel either way.
fn clip_edges(c: [f32; 4], size: (u32, u32)) -> ([f32; 4], [f32; 4]) {
    let whole = |v: f32| (v - v.round()).abs() < 1.0e-3;
    let lo_in = |v: f32| if whole(v) { v.round() } else { v.ceil() + 1.0 };
    let hi_in = |v: f32| if whole(v) { v.round() } else { v.floor() - 1.0 };
    let lo_out = |v: f32| if whole(v) { v.round() } else { v.floor() - 1.0 };
    let hi_out = |v: f32| if whole(v) { v.round() } else { v.ceil() + 1.0 };
    let inner = clamp_edges([lo_in(c[0]), lo_in(c[1]), hi_in(c[2]), hi_in(c[3])], size);
    let outer = clamp_edges([lo_out(c[0]), lo_out(c[1]), hi_out(c[2]), hi_out(c[3])], size);
    (inner, outer)
}

fn cut(a: [f32; 4], b: [f32; 4]) -> Option<[f32; 4]> {
    let e = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (e[2] > e[0] && e[3] > e[1]).then_some(e)
}

fn moved_by(e: [f32; 4], dx: f32, dy: f32) -> [f32; 4] { [e[0] + dx, e[1] + dy, e[2] + dx, e[3] + dy] }

/// Whether `i` draws the same along the way `m` moves, all over `area`, so moving its pixels
/// there changes nothing: a plain fill (with or without a border) whose edges across the move
/// (with their border, antialiasing and rounded corners), and its clip's, are clear of `area`
/// or out past the window's. A vertical move keeps each column, so then its left and right
/// edges can be anywhere; a horizontal one keeps each row. A quad banded to nothing
/// (`render/cull.rs`) draws nothing at all.
fn flat_over(i: &Instance, area: [f32; 4], size: (u32, u32), m: Move) -> bool {
    let kind = i.params[1];
    if kind <= KIND_SHADOW && i.uv[2] < 0.0 && i.uv[3] <= i.uv[1] { return true; }
    // Faded clip edges vary across them; a rounded clip only near its edges.
    if kind != KIND_RECT || i.uv != [0.0; 4] || i.fade[..3] != [0; 3] || !(i.params[0] >= 0.0) { return false; }
    let radius = i.radii.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let reach = radius + i.params[0] + 1.0 + 2.0 * i.params[2].abs();
    let clip_reach = unpack_clip_radii(i.fade[3]).iter().fold(0.0f32, |m, r| m.max(r.abs() * 1.6)) + 1.0;
    if !clip_reach.is_finite() { return false; }
    let (w, h) = (size.0 as f32, size.1 as f32);
    // `a` and `b`: the low and high edges of `area` this way; `end`, the window's.
    let spans = |lo: f32, hi: f32, by: f32, a: f32, b: f32, end: f32| (lo + by <= a || lo <= -by) && (hi - by >= b || hi >= end + by);
    let (r, c) = (i.rect, i.clip);
    (m.dy == 0.0 || (spans(r[1], r[1] + r[3], reach, area[1], area[3], h) && spans(c[1], c[3], clip_reach, area[1], area[3], h)))
        && (m.dx == 0.0 || (spans(r[0], r[0] + r[2], reach, area[0], area[2], w) && spans(c[0], c[2], clip_reach, area[0], area[2], w)))
}

/// How a frame's instances pair with the kept frame's.
#[derive(Clone, Copy, PartialEq)]
enum Pair {
    None,
    /// Byte for byte the same.
    Same,
    /// Moved by the offset; whether its clip moved too.
    Moved(bool),
    /// Moved by the offset with its clip, which the viewport's edge cuts differently: alike
    /// between the edges but for its rounded corners.
    Cut,
}

impl Pair {
    fn moved(self) -> bool { matches!(self, Pair::Moved(_) | Pair::Cut) }
}

/// The move that follows a scroll or a pan from `old` to `new`, with the edges still to redraw
/// pushed onto `out`; None when nothing scrolled.
pub(super) fn scrolled(old: &Painted, new: &Painted, size: (u32, u32), out: &mut Vec<[f32; 4]>, b: &mut Buffers) -> Option<Shift> {
    let m = guess(old, new, b)?;
    let (dx, dy) = (m.dx, m.dy);
    let Buffers { exact, moved, used, taken, pairs, old_layers, olds_layered: olds, new_layers: news, ways, clips, loose, order, run: lis, broken, paired, .. } = b;
    used.clear();
    used.resize(old.instances.len(), false);
    taken.clear();
    taken.resize(new.instances.len(), false);
    // (old, new, how, the outermost layer moved whole it belongs to or `NONE`)
    pairs.clear();
    // Each new instance against the old one on the diagonal the last pair found (a scroll or a
    // pan keeps the painter order), and only failing that against all old instances, under
    // their bytes (exact) and moved by `m` (clip kept, and clip moved): tables made the first
    // time one is needed.
    let mut tables = false;
    let mut diagonal = 0isize;
    for j in 0..new.instances.len() {
        let n = &new.instances[j];
        let top = new.scopes[j] == NONE && movable(n);
        let mine = top.then(|| canon(n, m.still(), false, false));
        let mut found = None;
        let i = j as isize + diagonal;
        if i >= 0 && (i as usize) < old.instances.len() && !used[i as usize] {
            let (i, o) = (i as usize, &old.instances[i as usize]);
            if old.keys[i] == new.keys[j] && same(o, n) {
                found = Some((i as u32, Pair::Same));
            } else if let Some(mine) = &mine && old.scopes[i] == NONE && movable(o) {
                if canon(o, m, false, false) == *mine { found = Some((i as u32, Pair::Moved(false))); }
                else if canon(o, m, true, false) == *mine { found = Some((i as u32, Pair::Moved(true))); }
            }
        }
        if found.is_none() {
            if !tables {
                tables = true;
                exact.clear();
                exact.extend((0..old.instances.len()).map(|i| (old.keys[i], i as u32)));
                exact.sort_unstable();
                moved.clear();
                for (i, o) in old.instances.iter().enumerate() {
                    if old.scopes[i] != NONE || !movable(o) { continue; }
                    let kept = hash_words(&canon(o, m, false, false));
                    moved.push((kept, i as u32, false));
                    let along = hash_words(&canon(o, m, true, false));
                    if along != kept { moved.push((along, i as u32, true)); }
                }
                moved.sort_unstable_by_key(|e| (e.0, e.1));
            }
            let key = new.keys[j];
            let at = exact.partition_point(|e| e.0 < key);
            found = exact[at..].iter().take_while(|e| e.0 == key)
                .find(|e| !used[e.1 as usize] && same(&old.instances[e.1 as usize], n)).map(|e| (e.1, Pair::Same));
            if found.is_none() && let Some(mine) = &mine {
                let key = hash_words(mine);
                let at = moved.partition_point(|e| e.0 < key);
                found = moved[at..].iter().take_while(|e| e.0 == key)
                    .find(|e| !used[e.1 as usize] && canon(&old.instances[e.1 as usize], m, e.2, false) == *mine)
                    .map(|e| (e.1, Pair::Moved(e.2)));
            }
        }
        if let Some((i, how)) = found {
            used[i as usize] = true;
            taken[j] = true;
            pairs.push((i, j as u32, how, NONE));
            diagonal = i as isize - j as isize;
        }
    }
    // An outermost layer (a rounded table, a code block) moves whole when all it holds moved.
    layers(old, old_layers);
    olds.clear();
    olds.extend(old_layers.iter().map(|&(a, b)| (layer_key(old, a, b, m), a, b)));
    olds.sort_unstable();
    layers(new, news);
    let (olds, news) = (&olds[..], &news[..]);
    let match_layers = |pairs: &mut Vec<(u32, u32, Pair, u32)>, used: &mut [bool], taken: &mut [bool], ways: &mut Vec<Pair>, edges: Option<[f32; 4]>| {
        for &(a, b) in news {
            if taken[a..b].iter().any(|t| *t) { continue; }
            let key = layer_key(new, a, b, m);
            let at = olds.partition_point(|e| e.0 < key);
            let Some(from) = olds[at..].iter().take_while(|e| e.0 == key)
                .filter(|e| e.2 - e.1 == b - a && !used[e.1..e.2].iter().any(|u| *u))
                .find(|e| moved_layer(&old.instances[e.1..e.2], &new.instances[a..b], m, edges, ways)).map(|e| e.1)
            else { continue };
            for (k, how) in ways.iter().enumerate() {
                used[from + k] = true;
                taken[a + k] = true;
                pairs.push(((from + k) as u32, (a + k) as u32, *how, a as u32));
            }
        }
    };
    match_layers(pairs, used, taken, ways, None);
    // The viewport: the clip most content that moved under a kept clip has.
    clips.clear();
    clips.extend(pairs.iter().filter(|p| p.2 == Pair::Moved(false))
        .map(|p| -> [u32; 4] { bytemuck::cast(new.instances[p.1 as usize].clip) }));
    clips.sort_unstable();
    let (mut viewport, mut most, mut run) = ([0u32; 4], 0, 0);
    for (k, c) in clips.iter().enumerate() {
        run = if k > 0 && clips[k - 1] == *c { run + 1 } else { 1 };
        if run > most { (viewport, most) = (*c, run); }
    }
    if most == 0 { return None; }
    let viewport: [f32; 4] = bytemuck::cast(viewport);
    // What is left: content placed a rounding step off, and content the viewport's edge cuts
    // (a code block half scrolled out), its clip cut where the edge was, or is: alike between
    // the edges, where pixels can move.
    let edges = [viewport[0].max(viewport[0] + dx), viewport[1].max(viewport[1] + dy), viewport[2].min(viewport[2] + dx), viewport[3].min(viewport[3] + dy)];
    let left = (0..new.instances.len()).any(|j| !taken[j] && new.scopes[j] == NONE && movable(&new.instances[j]));
    if left {
        loose.clear();
        loose.extend((0..old.instances.len())
            .filter(|&i| !used[i] && old.scopes[i] == NONE && movable(&old.instances[i]))
            .map(|i| (hash_words(&canon(&old.instances[i], m.still(), false, true)), i as u32)));
        loose.sort_unstable();
        for j in 0..new.instances.len() {
            let n = &new.instances[j];
            if taken[j] || new.scopes[j] != NONE || !movable(n) { continue; }
            let key = hash_words(&canon(n, m.still(), false, true));
            let at = loose.partition_point(|e| e.0 < key);
            if let Some((i, how)) = loose[at..].iter().take_while(|e| e.0 == key).map(|e| e.1 as usize)
                .filter(|&i| !used[i]).find_map(|i| moved_near(&old.instances[i], n, m, edges).map(|how| (i, how))) {
                used[i] = true;
                taken[j] = true;
                pairs.push((i as u32, j as u32, how, NONE));
            }
        }
    }
    match_layers(pairs, used, taken, ways, Some(edges));
    pairs.sort_unstable_by_key(|p| p.1);
    // Pairs out of their old order draw differently where they overlap.
    order.clear();
    order.extend(pairs.iter().map(|p| (p.0, p.1)));
    for (p, kept) in pairs.iter_mut().zip(lis.increasing(order)) {
        if !kept { p.2 = Pair::None; }
    }
    // Moved content elsewhere (another viewport, a clip reaching out of this one) is redrawn.
    for p in pairs.iter_mut() {
        if !p.2.moved() { continue };
        let n = &new.instances[p.1 as usize];
        if n.params[1] == KIND_LAYER_END { continue; }
        let c = n.clip;
        let inside = if p.2 == Pair::Moved(false) {
            c == viewport && radius(n).is_finite()
        } else {
            c[0] >= viewport[0] && c[1] >= viewport[1] && c[2] <= viewport[2] && c[3] <= viewport[3] && radius(n).is_finite()
        };
        if !inside { p.2 = Pair::None; }
    }
    // A layer moves whole or not at all.
    broken.clear();
    broken.extend(pairs.iter().filter(|p| p.3 != NONE && !p.2.moved()).map(|p| p.3));
    broken.sort_unstable();
    broken.dedup();
    for p in pairs.iter_mut() {
        if p.3 != NONE && broken.binary_search(&p.3).is_ok() { p.2 = Pair::None; }
    }
    // Content clipped by the viewport itself keeps its faded edges and rounded corners still; a
    // clip that moved with its content takes its own along, but one the edge cut has its
    // rounded corners where it was cut.
    let mut shrink = [0.0f32; 4];
    let mut count = 0;
    for p in pairs.iter() {
        if !p.2.moved() { continue; }
        count += 1;
        let n = &new.instances[p.1 as usize];
        let radius = radius(n).ceil() + 1.0;
        if p.2 == Pair::Cut {
            shrink[1] = shrink[1].max(radius);
            shrink[3] = shrink[3].max(radius);
            if m.xs {
                shrink[0] = shrink[0].max(radius);
                shrink[2] = shrink[2].max(radius);
            }
        }
        if p.2 != Pair::Moved(false) { continue; }
        let (widths, _) = unpack_fade(n.fade);
        let widths = if n.fade[0] == 0 && n.fade[1] == 0 { [0.0; 4] } else { widths.map(|w| w.abs().ceil() + 1.0) };
        // Across the move, a pixel more as well (the rounding radius plus one): a fill that
        // ends at the window's edge then still spans what moves (`flat_over`).
        let across = if m.xs { radius } else { 0.0 };
        shrink = [shrink[0].max(widths[0].max(across)), shrink[1].max(widths[1].max(radius)), shrink[2].max(widths[2].max(across)),
            shrink[3].max(widths[3].max(radius))];
    }
    if count < MIN_MOVED { return None; }
    // Where pixels can move to: inside the viewport's fully covered pixels, clear of its faded
    // edges and rounded corners, from and to.
    let (inner, outer) = clip_edges(viewport, size);
    let v = [inner[0] + shrink[0], inner[1] + shrink[1], inner[2] - shrink[2], inner[3] - shrink[3]];
    let to = cut([v[0].max(v[0] + dx), v[1].max(v[1] + dy), v[2].min(v[2] + dx), v[3].min(v[3] + dy)], [0.0, 0.0, size.0 as f32, size.1 as f32])?;
    let from = moved_by(to, -dx, -dy);
    // Where pixels move from or to.
    let moving = [to[0].min(from[0]), to[1].min(from[1]), to[2].max(from[2]), to[3].max(from[3])];
    // The strips the move leaves: everything in the viewport outside `to`.
    for strip in [[outer[0], outer[1], outer[2], to[1]], [outer[0], to[3], outer[2], outer[3]],
                  [outer[0], to[1], to[0], to[3]], [to[2], to[1], outer[2], to[3]]] {
        if strip[2] > strip[0] && strip[3] > strip[1] { out.push(strip); }
    }
    paired.clear();
    paired.resize(old.instances.len(), Pair::None);
    for p in pairs.iter() { paired[p.0 as usize] = p.2; }
    for (i, how) in paired.iter().enumerate() {
        if how.moved() { continue; }
        let Some(r) = old.reach(i, size) else { continue };
        // Gone or changed: inside the viewport, the move and the strips cover what it drew.
        let within = r[0] >= outer[0] && r[1] >= outer[1] && r[2] <= outer[2] && r[3] <= outer[3];
        if *how == Pair::None && !within { out.push(r); }
        // What it drew where pixels move from lands where they move to.
        if old.scopes[i] != NONE || !flat_over(&old.instances[i], moving, size, m) {
            if let Some(e) = cut(r, from) { out.push(moved_by(e, dx, dy)); }
        }
    }
    paired.clear();
    paired.resize(new.instances.len(), Pair::None);
    for p in pairs.iter() { paired[p.1 as usize] = p.2; }
    for (j, how) in paired.iter().enumerate() {
        let Some(r) = new.reach(j, size) else { continue };
        match how {
            Pair::Moved(_) | Pair::Cut => {}
            Pair::None => out.push(r),
            // It stays, but the move covered it.
            Pair::Same => {
                if new.scopes[j] != NONE || !flat_over(&new.instances[j], moving, size, m) {
                    if let Some(e) = cut(r, to) { out.push(e); }
                }
            }
        }
    }
    Some(Shift { to: to.map(|v| v as u32), dx: dx as i32, dy: dy as i32 })
}
