//! Low-power rendering (AgentOps patch, PARITY-PATCH.md): for a window drawn by a software
//! adapter (WARP, llvmpipe/lavapipe) or shown over Remote Desktop, where every drawn pixel
//! costs CPU. Two process-wide switches, both off by default, set by the app before its window
//! opens:
//!
//! - [`set_low_power_rendering`] paces a window's frames: a frame starts no sooner than
//!   [`FRAME_INTERVAL`] after the last one started (30 Hz), nor sooner than that frame's cost
//!   (build to present, at most [`MAX_FRAME_COST`]) after it was presented. Input, timers and
//!   changes that arrive meanwhile are drawn together in that next frame. On Windows the
//!   window also stops re-presenting unchanged frames for a second after a change.
//! - [`set_software_rendering_effects`] leaves blurred shadows (drop and inset) and backdrop
//!   blur out of the painted frame; crisp (zero-blur) shadows, which draw as rings or solid
//!   copies, stay.
//!
//! Low power also redraws only what changed (`render/damage.rs`): the window keeps its last
//! frame in a texture, shades again only the rectangles whose instances changed, and copies
//! that texture to the drawable. [`redrawn_pixels`] counts what it shaded.
//!
//! A resize the user drags still draws at once, and nothing changes the surface's present mode.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static LOW_POWER: AtomicBool = AtomicBool::new(false);
static SOFTWARE_EFFECTS: AtomicBool = AtomicBool::new(false);

/// The shortest time between two frames' starts in low-power mode: 30 Hz.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// The longest a frame's own cost delays the next one in low-power mode.
pub const MAX_FRAME_COST: Duration = Duration::from_millis(100);

/// Turns frame pacing on or off for every window.
pub fn set_low_power_rendering(on: bool) {
    LOW_POWER.store(on, Ordering::Relaxed);
}

/// Whether windows pace their frames.
pub fn low_power_rendering() -> bool {
    LOW_POWER.load(Ordering::Relaxed)
}

/// Leaves blurred shadows and backdrop blur out of every frame painted from now on.
pub fn set_software_rendering_effects(on: bool) {
    SOFTWARE_EFFECTS.store(on, Ordering::Relaxed);
}

/// Whether blurred shadows and backdrop blur are left out.
pub fn software_rendering_effects() -> bool {
    SOFTWARE_EFFECTS.load(Ordering::Relaxed)
}

static REDRAWN: AtomicU64 = AtomicU64::new(0);
static WINDOW_PIXELS: AtomicU64 = AtomicU64::new(0);
static QUAD_PIXELS: AtomicU64 = AtomicU64::new(0);

/// Pixel counts of low-power windows' frames since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RedrawnPixels {
    /// The window pixels redrawn (scissored), after damage tracking.
    pub shaded: u64,
    /// The window pixels the frames covered: what full redraws would have shaded.
    pub window: u64,
    /// The pixels the frames' quads rasterize, summed over their quads whatever their clips
    /// show: what full redraws run the shader on (each pixel once per quad over it).
    pub quads: u64,
}

pub fn redrawn_pixels() -> RedrawnPixels {
    RedrawnPixels { shaded: REDRAWN.load(Ordering::Relaxed), window: WINDOW_PIXELS.load(Ordering::Relaxed),
        quads: QUAD_PIXELS.load(Ordering::Relaxed) }
}

pub(crate) fn count_redrawn(shaded: u64, window: u64, quads: u64) {
    REDRAWN.fetch_add(shaded, Ordering::Relaxed);
    WINDOW_PIXELS.fetch_add(window, Ordering::Relaxed);
    QUAD_PIXELS.fetch_add(quads, Ordering::Relaxed);
}

/// When the frame after one that started at `started` and was presented at `presented` may
/// start.
pub fn next_frame_at(started: Instant, presented: Instant) -> Instant {
    let cost = presented.saturating_duration_since(started).min(MAX_FRAME_COST);
    (started + FRAME_INTERVAL).max(presented + cost)
}

/// When a shown window's next frame may start in low-power mode, after one that started at
/// `started` and finished building, or presenting, at `finished`; `None` when not paced.
/// Frames that build and present nothing new (on-demand frames, `FrameOutcome::Unchanged`)
/// are paced too: a stream of wakes or window events that change nothing (an app waking its
/// loop hundreds of times a second, a platform echoing each frame's IME or cursor update)
/// builds at most one frame a pace instead of one each.
pub(crate) fn pace_after(shown: bool, started: Instant, finished: Instant) -> Option<Instant> {
    (shown && low_power_rendering()).then(|| next_frame_at(started, finished))
}

/// What woke a window's loop or asked it for a frame, counted while low power is on, for an
/// app's frame log ([`take_wake_causes`]): what keeps a resting window building frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum WakeCause {
    /// `AppHandle::update`, `wake` or a waker (`WakeEvent::App`).
    App,
    /// A live value (`WakeEvent::Live`).
    Live,
    /// Another `WakeEvent`: a window request, focus, a frame change, accessibility.
    OtherWake,
    /// A due timer (`Tx::wake_after`).
    Timer,
    /// A paced or missed frame's retry.
    Retry,
    /// `RedrawRequested` from the platform.
    Redraw,
    /// Pointer motion, presses and wheel.
    Pointer,
    /// Keys and modifiers.
    Key,
    /// Input method events.
    Ime,
    /// Focus, occlusion, move, resize, scale, theme.
    Window,
    /// Any other window event.
    OtherEvent,
}

const CAUSES: [&str; 11] = ["app", "live", "other wake", "timer", "retry", "redraw", "pointer", "key", "ime", "window", "other event"];

static WAKES: [AtomicU64; 11] = [const { AtomicU64::new(0) }; 11];

pub(crate) fn count_wake(cause: WakeCause) {
    if low_power_rendering() { WAKES[cause as usize].fetch_add(1, Ordering::Relaxed); }
}

/// The causes counted since the last call, most first, leaving out those that didn't happen.
pub fn take_wake_causes() -> Vec<(&'static str, u64)> {
    let mut out: Vec<_> = CAUSES.iter().zip(&WAKES).map(|(name, n)| (*name, n.swap(0, Ordering::Relaxed))).filter(|(_, n)| *n > 0).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// The interval a timer that asks for every frame steps at with no display link: `default`,
/// or [`FRAME_INTERVAL`] in low-power mode.
pub(crate) fn timer_frame(default: Duration) -> Duration {
    timer_frame_in(low_power_rendering(), default)
}

fn timer_frame_in(low_power: bool, default: Duration) -> Duration {
    if low_power { default.max(FRAME_INTERVAL) } else { default }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cheap_frame_waits_out_the_interval() {
        let start = Instant::now();
        let presented = start + Duration::from_millis(5);
        assert_eq!(next_frame_at(start, presented), start + FRAME_INTERVAL);
    }

    #[test]
    fn a_costly_frame_waits_its_own_cost_after_presenting() {
        let start = Instant::now();
        let presented = start + Duration::from_millis(40);
        assert_eq!(next_frame_at(start, presented), presented + Duration::from_millis(40));
    }

    #[test]
    fn the_cost_a_frame_waits_is_capped() {
        let start = Instant::now();
        let presented = start + Duration::from_millis(400);
        assert_eq!(next_frame_at(start, presented), presented + MAX_FRAME_COST);
    }

    /// 350 requests a second for frames that change nothing, as a WARP window saw: each build
    /// paces the next, so about 30 build.
    #[test]
    fn frames_that_change_nothing_build_at_the_pace() {
        let start = Instant::now();
        let (mut builds, mut paced_until) = (0, None::<Instant>);
        for n in 0..350u32 {
            let now = start + Duration::from_micros(u64::from(n) * 2_857);
            if paced_until.is_some_and(|at| at > now) { continue; }
            builds += 1;
            // A 3 ms build, then the pace `pace_after` sets in low power.
            paced_until = Some(next_frame_at(now, now + Duration::from_millis(3)));
        }
        assert!((29..=31).contains(&builds), "{builds} builds");
    }

    #[test]
    fn timers_step_at_the_interval_only_in_low_power_mode() {
        let frame = Duration::from_micros(16_667);
        assert_eq!(timer_frame_in(false, frame), frame);
        assert_eq!(timer_frame_in(true, frame), FRAME_INTERVAL);
        assert_eq!(timer_frame_in(true, Duration::from_millis(50)), Duration::from_millis(50));
    }

    // The switches themselves are left alone here: other tests paint and pace in parallel.
    #[test]
    fn both_switches_start_off() {
        assert!(!low_power_rendering());
        assert!(!software_rendering_effects());
    }
}
