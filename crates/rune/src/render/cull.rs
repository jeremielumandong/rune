//! Fewer rasterized pixels for software adapters.
//!
//! On WARP or lavapipe every pixel a quad covers runs the uber shader on the CPU, whether it
//! shows or not. Two kinds of pixel draw nothing, and [`cull`] keeps the shader off them
//! without changing any pixel the frame draws:
//!
//! - Pixels outside a quad's clip: `base_vs` rasterizes the whole quad and `fs` clips per pixel.
//!   A plain or shadow quad (`KIND_RECT`, `KIND_SHADOW` without a hole) whose clip cuts it
//!   draws as one band (`runtime/paint/bands.rs`: `uv` = `[x0, y0, -x1, y1]` relative to its
//!   rect) of the clip's pixels, which shades exactly what the whole quad did there.
//! - A flat opaque fill drawn over pixels that already are its colour: an app's nested panels
//!   often paint the same background three or four deep. A square, opaque, borderless,
//!   unfaded fill whose pixels all lie where an earlier such fill of the same colour fully
//!   covered them, with nothing drawn over those pixels since, draws as an empty band.
//!
//! The instance count and order stay, so batches, layers and effect slots keep their indices.
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
pub(crate) fn cull(instances: &[Instance], size: (u32, u32), out: &mut Vec<Instance>) {
    out.clear();
    out.extend_from_slice(instances);
    hidden(out, size);
    same_colour(out, size);
}

/// Quads every pixel of which a later opaque fill covers: an empty band. Walking back from the
/// top of the painter order, it keeps the rectangles later fills paint over, joining those that
/// line up (a toolbar over a panel), and skips layers, whose contents and composites it leaves
/// be. A frame with an effect slot or a backdrop blur, which read what is under them, is left
/// whole.
fn hidden(out: &mut [Instance], size: (u32, u32)) {
    if out.iter().any(|i| i.params[1] == KIND_EFFECT || i.params[1] == KIND_BACKDROP_BLUR) { return; }
    let mut covered: Vec<[f32; 4]> = Vec::new();
    let mut depth = 0u32;
    for i in out.iter_mut().rev() {
        let kind = i.params[1];
        if kind == KIND_LAYER_END { depth += 1; continue; }
        if kind == KIND_LAYER { depth = depth.saturating_sub(1); continue; }
        if depth > 0 || i.uv == EMPTY { continue; }
        if let Some(reach) = touched(i, size) && covered.iter().any(|c| inside(reach, *c)) {
            i.uv = EMPTY;
            continue;
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
        if covered.len() > MAX_REGIONS {
            let area = |e: &[f32; 4]| (e[2] - e[0]) * (e[3] - e[1]);
            covered.sort_unstable_by(|a, b| area(b).total_cmp(&area(a)));
            covered.truncate(MAX_REGIONS);
        }
    }
}

/// Flat fills drawn over pixels that already are their colour: an empty band. Quads `hidden`
/// emptied draw nothing and are passed over.
fn same_colour(out: &mut [Instance], size: (u32, u32)) {
    // Rectangles of window pixels known to hold one colour at this point of the painter order.
    let mut regions: Vec<([f32; 4], [f32; 4])> = Vec::new();
    let mut depth = 0u32;
    for i in out.iter_mut() {
        if i.uv == EMPTY { continue; }
        let kind = i.params[1];
        let reach = touched(i, size);
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
        let Some(reach) = reach else { band(i, size); continue };
        if depth == 0 && flat(i) && regions.iter().any(|r| r.1 == i.fill && inside(reach, r.0)) {
            // Its pixels already are its colour: an empty band.
            i.uv = EMPTY;
            continue;
        }
        if regions.iter().any(|r| overlaps(r.0, reach)) {
            let mut kept = Vec::with_capacity(regions.len() + 4);
            for (r, color) in regions.drain(..) {
                if !overlaps(r, reach) { kept.push((r, color)); continue; }
                // What of `r` the quad leaves, as the largest rectangles: all of `r` above,
                // below, left and right of it (they overlap at the corners).
                for piece in [[r[0], r[1], r[2], reach[1]], [r[0], reach[3], r[2], r[3]],
                              [r[0], r[1], reach[0], r[3]], [reach[2], r[1], r[2], r[3]]] {
                    if piece[2] > piece[0] && piece[3] > piece[1] { kept.push((piece, color)); }
                }
            }
            regions = kept;
        }
        if depth == 0 && opaque(i) {
            for e in painted(i, size) {
                if e[2] > e[0] && e[3] > e[1] { regions.push((e, i.fill)); }
            }
        }
        if regions.len() > MAX_REGIONS {
            let area = |e: &[f32; 4]| (e[2] - e[0]) * (e[3] - e[1]);
            regions.sort_unstable_by(|a, b| area(&b.0).total_cmp(&area(&a.0)));
            regions.truncate(MAX_REGIONS);
        }
        band(i, size);
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
