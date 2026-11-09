//! A canvas-like scene for timing low-power rendering (AgentOps patch, PARITY-PATCH.md): a
//! toolbar, a sidebar and a minimap around a viewport of a few hundred cards (a shadow, a
//! bordered body, a header, a title, two lines of text and two ports each), dragged, hovered,
//! scrolled, panned and zoomed. Ignored by default; run with
//! `cargo test -p rune --lib canvas_bench -- --ignored --nocapture --test-threads=1`, with the
//! rune crate optimized (`--config 'profile.dev.package.rune.opt-level=3'`) for numbers that mean
//! something, and on lavapipe for the GPU half.
use super::*;
use std::time::Instant;

const SIZE: (u32, u32) = (1600, 1000);
/// The canvas viewport, `[x0, y0, x1, y1]`.
const VIEW: [f32; 4] = [240.0, 40.0, 1600.0, 1000.0];
const COLS: usize = 18;
const ROWS: usize = 12;
const BG: [f32; 4] = [0.06, 0.06, 0.07, 1.0];
const PANEL: [f32; 4] = [0.09, 0.09, 0.11, 1.0];
const CANVAS: [f32; 4] = [0.04, 0.045, 0.05, 1.0];
const CARD: [f32; 4] = [0.13, 0.13, 0.16, 1.0];

#[derive(Clone, Copy, Default)]
pub(super) struct Camera {
    pub pan: [f32; 2],
    pub zoom: f32,
    /// How far card `DRAGGED` is dragged.
    pub drag: [f32; 2],
    /// The card the pointer is over.
    pub hovered: usize,
}

const DRAGGED: usize = 40;

fn quad(x: f32, y: f32, w: f32, h: f32, fill: [f32; 4]) -> Instance {
    Instance { rect: [x, y, w, h], fill, clip: NO_CLIP, params: [0.0, KIND_RECT, 0.0, 0.0], ..Instance::zeroed() }
}

fn glyphs(out: &mut Vec<Instance>, x: f32, y: f32, n: usize, size: f32, clip: [f32; 4]) {
    for k in 0..n {
        let w = (size * 0.6).round().max(1.0);
        out.push(Instance {
            rect: [(x + k as f32 * w).round(), y.round(), w, size.round().max(1.0)],
            fill: [0.9, 0.9, 0.92, 1.0],
            params: [0.0, KIND_GLYPH, 0.0, 1.0],
            uv: [(k % 16) as f32 * 8.0, 0.0, 8.0, 12.0],
            clip,
            ..Instance::zeroed()
        });
    }
}

/// The frame's instances for a camera.
pub(super) fn canvas(c: Camera) -> Vec<Instance> {
    let mut out = vec![quad(0.0, 0.0, 1600.0, 1000.0, BG), quad(0.0, 0.0, 1600.0, 40.0, PANEL)];
    glyphs(&mut out, 12.0, 12.0, 30, 14.0, NO_CLIP);
    out.push(quad(0.0, 40.0, 240.0, 960.0, PANEL));
    for n in 0..30 { glyphs(&mut out, 16.0, 56.0 + n as f32 * 28.0, 16, 13.0, [0.0, 40.0, 240.0, 1000.0]); }
    out.push(Instance { clip: VIEW, ..quad(VIEW[0], VIEW[1], VIEW[2] - VIEW[0], VIEW[3] - VIEW[1], CANVAS) });
    let z = c.zoom;
    for k in 0..COLS * ROWS {
        let (col, row) = ((k % COLS) as f32, (k / COLS) as f32);
        let mut x = VIEW[0] + 40.0 * z + col * 200.0 * z + c.pan[0];
        let mut y = VIEW[1] + 40.0 * z + row * 130.0 * z + c.pan[1];
        if k == DRAGGED { (x, y) = (x + c.drag[0], y + c.drag[1]); }
        let (w, h) = (160.0 * z, 96.0 * z);
        out.push(Instance { radii: [8.0 * z; 4], params: [0.0, KIND_SHADOW, 4.0 * z, 0.0], clip: VIEW,
            ..quad(x, y + 2.0 * z, w, h, [0.0, 0.0, 0.0, 0.4]) });
        let border = if k == c.hovered { [0.4, 0.5, 0.9, 1.0] } else { [0.25, 0.25, 0.3, 1.0] };
        out.push(Instance { radii: [8.0 * z; 4], border, params: [1.0, KIND_RECT, 0.0, 0.0], clip: VIEW, ..quad(x, y, w, h, CARD) });
        out.push(Instance { radii: [7.0 * z, 7.0 * z, 0.0, 0.0], clip: VIEW,
            ..quad(x + 1.0, y + 1.0, w - 2.0, 24.0 * z, [0.18, 0.2, 0.3, 1.0]) });
        glyphs(&mut out, x + 10.0 * z, y + 6.0 * z, 14, 13.0 * z, VIEW);
        glyphs(&mut out, x + 10.0 * z, y + 36.0 * z, 22, 11.0 * z, VIEW);
        glyphs(&mut out, x + 10.0 * z, y + 56.0 * z, 22, 11.0 * z, VIEW);
        for (px, py) in [(x - 5.0 * z, y + 40.0 * z), (x + w - 5.0 * z, y + 40.0 * z)] {
            out.push(Instance { radii: [5.0 * z; 4], clip: VIEW, ..quad(px, py, 10.0 * z, 10.0 * z, [0.5, 0.7, 0.4, 1.0]) });
        }
    }
    // The minimap over the canvas's corner, which stays.
    out.push(Instance { radii: [6.0; 4], border: [0.3, 0.3, 0.35, 1.0], params: [1.0, KIND_RECT, 0.0, 0.0], ..quad(1380.0, 840.0, 200.0, 140.0, PANEL) });
    for k in 0..12 { out.push(quad(1390.0 + (k % 4) as f32 * 46.0, 850.0 + (k / 4) as f32 * 40.0, 36.0, 24.0, CARD)); }
    out
}

/// Each scenario's cameras, frame by frame.
pub(super) fn scenarios() -> Vec<(&'static str, Vec<Camera>)> {
    let start = Camera { pan: [0.0, 0.0], zoom: 1.0, drag: [0.0, 0.0], hovered: 0 };
    let n = 40;
    let frames = |f: &dyn Fn(usize, &mut Camera)| (0..n).map(|k| { let mut c = start; f(k, &mut c); c }).collect::<Vec<_>>();
    vec![
        ("hover", frames(&|k, c| c.hovered = 20 + k % 6)),
        ("drag", frames(&|k, c| c.drag = [k as f32 * 3.0, k as f32 * 2.0])),
        ("wheel", frames(&|k, c| c.pan = [0.0, -(k as f32) * 40.0])),
        ("pan", frames(&|k, c| c.pan = [-(k as f32) * 7.0, -(k as f32) * 5.0])),
        ("zoom", frames(&|k, c| c.zoom = 1.0 + k as f32 * 0.02)),
    ]
}

/// Process CPU seconds (all threads: lavapipe's included), from /proc.
pub(super) fn cpu_secs() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else { return 0.0 };
    let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
    let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
}

#[test]
#[ignore]
fn canvas_bench_cpu() {
    for (name, cams) in scenarios() {
        let frames: Vec<Vec<Instance>> = cams.iter().map(|c| canvas(*c)).collect();
        let (mut old, mut new) = (Painted::default(), Painted::default());
        let (mut culled, mut culler, mut work) = (Vec::new(), super::super::cull::Culler::default(), Work::default());
        let (mut t_damage, mut t_cull, mut shade, mut moved, mut drawn) = (0.0, 0.0, 0u64, 0, 0u64);
        let reps = 5;
        for _ in 0..reps {
            old.fill(&frames[0], SIZE);
            for f in &frames[1..] {
                let t = Instant::now();
                culler.cull(f, SIZE, &mut culled, true);
                t_cull += t.elapsed().as_secs_f64();
                let t = Instant::now();
                new.fill(f, SIZE);
                new.quads = culled.iter().map(|i| rasterized(i, SIZE)).sum();
                let (damage, shift) = compare(&old, &new, SIZE, &mut work);
                t_damage += t.elapsed().as_secs_f64();
                shade += damage.area(SIZE);
                drawn += new.quads;
                moved += usize::from(shift.is_some());
                std::mem::swap(&mut old, &mut new);
            }
        }
        let n = (reps * (frames.len() - 1)) as f64;
        let window = f64::from(SIZE.0 * SIZE.1);
        old.fill(&frames[0], SIZE);
        new.fill(&frames[1], SIZE);
        println!("BENCH {name:6} guessed {:?}", scroll::guess(&old, &new, &mut Default::default()));
        println!("BENCH {name:6} instances {:5}  cull {:6.0} µs  damage {:6.0} µs  redrawn {:5.1} %  culled quads {:4.2}x window  moved {:3.0} %",
            frames[0].len(), t_cull / n * 1e6, t_damage / n * 1e6, shade as f64 / n / window * 100.0, drawn as f64 / n / window,
            moved as f64 / n * 100.0);
    }
}

/// The whole low-power frame on the headless GPU (lavapipe here): cull, damage, prepare, the
/// scroll move, the damaged redraw and the copy into a drawable, waited for; against the same
/// frames drawn whole, and without the copy, to tell what each costs.
#[test]
#[ignore]
fn canvas_bench_gpu() {
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    fn view(_: &(), _: &mut crate::Cx<()>) -> crate::El { crate::el() }
    let gpu = Gpu::headless();
    let mut rt = crate::Runtime::new((), view, (16.0, 16.0), crate::Env::new(true, 1.0));
    rt.frame(0.0);
    let drawable = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None, size: wgpu::Extent3d { width: SIZE.0, height: SIZE.1, depth_or_array_layers: 1 }, mip_level_count: 1,
        sample_count: 1, dimension: wgpu::TextureDimension::D2, format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST, view_formats: &[],
    });
    let drawable_view = drawable.create_view(&Default::default());
    let clear = BG;
    let around = Surroundings { size: SIZE, clear, srgb: false, scale: 1.0, atlas_resets: 0 };
    let only = std::env::var("CANVAS_BENCH").ok();
    for (name, cams) in scenarios() {
        if only.as_deref().is_some_and(|o| o != name) { continue; }
        let frames: Vec<Vec<Instance>> = cams.iter().map(|c| canvas(*c)).collect();
        for mode in ["tracked", "no copy", "whole"] {
            let mut renderer = Renderer::new(&gpu.device, FORMAT);
            let mut retained = Retained::new();
            let mut culler = super::super::cull::Culler::default();
            let (mut culled, mut damaged) = (Vec::new(), Vec::new());
            let (mut wall, mut cpu, mut shaded) = (0.0, 0.0, 0u64);
            // The first frame draws everything; the rest are what the scenario measures.
            for (k, f) in frames.iter().enumerate() {
                let (t, c) = (Instant::now(), cpu_secs());
                culler.cull(f, SIZE, &mut culled, true);
                retained.fit(&gpu.device, FORMAT, FORMAT, SIZE);
                let damage = if mode == "whole" { Damage::Full } else { retained.damage(f, &culled, around, &Texels::default()) };
                let narrowed = keep_damaged(&culled, &damage, SIZE, &mut damaged);
                renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, if narrowed { &damaged } else { &culled }, SIZE);
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                if mode == "whole" {
                    renderer.draw(&mut enc, &drawable_view, clear);
                } else {
                    retained.fit_fill(&gpu.device, &gpu.queue, clear);
                    retained.blit(&gpu.device, &mut enc);
                    renderer.draw_damaged(&mut enc, retained.view().unwrap(), clear, &damage, retained.fill().unwrap());
                    if mode == "tracked" { retained.copy_to(&mut enc, &drawable); }
                    retained.drawn(around, &damage);
                }
                renderer.submit(&gpu.queue, enc);
                gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
                if k > 0 {
                    wall += t.elapsed().as_secs_f64();
                    cpu += cpu_secs() - c;
                    shaded += damage.area(SIZE);
                }
            }
            let n = (frames.len() - 1) as f64;
            println!("GPU {name:6} {mode:8} {:6.1} ms a frame (wall), {:6.1} ms CPU, redrawn {:5.1} %", wall / n * 1e3, cpu / n * 1e3,
                shaded as f64 / n / f64::from(SIZE.0 * SIZE.1) * 100.0);
        }
    }
}

/// A pan moves the kept pixels both ways and redraws the L-shaped strips it exposes, and every
/// damage-tracked frame (pans, a wheel step, a drag, a zoom) draws what a whole frame draws,
/// pixel for pixel.
#[test]
fn a_pan_moves_the_kept_pixels_both_ways_and_draws_what_a_whole_frame_draws() {
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    fn view(_: &(), _: &mut crate::Cx<()>) -> crate::El { crate::el() }
    let gpu = Gpu::headless();
    let mut rt = crate::Runtime::new((), view, (16.0, 16.0), crate::Env::new(true, 1.0));
    rt.frame(0.0);
    let texture = || gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None, size: wgpu::Extent3d { width: SIZE.0, height: SIZE.1, depth_or_array_layers: 1 }, mip_level_count: 1,
        sample_count: 1, dimension: wgpu::TextureDimension::D2, format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST, view_formats: &[],
    });
    let read = |texture: &wgpu::Texture| {
        let (w, h) = SIZE;
        let padded = (w * 4).div_ceil(256) * 256;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: u64::from(padded * h), usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(texture.as_image_copy(), wgpu::TexelCopyBufferInfo {
            buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
        }, texture.size());
        gpu.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let data = slice.get_mapped_range().expect("mapped").to_vec();
        crate::tooling::record::unpad(&data, w, h, padded)
    };
    let (expected, got) = (texture(), texture());
    let mut full = Renderer::new(&gpu.device, FORMAT);
    let mut partial = Renderer::new(&gpu.device, FORMAT);
    let mut retained = Retained::new();
    let around = Surroundings { size: SIZE, clear: BG, srgb: false, scale: 1.0, atlas_resets: 0 };
    let at = |pan: [f32; 2], zoom: f32, drag: [f32; 2]| Camera { pan, zoom, drag, hovered: 0 };
    let steps = [
        ("first", at([0.0, 0.0], 1.0, [0.0; 2])),
        ("pan", at([-7.0, -5.0], 1.0, [0.0; 2])),
        ("pan on", at([-14.0, -10.0], 1.0, [0.0; 2])),
        ("pan right", at([10.0, -10.0], 1.0, [0.0; 2])),
        ("pan sideways", at([30.0, -10.0], 1.0, [0.0; 2])),
        ("wheel", at([30.0, -50.0], 1.0, [0.0; 2])),
        ("drag", at([30.0, -50.0], 1.0, [6.0, 4.0])),
        ("zoom", at([30.0, -50.0], 1.1, [6.0, 4.0])),
        ("pan zoomed", at([23.0, -45.0], 1.1, [6.0, 4.0])),
    ];
    let mut culler = super::super::cull::Culler::default();
    let (mut culled, mut damaged) = (Vec::new(), Vec::new());
    for (label, cam) in steps {
        let frame = canvas(cam);
        culler.cull(&frame, SIZE, &mut culled, true);
        retained.fit(&gpu.device, FORMAT, FORMAT, SIZE);
        let damage = retained.damage(&frame, &culled, around, &Texels::default());
        let shift = retained.shift;
        full.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &culled, SIZE);
        // As the window does: the damaged redraw draws only what touches its rectangles.
        let narrowed = keep_damaged(&culled, &damage, SIZE, &mut damaged);
        partial.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, if narrowed { &damaged } else { &culled }, SIZE);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        full.draw(&mut enc, &expected.create_view(&Default::default()), BG);
        retained.fit_fill(&gpu.device, &gpu.queue, BG);
        retained.blit(&gpu.device, &mut enc);
        partial.draw_damaged(&mut enc, retained.view().unwrap(), BG, &damage, retained.fill().unwrap());
        retained.copy_to(&mut enc, &got);
        full.before_submit(&mut enc);
        partial.submit(&gpu.queue, enc);
        retained.drawn(around, &damage);
        let (a, b) = (read(&expected), read(&got));
        // Where they differ, and by how much at most.
        let (mut differ, mut worst, mut boxed) = (0, 0, [u32::MAX, u32::MAX, 0, 0]);
        for (k, (a, b)) in a.chunks(4).zip(b.chunks(4)).enumerate() {
            if a == b { continue; }
            let (x, y) = (k as u32 % SIZE.0, k as u32 / SIZE.0);
            differ += 1;
            worst = worst.max(a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0));
            boxed = [boxed[0].min(x), boxed[1].min(y), boxed[2].max(x), boxed[3].max(y)];
        }
        // Moved pixels were shaded where the quads were, and lavapipe's interpolation of a quad's
        // local coordinates can round a step apart at another place (a band `render/cull.rs` cut
        // differently, too): a few pixels may come out one level off, never more.
        assert!(worst <= 1 && differ < 200, "{label}: {differ} pixels differ, by {worst} at most, within {boxed:?} ({damage:?}, {shift:?})");
        let share = damage.area(SIZE) * 100 / u64::from(SIZE.0 * SIZE.1);
        if label.starts_with("pan") {
            assert!(shift.is_some_and(|s| s.dx != 0), "{label}: the kept pixels move sideways ({damage:?}, {shift:?})");
            assert!(share < 25, "{label}: the strips the pan exposes, {share} %");
        }
        if label == "wheel" { assert!(shift.is_some_and(|s| s.dx == 0 && s.dy == -40), "{label}: {shift:?}"); }
    }
}
