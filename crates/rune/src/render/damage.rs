//! Damage-region redraw, for low-power windows.
//!
//! On a software adapter (WARP, lavapipe) every shaded pixel costs CPU, and a frame that changes
//! a few rows of text still shaded the whole window. Here the window keeps its last frame in a
//! texture of its own ([`Retained`]); each frame compares its instances with the ones that frame
//! drew, redraws only the pixels they can reach (cleared to the window colour, every instance
//! drawn again under a scissor), and copies the texture to the drawable.
//!
//! The comparison holds an instance's output to be a function of its own bytes, the layers it
//! sits in and the atlas texels it samples, as `Presented::same` does. An instance that appears,
//! disappears, changes or changes place in the painter order among the others damages the
//! pixels its quad can cover (`bounds`), or the whole of the outermost layer it sits in.
//! Instances that stay, in the same relative order, draw the same pixels everywhere outside
//! the damage, so those pixels can be kept. New image texels (a frame of a video or an
//! animated image written in place) damage the image instances on that page. New glyphs go
//! into unused atlas space, so they change no texel a kept glyph samples; the glyph atlas
//! reuses space only by resetting a page (`Atlas::resets`), which redraws everything, as a
//! new size, format, scale or clear colour does.
//!
//! A scroll moves the kept pixels instead of redrawing the viewport (`scroll.rs`).
use super::*;

mod scroll;
pub(crate) use scroll::Shift;

/// The most scissored rectangles a frame redraws; more are merged.
pub(crate) const MAX_RECTS: usize = 4;
/// A frame whose damage covers more than this share of the window redraws all of it.
const FULL_SHARE: u64 = 70;
/// A frame whose plain damage covers more than one in this many of the window's pixels tries
/// a scroll move.
const SCROLL_SHARE: u64 = 32;
/// Pixels added around each damaged quad, for rounding and antialiased edges.
const MARGIN: f32 = 2.0;

/// What a frame redraws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Damage {
    /// Everything: cleared, as a frame without damage tracking draws.
    Full,
    /// Only these `[x, y, width, height]` device-pixel rectangles; none when nothing changed.
    Rects(Vec<[u32; 4]>),
}

impl Damage {
    /// Device pixels it redraws in a `size` window.
    pub(crate) fn area(&self, size: (u32, u32)) -> u64 {
        match self {
            Damage::Full => u64::from(size.0) * u64::from(size.1),
            Damage::Rects(rects) => rects.iter().map(|r| u64::from(r[2]) * u64::from(r[3])).sum(),
        }
    }
}

/// One frame's instances, as the comparison sees them.
#[derive(Default)]
struct Painted {
    instances: Vec<Instance>,
    /// Each instance's bytes hashed with its layer scopes.
    keys: Vec<u64>,
    /// Each instance's outermost layer (an index into `layers`), or `NONE`.
    scopes: Vec<u32>,
    /// Edges `[x0, y0, x1, y1]` an outermost layer can draw in (see `Scope::reach`).
    layers: Vec<[f32; 4]>,
    /// The pixels its quads cover, summed: what drawing it all rasterizes.
    quads: u64,
}

const NONE: u32 = u32::MAX;

impl Painted {
    fn fill(&mut self, instances: &[Instance], size: (u32, u32)) {
        self.instances.clear();
        self.instances.extend_from_slice(instances);
        self.keys.clear();
        self.scopes.clear();
        self.layers.clear();
        self.quads = 0;
        // (hash of the enclosing scopes, outermost layer)
        let mut stack: Vec<(u64, u32)> = Vec::new();
        let mut open = Scope::default();
        for i in instances {
            let (chain, outer) = stack.last().copied().unwrap_or((0, NONE));
            let own = hash(i);
            self.keys.push(own ^ chain.rotate_left(29).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let kind = i.params[1];
            self.quads += rasterized(i, size);
            if kind == KIND_LAYER {
                let outer = if outer == NONE {
                    open = Scope { clip: i.clip, ..Scope::default() };
                    self.layers.push([0.0; 4]);
                    self.layers.len() as u32 - 1
                } else {
                    outer
                };
                open.add(i, size);
                self.scopes.push(outer);
                stack.push((chain.rotate_left(7) ^ own, outer));
            } else {
                self.scopes.push(outer);
                if outer != NONE { open.add(i, size); }
                if kind == KIND_LAYER_END {
                    stack.pop();
                    if stack.is_empty() { self.layers[outer as usize] = open.reach(size); }
                }
            }
        }
        // An unclosed scope (never built, but kept safe) reaches its clip.
        if let Some(&(_, outer)) = stack.first() { self.layers[outer as usize] = clamp_edges(open.clip, size); }
    }

    /// Edges of the pixels instance `at` can change: its outermost layer's, or its own quad's.
    fn reach(&self, at: usize, size: (u32, u32)) -> Option<[f32; 4]> {
        match self.scopes[at] {
            NONE => bounds(&self.instances[at], size),
            layer => Some(self.layers[layer as usize]).filter(|e| e[2] > e[0] && e[3] > e[1]),
        }
    }
}

/// An outermost layer while its instances are read.
#[derive(Default)]
struct Scope {
    clip: [f32; 4],
    /// Union of what its records and instances draw, upright.
    drawn: Option<[f32; 4]>,
    /// The widest blur of it and its inner layers, in device pixels.
    sigma: f32,
    turned: bool,
}

impl Scope {
    fn add(&mut self, i: &Instance, size: (u32, u32)) {
        if i.params[1] == KIND_LAYER {
            self.sigma = self.sigma.max(i.params[2]);
            self.turned |= i.border[1] != 0.0;
        }
        if let Some(e) = bounds(i, size) {
            self.drawn = Some(self.drawn.map_or(e, |d| [d[0].min(e[0]), d[1].min(e[1]), d[2].max(e[2]), d[3].max(e[3])]));
        }
    }

    /// The window pixels the composited layer can change: what it draws, spread by its blur,
    /// cut by its clip. A turned layer (or a non-finite one) can reach its whole clip.
    fn reach(&self, size: (u32, u32)) -> [f32; 4] {
        let clip = clamp_edges(self.clip, size);
        let Some(d) = self.drawn.filter(|_| !self.turned && self.sigma.is_finite()) else { return clip };
        let g = self.sigma * 3.0 + 2.0;
        let e = [(d[0] - g).max(clip[0] - 1.0), (d[1] - g).max(clip[1] - 1.0), (d[2] + g).min(clip[2] + 1.0), (d[3] + g).min(clip[3] + 1.0)];
        let e = clamp_edges(e, size);
        if e[2] > e[0] && e[3] > e[1] { e } else { [0.0; 4] }
    }
}

/// FNV-1a over an instance's 32 words.
fn hash(i: &Instance) -> u64 {
    let words: &[u32; 32] = bytemuck::cast_ref(i);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words { h = (h ^ u64::from(*w)).wrapping_mul(0x0000_0100_0000_01b3); }
    h
}

fn same(a: &Instance, b: &Instance) -> bool {
    bytemuck::bytes_of(a) == bytemuck::bytes_of(b)
}

fn clamp_edges(e: [f32; 4], (w, h): (u32, u32)) -> [f32; 4] {
    let fix = |v: f32, max: u32| if v.is_nan() { 0.0 } else { v.clamp(0.0, max as f32) };
    [fix(e[0], w), fix(e[1], h), fix(e[2], w), fix(e[3], h)]
}

/// The window pixels `base_vs` makes instance `i`'s quad cover, which the shader runs on
/// whether its clip shows them or not: its rect grown as `bounds` grows it, cut to its band
/// (`uv.z < 0`, plain and shadow quads) and to the window.
fn rasterized(i: &Instance, size: (u32, u32)) -> u64 {
    let kind = i.params[1];
    if kind == KIND_LAYER_END || kind == KIND_EFFECT { return 0; }
    let r = i.rect;
    let grow = grown(i);
    let mut e = [r[0] - grow, r[1] - grow, r[0] + r[2] + grow, r[1] + r[3] + grow];
    if kind <= KIND_SHADOW && i.uv[2] < 0.0 {
        e = [e[0].max(r[0] + i.uv[0]), e[1].max(r[1] + i.uv[1]), e[2].min(r[0] - i.uv[2]), e[3].min(r[1] + i.uv[3])];
    }
    let e = clamp_edges(e, size);
    if e[2] > e[0] && e[3] > e[1] { ((e[2] - e[0]) * (e[3] - e[1])) as u64 } else { 0 }
}

/// How far `base_vs` grows an instance's quad past its rect.
fn grown(i: &Instance) -> f32 {
    match i.params[1] {
        k if k == KIND_SHADOW => i.params[2] * 3.0 + 1.0,
        k if k == KIND_GLYPH => 0.0,
        k if k == KIND_INSET || (KIND_SEGMENT..=KIND_LAYER_END).contains(&k) => 1.0,
        _ => 1.0 + i.params[2] * 2.0,
    }
}

/// Edges `[x0, y0, x1, y1]` of the window pixels instance `i` can draw, clamped to the window:
/// the quad `base_vs` in shader.wgsl makes of it, cut by its clip. None when it draws nothing.
pub(crate) fn bounds(i: &Instance, size: (u32, u32)) -> Option<[f32; 4]> {
    let kind = i.params[1];
    if kind == KIND_LAYER_END || kind == KIND_EFFECT { return None; }
    let r = i.rect;
    let grow = grown(i);
    let mut e = [r[0] - grow, r[1] - grow, r[0] + r[2] + grow, r[1] + r[3] + grow];
    // A turned icon (`params.x` radians about its centre) can reach its half diagonal around it.
    if kind == KIND_GLYPH && i.params[0] != 0.0 {
        let (cx, cy) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
        let reach = (r[2] * r[2] + r[3] * r[3]).sqrt() * 0.5 + 1.0;
        e = [cx - reach, cy - reach, cx + reach, cy + reach];
    }
    // A band (`uv.z < 0`, plain and shadow quads: `runtime/paint/bands.rs`, `render/cull.rs`)
    // rasterizes only its window.
    if kind <= KIND_SHADOW && i.uv[2] < 0.0 {
        e = [e[0].max(r[0] + i.uv[0]), e[1].max(r[1] + i.uv[1]), e[2].min(r[0] - i.uv[2]), e[3].min(r[1] + i.uv[3])];
    }
    // `clip_alpha` is zero half a pixel outside the clip.
    let c = i.clip;
    let e = [e[0].max(c[0] - 1.0), e[1].max(c[1] - 1.0), e[2].min(c[2] + 1.0), e[3].min(c[3] + 1.0)];
    if e.iter().any(|v| !v.is_finite()) { return Some([0.0, 0.0, size.0 as f32, size.1 as f32]); }
    let e = clamp_edges(e, size);
    (e[2] > e[0] && e[3] > e[1]).then_some(e)
}

/// The image atlas pages written this frame (index = an image instance's `params[0]`).
#[derive(Default)]
pub(crate) struct Texels {
    pub images: Vec<bool>,
}

impl Texels {
    pub(crate) fn of(images: &crate::pixels::ImageAtlas) -> Self {
        Texels { images: images.pages.iter().map(|p| p.dirty.is_some()).collect() }
    }

    fn any(&self) -> bool { self.images.iter().any(|w| *w) }

    /// Whether `i` samples texels written this frame.
    fn sampled_by(&self, i: &Instance) -> bool {
        i.params[1] == KIND_IMAGE && self.images.get(i.params[0] as usize).copied().unwrap_or(false)
    }
}

/// What the retained frame was drawn from, beyond its instances.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Surroundings {
    pub size: (u32, u32),
    pub clear: [f32; 4],
    pub srgb: bool,
    pub scale: f32,
    /// `Atlas::resets`: a reset moves glyphs whose instances may not change.
    pub atlas_resets: u64,
}

/// A window's last frame, kept to redraw only what changes.
pub(crate) struct Retained {
    target: Option<(wgpu::Texture, wgpu::TextureView)>,
    view_format: wgpu::TextureFormat,
    painted: Painted,
    next: Painted,
    drawn: Option<Surroundings>,
    clear: Option<ClearPipeline>,
    /// The move `damage` found for this frame, for `blit`.
    pub(crate) shift: Option<Shift>,
    /// Where `blit` holds the moved pixels between its two copies.
    scratch: Option<wgpu::Texture>,
    /// Device pixels the last frame redrew and the window's (for tests and the frame log).
    pub(crate) last: (u64, u64),
}

impl Retained {
    pub(crate) fn new() -> Self {
        Retained { target: None, view_format: wgpu::TextureFormat::Bgra8UnormSrgb, painted: Painted::default(),
            next: Painted::default(), drawn: None, clear: None, shift: None, scratch: None, last: (0, 0) }
    }

    /// Forgets the kept frame: the next one redraws everything.
    pub(crate) fn forget(&mut self) {
        self.drawn = None;
        self.shift = None;
    }

    /// Makes or remakes the kept frame, a texture like the drawable (`format`, viewed as
    /// `view_format`), for `size`. A new one forgets what was drawn.
    pub(crate) fn fit(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, view_format: wgpu::TextureFormat, size: (u32, u32)) {
        let fits = self.target.as_ref().is_some_and(|(t, _)| (t.width(), t.height()) == size && t.format() == format)
            && self.view_format == view_format;
        if fits { return; }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("retained frame"),
            size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: if view_format != format { std::slice::from_ref(&view_format) } else { &[] },
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor { format: Some(view_format), ..Default::default() });
        self.target = Some((texture, view));
        self.view_format = view_format;
        self.scratch = None;
        self.forget();
    }

    /// The kept frame's view, once `fit`.
    pub(crate) fn view(&self) -> Option<&wgpu::TextureView> { self.target.as_ref().map(|(_, v)| v) }

    /// What this frame must redraw, comparing `instances` with the kept frame's. `drawn` are
    /// the instances the frame draws: `instances` as `render/cull.rs` culls them (the same
    /// pixels), or `instances` themselves; only their rasterized pixels count. The comparison reads
    /// the unculled ones: a culled quad's band depends on where its clip cuts it, so a card
    /// the viewport's edge cuts would change as it scrolls. Call before `Renderer::prepare`,
    /// which takes the atlases' written rows.
    pub(crate) fn damage(&mut self, instances: &[Instance], drawn: &[Instance], around: Surroundings, texels: &Texels) -> Damage {
        self.next.fill(instances, around.size);
        if !std::ptr::eq(instances, drawn) { self.next.quads = drawn.iter().map(|i| rasterized(i, around.size)).sum(); }
        self.shift = None;
        if self.target.is_none() || self.drawn != Some(around) { return Damage::Full; }
        let size = around.size;
        let mut sampled = Vec::new();
        if texels.any() {
            for at in 0..self.next.instances.len() {
                if texels.sampled_by(&self.next.instances[at]) && let Some(e) = self.next.reach(at, size) { sampled.push(e); }
            }
        }
        let (damage, shift) = compare(&self.painted, &self.next, size, sampled);
        self.shift = shift;
        damage
    }

    /// Moves the kept pixels as `damage` found the frame scrolled, before the frame's damage
    /// is redrawn (two copies through a scratch texture: a texture cannot copy onto itself).
    pub(crate) fn blit(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let (Some(shift), Some((target, _))) = (self.shift, &self.target) else { return };
        if self.scratch.is_none() {
            self.scratch = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("retained frame scroll"),
                size: target.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: target.format(),
                usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        let scratch = self.scratch.as_ref().unwrap();
        let [x0, y0, x1, y1] = shift.to;
        let extent = wgpu::Extent3d { width: x1 - x0, height: y1 - y0, depth_or_array_layers: 1 };
        fn at(texture: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All }
        }
        let from = y0.wrapping_add_signed(-shift.dy);
        encoder.copy_texture_to_texture(at(target, x0, from), at(scratch, x0, y0), extent);
        encoder.copy_texture_to_texture(at(scratch, x0, y0), at(target, x0, y0), extent);
    }

    /// The frame `damage` looked at was drawn into the kept frame.
    pub(crate) fn drawn(&mut self, around: Surroundings, damage: &Damage) {
        std::mem::swap(&mut self.painted, &mut self.next);
        self.drawn = Some(around);
        self.last = (damage.area(around.size), u64::from(around.size.0) * u64::from(around.size.1));
        crate::low_power::count_redrawn(self.last.0, self.last.1, self.painted.quads);
    }

    /// Readies the pipeline that fills a damaged rectangle with the clear colour `color`, for
    /// the kept frame's view format.
    pub(crate) fn fit_fill(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, color: [f32; 4]) {
        let format = self.view_format;
        if self.clear.as_ref().is_none_or(|c| c.format != format) { self.clear = Some(ClearPipeline::new(device, format)); }
        let c = self.clear.as_mut().unwrap();
        if c.color != Some(color) {
            queue.write_buffer(&c.buffer, 0, bytemuck::cast_slice(&color));
            c.color = Some(color);
        }
    }

    /// The pipeline `fit_fill` readied.
    pub(crate) fn fill(&self) -> Option<&ClearPipeline> { self.clear.as_ref() }

    /// Copies the kept frame into `drawable`, a texture of the same size and format.
    pub(crate) fn copy_to(&self, encoder: &mut wgpu::CommandEncoder, drawable: &wgpu::Texture) {
        let Some((texture, _)) = &self.target else { return };
        encoder.copy_texture_to_texture(texture.as_image_copy(), drawable.as_image_copy(), texture.size());
    }
}

/// What a frame redraws going from `old` to `new`, past the `sampled` edges, and the move it
/// makes of the kept pixels, if any.
fn compare(old: &Painted, new: &Painted, size: (u32, u32), sampled: Vec<[f32; 4]>) -> (Damage, Option<Shift>) {
    let mut boxes = sampled.clone();
    changed(old, new, size, &mut boxes);
    let plain = reduce(&boxes, size);
    // A change of more than a few rows may be a scroll: try moving the kept pixels. (A
    // scrolled viewport can be a small share of a big window: a chat beside open panels.)
    let window = u64::from(size.0) * u64::from(size.1);
    if plain.area(size) * SCROLL_SHARE > window {
        let mut boxes = sampled;
        if let Some(shift) = scroll::scrolled(old, new, size, &mut boxes) {
            let moved = reduce(&boxes, size);
            if moved != Damage::Full && moved.area(size) < plain.area(size) {
                return (moved, Some(shift));
            }
        }
    }
    (plain, None)
}

/// The share of a `size` window (device pixels) a damage-tracked frame redraws going from the
/// instances `old` to `new` (`Runtime::instances`, before culling), and whether it moves the
/// kept pixels to follow a scroll. For tests of an app's own screens.
pub fn damage_share(old: &[Instance], new: &[Instance], size: (u32, u32)) -> (f64, bool) {
    let (mut a, mut b) = (Painted::default(), Painted::default());
    a.fill(old, size);
    b.fill(new, size);
    let (damage, shift) = compare(&a, &b, size, Vec::new());
    (damage.area(size) as f64 / (f64::from(size.0) * f64::from(size.1)), shift.is_some())
}

/// Edges of what changed from `old` to `new`, pushed onto `out`.
fn changed(old: &Painted, new: &Painted, size: (u32, u32), out: &mut Vec<[f32; 4]>) {
    let (a, b) = (&old.keys, &new.keys);
    let eq = |i: usize, j: usize| a[i] == b[j] && same(&old.instances[i], &new.instances[j]);
    let shared = a.len().min(b.len());
    let mut lo = 0;
    while lo < shared && eq(lo, lo) { lo += 1; }
    let mut tail = 0;
    while tail < shared - lo && eq(a.len() - 1 - tail, b.len() - 1 - tail) { tail += 1; }
    let (old_mid, new_mid) = (lo..a.len() - tail, lo..b.len() - tail);
    let mut push = |p: &Painted, at: usize| if let Some(e) = p.reach(at, size) { out.push(e) };
    if old_mid.len() == new_mid.len() {
        // The same count: one changed in place, or a scroll that moved them all.
        for (i, j) in old_mid.zip(new_mid) {
            if !eq(i, j) { push(old, i); push(new, j); }
        }
        return;
    }
    // Match each new instance with the first unmatched old one of the same key; matched pairs
    // whose old order is kept (a longest increasing run of old indices) stay, the rest change.
    let mut sorted: Vec<(u64, u32)> = old_mid.clone().map(|i| (a[i], i as u32)).collect();
    sorted.sort_unstable();
    let mut taken = vec![0u32; sorted.len()];
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(new_mid.len());
    let mut matched_old = vec![false; old_mid.len()];
    for j in new_mid.clone() {
        let start = sorted.partition_point(|e| e.0 < b[j]);
        let k = start + taken.get(start).copied().unwrap_or(0) as usize;
        if k < sorted.len() && sorted[k].0 == b[j] && same(&old.instances[sorted[k].1 as usize], &new.instances[j]) {
            taken[start] += 1;
            pairs.push((sorted[k].1, j as u32));
            matched_old[sorted[k].1 as usize - lo] = true;
        } else {
            push(new, j);
        }
    }
    for (i, m) in old_mid.clone().zip(&matched_old) { if !m { push(old, i); } }
    let keep = increasing(&pairs);
    for ((i, j), kept) in pairs.iter().zip(keep) {
        if !kept { push(old, *i as usize); push(new, *j as usize); }
    }
}

/// Marks the pairs in one longest run whose old indices increase (new ones already do).
fn increasing(pairs: &[(u32, u32)]) -> Vec<bool> {
    // Patience sorting: `tails[k]` is the pair ending the best run of length k + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut before = vec![usize::MAX; pairs.len()];
    for (n, (i, _)) in pairs.iter().enumerate() {
        let k = tails.partition_point(|&t| pairs[t].0 < *i);
        if k > 0 { before[n] = tails[k - 1]; }
        if k == tails.len() { tails.push(n) } else { tails[k] = n }
    }
    let mut keep = vec![false; pairs.len()];
    let mut at = tails.last().copied().unwrap_or(usize::MAX);
    while at != usize::MAX { keep[at] = true; at = before[at]; }
    keep
}

/// Whole-pixel rectangles covering `boxes` (edges), at most `MAX_RECTS`, or `Full` when they
/// cover most of the window. Two rectangles join when their union costs no more than drawing
/// both; otherwise they may overlap, as redrawing a rectangle twice draws the same pixels (a
/// tall card whose corner pokes into a wide strip shades less than their union).
fn reduce(boxes: &[[f32; 4]], size: (u32, u32)) -> Damage {
    let (w, h) = (size.0 as i64, size.1 as i64);
    let area = |r: &[i64; 4]| (r[2] - r[0]) * (r[3] - r[1]);
    let union = |a: &[i64; 4], b: &[i64; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
    let cheap = |a: &[i64; 4], b: &[i64; 4]| area(&union(a, b)) <= area(a) + area(b);
    let mut rects: Vec<[i64; 4]> = Vec::new();
    for e in boxes {
        let r = [((e[0] - MARGIN).floor() as i64).max(0), ((e[1] - MARGIN).floor() as i64).max(0),
            ((e[2] + MARGIN).ceil() as i64).min(w), ((e[3] + MARGIN).ceil() as i64).min(h)];
        if r[2] <= r[0] || r[3] <= r[1] { continue; }
        if let Some(q) = rects.iter_mut().find(|q| cheap(q, &r)) {
            *q = union(q, &r);
            continue;
        }
        rects.push(r);
        if rects.len() > MAX_RECTS {
            // Merge the two whose union adds the least.
            let mut best = (i64::MAX, 0, 1);
            for x in 0..rects.len() {
                for y in x + 1..rects.len() {
                    let grow = area(&union(&rects[x], &rects[y])) - area(&rects[x]) - area(&rects[y]);
                    if grow < best.0 { best = (grow, x, y); }
                }
            }
            let merged = union(&rects[best.1], &rects[best.2]);
            rects.swap_remove(best.2);
            rects[best.1] = merged;
        }
    }
    // Merging can make more pairs cheap to join.
    let mut i = 0;
    while i < rects.len() {
        if let Some(j) = (0..rects.len()).find(|&j| j != i && cheap(&rects[i], &rects[j])) {
            let merged = union(&rects[i], &rects[j]);
            rects[i.min(j)] = merged;
            rects.swap_remove(i.max(j));
            i = 0;
        } else {
            i += 1;
        }
    }
    let total: i64 = rects.iter().map(area).sum();
    if total as u64 * 100 > FULL_SHARE * (w * h) as u64 { return Damage::Full; }
    Damage::Rects(rects.iter().map(|r| [r[0] as u32, r[1] as u32, (r[2] - r[0]) as u32, (r[3] - r[1]) as u32]).collect())
}

/// Fills the scissored rectangle with one colour, replacing what is there, as a pass's clear
/// would.
pub(crate) struct ClearPipeline {
    format: wgpu::TextureFormat,
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) bind: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    color: Option<[f32; 4]>,
}

const CLEAR_WGSL: &str = "
@group(0) @binding(0) var<uniform> color: vec4<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return color; }
";

impl ClearPipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("damage clear"),
            source: wgpu::ShaderSource::Wgsl(CLEAR_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("damage clear"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("damage clear"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("damage clear"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("damage clear"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("damage clear"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        ClearPipeline { format, pipeline, bind, buffer, color: None }
    }
}

impl Renderer {
    /// Whether the prepared frame can be redrawn in part (`draw_damaged`): nothing reads the
    /// target mid-frame (effect slots, backdrop blur) or moves on the GPU (springs). The window
    /// also checks that nothing draws outside the instances (native surfaces, materials).
    pub(crate) fn damage_tracking_fits(&self) -> bool {
        self.springs.is_none() && self.effects.is_empty() && !self.has_backdrop_blur()
    }

    /// Draws the prepared frame into `view`, the kept frame: all of it, cleared to `clear`, for
    /// `Damage::Full`; otherwise only inside each rectangle, filled with the clear colour by
    /// `fill` and then every instance drawn again under that scissor.
    pub(crate) fn draw_damaged(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, clear: [f32; 4],
        damage: &Damage, fill: &ClearPipeline) {
        if let Damage::Rects(rects) = damage && rects.is_empty() {
            self.flush_uploads(encoder);
            return;
        }
        self.encode_layers_timed(encoder, None, &mut |_, _| {});
        let load = match damage {
            Damage::Full => wgpu::LoadOp::Clear(wgpu::Color { r: clear[0] as f64, g: clear[1] as f64, b: clear[2] as f64, a: clear[3] as f64 }),
            Damage::Rects(_) => wgpu::LoadOp::Load,
        };
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }).forget_lifetime();
        let mut active = ActivePass::owned(pass, encoder, view);
        let size = self.target_frame(None).1;
        match damage {
            Damage::Full => self.draw_layer_range(&mut active, self.format, 0..self.count, None, &mut |_, _| {}),
            Damage::Rects(rects) => for r in rects {
                active.set_scissor(Some(*r), size);
                active.set_pipeline(&fill.pipeline);
                active.set_bind_group(0, &fill.bind, &[]);
                active.draw(0..3, 0..1);
                self.draw_layer_range(&mut active, self.format, 0..self.count, None, &mut |_, _| {});
            },
        }
    }
}

#[cfg(test)]
#[path = "damage_tests.rs"]
mod tests;
