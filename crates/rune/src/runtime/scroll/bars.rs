//! Scroll bars: a thumb over each overflowing scroll viewport's trailing edge (and its bottom
//! edge when it scrolls sideways), drawn above its content, that a drag moves.
//!
//! How they show follows the window's [`ScrollBars`](crate::ScrollBars) mode: the app's
//! [`Cx::show_scroll_bars`] for the commit, else the system preference. Under an overlay mode
//! (`WhenScrolling`, or `Automatic` with a trackpad) a thumb appears while its viewport moves
//! by hand and fades out after a short rest; otherwise it stays while the content overflows.
//! `El::scroll_bar_hidden` leaves a viewport without one.
use super::*;

/// The thumb's thickness, and its distance from the viewport's edges (points).
const THICK: f32 = 6.0;
const INSET: f32 = 2.0;
/// The shortest a thumb gets (points), however long the content.
const MIN_LEN: f32 = 24.0;
/// A grab reaches this far past the thumb's sides (points): a 6 pt target is hard to hit.
const GRAB: f32 = 3.0;
/// Under an overlay mode, how long (seconds) a thumb stays after its viewport stops, and how
/// long it then takes to fade out.
const HOLD: f64 = 0.9;
const FADE: f64 = 0.3;
/// How much stronger a held thumb draws than one at rest.
const DRAG_BOOST: f32 = 1.6;

/// This commit's choices from [`Cx::show_scroll_bars`] and [`Cx::scroll_bar_color`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Prefs {
    pub(crate) bars: Option<crate::ScrollBars>,
    pub(crate) color: Option<Color>,
}

/// A viewport's last drawn offsets, and when (`Runtime::clock`) a hand last moved it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fade {
    offsets: [f64; 2],
    moved_at: f64,
}

/// A thumb drawn this frame, in window points, for a press to grab.
#[derive(Clone, Copy, Debug)]
pub(super) struct Hit {
    id: NodeId,
    horizontal: bool,
    /// The thumb grown by `GRAB` across, inside the clips around it: x0, y0, x1, y1.
    grab: [f32; 4],
    /// Content points one point of thumb travel scrolls.
    ratio: f64,
}

/// A thumb under a held press.
#[derive(Clone, Copy, Debug)]
pub(super) struct Drag {
    pub(super) id: NodeId,
    horizontal: bool,
    /// The pointer, along the bar's axis, and the offset, as the press began.
    from: f32,
    start: f64,
    ratio: f64,
}

/// A viewport whose subtree is still painting: its bars draw when it ends.
#[derive(Clone, Copy, Debug)]
pub(super) struct Open {
    pub(super) i: usize,
    /// The paint clip the viewport's content drew in (device pixels: x0, y0, x1, y1).
    pub(super) clip: [f32; 4],
    pub(super) opacity: f32,
}

/// A thumb `len` long on a track `track` long at `t` (0..=1) of the way along it: its start
/// from the track's and its length, in the track's units.
fn thumb(track: f32, view: f64, max: f64, t: f64) -> Option<(f32, f32)> {
    if track < MIN_LEN || view <= 0.0 || max <= 0.5 { return None; }
    let len = ((f64::from(track) * view / (view + max)) as f32).clamp(MIN_LEN, track);
    Some(((track - len) * t.clamp(0.0, 1.0) as f32, len))
}

/// The thumb's opacity (0..=1) at `age` seconds since its viewport last moved under an overlay
/// mode; under reduced motion it goes at once instead of fading.
fn shown(age: f64, reduced: bool) -> f32 {
    if age < HOLD { 1.0 } else if reduced || age >= HOLD + FADE { 0.0 } else { (1.0 - (age - HOLD) / FADE) as f32 }
}

impl<S: 'static> Runtime<S> {
    /// The mode this window draws its bars in: the app's for this commit, else the system's.
    pub(super) fn drawn_scroll_bars(&self) -> crate::platform::scroll_bars::SystemScrollBars {
        self.scroll_bars.state.with_override(self.scroll_bars.prefs.bars)
    }

    /// Starts a frame's bars: last frame's grabs go, and nothing fades yet.
    pub(super) fn begin_scroll_bars(&mut self) {
        self.bar_hits.clear();
        self.bars_fading = false;
    }

    /// A hand scrolled viewport `id` (the wheel, keys, a touch): its overlay thumb shows.
    pub(super) fn poke_scroll_bar(&mut self, id: NodeId) {
        if !self.bar_pokes.contains(&id) { self.bar_pokes.push(id); }
    }

    /// Ends a frame's bars: forgets viewports that went, and the hands that moved them.
    pub(super) fn end_scroll_bars(&mut self) {
        self.bar_pokes.clear();
        if !self.bar_fades.is_empty() {
            let index = &self.node_index;
            self.bar_fades.retain(|id, _| index.contains_key(id));
        }
        if self.bar_drag.is_some_and(|d| !self.node_index.contains_key(&d.id)) { self.bar_drag = None; }
    }

    /// Draws the bars of viewport `open.i`, whose content just finished painting, above it.
    pub(super) fn paint_scroll_bars(&mut self, open: Open) {
        let i = open.i;
        let n = &self.nodes[i];
        let id = n.id;
        let Some(st) = self.scrolls.get(&id) else { return };
        let (ymax, xmax) = (if n.scroll_axes.y() { st.max } else { 0.0 }, if n.scroll_axes.x() { st.x.max } else { 0.0 });
        let (vert, horiz) = (ymax > 0.5, xmax > 0.5);
        if !vert && !horiz { return; }
        let offsets = [st.x.offset, st.offset];
        let (view_w, view_h) = (f64::from(n.local.w), f64::from(n.local.h));
        // Only a hand shows an overlay thumb (the wheel, a trackpad, keys, a touch, a drag, and
        // the momentum they leave): an app's scroll, a glide or a viewport following its growing
        // end moves on its own.
        let poked = self.bar_pokes.contains(&id);
        let coasting = st.v.abs() > 1.0 || st.x.v.abs() > 1.0;
        let dragging = self.bar_drag.filter(|d| d.id == id).map(|d| d.horizontal);
        let now = self.clock;
        let fade = self.bar_fades.entry(id).or_insert(Fade { offsets, moved_at: f64::NEG_INFINITY });
        let moved = (offsets[0] - fade.offsets[0]).abs() > 0.25 || (offsets[1] - fade.offsets[1]).abs() > 0.25;
        if poked || dragging.is_some() || (moved && coasting) { fade.moved_at = now; }
        fade.offsets = offsets;
        let moved_at = fade.moved_at;
        let alpha = if self.drawn_scroll_bars().is_overlay() {
            let age = now - moved_at;
            let alpha = shown(age, self.reduced_motion);
            if alpha > 0.0 {
                if age < HOLD {
                    // Sleep through the hold; wake to fade.
                    let wake = moved_at + HOLD;
                    let at = self.t + (wake - now).max(0.0) * f64::from(self.time_scale);
                    if self.loop_wake.is_none_or(|w| wake < w.0) { self.loop_wake = Some((wake, at)); }
                } else {
                    self.bars_fading = true;
                }
            }
            alpha
        } else { 1.0 };
        if alpha <= 0.004 || open.opacity <= 0.001 { return; }

        let s = self.env.scale;
        let [x, y, w, h] = self.drawn[i];
        let base = self.scroll_bars.prefs.color.unwrap_or(if self.env.system_dark {
            Color { r: 1.0, g: 1.0, b: 1.0, a: 0.4 }
        } else {
            Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 }
        });
        let corner = (INSET + THICK) * f32::from(u8::from(vert && horiz));
        let clip_pt = open.clip.map(|v| v / s);
        let srgb = self.srgb_blend;
        for horizontal in [false, true] {
            let (on, max, view, offset) = if horizontal { (horiz, xmax, view_w, offsets[0]) } else { (vert, ymax, view_h, offsets[1]) };
            if !on { continue; }
            let track = if horizontal { w } else { h } - 2.0 * INSET - corner;
            let Some((at, len)) = thumb(track, view, max, offset / max) else { continue };
            // Points: x0, y0, x1, y1.
            let rect = if horizontal {
                [x + INSET + at, y + h - INSET - THICK, x + INSET + at + len, y + h - INSET]
            } else {
                [x + w - INSET - THICK, y + INSET + at, x + w - INSET, y + INSET + at + len]
            };
            let held = dragging == Some(horizontal);
            let a = (base.a * if held { DRAG_BOOST } else { 1.0 }).min(1.0) * alpha * open.opacity;
            let px = rect.map(|v| (v * s).round());
            self.instances.push(Instance {
                rect: [px[0], px[1], px[2] - px[0], px[3] - px[1]],
                radii: [THICK * 0.5 * s; 4],
                fill: premul(Color { a: 1.0, ..base }, a, srgb),
                border: [0.0; 4],
                params: [0.0, KIND_RECT, 0.0, 1.0],
                uv: [0.0; 4],
                clip: open.clip,
                fade: crate::render::pack_fade([0.0; 4], open.clip, open.clip),
            });
            let grab = if horizontal {
                [rect[0], rect[1] - GRAB, rect[2], rect[3] + INSET]
            } else {
                [rect[0] - GRAB, rect[1], rect[2] + INSET, rect[3]]
            };
            let grab = [grab[0].max(clip_pt[0]), grab[1].max(clip_pt[1]), grab[2].min(clip_pt[2]), grab[3].min(clip_pt[3])];
            if grab[0] < grab[2] && grab[1] < grab[3] {
                let travel = f64::from((track - len).max(1.0));
                self.bar_hits.push(Hit { id, horizontal, grab, ratio: max / travel });
            }
        }
        if self.track_marks { self.marks.resize(self.instances.len(), 0); }
    }

    /// A primary press at (x, y) on a thumb drawn there grabs it, unless something drawn over
    /// the viewport (a popover, a menu) takes the press. Returns whether it did.
    pub(super) fn grab_scroll_bar(&mut self, x: f32, y: f32, target: Option<usize>) -> bool {
        let Some(hit) = self.bar_hits.iter().rev().find(|h| contains(h.grab, x, y)).copied() else { return false };
        let Some(&vi) = self.node_index.get(&hit.id) else { return false };
        if target.is_some_and(|t| t != vi && !std::iter::successors(self.nodes[t].parent, |&p| self.nodes[p].parent).any(|p| p == vi)) {
            return false;
        }
        let Some(st) = self.scrolls.get_mut(&hit.id) else { return false };
        let start = if hit.horizontal {
            st.x.v = 0.0;
            st.x.glide = None;
            st.x.offset
        } else {
            st.v = 0.0;
            st.glide = None;
            st.glide_to = None;
            st.offset
        };
        st.direct = true;
        self.bar_drag = Some(Drag { id: hit.id, horizontal: hit.horizontal, from: if hit.horizontal { x } else { y }, start, ratio: hit.ratio });
        log_event!(self, "scroll bar grab");
        true
    }

    /// The held thumb follows the pointer at (x, y), its content with it.
    pub(super) fn drag_scroll_bar(&mut self, x: f32, y: f32) {
        let Some(d) = self.bar_drag else { return };
        let stick = self.node_index.get(&d.id).and_then(|&i| self.nodes[i].scroll).is_some_and(|s| s.stick_bottom);
        let Some(st) = self.scrolls.get_mut(&d.id) else { return };
        let to = d.start + f64::from((if d.horizontal { x } else { y }) - d.from) * d.ratio;
        if d.horizontal {
            st.x.offset = to.clamp(0.0, st.x.max);
            st.x.v = 0.0;
        } else {
            st.offset = to.clamp(0.0, st.max);
            st.v = 0.0;
            st.pinned = stick && st.offset >= st.max - 0.5;
        }
        st.direct = true;
    }

    /// Lets go of a held thumb; returns whether one was held.
    pub(super) fn release_scroll_bar(&mut self) -> bool {
        self.bar_drag.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumb_is_as_long_as_the_view_is_of_the_content_and_travels_its_track() {
        // A 100 pt view over 400 pt of content: a quarter of the track.
        assert_eq!(thumb(200.0, 100.0, 300.0, 0.0), Some((0.0, 50.0)));
        assert_eq!(thumb(200.0, 100.0, 300.0, 1.0), Some((150.0, 50.0)));
        assert_eq!(thumb(200.0, 100.0, 300.0, 0.5), Some((75.0, 50.0)));
        // Very long content still leaves a thumb you can grab.
        assert_eq!(thumb(200.0, 100.0, 1e6, 0.0).map(|t| t.1), Some(MIN_LEN));
        // No overflow, or no room for a thumb: none.
        assert_eq!(thumb(200.0, 100.0, 0.0, 0.0), None);
        assert_eq!(thumb(10.0, 100.0, 300.0, 0.0), None);
    }

    #[test]
    fn an_overlay_thumb_holds_then_fades_or_goes_at_once_under_reduced_motion() {
        assert_eq!(shown(0.0, false), 1.0);
        assert_eq!(shown(HOLD - 0.01, false), 1.0);
        assert!((shown(HOLD + FADE * 0.5, false) - 0.5).abs() < 1e-4);
        assert_eq!(shown(HOLD + FADE, false), 0.0);
        assert_eq!(shown(HOLD + 0.01, true), 0.0);
        assert_eq!(shown(f64::INFINITY, false), 0.0);
    }
}
