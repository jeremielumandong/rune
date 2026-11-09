use super::*;
use crate::ScrollBars;

/// The app's scroll bar mode, whether the viewport hides its bar, and rows pressed.
#[derive(Default)]
struct S {
    bars: Option<ScrollBars>,
    hidden: bool,
    rows: usize,
    pressed: Vec<usize>,
}

/// `rows` 20-point rows, each pressable, in a 200-point viewport.
fn view(s: &S, cx: &mut Cx<S>) -> El {
    if let Some(bars) = s.bars { cx.show_scroll_bars(bars); }
    let rows = col().key("rows").w_full().children((0..s.rows).map(|k| {
        el().key(format!("r{k}")).w_full().h(20.0).on_press(move |s: &mut S| s.pressed.push(k))
    }));
    let mut viewport = col().key("scroll").size(200.0, 200.0).scroll(crate::ScrollSpec { stick_bottom: false }).child(rows);
    if s.hidden { viewport = viewport.scroll_bar_hidden(); }
    col().size(200.0, 200.0).child(viewport)
}

fn runtime(bars: Option<ScrollBars>, rows: usize) -> Runtime<S> {
    let mut rt = Runtime::new(S { bars, rows, ..S::default() }, view, (200.0, 200.0), Env::new(false, 1.0));
    rt.frame(0.0);
    rt.frame(1.0 / 60.0);
    rt
}

/// The thumb drawn over the viewport's trailing edge, `[x, y, w, h]` in points, if any.
fn thumb(rt: &Runtime<S>) -> Option<[f32; 4]> {
    rt.instances.iter().find(|i| i.params[1] == KIND_RECT && (i.rect[0] - 192.0).abs() < 0.5 && (i.rect[2] - 6.0).abs() < 0.5).map(|i| i.rect)
}

fn offset(rt: &Runtime<S>) -> f64 {
    rt.scrolls.values().next().unwrap().offset
}

#[test]
fn always_keeps_a_thumb_sized_to_the_view_on_an_overflowing_viewport() {
    // 40 rows: 800 pt of content in a 200 pt view, so the 196 pt track's thumb is a quarter.
    let rt = runtime(Some(ScrollBars::Always), 40);
    let t = thumb(&rt).expect("a thumb at rest");
    assert_eq!(t[1], 2.0, "at the top of the track");
    assert!((t[3] - 49.0).abs() <= 1.0, "a quarter of the track: {t:?}");
    // Nothing overflows: no thumb.
    assert_eq!(thumb(&runtime(Some(ScrollBars::Always), 5)), None);
    // A viewport that hides its bar draws none.
    let mut rt = runtime(Some(ScrollBars::Always), 40);
    rt.update(|s, _| s.hidden = true);
    rt.frame(1.0 / 60.0);
    assert_eq!(thumb(&rt), None);
}

#[test]
fn when_scrolling_shows_the_thumb_while_it_moves_then_fades_it() {
    let mut rt = runtime(None, 40);
    assert_eq!(rt.overlay_scroll_bars(), true, "headless runs overlay");
    assert_eq!(thumb(&rt), None, "a still viewport shows no thumb");
    rt.wheel(100.0, 100.0, -100.0);
    rt.frame(1.0 / 60.0);
    let t = thumb(&rt).expect("shown while it scrolls");
    assert!(t[1] > 2.0, "it moved down the track: {t:?}");
    // It holds without drawing frames, then fades out.
    let mut drew = 0;
    for _ in 0..60 {
        if rt.frame(1.0 / 60.0) { drew += 1; }
    }
    assert!(thumb(&rt).is_some(), "still there 1 s on");
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    assert_eq!(thumb(&rt), None, "gone after the fade");
    assert!(drew < 30, "the hold sleeps: {drew} animated frames");
}

#[test]
fn dragging_the_thumb_scrolls_and_presses_nothing_under_it() {
    let mut rt = runtime(Some(ScrollBars::Always), 40);
    let t = thumb(&rt).unwrap();
    let (x, y) = (t[0] + 3.0, t[1] + 10.0);
    rt.pointer_down(x, y);
    rt.pointer_move(x, y + 49.0);
    rt.frame(1.0 / 60.0);
    // 147 pt of travel scrolls 600 pt of content: 49 pt is a third.
    assert!((offset(&rt) - 200.0).abs() < 3.0, "offset {}", offset(&rt));
    rt.pointer_move(x, y + 1000.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(offset(&rt), 600.0, "held to the end");
    rt.pointer_up(x, y + 1000.0);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.pressed.is_empty(), "no row pressed: {:?}", rt.state.pressed);
    let t = thumb(&rt).unwrap();
    assert!((t[1] + t[3] - 198.0).abs() <= 1.0, "at the bottom of the track: {t:?}");
    // A press beside the thumb still presses the row there.
    rt.pointer_down(50.0, 50.0);
    rt.pointer_up(50.0, 50.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.pressed, [32]);
}

#[test]
fn a_hidden_overlay_thumb_does_not_take_presses() {
    let mut rt = runtime(None, 40);
    assert_eq!(thumb(&rt), None);
    rt.pointer_down(195.0, 10.0);
    rt.pointer_up(195.0, 10.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.pressed, [0], "the row under the hidden thumb has the press");
}
