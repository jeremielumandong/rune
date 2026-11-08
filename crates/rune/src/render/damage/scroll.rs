//! Following a scroll by moving the kept frame's pixels.
//!
//! A scroll moves every instance in a viewport by the same offset, so the plain comparison
//! damages the whole viewport. Here the kept frame's viewport pixels are moved by that offset
//! instead ([`Shift`], two texture copies), and only what the move cannot account for is
//! redrawn: the strip the scroll exposes, and anything in the viewport that did not move with
//! the content (a scrollbar, an overlay, a background that is not one flat colour).
//!
//! A moved pixel equals a redrawn one when everything that covers it moved by the same whole
//! number of device pixels: the shader computes coverage from the pixel's position relative to
//! each quad. Instances qualify when their bytes are equal but for their y coordinates, moved
//! by the offset (within 1/64 px), and their clip is the viewport's or a clip that moved with
//! them inside it. The moved area stays clear of the viewport's antialiased or faded edges and
//! rounded corners.
use super::*;

/// Fixed-point steps per device pixel for comparing moved coordinates.
const Q: f32 = 64.0;
/// The fewest moved instances worth a move.
const MIN_MOVED: usize = 8;

/// The kept frame's pixels in `to` (`[x0, y0, x1, y1]`) come from `dy` pixels above (below
/// when `dy` is negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shift {
    pub to: [u32; 4],
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

/// Whether moving an instance's y words moves all it draws (layers and effect slots composite
/// other passes).
fn movable(i: &Instance) -> bool {
    let kind = i.params[1];
    !(kind == KIND_LAYER || kind == KIND_LAYER_END || kind == KIND_EFFECT || kind == KIND_BACKDROP_BLUR) && kind.is_finite()
}

/// Whether a layer record composites what it holds the same wherever it moves by whole
/// pixels: no blur (whose taps reach past what moved), turn or cache.
fn plain_layer(i: &Instance) -> bool {
    i.params[1] == KIND_LAYER && i.params[2] == 0.0 && i.params[3] == 0.0 && i.border == [0.0; 4]
}

/// Each outermost layer's instances, `first..past`, from its record to its end.
fn layers(p: &Painted) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (k, &s) in p.scopes.iter().enumerate() {
        if s == NONE { continue; }
        match out.last_mut() {
            Some(last) if last.1 == k && p.scopes[last.0] == s => last.1 = k + 1,
            _ => out.push((k, k + 1)),
        }
    }
    out
}

/// How far an instance's rounded clip bends in from its edges: continuous corners bend in
/// further than circular ones of the same radius.
fn radius(i: &Instance) -> f32 {
    unpack_clip_radii(i.fade[3]).iter().fold(0.0f32, |m, r| m.max(r.abs() * 1.6))
}

/// A layer's instances hashed but for their y coordinates.
fn layer_key(p: &Painted, a: usize, b: usize) -> u64 {
    p.instances[a..b].iter().fold(0xcbf2_9ce4_8422_2325u64, |h, i| (h ^ hash_words(&canon(i, 0.0, false, true))).wrapping_mul(0x0000_0100_0000_01b3))
}

/// How each instance of a layer moved `dy` from `old` to `new`, when all of them did (as
/// `moved_near` finds, when `edges` are given) and its records are `plain_layer`s; an end record
/// counts as moved with its clip.
fn moved_layer(old: &[Instance], new: &[Instance], dy: f32, edges: Option<[f32; 4]>) -> Option<Vec<Pair>> {
    old.iter().zip(new).map(|(o, n)| {
        let kind = n.params[1];
        if kind == KIND_LAYER_END { return same(o, n).then_some(Pair::Moved(true)); }
        if !(movable(n) || plain_layer(n)) || (kind == KIND_LAYER && !plain_layer(o)) { return None; }
        if let Some(edges) = edges { return moved_near(o, n, dy, edges); }
        let mine = canon(n, 0.0, false, false);
        if canon(o, dy, false, false) == mine { Some(Pair::Moved(false)) }
        else if canon(o, dy, true, false) == mine { Some(Pair::Moved(true)) }
        else { None }
    }).collect()
}

/// How `n` is `o` moved `dy`, its y coordinates within a step of `1/Q` px either way (a
/// position computed another way can round to the next step): with its clip kept or moved, or
/// moved with clips the viewport's edge cuts differently, alike between `edges`' top and bottom
/// (the pixels a move can reach). Faded clips, whose ramps start at their edges, are not cut.
fn moved_near(o: &Instance, n: &Instance, dy: f32, edges: [f32; 4]) -> Option<Pair> {
    let ys = y_words(n.params[1]);
    let (ow, nw): ([u32; 32], [u32; 32]) = (bytemuck::cast(*o), bytemuck::cast(*n));
    let near = |a: &[u32; 32], b: &[u32; 32]| (0..32).all(|k| {
        if (ys.contains(&k) || (k == 3 && derived_height(n.params[1]))) && a[k] != b[k] {
            let (x, y) = (f32::from_bits(ow[k]), f32::from_bits(nw[k]));
            x.is_finite() && y.is_finite() && x.abs() < 1.0e7 && y.abs() < 1.0e7 && (a[k] as i32).abs_diff(b[k] as i32) <= 1
        } else {
            a[k] == b[k]
        }
    });
    let mine = canon(n, 0.0, false, false);
    if near(&canon(o, dy, false, false), &mine) { return Some(Pair::Moved(false)); }
    let mut moved = canon(o, dy, true, false);
    if near(&moved, &mine) { return Some(Pair::Moved(true)); }
    if o.fade[..3] != [0; 3] || n.fade[..3] != [0; 3] { return None; }
    let q = |v: f32| ((v * Q).round() as i32) as u32;
    let mut mine = mine;
    (moved[25], moved[27]) = (q((o.clip[1] + dy).max(edges[1])), q((o.clip[3] + dy).min(edges[3])));
    (mine[25], mine[27]) = (q(n.clip[1].max(edges[1])), q(n.clip[3].min(edges[3])));
    near(&moved, &mine).then_some(Pair::Cut)
}

/// An instance's words with its y coordinates moved `dy` (its clip's too when `clip_moves`) and
/// fixed to `1/Q` px, or zeroed when `zero`, for hashing and comparing.
fn canon(i: &Instance, dy: f32, clip_moves: bool, zero: bool) -> [u32; 32] {
    let mut w: [u32; 32] = bytemuck::cast(*i);
    for &k in y_words(i.params[1]) {
        let v = f32::from_bits(w[k]);
        w[k] = if zero {
            0
        } else if v.is_finite() && v.abs() < 1.0e7 {
            let moved = if k < 24 || clip_moves { v + dy } else { v };
            ((moved * Q).round() as i32) as u32
        } else {
            // `NO_CLIP` and the like stay put.
            w[k]
        };
    }
    // A segment's or triangle's rect bounds its points, its height their difference, which
    // can round differently as they move.
    if derived_height(i.params[1]) {
        let h = i.rect[3];
        w[3] = if zero { 0 } else if h.is_finite() && h.abs() < 1.0e7 { ((h * Q).round() as i32) as u32 } else { w[3] };
    }
    w
}

fn derived_height(kind: f32) -> bool { kind == KIND_SEGMENT || kind == KIND_TRIANGLE }

fn hash_words(w: &[u32; 32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in w { h = (h ^ u64::from(*v)).wrapping_mul(0x0000_0100_0000_01b3); }
    h
}

/// The offset most instances moved by, a whole number of device pixels, guessed from up to 256
/// new instances: each against the old ones that match it but for y, nearest in y first.
fn guess(old: &Painted, new: &Painted) -> Option<f32> {
    let mut olds: Vec<(u64, f32)> = (0..old.instances.len())
        .filter(|&i| old.scopes[i] == NONE && movable(&old.instances[i]))
        .map(|i| (hash_words(&canon(&old.instances[i], 0.0, false, true)), old.instances[i].rect[1]))
        .collect();
    olds.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let step = (new.instances.len() / 256).max(1);
    let sample: Vec<(u64, f32)> = (0..new.instances.len()).step_by(step)
        .filter(|&j| new.scopes[j] == NONE && movable(&new.instances[j]))
        .map(|j| (hash_words(&canon(&new.instances[j], 0.0, false, true)), new.instances[j].rect[1]))
        .collect();
    let range = |key: u64| &olds[olds.partition_point(|e| e.0 < key)..olds.partition_point(|e| e.0 <= key)];
    let mut votes: Vec<i64> = Vec::new();
    for &(key, y) in &sample {
        let same = range(key);
        let near = same.partition_point(|e| e.1 < y);
        for e in &same[near.saturating_sub(4)..(near + 4).min(same.len())] {
            let d = y - e.1;
            if d != 0.0 && d.is_finite() { votes.push((d * Q).round() as i64); }
        }
    }
    votes.sort_unstable();
    let mut runs: Vec<(usize, i64)> = Vec::new();
    for (k, v) in votes.iter().enumerate() {
        if k > 0 && votes[k - 1] == *v { runs.last_mut().unwrap().0 += 1 } else { runs.push((1, *v)) }
    }
    runs.sort_unstable_by(|a, b| b.cmp(a));
    // Repeating content (the same glyph in each row) votes for its pitch too: of the likeliest
    // offsets, take the one that accounts for the most sampled instances.
    let q = Q as i64;
    let accounted = |d: i64| sample.iter().filter(|&&(key, y)| {
        let want = y - d as f32 / Q;
        let same = range(key);
        let at = same.partition_point(|e| e.1 < want - 1.0 / Q);
        same.get(at).is_some_and(|e| (e.1 - want).abs() <= 1.0 / Q)
    }).count();
    let (count, best) = runs.iter().take(4).filter(|r| r.0 >= 4 && (r.1 % q).abs() <= 1 && r.1.abs() >= q / 2)
        .map(|r| (accounted(r.1), r.1)).max()?;
    (count >= 4).then(|| ((best as f32) / Q).round()).filter(|d| *d != 0.0)
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

fn down(e: [f32; 4], dy: f32) -> [f32; 4] { [e[0], e[1] + dy, e[2], e[3] + dy] }

/// Whether `i` draws the same down each column of `area`, so moving its pixels there up or
/// down changes nothing: a plain fill (with or without a border) whose top and bottom edges
/// (with their border, antialiasing and rounded corners), and its clip's, are clear of `area`
/// or out past the window's. Its left and right edges and borders can be anywhere: a vertical
/// move keeps each column. A quad banded to nothing (`render/cull.rs`) draws nothing at all.
fn flat_over(i: &Instance, area: [f32; 4], size: (u32, u32)) -> bool {
    let kind = i.params[1];
    if kind <= KIND_SHADOW && i.uv[2] < 0.0 && i.uv[3] <= i.uv[1] { return true; }
    // Faded clip edges vary down the columns; a rounded clip only near its top and bottom.
    if kind != KIND_RECT || i.uv != [0.0; 4] || i.fade[..3] != [0; 3] || !(i.params[0] >= 0.0) { return false; }
    let radius = i.radii.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let reach = radius + i.params[0] + 1.0 + 2.0 * i.params[2].abs();
    let clip_reach = unpack_clip_radii(i.fade[3]).iter().fold(0.0f32, |m, r| m.max(r.abs() * 1.6)) + 1.0;
    if !clip_reach.is_finite() { return false; }
    let h = size.1 as f32;
    let spans = |top: f32, bottom: f32, by: f32| (top + by <= area[1] || top <= -by) && (bottom - by >= area[3] || bottom >= h + by);
    let (r, c) = (i.rect, i.clip);
    spans(r[1], r[1] + r[3], reach) && spans(c[1], c[3], clip_reach)
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

/// The move that follows a scroll from `old` to `new`, with the edges still to redraw pushed
/// onto `out`; None when nothing scrolled.
pub(super) fn scrolled(old: &Painted, new: &Painted, size: (u32, u32), out: &mut Vec<[f32; 4]>) -> Option<Shift> {
    let dy = guess(old, new)?;
    // Old instances under their bytes (exact) and moved by `dy` (clip kept, and clip moved).
    let mut exact: Vec<(u64, u32)> = (0..old.instances.len()).map(|i| (old.keys[i], i as u32)).collect();
    exact.sort_unstable();
    let mut moved: Vec<(u64, u32, bool)> = Vec::new();
    for (i, o) in old.instances.iter().enumerate() {
        if old.scopes[i] != NONE || !movable(o) { continue; }
        let kept = hash_words(&canon(o, dy, false, false));
        moved.push((kept, i as u32, false));
        let along = hash_words(&canon(o, dy, true, false));
        if along != kept { moved.push((along, i as u32, true)); }
    }
    moved.sort_unstable_by_key(|e| (e.0, e.1));
    let mut used = vec![false; old.instances.len()];
    let mut taken = vec![false; new.instances.len()];
    // (old, new, how, the outermost layer moved whole it belongs to or `NONE`)
    let mut pairs: Vec<(u32, u32, Pair, u32)> = Vec::new();
    for j in 0..new.instances.len() {
        let n = &new.instances[j];
        let key = new.keys[j];
        let at = exact.partition_point(|e| e.0 < key);
        let found = exact[at..].iter().take_while(|e| e.0 == key)
            .find(|e| !used[e.1 as usize] && same(&old.instances[e.1 as usize], n)).map(|e| (e.1, Pair::Same));
        let found = found.or_else(|| {
            if new.scopes[j] != NONE || !movable(n) { return None; }
            let mine = canon(n, 0.0, false, false);
            let key = hash_words(&mine);
            let at = moved.partition_point(|e| e.0 < key);
            moved[at..].iter().take_while(|e| e.0 == key)
                .find(|e| !used[e.1 as usize] && canon(&old.instances[e.1 as usize], dy, e.2, false) == mine)
                .map(|e| (e.1, Pair::Moved(e.2)))
        });
        if let Some((i, how)) = found {
            used[i as usize] = true;
            taken[j] = true;
            pairs.push((i, j as u32, how, NONE));
        }
    }
    // An outermost layer (a rounded table, a code block) moves whole when all it holds moved.
    let mut olds: Vec<(u64, usize, usize)> = layers(old).into_iter().map(|(a, b)| (layer_key(old, a, b), a, b)).collect();
    olds.sort_unstable();
    let news = layers(new);
    let match_layers = |pairs: &mut Vec<(u32, u32, Pair, u32)>, used: &mut [bool], taken: &mut [bool], edges: Option<[f32; 4]>| {
        for &(a, b) in &news {
            if taken[a..b].iter().any(|t| *t) { continue; }
            let key = layer_key(new, a, b);
            let at = olds.partition_point(|e| e.0 < key);
            let Some((from, ways)) = olds[at..].iter().take_while(|e| e.0 == key)
                .filter(|e| e.2 - e.1 == b - a && !used[e.1..e.2].iter().any(|u| *u))
                .find_map(|e| moved_layer(&old.instances[e.1..e.2], &new.instances[a..b], dy, edges).map(|w| (e.1, w)))
            else { continue };
            for (k, how) in ways.into_iter().enumerate() {
                used[from + k] = true;
                taken[a + k] = true;
                pairs.push(((from + k) as u32, (a + k) as u32, how, a as u32));
            }
        }
    };
    match_layers(&mut pairs, &mut used, &mut taken, None);
    // The viewport: the clip most content that moved under a kept clip has.
    let mut clips: Vec<[u32; 4]> = pairs.iter().filter(|p| p.2 == Pair::Moved(false))
        .map(|p| bytemuck::cast(new.instances[p.1 as usize].clip)).collect();
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
    let edges = [viewport[0], viewport[1].max(viewport[1] + dy), viewport[2], viewport[3].min(viewport[3] + dy)];
    let mut loose: Vec<(u64, u32)> = (0..old.instances.len())
        .filter(|&i| !used[i] && old.scopes[i] == NONE && movable(&old.instances[i]))
        .map(|i| (hash_words(&canon(&old.instances[i], 0.0, false, true)), i as u32)).collect();
    loose.sort_unstable();
    for j in 0..new.instances.len() {
        let n = &new.instances[j];
        if taken[j] || new.scopes[j] != NONE || !movable(n) { continue; }
        let key = hash_words(&canon(n, 0.0, false, true));
        let at = loose.partition_point(|e| e.0 < key);
        if let Some((i, how)) = loose[at..].iter().take_while(|e| e.0 == key).map(|e| e.1 as usize)
            .filter(|&i| !used[i]).find_map(|i| moved_near(&old.instances[i], n, dy, edges).map(|how| (i, how))) {
            used[i] = true;
            taken[j] = true;
            pairs.push((i as u32, j as u32, how, NONE));
        }
    }
    match_layers(&mut pairs, &mut used, &mut taken, Some(edges));
    pairs.sort_unstable_by_key(|p| p.1);
    // Pairs out of their old order draw differently where they overlap.
    let order: Vec<(u32, u32)> = pairs.iter().map(|p| (p.0, p.1)).collect();
    for (p, kept) in pairs.iter_mut().zip(increasing(&order)) {
        if !kept { p.2 = Pair::None; }
    }
    // Moved content elsewhere (another viewport, a clip reaching out of this one) is redrawn.
    for p in &mut pairs {
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
    let mut broken: Vec<u32> = pairs.iter().filter(|p| p.3 != NONE && !p.2.moved()).map(|p| p.3).collect();
    broken.sort_unstable();
    broken.dedup();
    for p in &mut pairs {
        if p.3 != NONE && broken.binary_search(&p.3).is_ok() { p.2 = Pair::None; }
    }
    // Content clipped by the viewport itself keeps its faded edges and rounded corners still; a
    // clip that moved with its content takes its own along, but one the edge cut has its
    // rounded corners where it was cut.
    let mut shrink = [0.0f32; 4];
    let mut count = 0;
    for p in &pairs {
        if !p.2.moved() { continue; }
        count += 1;
        let n = &new.instances[p.1 as usize];
        let radius = radius(n).ceil() + 1.0;
        if p.2 == Pair::Cut {
            shrink[1] = shrink[1].max(radius);
            shrink[3] = shrink[3].max(radius);
        }
        if p.2 != Pair::Moved(false) { continue; }
        let (widths, _) = unpack_fade(n.fade);
        let widths = if n.fade[0] == 0 && n.fade[1] == 0 { [0.0; 4] } else { widths.map(|w| w.abs().ceil() + 1.0) };
        shrink = [shrink[0].max(widths[0]), shrink[1].max(widths[1].max(radius)), shrink[2].max(widths[2]), shrink[3].max(widths[3].max(radius))];
    }
    if count < MIN_MOVED { return None; }
    // Where pixels can move to: inside the viewport's fully covered pixels, clear of its faded
    // edges and rounded corners, from and to.
    let (inner, outer) = clip_edges(viewport, size);
    let v = [inner[0] + shrink[0], inner[1] + shrink[1], inner[2] - shrink[2], inner[3] - shrink[3]];
    let to = cut([v[0], v[1].max(v[1] + dy), v[2], v[3].min(v[3] + dy)], [0.0, 0.0, size.0 as f32, size.1 as f32])?;
    let from = down(to, -dy);
    // Where pixels move from or to.
    let moving = [to[0], to[1].min(from[1]), to[2], to[3].max(from[3])];
    // The strips the move leaves: everything in the viewport outside `to`.
    for strip in [[outer[0], outer[1], outer[2], to[1]], [outer[0], to[3], outer[2], outer[3]],
                  [outer[0], to[1], to[0], to[3]], [to[2], to[1], outer[2], to[3]]] {
        if strip[2] > strip[0] && strip[3] > strip[1] { out.push(strip); }
    }
    let mut paired = vec![Pair::None; old.instances.len()];
    for p in &pairs { paired[p.0 as usize] = p.2; }
    for (i, how) in paired.iter().enumerate() {
        if how.moved() { continue; }
        let Some(r) = old.reach(i, size) else { continue };
        // Gone or changed: inside the viewport, the move and the strips cover what it drew.
        let within = r[0] >= outer[0] && r[1] >= outer[1] && r[2] <= outer[2] && r[3] <= outer[3];
        if *how == Pair::None && !within { out.push(r); }
        // What it drew where pixels move from lands where they move to.
        if old.scopes[i] != NONE || !flat_over(&old.instances[i], moving, size) {
            if let Some(e) = cut(r, from) { out.push(down(e, dy)); }
        }
    }
    let mut paired = vec![Pair::None; new.instances.len()];
    for p in &pairs { paired[p.1 as usize] = p.2; }
    for (j, how) in paired.iter().enumerate() {
        let Some(r) = new.reach(j, size) else { continue };
        match how {
            Pair::Moved(_) | Pair::Cut => {}
            Pair::None => out.push(r),
            // It stays, but the move covered it.
            Pair::Same => {
                if new.scopes[j] != NONE || !flat_over(&new.instances[j], moving, size) {
                    if let Some(e) = cut(r, to) { out.push(e); }
                }
            }
        }
    }
    Some(Shift { to: to.map(|v| v as u32), dy: dy as i32 })
}
