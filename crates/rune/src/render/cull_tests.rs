use super::*;

const SIZE: (u32, u32) = (200, 100);
const BG: [f32; 4] = [0.05, 0.05, 0.06, 1.0];

fn quad(x: f32, y: f32, w: f32, h: f32, fill: [f32; 4]) -> Instance {
    Instance { rect: [x, y, w, h], fill, clip: NO_CLIP, params: [0.0, KIND_RECT, 0.0, 0.0], ..Instance::zeroed() }
}

fn culled(frame: &[Instance]) -> Vec<Instance> {
    let mut out = Vec::new();
    cull(frame, SIZE, &mut out);
    assert_eq!(out.len(), frame.len(), "the count and order stay");
    out
}

#[test]
fn a_panel_the_colour_of_the_one_under_it_draws_nothing() {
    let frame = [quad(0.0, 0.0, 200.0, 100.0, BG), quad(20.0, 10.0, 100.0, 50.0, BG), quad(30.0, 20.0, 10.0, 10.0, [1.0; 4])];
    let out = culled(&frame);
    assert_eq!(out[0].uv, frame[0].uv);
    assert_eq!(out[1].uv, EMPTY, "its pixels already are its colour");
    assert_eq!(out[2].uv, frame[2].uv, "what draws over them still draws");
}

#[test]
fn a_panel_over_something_else_or_another_colour_still_draws() {
    // Partly outside the panel, so it still shows.
    let between = quad(15.0, 5.0, 20.0, 20.0, [1.0; 4]);
    let frame = [quad(0.0, 0.0, 200.0, 100.0, BG), between, quad(20.0, 10.0, 100.0, 50.0, BG)];
    assert_eq!(culled(&frame)[2].uv, frame[2].uv, "a quad between them changed some of its pixels");
    // Away from what came between, it is still covered.
    let frame = [quad(0.0, 0.0, 200.0, 100.0, BG), between, quad(60.0, 10.0, 100.0, 50.0, BG)];
    assert_eq!(culled(&frame)[2].uv, EMPTY);
    let frame = [quad(0.0, 0.0, 200.0, 100.0, BG), quad(20.0, 10.0, 100.0, 50.0, [0.1, 0.1, 0.1, 1.0])];
    assert_eq!(culled(&frame)[1].uv, frame[1].uv, "another colour");
    let translucent = [0.05, 0.05, 0.06, 0.5];
    let frame = [quad(0.0, 0.0, 200.0, 100.0, translucent), quad(20.0, 10.0, 100.0, 50.0, translucent)];
    assert_eq!(culled(&frame)[1].uv, frame[1].uv, "translucent fills add up");
    // The rounded corners of the one under it don't cover its corners.
    let frame = [Instance { radii: [12.0; 4], ..quad(0.0, 0.0, 200.0, 100.0, BG) }, quad(0.0, 0.0, 30.0, 30.0, BG)];
    assert_eq!(culled(&frame)[1].uv, frame[1].uv);
    let frame = [Instance { radii: [12.0; 4], ..quad(0.0, 0.0, 200.0, 100.0, BG) }, quad(20.0, 20.0, 30.0, 30.0, BG)];
    assert_eq!(culled(&frame)[1].uv, EMPTY);
}

#[test]
fn a_quad_its_clip_cuts_rasterizes_only_the_clip() {
    let row = Instance { clip: [50.0, 0.0, 100.0, 100.0], ..quad(0.0, 10.0, 200.0, 20.0, [1.0; 4]) };
    let out = culled(&[row]);
    // Its rect grown a pixel for antialiasing, cut to the clip a pixel out.
    assert_eq!(out[0].uv, [49.0, -1.0, -101.0, 21.0]);
    assert_eq!(out[0].rect, row.rect);
    let inside = Instance { clip: [0.0, 0.0, 200.0, 100.0], ..quad(10.0, 10.0, 20.0, 20.0, [1.0; 4]) };
    assert_eq!(culled(&[inside])[0].uv, inside.uv, "a quad its clip doesn't cut is left whole");
    let gone = Instance { clip: [150.0, 0.0, 200.0, 100.0], ..quad(10.0, 10.0, 20.0, 20.0, [1.0; 4]) };
    assert_eq!(culled(&[gone])[0].uv, EMPTY, "nothing of it shows");
}

#[test]
fn a_quad_later_opaque_fills_cover_draws_nothing() {
    let card = Instance { radii: [6.0; 4], ..quad(20.0, 30.0, 100.0, 40.0, [0.9, 0.1, 0.1, 1.0]) };
    let label = Instance { params: [0.0, KIND_GLYPH, 0.0, 1.0], ..quad(30.0, 40.0, 8.0, 10.0, [1.0; 4]) };
    // A toolbar and a body that meet at y = 50 cover both between them.
    let toolbar = quad(0.0, 0.0, 200.0, 50.0, [0.2, 0.2, 0.2, 1.0]);
    let body = quad(0.0, 50.0, 200.0, 50.0, [0.1, 0.1, 0.1, 1.0]);
    let out = culled(&[card, label, toolbar, body]);
    assert_eq!(out[0].uv, EMPTY, "covered by the two together");
    assert_eq!(out[1].uv, EMPTY);
    assert_eq!(out[2].uv, toolbar.uv);
    // A translucent body or a rounded one leaves what's under it showing.
    let out = culled(&[card, toolbar, Instance { fill: [0.1, 0.1, 0.1, 0.9], ..body }]);
    assert_eq!(out[0].uv, card.uv);
    let corner = quad(2.0, 2.0, 20.0, 20.0, [0.9, 0.1, 0.1, 1.0]);
    let out = culled(&[corner, Instance { radii: [30.0; 4], ..quad(0.0, 0.0, 200.0, 100.0, [0.1, 0.1, 0.1, 1.0]) }]);
    assert_eq!(out[0].uv, corner.uv, "its corner pixels show under the rounded corner");
    // Inside a layer, nothing is taken for covered or covering.
    let layer = Instance::begin_layer([0.0, 0.0, 200.0, 100.0], NO_CLIP, LayerSpec { opacity: 0.5, ..Default::default() });
    let out = culled(&[card, layer, quad(0.0, 0.0, 200.0, 100.0, [0.1, 0.1, 0.1, 1.0]), Instance::end_layer()]);
    assert_eq!(out[0].uv, card.uv);
}
