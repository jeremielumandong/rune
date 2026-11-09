//! Fewer rasterized pixels for software adapters (AgentOps patch, PARITY-PATCH.md).
//!
//! On WARP or lavapipe every pixel a quad covers runs the uber shader on the CPU, whether it
//! shows or not. Three kinds of pixel draw nothing, and [`cull`] keeps the shader off them
//! without changing any pixel the frame draws:
//!
//! - Pixels outside a quad's clip: `base_vs` rasterizes the whole quad and `fs` clips per pixel.
//!   A plain or shadow quad (`KIND_RECT`, `KIND_SHADOW` without a hole) whose clip cuts it
//!   draws as one band (`runtime/paint/bands.rs`: `uv` = `[x0, y0, -x1, y1]` relative to its
//!   rect) of the clip's pixels, which shades exactly what the whole quad did there.
//! - Pixels a later opaque fill paints over (`hidden`): a window's background under its
//!   sidebar and panels, a panel under the cards on it.
//! - Pixels that already are a fill's colour (`same_colour`): an app's nested panels often
//!   paint the same background three or four deep, and a bordered card the colour of the
//!   panel under it changes only its border and corners.
//!
//! A quad partly culled draws what is left as one band, or, when the caller allows it, as up
//! to `MAX_BANDS` bands, one instance each. Without that the instance count and order stay, so
//! batches, layers and effect slots keep their indices.
use super::*;

/// The flat regions tracked at once; the smallest go first.
const MAX_REGIONS: usize = 32;
/// A band with no area: `base_vs` makes the quad zero pixels tall.
const EMPTY: [f32; 4] = [0.0, 0.0, -1.0, 0.0];

/// Pixel edges `[x0, y0, x1, y1]` a quad's clip leaves whole: a pixel is in it when its centre
/// is at least half a pixel inside, where `clip_alpha` is 1.
fn whole_pixels(e: [f32; 4]) -> [f32; 4] {
    [(e[0]).ceil(), (e[1]).ceil(), (e[2]).floor(), (e[3]).floor()]
}

fn inside(a: [f32; 4], b: [f32; 4]) -> bool { a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3] }

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool { a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3] }

/// A fill that paints one opaque colour inside its rounded rectangle (and its border, if it
/// has one, around that).
fn opaque(i: &Instance) -> bool {
    i.params[1] == KIND_RECT && i.fill[3] == 1.0 && i.params[0] >= 0.0 && i.params[2] == 0.0 && i.fade[..3] == [0; 3]
        && i.uv == [0.0; 4] && !i.rect.iter().chain(&i.clip).chain(&i.radii).chain(&i.params).any(|v| v.is_nan())
}

/// An `opaque` fill with no border: one colour all over.
fn flat(i: &Instance) -> bool { opaque(i) && i.params[0] == 0.0 }

/// Pixels where `i` (an `opaque` fill) paints exactly its fill colour, replacing what was
/// there: the two rectangles its corners leave whole inside its border, cut to its clip's
/// whole pixels.
fn painted(i: &Instance, size: (u32, u32)) -> [[f32; 4]; 2] {
    let b = i.params[0];
    let [x, y, w, h] = [i.rect[0] + b, i.rect[1] + b, i.rect[2] - 2.0 * b, i.rect[3] - 2.0 * b];
    let r = i.radii.iter().fold(0.0f32, |m, v| m.max(super::corner_extent(*v)));
    // A rounded clip (`fade[3]`) cuts its corners as the rect's own corners do.
    let rc = super::unpack_clip_radii(i.fade[3]).iter().fold(0.0f32, |m, v| m.max(super::corner_extent(*v)));
    let c = i.clip;
    let cut = |e: [f32; 4], c: [f32; 4]| {
        let (e, c) = (whole_pixels(e), clamp_edges(whole_pixels(c), size));
        [e[0].max(c[0]), e[1].max(c[1]), e[2].min(c[2]), e[3].min(c[3])]
    };
    [cut([x + r, y, x + w - r, y + h], [c[0] + rc, c[1], c[2] - rc, c[3]]),
     cut([x, y + r, x + w, y + h - r], [c[0], c[1] + rc, c[2], c[3] - rc])]
}

fn clamp_edges(e: [f32; 4], (w, h): (u32, u32)) -> [f32; 4] {
    let fix = |v: f32, max: u32| if v.is_nan() { 0.0 } else { v.clamp(0.0, max as f32) };
    [fix(e[0], w), fix(e[1], h), fix(e[2], w), fix(e[3], h)]
}

/// Whole window pixels instance `i` can change: for a plain quad, those whose centre is
/// within its softness (half a pixel at least) outside its rect and inside its clip, as `fs`
/// computes coverage; for others, `damage::bounds`, which allows a pixel more.
fn touched(i: &Instance, size: (u32, u32)) -> Option<[f32; 4]> {
    if i.params[1] != KIND_RECT { return super::damage::bounds(i, size); }
    let soft = i.params[2].max(0.5);
    let [x, y, w, h] = i.rect;
    let (lo, hi) = (|v: f32, s: f32| (v - s + 0.5).floor(), |v: f32, s: f32| (v + s - 0.5).ceil());
    let c = i.clip;
    let e = [lo(x, soft).max(lo(c[0], 0.5)), lo(y, soft).max(lo(c[1], 0.5)), hi(x + w, soft).min(hi(c[2], 0.5)), hi(y + h, soft).min(hi(c[3], 0.5))];
    if e.iter().any(|v| v.is_nan()) { return super::damage::bounds(i, size); }
    let e = clamp_edges(e, size);
    (e[2] > e[0] && e[3] > e[1]).then_some(e)
}

/// Copies `instances` into `out`, with the pixels they cannot change kept from the shader.
/// With `split`, a quad whose pixels left to draw are not one rectangle draws as several
/// bands, one instance each, so the count changes: only for a frame nothing else addresses
/// by instance index (native surfaces, GPU springs). A window keeps a [`Culler`] instead, so
/// its buffers last from frame to frame.
#[cfg(test)]
pub(crate) fn cull(instances: &[Instance], size: (u32, u32), out: &mut Vec<Instance>, split: bool) {
    Culler::default().cull(instances, size, out, split);
}

/// What of a quad still draws.
#[derive(Clone, Copy, PartialEq)]
enum Keep {
    /// All of it (cut to its clip by `band`).
    Whole,
    /// Only these whole-pixel rectangles, `Culler::bands[start..start + len]`, each a band
    /// (`runtime/paint/bands.rs`); none draws nothing.
    Bands(u32, u32),
}

/// Draws nothing.
const NOTHING: Keep = Keep::Bands(0, 0);

impl Keep {
    fn nothing(self) -> bool { matches!(self, Keep::Bands(_, 0)) }
}

/// The buffers culling works in, kept from frame to frame so a frame allocates nothing once
/// they have grown to fit.
#[derive(Default)]
pub(crate) struct Culler {
    keep: Vec<Keep>,
    /// Every `Keep::Bands`' rectangles, one after another.
    bands: Vec<[f32; 4]>,
    /// `hidden`'s rectangles later opaque fills paint.
    covered: Vec<[f32; 4]>,
    /// `same_colour`'s rectangles of one colour, and the next set of them.
    regions: Vec<([f32; 4], [f32; 4])>,
    kept: Vec<([f32; 4], [f32; 4])>,
    skip: Vec<[f32; 4]>,
    shown: Vec<[f32; 4]>,
    work: Work,
}

/// `without`'s own buffers.
#[derive(Default)]
struct Work {
    pieces: Vec<[f32; 4]>,
    next: Vec<[f32; 4]>,
    cuts: Vec<[f32; 4]>,
}

impl Culler {
    /// `cull`, in this culler's buffers.
    pub(crate) fn cull(&mut self, instances: &[Instance], size: (u32, u32), out: &mut Vec<Instance>, split: bool) {
        self.keep.clear();
        self.keep.resize(instances.len(), Keep::Whole);
        self.bands.clear();
        hidden(instances, size, self, split);
        same_colour(instances, size, self, split);
        out.clear();
        out.reserve(instances.len());
        for (i, k) in instances.iter().zip(&self.keep) {
            match *k {
                Keep::Whole => {
                    let mut i = *i;
                    band(&mut i, size);
                    out.push(i);
                }
                Keep::Bands(_, 0) => out.push(Instance { uv: EMPTY, ..*i }),
                Keep::Bands(start, len) => for p in &self.bands[start as usize..(start + len) as usize] {
                    let [x, y, ..] = i.rect;
                    out.push(Instance { uv: [p[0] - x, p[1] - y, -(p[2] - x), p[3] - y], ..*i });
                },
            }
        }
    }
}

/// Once `v` holds `SLACK` more than `MAX_REGIONS`, keeps its `MAX_REGIONS` biggest: one
/// selection (linear) every `SLACK` additions or so, where sorting at each one past the limit
/// (nearly every glyph over a card splits a region) took most of culling's time.
fn keep_biggest<T>(v: &mut Vec<T>, area: impl Fn(&T) -> f32) {
    if v.len() <= MAX_REGIONS + SLACK { return; }
    v.select_nth_unstable_by(MAX_REGIONS - 1, |a, b| area(b).total_cmp(&area(a)));
    v.truncate(MAX_REGIONS);
}

/// How far past `MAX_REGIONS` a region list grows before it is cut back.
const SLACK: usize = 16;

/// The most bands one quad splits into.
const MAX_BANDS: usize = 12;
/// A quad bands only when that saves this many pixels or more, and a quarter of its own for
/// one band or `BAND_SAVED` a band for more.
const MIN_SAVED: f32 = 4096.0;
const BAND_SAVED: f32 = 1024.0;

fn area(e: &[f32; 4]) -> f32 { (e[2] - e[0]).max(0.0) * (e[3] - e[1]).max(0.0) }

/// `pieces` without `cut`, as at most four rectangles each.
fn subtract(pieces: &[[f32; 4]], cut: [f32; 4], out: &mut Vec<[f32; 4]>) {
    out.clear();
    for &p in pieces {
        if !overlaps(p, cut) { out.push(p); continue; }
        let (y0, y1) = (p[1].max(cut[1]), p[3].min(cut[3]));
        for piece in [[p[0], p[1], p[2], cut[1]], [p[0], cut[3], p[2], p[3]], [p[0], y0, cut[0], y1], [cut[2], y0, p[2], y1]] {
            if piece[2] > piece[0] && piece[3] > piece[1] { out.push(piece); }
        }
    }
}

/// What a plain quad (`reach`, its `touched` pixels) still draws once `skip` is left out, when
/// that is worth it: none at all, one band, or with `split` up to `MAX_BANDS`, added to
/// `bands`. The biggest rectangles go first, and one that would make too many bands stays
/// drawn. Its bands must end right of its rect's origin to carry the band mark.
fn without(i: &Instance, reach: [f32; 4], skip: &[[f32; 4]], keep: Keep, bands: &mut Vec<[f32; 4]>, w: &mut Work, split: bool) -> Option<Keep> {
    if !(i.params[1] == KIND_RECT || i.params[1] == KIND_SHADOW) || i.uv != [0.0; 4] { return None; }
    let Work { pieces, next, cuts } = w;
    pieces.clear();
    match keep {
        Keep::Whole => pieces.push(reach),
        Keep::Bands(start, len) => pieces.extend_from_slice(&bands[start as usize..(start + len) as usize]),
    }
    let before: f32 = pieces.iter().map(area).sum();
    let most = if split { MAX_BANDS } else { 1 };
    cuts.clear();
    cuts.extend(skip.iter().copied().filter(|c| overlaps(reach, *c)));
    cuts.sort_unstable_by(|a, b| area(b).total_cmp(&area(a)));
    for &cut in cuts.iter() {
        if !pieces.iter().any(|p| overlaps(*p, cut)) { continue; }
        subtract(pieces, cut, next);
        if next.len() <= most { std::mem::swap(pieces, next); }
    }
    let after: f32 = pieces.iter().map(area).sum();
    let saved = before - after;
    // One band must save a quarter of the quad; more must each save `BAND_SAVED`.
    let worth = if pieces.len() > 1 { saved >= BAND_SAVED * pieces.len() as f32 } else { saved * 4.0 >= before };
    if saved < MIN_SAVED.min(before) || !worth { return None; }
    if pieces.iter().any(|p| p[2] <= i.rect[0]) { return None; }
    let start = bands.len() as u32;
    bands.extend_from_slice(pieces);
    Some(Keep::Bands(start, pieces.len() as u32))
}

/// Quads every pixel of which a later opaque fill covers draw nothing; with fills covering
/// part of a quad, it draws what they leave (`without`). Walking back from the top of the
/// painter order, it keeps the rectangles later fills paint over, joining those that line up
/// (a toolbar over a panel), and skips layers, whose contents and composites it leaves be. A
/// frame with an effect slot or a backdrop blur, which read what is under them, is left whole.
fn hidden(instances: &[Instance], size: (u32, u32), c: &mut Culler, split: bool) {
    if instances.iter().any(|i| i.params[1] == KIND_EFFECT || i.params[1] == KIND_BACKDROP_BLUR) { return; }
    let Culler { keep, bands, covered, work, .. } = c;
    covered.clear();
    let mut depth = 0u32;
    for (at, i) in instances.iter().enumerate().rev() {
        let kind = i.params[1];
        if kind == KIND_LAYER_END { depth += 1; continue; }
        if kind == KIND_LAYER { depth = depth.saturating_sub(1); continue; }
        if depth > 0 || i.uv == EMPTY { continue; }
        if let Some(reach) = touched(i, size) {
            if covered.iter().any(|c| inside(reach, *c)) {
                keep[at] = NOTHING;
                continue;
            }
            if let Some(k) = without(i, reach, covered, keep[at], bands, work, split) { keep[at] = k; }
        }
        if !opaque(i) { continue; }
        for mut e in painted(i, size) {
            if !(e[2] > e[0] && e[3] > e[1]) { continue; }
            // Join what lines up with it, and drop what it holds.
            loop {
                let before = covered.len();
                covered.retain(|c| !inside(*c, e));
                let joined = covered.iter().position(|c| {
                    (c[0] == e[0] && c[2] == e[2] && c[1] <= e[3] && e[1] <= c[3]) || (c[1] == e[1] && c[3] == e[3] && c[0] <= e[2] && e[0] <= c[2])
                });
                if let Some(at) = joined {
                    let c = covered.swap_remove(at);
                    e = [c[0].min(e[0]), c[1].min(e[1]), c[2].max(e[2]), c[3].max(e[3])];
                } else if covered.len() == before {
                    break;
                }
            }
            if covered.iter().all(|c| !inside(e, *c)) { covered.push(e); }
        }
        keep_biggest(covered, area);
    }
}

/// Flat fills drawn over pixels that already are their colour draw nothing; an opaque fill
/// (a bordered or rounded card) over a region of its colour draws what it changes there: its
/// border and corners (`without`). Quads `hidden` emptied draw nothing and are passed over.
fn same_colour(instances: &[Instance], size: (u32, u32), c: &mut Culler, split: bool) {
    // `regions`: rectangles of window pixels known to hold one colour at this point of the
    // painter order.
    let Culler { keep, bands, regions, kept, skip, shown, work, .. } = c;
    regions.clear();
    let mut depth = 0u32;
    for (at, i) in instances.iter().enumerate() {
        if i.uv == EMPTY || keep[at].nothing() { continue; }
        let kind = i.params[1];
        if kind == KIND_LAYER {
            // Its contents may draw straight into the window (`draw_layer_range`) or composite
            // there at its end: what it covers holds no known colour from here on.
            depth += 1;
            let clip = clamp_edges(i.clip, size);
            regions.retain(|r| !overlaps(r.0, clip));
            continue;
        }
        if kind == KIND_LAYER_END {
            depth = depth.saturating_sub(1);
            continue;
        }
        // Clipped away: an empty band.
        let Some(reach) = touched(i, size) else { keep[at] = NOTHING; continue };
        let opaque = depth == 0 && opaque(i);
        if opaque {
            // What `hidden` leaves of it: where it paints its colour, skipped here or not.
            shown.clear();
            match keep[at] {
                Keep::Whole => shown.push(reach),
                Keep::Bands(start, len) => shown.extend_from_slice(&bands[start as usize..(start + len) as usize]),
            }
            if flat(i) && regions.iter().any(|r| r.1 == i.fill && inside(reach, r.0)) {
                // Its pixels already are its colour.
                keep[at] = NOTHING;
                continue;
            }
            // Where it paints just its colour over that colour (`without` takes the biggest
            // first).
            skip.clear();
            for e in painted(i, size) {
                for r in regions.iter().filter(|r| r.1 == i.fill) {
                    let c = [e[0].max(r.0[0]), e[1].max(r.0[1]), e[2].min(r.0[2]), e[3].min(r.0[3])];
                    if c[2] > c[0] && c[3] > c[1] { skip.push(c); }
                }
            }
            if let Some(k) = without(i, reach, skip, keep[at], bands, work, split) { keep[at] = k; }
        }
        if regions.iter().any(|r| overlaps(r.0, reach)) {
            kept.clear();
            for &(r, color) in regions.iter() {
                if !overlaps(r, reach) { kept.push((r, color)); continue; }
                // What of `r` the quad leaves, as the largest rectangles: all of `r` above,
                // below, left and right of it (they overlap at the corners).
                for piece in [[r[0], r[1], r[2], reach[1]], [r[0], reach[3], r[2], r[3]],
                              [r[0], r[1], reach[0], r[3]], [reach[2], r[1], r[2], r[3]]] {
                    if piece[2] > piece[0] && piece[3] > piece[1] { kept.push((piece, color)); }
                }
            }
            std::mem::swap(regions, kept);
        }
        if opaque {
            // Not where `hidden` took it off: a later fill paints there, and what is under
            // that is not this quad's colour.
            for e in painted(i, size) {
                for d in shown.iter() {
                    let c = [e[0].max(d[0]), e[1].max(d[1]), e[2].min(d[2]), e[3].min(d[3])];
                    if c[2] > c[0] && c[3] > c[1] { regions.push((c, i.fill)); }
                }
            }
        }
        keep_biggest(regions, |r| area(&r.0));
    }
}

/// Bands a plain or shadow quad to its clip's pixels, when the clip cuts it.
fn band(i: &mut Instance, size: (u32, u32)) {
    let kind = i.params[1];
    if !(kind == KIND_RECT || kind == KIND_SHADOW) || i.uv != [0.0; 4] { return; }
    let grow = if kind == KIND_SHADOW { i.params[2] * 3.0 + 1.0 } else { 1.0 + i.params[2] * 2.0 };
    let [x, y, w, h] = i.rect;
    let quad = [x - grow, y - grow, x + w + grow, y + h + grow];
    // `clip_alpha` is zero half a pixel outside the clip; the window ends the rest.
    let c = i.clip;
    let keep = [(c[0] - 1.0).floor(), (c[1] - 1.0).floor(), (c[2] + 1.0).ceil(), (c[3] + 1.0).ceil()];
    // A clip that leaves the quad whole leaves it be (the window's edge cuts it for free).
    if quad[0] >= keep[0] && quad[1] >= keep[1] && quad[2] <= keep[2] && quad[3] <= keep[3] { return; }
    let keep = [keep[0].max(0.0), keep[1].max(0.0), keep[2].min(size.0 as f32), keep[3].min(size.1 as f32)];
    let window = [quad[0].max(keep[0]), quad[1].max(keep[1]), quad[2].min(keep[2]), quad[3].min(keep[3])];
    // Relative to the rect; the right edge's sign marks the band, so it must be past the origin.
    let band = [window[0] - x, window[1] - y, window[2] - x, window[3] - y];
    if !(band[2] > band[0] && band[3] > band[1]) {
        i.uv = EMPTY;
    } else if band[2] > 0.0 {
        i.uv = [band[0], band[1], -band[2], band[3]];
    }
    // A window wholly left of the rect's origin can't carry the mark: the quad stays whole.
}

#[cfg(test)]
#[path = "cull_tests.rs"]
mod tests;
