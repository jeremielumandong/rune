use super::*;
use crate::{Color, Cx, El, Env, Runtime, ScrollSpec, col, el, row, text};

fn quad(x: f32, y: f32, w: f32, h: f32, fill: [f32; 4]) -> Instance {
    Instance { rect: [x, y, w, h], fill, clip: NO_CLIP, params: [0.0, KIND_RECT, 0.0, 0.0], ..Instance::zeroed() }
}

const SIZE: (u32, u32) = (200, 100);

fn damage_between(old: &[Instance], new: &[Instance]) -> Damage {
    let (mut a, mut b) = (Painted::default(), Painted::default());
    a.fill(old, SIZE);
    b.fill(new, SIZE);
    let mut boxes = Vec::new();
    changed(&a, &b, SIZE, &mut boxes);
    reduce(&boxes, SIZE)
}

#[test]
fn a_quad_reaches_its_grown_rect_inside_its_clip() {
    let mut q = quad(10.0, 20.0, 30.0, 5.0, [1.0; 4]);
    assert_eq!(bounds(&q, SIZE), Some([9.0, 19.0, 41.0, 26.0]));
    q.clip = [0.0, 0.0, 15.0, 100.0];
    assert_eq!(bounds(&q, SIZE), Some([9.0, 19.0, 16.0, 26.0]));
    q.clip = [100.0, 0.0, 120.0, 100.0];
    assert_eq!(bounds(&q, SIZE), None, "clipped away");
    let shadow = Instance { params: [0.0, KIND_SHADOW, 4.0, 0.0], ..quad(50.0, 50.0, 10.0, 10.0, [1.0; 4]) };
    assert_eq!(bounds(&shadow, SIZE), Some([37.0, 37.0, 73.0, 73.0]), "three sigmas and a pixel");
}

#[test]
fn the_same_frame_redraws_nothing() {
    let frame = [quad(0.0, 0.0, 200.0, 100.0, [0.1; 4]), quad(10.0, 10.0, 20.0, 20.0, [1.0; 4])];
    assert_eq!(damage_between(&frame, &frame), Damage::Rects(vec![]));
}

#[test]
fn a_changed_quad_redraws_where_it_was_and_is() {
    let old = [quad(0.0, 0.0, 200.0, 100.0, [0.1; 4]), quad(10.0, 10.0, 20.0, 20.0, [1.0; 4])];
    let mut new = old;
    new[1].rect[0] = 14.0;
    assert_eq!(damage_between(&old, &new), Damage::Rects(vec![[7, 7, 30, 26]]));
}

#[test]
fn far_apart_changes_redraw_apart_and_a_big_one_redraws_all() {
    let old = [quad(0.0, 0.0, 200.0, 100.0, [0.1; 4]), quad(2.0, 2.0, 4.0, 4.0, [1.0; 4]), quad(190.0, 90.0, 4.0, 4.0, [1.0; 4])];
    let mut new = old;
    new[1].fill = [0.5; 4];
    new[2].fill = [0.5; 4];
    assert_eq!(damage_between(&old, &new), Damage::Rects(vec![[0, 0, 9, 9], [187, 87, 10, 10]]));
    new[0].fill = [0.2; 4];
    assert_eq!(damage_between(&old, &new), Damage::Full);
}

#[test]
fn an_inserted_quad_redraws_only_itself_and_reordering_redraws_both() {
    let a = quad(10.0, 10.0, 20.0, 20.0, [1.0, 0.0, 0.0, 1.0]);
    let b = quad(20.0, 20.0, 20.0, 20.0, [0.0, 1.0, 0.0, 1.0]);
    let c = quad(150.0, 50.0, 10.0, 10.0, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(damage_between(&[a, b], &[a, c, b]), Damage::Rects(vec![[147, 47, 16, 16]]));
    // The same quads, b now under a: their overlap changes.
    assert_eq!(damage_between(&[a, b, c], &[b, a, c]), Damage::Rects(vec![[7, 7, 36, 36]]));
}

#[test]
fn a_change_inside_a_layer_redraws_the_layer() {
    let layer = Instance::begin_layer([50.0, 10.0, 40.0, 40.0], [50.0, 10.0, 90.0, 50.0], LayerSpec { opacity: 0.5, ..Default::default() });
    let inner = quad(60.0, 20.0, 5.0, 5.0, [1.0; 4]);
    let old = [layer, inner, Instance::end_layer()];
    let mut new = old;
    new[1].fill = [0.5; 4];
    assert_eq!(damage_between(&old, &new), Damage::Rects(vec![[47, 7, 46, 46]]));
}

#[test]
fn a_layer_reaches_what_it_draws_inside_its_clip_not_the_whole_clip() {
    let layer = Instance::begin_layer([50.0, 10.0, 20.0, 20.0], NO_CLIP, LayerSpec { opacity: 0.5, ..Default::default() });
    let inner = quad(60.0, 20.0, 5.0, 5.0, [1.0; 4]);
    let old = [layer, inner, Instance::end_layer()];
    let mut new = old;
    new[1].fill = [0.5; 4];
    // The layer's rect grown a pixel, two more for the composite, and the margin.
    assert_eq!(damage_between(&old, &new), Damage::Rects(vec![[45, 5, 30, 30]]));
    let blurred = Instance::begin_layer([50.0, 10.0, 20.0, 20.0], NO_CLIP, LayerSpec { opacity: 0.5, blur: 4.0, ..Default::default() });
    let (old, mut new) = ([blurred, inner, Instance::end_layer()], [blurred, inner, Instance::end_layer()]);
    new[1].fill = [0.5; 4];
    let Damage::Rects(r) = damage_between(&old, &new) else { panic!("full") };
    assert!(r[0][2] > 30, "the blur spreads it: {r:?}");
    let turned = layer.turned(0.5, [60.0, 20.0]);
    let (old, mut new) = ([turned, inner, Instance::end_layer()], [turned, inner, Instance::end_layer()]);
    new[1].fill = [0.5; 4];
    assert_eq!(damage_between(&old, &new), Damage::Full, "a turned layer reaches its whole clip");
}

#[test]
fn new_image_texels_redraw_the_images_on_their_page() {
    let image = |page: f32| Instance { params: [page, KIND_IMAGE, 0.0, 1.0], ..quad(5.0, 5.0, 8.0, 10.0, [1.0; 4]) };
    let texels = Texels { images: vec![false, true] };
    assert!(texels.sampled_by(&image(1.0)));
    assert!(!texels.sampled_by(&image(0.0)));
    let glyph = Instance { params: [1.0, KIND_GLYPH, 0.0, 1.0], ..quad(5.0, 5.0, 8.0, 10.0, [1.0; 4]) };
    assert!(!texels.sampled_by(&glyph));
}

/// A window background, rows in a viewport `[0, 10, 200, 90]` scrolled `offset` down, and a
/// badge over the viewport that stays.
fn scrolled_list(offset: f32) -> Vec<Instance> {
    let viewport = [0.0, 10.0, 200.0, 90.0];
    let mut frame = vec![quad(0.0, 0.0, 200.0, 100.0, [0.1; 4])];
    for n in 0..12 {
        let y = 10.0 + n as f32 * 12.0 - offset;
        if y + 10.0 < viewport[1] || y > viewport[3] { continue; }
        frame.push(Instance { clip: viewport, ..quad(4.0, y, 150.0, 10.0, [0.2, 0.2, 0.3 + n as f32 * 0.01, 1.0]) });
        frame.push(Instance { clip: viewport, ..quad(8.0, y + 2.0, 6.0, 6.0, [1.0; 4]) });
    }
    frame.push(quad(180.0, 40.0, 10.0, 10.0, [0.9, 0.5, 0.1, 1.0]));
    frame
}

#[test]
fn a_scroll_moves_the_kept_pixels_and_redraws_the_strip_it_exposes() {
    let (mut old, mut new) = (Painted::default(), Painted::default());
    old.fill(&scrolled_list(0.0), SIZE);
    new.fill(&scrolled_list(7.0), SIZE);
    let mut boxes = Vec::new();
    let shift = scroll::scrolled(&old, &new, SIZE, &mut boxes).expect("a scroll");
    assert_eq!(shift, Shift { to: [0, 11, 200, 82], dy: -7 }, "inside the viewport, clear of its edges");
    let Damage::Rects(rects) = reduce(&boxes, SIZE) else { panic!("full") };
    let area: u32 = rects.iter().map(|r| r[2] * r[3]).sum();
    // The strips above and below the move, the badge where it is and where the move drags
    // it; the rows' own pixels move.
    assert!(area < 200 * 30, "{rects:?}");
    assert!(rects.iter().any(|r| r[1] <= 82 && r[1] + r[3] >= 90), "the exposed strip: {rects:?}");
    // Nothing moved: no move.
    let mut boxes = Vec::new();
    assert_eq!(scroll::scrolled(&old, &old, SIZE, &mut boxes), None);
}

// The GPU check: a damage-tracked frame matches a full redraw, pixel for pixel.

#[derive(Clone)]
struct Rows {
    names: Vec<String>,
    hovered: Option<usize>,
}

fn rows_view(s: &Rows, _: &mut Cx<Rows>) -> El {
    let list = s.names.iter().enumerate().fold(col().key("list").w(200.0).gap(4.0).padding(6.0), |list, (n, name)| {
        let bg = if s.hovered == Some(n) { Color { r: 0.3, g: 0.3, b: 0.5, a: 1.0 } } else { Color { r: 0.15, g: 0.15, b: 0.2, a: 1.0 } };
        list.child(row().key(format!("row {n}")).padding(4.0).radius(4.0).fill(bg).child(text(name.clone()).color(Color::WHITE)))
    });
    let viewport = col().key("scroll").size(220.0, 150.0).scroll(ScrollSpec { stick_bottom: false }).child(list);
    el().size(240.0, 160.0).fill(Color { r: 0.05, g: 0.05, b: 0.08, a: 1.0 })
        .child(viewport)
        .child(el().absolute(200.0, 10.0).size(30.0, 30.0).radius(15.0).fill(Color { r: 0.8, g: 0.4, b: 0.1, a: 0.6 }))
}

fn same_list(a: &[Instance], b: &[Instance]) -> bool {
    bytemuck::cast_slice::<_, u8>(a) == bytemuck::cast_slice::<_, u8>(b)
}

fn read(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (texture.width(), texture.height());
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
}

fn target(gpu: &Gpu, (w, h): (u32, u32)) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None, size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST, view_formats: &[],
    })
}

#[test]
fn a_damage_tracked_frame_draws_what_a_full_frame_draws() {
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    let gpu = Gpu::headless();
    let scale = 2.0;
    let size = (480, 320);
    let mut full = Renderer::new(&gpu.device, FORMAT);
    let mut partial = Renderer::new(&gpu.device, FORMAT);
    let mut retained = Retained::new();
    // Two runtimes go through the same steps: one drawn whole, one damage tracked, each
    // uploading its own atlas rows.
    let runtime = || {
        let names = (0..14).map(|n| format!("Row number {n}")).collect();
        let mut rt = Runtime::new(Rows { names, hovered: None }, rows_view, (240.0, 160.0), Env::new(true, scale));
        rt.frame(0.0);
        rt
    };
    let (mut whole, mut rt) = (runtime(), runtime());
    type Step = Box<dyn Fn(&mut Runtime<Rows>)>;
    let update = |f: fn(&mut Rows)| -> Step { Box::new(move |r: &mut Runtime<Rows>| r.update(|s, _| f(s))) };
    let wheel = |dy: f32| -> Step { Box::new(move |r: &mut Runtime<Rows>| r.wheel(60.0, 60.0, dy)) };
    let steps: Vec<(&str, Step)> = vec![
        ("first", update(|_| {})),
        ("unchanged", update(|_| {})),
        ("hover", update(|s| s.hovered = Some(2))),
        ("hover moves", update(|s| s.hovered = Some(3))),
        ("text grows", update(|s| s.names[1].push_str(" ok"))),
        ("new glyphs", update(|s| s.names[5] = "Quiz jab 5".into())),
        ("row inserted", update(|s| s.names.insert(4, "Inserted".into()))),
        ("row removed", update(|s| { s.names.remove(0); })),
        ("scrolled", wheel(-7.0)),
        ("scrolled on", wheel(-7.0)),
        ("scrolled on more", wheel(-7.0)),
        ("scrolled back", wheel(7.0)),
    ];
    let mut redrawn = Vec::new();
    for (label, step) in steps {
        for r in [&mut whole, &mut rt] {
            step(r);
            r.frame(1.0 / 60.0);
        }
        assert!(same_list(&whole.instances, &rt.instances), "{label}: both runtimes build the same frame");
        let clear = rt.clear_color();
        let around = Surroundings { size, clear, srgb: false, scale, atlas_resets: rt.text.atlas.resets };
        retained.fit(&gpu.device, FORMAT, FORMAT, size);
        let texels = Texels::of(&rt.images);
        let damage = retained.damage(&rt.instances, around, &texels);
        let shift = retained.shift;
        full.prepare(&gpu.device, &gpu.queue, &mut whole.text.atlas, &whole.instances, size);
        partial.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, size);
        assert!(partial.damage_tracking_fits());
        let expected_target = target(&gpu, size);
        let got_target = target(&gpu, size);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        full.draw(&mut enc, &expected_target.create_view(&Default::default()), clear);
        retained.fit_fill(&gpu.device, &gpu.queue, clear);
        retained.blit(&gpu.device, &mut enc);
        partial.draw_damaged(&mut enc, retained.view().unwrap(), clear, &damage, retained.fill().unwrap());
        retained.copy_to(&mut enc, &got_target);
        full.before_submit(&mut enc);
        partial.submit(&gpu.queue, enc);
        retained.drawn(around, &damage);
        let (expected, got) = (read(&gpu, &expected_target), read(&gpu, &got_target));
        let differ = expected.chunks(4).zip(got.chunks(4)).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "{label}: {differ} pixels differ ({damage:?}, {shift:?})");
        if label.starts_with("scrolled") {
            assert!(shift.is_some_and(|s| s.dy != 0), "{label}: the kept pixels move ({damage:?})");
        }
        redrawn.push((label, damage.area(size) * 100 / u64::from(size.0 * size.1)));
    }
    let share = |l: &str| redrawn.iter().find(|(x, _)| *x == l).unwrap().1;
    assert_eq!(share("first"), 100);
    assert_eq!(share("unchanged"), 0);
    // A row is an eighth of this small window; the hover colour still eases while the
    // next steps run, so they redraw those rows too.
    assert!(share("hover") < 16, "one row: {redrawn:?}");
    for step in ["hover moves", "text grows", "new glyphs"] {
        assert!(share(step) < 30, "{step}, two rows: {redrawn:?}");
    }
    assert!(share("row inserted") < 70 && share("row removed") < 70, "the rows below: {redrawn:?}");
    for step in ["scrolled", "scrolled on", "scrolled on more", "scrolled back"] {
        // The strips, the overlay circle the move drags and the rows the pointer now hovers.
        assert!(share(step) < 65, "{step}: {redrawn:?}");
    }
}

#[test]
fn culled_quads_draw_what_whole_quads_draw() {
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    let gpu = Gpu::headless();
    let size = (480, 320);
    let names = (0..14).map(|n| format!("Row number {n}")).collect();
    let mut rt = Runtime::new(Rows { names, hovered: Some(1) }, rows_view, (240.0, 160.0), Env::new(true, 2.0));
    rt.frame(0.0);
    // The rows over panels nested three deep in the window's colour, one rounded, and a row
    // its clip cuts.
    let bg = [0.05, 0.05, 0.08, 1.0];
    let mut frame = vec![
        quad(0.0, 0.0, 480.0, 320.0, bg),
        Instance { radii: [16.0; 4], ..quad(10.0, 10.0, 460.0, 300.0, bg) },
        quad(40.0, 40.0, 400.0, 240.0, bg),
        Instance { clip: [100.0, 0.0, 300.0, 320.0], ..quad(0.0, 290.0, 480.0, 20.0, [0.3, 0.6, 0.2, 1.0]) },
    ];
    frame.extend_from_slice(&rt.instances);
    let mut out = Vec::new();
    super::super::cull::cull(&frame, size, &mut out);
    assert_eq!(out[1].uv, [0.0, 0.0, -1.0, 0.0], "{:?}", out[1].uv);
    assert_eq!(out[2].uv, [0.0, 0.0, -1.0, 0.0]);
    assert!(out[3].uv[2] < 0.0, "banded to its clip");
    let mut draw = |instances: &[Instance]| {
        let mut renderer = Renderer::new(&gpu.device, FORMAT);
        rt.text.atlas.invalidate();
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, instances, size);
        let texture = target(&gpu, size);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.draw(&mut enc, &texture.create_view(&Default::default()), [0.0, 0.0, 0.0, 1.0]);
        renderer.submit(&gpu.queue, enc);
        read(&gpu, &texture)
    };
    let (whole, culled) = (draw(&frame), draw(&out));
    let worst = whole.iter().zip(&culled).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 1, "the same pixels, to blending's rounding: off by {worst}");
}
