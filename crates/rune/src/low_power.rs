//! Low-power rendering: for a window drawn by a software
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
//! A resize the user drags still draws at once, and nothing changes the surface's present mode.
use std::sync::atomic::{AtomicBool, Ordering};
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

/// When the frame after one that started at `started` and was presented at `presented` may
/// start.
pub fn next_frame_at(started: Instant, presented: Instant) -> Instant {
    let cost = presented.saturating_duration_since(started).min(MAX_FRAME_COST);
    (started + FRAME_INTERVAL).max(presented + cost)
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
