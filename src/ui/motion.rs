//! Motion: the one place the app says how a thing moves.
//!
//! **The strip is why this exists.** Columns are absolutely positioned and the
//! scroll offset was applied the instant it changed, so focusing the column
//! next door teleported the whole strip sideways -- the picture before and the
//! picture after were both correct and nothing joined them, which left you
//! working out where you had just gone. A scroll you can *watch* is the whole
//! point of a strip that is longer than the window: the motion is what says
//! which way it went and how far.
//!
//! Three rules kept everything here small:
//!
//! * **Nothing animates state, only the drawing of it.** `Glide` holds a
//!   target that the rest of the app sets and reads as if it were a plain
//!   number; only `sample` -- called while painting -- lags behind it. So
//!   layout maths, the session file and every invariant go on working in
//!   final values, and switching animation off removes the lag and nothing
//!   else.
//! * **Retargeting starts from where the eye is**, not from where the last
//!   leg began. Pressing the focus key four times quickly is four new legs
//!   from four intermediate positions, which reads as one accelerating slide
//!   rather than four fights over the same pixels.
//! * **Frames are asked for one at a time**, the same loop the home screen
//!   runs: whatever is animating calls `request_animation_frame` while it
//!   still is, and the window goes back to painting only on change the moment
//!   nothing does. There is no timer and no global ticker.
//!
//! The user owns all three dials (Settings -> Window -> Motion): off, how
//! long, and which curve. Off is a real setting and not a degraded mode --
//! `Motion::disabled` makes every `sample` return its target, which is exactly
//! the behaviour this module replaced.

use std::time::{Duration, Instant};

use gpui::App;

use crate::store::Settings;

/// How the app is moving right now, read once per frame rather than per
/// animated value -- a render that glides a scroll offset and eight column
/// widths should look up the settings once.
#[derive(Clone, Copy)]
pub struct Motion {
    pub enabled: bool,
    pub duration: Duration,
    pub curve: Curve,
}

impl Motion {
    pub fn from_settings(cx: &App) -> Self {
        let settings = cx.global::<Settings>();
        Self {
            enabled: settings.animations && settings.animation_ms >= 1.,
            duration: Duration::from_secs_f32(settings.animation_ms / 1000.),
            curve: Curve::from_name(&settings.animation_curve),
        }
    }
}

/// The shape of a move. Three, because a fourth would be a preference nobody
/// can describe: one that does not ease, the one that does, and one that
/// overshoots.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Curve {
    /// No easing at all. Honest about distance, which is why it reads as
    /// mechanical: nothing in the physical world starts and stops at speed.
    Linear,
    /// Out-cubic. Fast off the mark and settling at the end, so the motion has
    /// said where it is going by the time you have finished reading it.
    Ease,
    /// Out-back: it passes the target by a little and comes back. The
    /// overshoot is small on purpose -- enough to feel sprung, not so much
    /// that a column visibly leaves the viewport and returns.
    Spring,
}

impl Curve {
    pub fn from_name(name: &str) -> Self {
        match name {
            "linear" => Curve::Linear,
            "spring" => Curve::Spring,
            _ => Curve::Ease,
        }
    }

    /// Progress 0..1 in, eased progress out. Only `Spring` ever returns more
    /// than 1, and only in the middle of its travel.
    fn at(self, t: f32) -> f32 {
        let t = t.clamp(0., 1.);
        match self {
            Curve::Linear => t,
            Curve::Ease => 1. - (1. - t).powi(3),
            Curve::Spring => {
                // The usual out-back constants, halved. At full strength the
                // overshoot is about 10% of the distance, which on a
                // full-width column is most of a neighbour sliding in and out
                // again; half of that is a settle rather than a bounce.
                const C1: f32 = 0.85;
                const C3: f32 = C1 + 1.;
                let u = t - 1.;
                1. + C3 * u.powi(3) + C1 * u.powi(2)
            }
        }
    }
}

/// A number whose drawing lags behind its value.
///
/// The target is set by ordinary code that knows nothing about time;
/// `sample` is called while painting and returns where the number has got to.
/// Both are plain `f32` -- a `Glide` is not a unit, so it carries pixels,
/// shares and opacities equally.
pub struct Glide {
    shown: f32,
    from: f32,
    target: f32,
    /// `None` once the glide has arrived, which is also what makes `running`
    /// cheap enough to ask every frame.
    since: Option<Instant>,
}

impl Glide {
    pub fn new(value: f32) -> Self {
        Self {
            shown: value,
            from: value,
            target: value,
            since: None,
        }
    }

    /// Moves without animating: a restored layout, a window resize, a seam
    /// under the cursor. **Anything the user is dragging must jump**, or the
    /// thing they are holding trails behind the pointer.
    pub fn jump(&mut self, value: f32) {
        self.shown = value;
        self.from = value;
        self.target = value;
        self.since = None;
    }

    /// Aims at a new value. A no-op when it is already the target, so this is
    /// safe to call every frame with a value that usually has not changed --
    /// which is how the strip keeps column widths in step with the viewport.
    pub fn to(&mut self, value: f32, motion: &Motion) {
        if (value - self.target).abs() < 0.5 {
            return;
        }
        if !motion.enabled {
            self.jump(value);
            return;
        }
        // From where the eye is, not from where the last leg started.
        self.from = self.shown;
        self.target = value;
        self.since = Some(Instant::now());
    }

    /// Where it was last drawn. For code that has to agree with the picture
    /// rather than with the layout -- a seam drag measures against the edge
    /// the user can see, not the one it is sliding towards.
    pub fn shown(&self) -> f32 {
        self.shown
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    /// Advances to now and returns what to draw. Call once per frame, while
    /// painting -- it is the only thing here that reads the clock.
    pub fn sample(&mut self, motion: &Motion) -> f32 {
        let Some(since) = self.since else {
            return self.shown;
        };
        if !motion.enabled || motion.duration.is_zero() {
            self.jump(self.target);
            return self.shown;
        }
        let t = since.elapsed().as_secs_f32() / motion.duration.as_secs_f32();
        if t >= 1. {
            self.jump(self.target);
            return self.shown;
        }
        self.shown = self.from + (self.target - self.from) * motion.curve.at(t);
        self.shown
    }
}

/// A one-way fade-in, for something that was not on screen a moment ago.
///
/// Separate from `Glide` because it is a different question: a glide is told
/// where to go and may be retold, an entrance only ever runs once and only
/// ever forwards. Overlays use it so the command bar and the overview arrive
/// instead of replacing the window between two frames.
pub struct Entrance {
    since: Option<Instant>,
}

impl Entrance {
    pub fn new() -> Self {
        Self { since: None }
    }

    /// Starts over. Called when the thing is shown, not when it is built: the
    /// command bar is built once at launch and shown many times.
    pub fn restart(&mut self) {
        self.since = Some(Instant::now());
    }

    /// 0 at the start, 1 once it is over, and 1 immediately when motion is
    /// off -- so a page that multiplies by this is correct either way without
    /// asking whether animations are on.
    pub fn progress(&mut self, motion: &Motion) -> f32 {
        if !motion.enabled || motion.duration.is_zero() {
            self.since = None;
            return 1.;
        }
        let Some(since) = self.since else {
            return 1.;
        };
        let t = since.elapsed().as_secs_f32() / motion.duration.as_secs_f32();
        if t >= 1. {
            self.since = None;
            return 1.;
        }
        motion.curve.at(t).clamp(0., 1.)
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }
}

impl Default for Entrance {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion() -> Motion {
        Motion {
            enabled: true,
            duration: Duration::from_millis(200),
            curve: Curve::Ease,
        }
    }

    fn off() -> Motion {
        Motion {
            enabled: false,
            ..motion()
        }
    }

    /// Every curve starts where it starts and ends where it ends. Only the
    /// middle is allowed to be a matter of taste -- and only `Spring` is
    /// allowed past the end.
    #[test]
    fn curves_begin_and_end_on_their_targets() {
        for curve in [Curve::Linear, Curve::Ease, Curve::Spring] {
            assert_eq!(curve.at(0.), 0., "{curve:?} starts at 0");
            assert_eq!(curve.at(1.), 1., "{curve:?} ends at 1");
            // Out of range is clamped rather than extrapolated: a dropped
            // frame must not throw the value past its target.
            assert_eq!(curve.at(-1.), 0.);
            assert_eq!(curve.at(9.), 1.);
        }
        assert!(Curve::Spring.at(0.8) > 1., "spring overshoots");
        assert!(Curve::Ease.at(0.8) < 1., "ease does not");
    }

    /// With motion off a glide is a plain number, which is the whole promise
    /// that lets the rest of the app ignore this module.
    #[test]
    fn disabled_motion_lands_immediately() {
        let off = off();
        let mut glide = Glide::new(0.);
        glide.to(400., &off);
        assert_eq!(glide.sample(&off), 400.);
        assert!(!glide.running());

        // And an entrance is simply over, so a page that multiplies by it
        // does not have to ask whether animations are on.
        let mut entrance = Entrance::new();
        entrance.restart();
        assert_eq!(entrance.progress(&off), 1.);
        assert!(!entrance.running());
    }

    /// A new target picks up from where the drawing had reached, so a second
    /// keypress mid-slide does not snap back to where the first one started.
    #[test]
    fn retargeting_starts_from_the_shown_value() {
        let motion = motion();
        let mut glide = Glide::new(0.);
        glide.to(100., &motion);
        glide.shown = 40.;
        glide.to(200., &motion);
        assert_eq!(glide.from, 40.);
        assert_eq!(glide.target, 200.);
    }

    /// Re-aiming at the target you already have is a no-op -- the strip calls
    /// `to` for every column every frame, and a restart each time would hold
    /// everything permanently at the start of a glide.
    #[test]
    fn aiming_at_the_same_target_does_not_restart() {
        let motion = motion();
        let mut glide = Glide::new(0.);
        glide.to(300., &motion);
        let started = glide.since;
        glide.to(300.2, &motion);
        assert_eq!(glide.since, started);
        assert_eq!(glide.target, 300.);
    }

    #[test]
    fn a_jump_is_not_running() {
        let mut glide = Glide::new(0.);
        glide.to(100., &motion());
        assert!(glide.running());
        glide.jump(10.);
        assert!(!glide.running());
        assert_eq!(glide.sample(&motion()), 10.);
    }
}
