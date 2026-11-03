//! The scrollable strip: one unbounded row of columns, niri-style.
//!
//! This replaces the nested split tree. There is no tree any more and no
//! recursion: a workspace is a `Vec<Column>`, a column is a `Pane`, and the
//! windows stacked inside a column are that pane's items -- all of them
//! visible at once rather than one in front behind tabs.
//!
//! **Opening a window never resizes what you are looking at.** A new column is
//! placed to the right of the focus and the strip scrolls to it; the columns
//! already on screen keep the width they had. That is the whole difference
//! from a split tree, where every new pane steals space from its neighbours.
//!
//! The strip is wider than the window on purpose. `scroll` is how far into it
//! the viewport has been moved, in pixels, and the one invariant that matters
//! is that the focused column is always fully inside the viewport --
//! `scroll_to_focus` enforces it during layout, because only then is the
//! viewport's width known (see `bounds`).
//!
//! **Every number here is final; only the drawing of it lags.** `scroll` and
//! `ColumnWidth` are what the layout, the read-out and the session file see,
//! and they change the instant something asks them to. Beside each one is a
//! `Glide` (`ui::motion`) that catches up over a frame or two, and that is
//! the only thing `render` positions anything from -- so the invariant above
//! is never a frame out of date, and switching animations off removes the
//! lag and nothing else. The strip is also the reason motion exists at all:
//! a jump cut between two correct pictures of a scrolling strip tells you
//! nothing about which way you went.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, DragMoveEvent, Entity, Pixels,
    Window,
};

use super::pane::Pane;
use super::Workspace;
use crate::theme::Theme;
use crate::ui::motion::{Glide, Motion};
use crate::ui::resize::NoPreview;

/// The payload for a seam drag. `on_drag_move::<T>` fires on every element
/// listening for payload type `T`, not only the one the drag started on, so the
/// seam has to say which one it is and each handler ignore the rest.
#[derive(Clone)]
struct SeamDrag {
    ix: usize,
}

/// How wide a column is. A share of the viewport rather than a pixel count, so
/// the layout survives a window resize; a drag on a seam pins it to `Fixed`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColumnWidth {
    Third,
    Half,
    TwoThirds,
    Full,
    /// Pixels, from dragging a seam.
    Fixed(f32),
}

impl ColumnWidth {
    pub fn pixels(self, viewport: f32) -> f32 {
        let raw = match self {
            ColumnWidth::Third => viewport / 3.,
            ColumnWidth::Half => viewport / 2.,
            ColumnWidth::TwoThirds => viewport * 2. / 3.,
            ColumnWidth::Full => viewport,
            ColumnWidth::Fixed(px) => px,
        };
        // A column narrower than this cannot show a usable grid, and a zero
        // one would make the strip impossible to click back out of.
        raw.clamp(MIN_COLUMN, viewport.max(MIN_COLUMN))
    }

    /// The preset order a single key steps through.
    pub fn cycled(self) -> Self {
        match self {
            ColumnWidth::Third => ColumnWidth::Half,
            ColumnWidth::Half => ColumnWidth::TwoThirds,
            ColumnWidth::TwoThirds => ColumnWidth::Full,
            ColumnWidth::Full => ColumnWidth::Third,
            // A dragged width rejoins the presets at the nearest sensible step
            // rather than jumping to a third.
            ColumnWidth::Fixed(_) => ColumnWidth::Half,
        }
    }

    /// The share of the viewport this width takes, which is what the session
    /// file records. A dragged width has no share to record, so it saves as
    /// the half it will come back as.
    pub fn share(self) -> f32 {
        match self {
            ColumnWidth::Third => 1. / 3.,
            ColumnWidth::Half => 0.5,
            ColumnWidth::TwoThirds => 2. / 3.,
            ColumnWidth::Full => 1.,
            ColumnWidth::Fixed(_) => 0.5,
        }
    }

    /// The nearest preset to a saved share. Presets rather than the exact
    /// number on purpose: a share restored into a window of a different size
    /// is an approximation either way, and a preset is one you can then cycle.
    pub fn from_share(share: f32) -> Self {
        match share {
            s if !s.is_finite() || s <= 0. => ColumnWidth::Half,
            s if s < 0.42 => ColumnWidth::Third,
            s if s < 0.58 => ColumnWidth::Half,
            s if s < 0.84 => ColumnWidth::TwoThirds,
            _ => ColumnWidth::Full,
        }
    }

}

const MIN_COLUMN: f32 = 180.;

/// A drawn share of the viewport as a window header prints it: the preset it
/// is, when it is one, and a percentage when a dragged seam or the stretch
/// that fills the window has made it something else.
pub fn width_label(share: f32) -> String {
    let near = |target: f32| (share - target).abs() < 0.02;
    if share >= 0.99 {
        crate::ui::kit::label("full")
    } else if near(1. / 3.) {
        "1/3".into()
    } else if near(0.5) {
        "1/2".into()
    } else if near(2. / 3.) {
        "2/3".into()
    } else {
        format!("{}%", (share * 100.).round() as u32)
    }
}

/// How a column stands in the navigation bar's read-out: the only place the
/// strip says how long it is and where you are on it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Segment {
    /// Scrolled off the side; it exists but you cannot see it.
    Offscreen,
    Visible,
    Focus,
}

pub struct Column {
    pub pane: Entity<Pane>,
    pub width: ColumnWidth,
    /// `width` in pixels, catching up. Separate from `width` because the
    /// pixels depend on the viewport, which only the layout pass knows.
    shown: Glide,
    /// Take the next width without animating. Set by a seam drag: a column
    /// being resized with the mouse has to be exactly under the pointer, and
    /// a glide would leave the edge trailing the cursor by a frame.
    snap: bool,
}

impl Column {
    fn new(pane: Entity<Pane>, width: ColumnWidth) -> Self {
        Self {
            pane,
            width,
            // Zero, so the first paint glides it out from nothing to its
            // width -- a new column grows into the strip rather than
            // appearing fully formed beside the one you were looking at.
            shown: Glide::new(0.),
            snap: false,
        }
    }
}

pub struct Strip {
    columns: Vec<Column>,
    /// Index of the focused column. The strip owns this; `Workspace` keeps its
    /// `active_pane` pointing at the same one through `focus_on`.
    focus: usize,
    /// How far into the strip the viewport starts, in pixels. Always the
    /// final value; `shown_scroll` is what gets drawn.
    scroll: f32,
    /// `scroll`, catching up. The whole reason `ui::motion` exists.
    shown_scroll: Glide,
    /// The viewport width the glides were last aimed with. A window resize
    /// re-derives every column's pixel width at once, and animating that
    /// would leave the strip a frame behind the window edge the user is
    /// dragging -- so a change here snaps everything instead.
    last_viewport: f32,
    /// The widest a column may be drawn beside others, as a share of the
    /// viewport (`Settings::max_column_width`). Set by the workspace.
    pub max_share: f32,
    /// The strip's laid-out bounds, recorded each frame by a `canvas`. Keeping
    /// the focus on screen needs the viewport's width, and nothing else in the
    /// layout pass can report it.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl Strip {
    pub fn new(pane: Entity<Pane>) -> Self {
        Self {
            columns: vec![Column::new(pane, ColumnWidth::Half)],
            focus: 0,
            scroll: 0.,
            shown_scroll: Glide::new(0.),
            last_viewport: 0.,
            max_share: 0.9,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    /// Rebuilds a strip from the session file. The first column takes the
    /// focus, since nothing about which one had it is worth saving -- the
    /// shells behind them are all new.
    pub fn restored(columns: Vec<(Entity<Pane>, ColumnWidth)>) -> Self {
        Self {
            columns: columns
                .into_iter()
                .map(|(pane, width)| Column::new(pane, width))
                .collect(),
            focus: 0,
            scroll: 0.,
            shown_scroll: Glide::new(0.),
            last_viewport: 0.,
            max_share: 0.9,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn panes(&self) -> Vec<Entity<Pane>> {
        self.columns.iter().map(|c| c.pane.clone()).collect()
    }

    pub fn focus_index(&self) -> usize {
        self.focus.min(self.columns.len().saturating_sub(1))
    }

    pub fn at(&self, ix: usize) -> Option<Entity<Pane>> {
        self.columns.get(ix).map(|c| c.pane.clone())
    }

    pub fn focused(&self) -> Option<Entity<Pane>> {
        self.columns.get(self.focus_index()).map(|c| c.pane.clone())
    }

    pub fn index_of(&self, pane: &Entity<Pane>) -> Option<usize> {
        self.columns.iter().position(|c| &c.pane == pane)
    }

    pub fn focus_on(&mut self, pane: &Entity<Pane>) {
        if let Some(ix) = self.index_of(pane) {
            self.focus = ix;
        }
    }

    /// Puts a new column immediately right of the focus and focuses it. Niri's
    /// rule, and the reason the strip never reflows: what was on screen keeps
    /// its width, the strip simply gets longer.
    pub fn insert_after_focus(&mut self, pane: Entity<Pane>, width: ColumnWidth) {
        let at = if self.columns.is_empty() {
            0
        } else {
            self.focus_index() + 1
        };
        self.columns.insert(at, Column::new(pane, width));
        self.focus = at;
    }


    /// Drops a column. Returns false when it is the last one -- the workspace
    /// always keeps somewhere for the next window to land.
    pub fn remove(&mut self, pane: &Entity<Pane>) -> bool {
        if self.columns.len() <= 1 {
            return false;
        }
        let Some(ix) = self.index_of(pane) else {
            return false;
        };
        self.columns.remove(ix);
        if self.focus >= self.columns.len() {
            self.focus = self.columns.len() - 1;
        } else if ix < self.focus {
            self.focus -= 1;
        }
        true
    }

    /// Moves focus one column over, returning the pane now focused. `None`
    /// when there is nothing that way -- the strip does not wrap, because
    /// position on it is the only thing you have to remember.
    pub fn focus_step(&mut self, delta: isize) -> Option<Entity<Pane>> {
        let next = self.focus_index() as isize + delta;
        if next < 0 || next as usize >= self.columns.len() {
            return None;
        }
        self.focus = next as usize;
        self.focused()
    }

    /// Carries the focused column past its neighbour, focus travelling with it.
    pub fn move_focused(&mut self, delta: isize) -> bool {
        let from = self.focus_index();
        let to = from as isize + delta;
        if to < 0 || to as usize >= self.columns.len() {
            return false;
        }
        let to = to as usize;
        self.columns.swap(from, to);
        self.focus = to;
        true
    }

    pub fn cycle_focused_width(&mut self) -> Option<ColumnWidth> {
        let ix = self.focus_index();
        let column = self.columns.get_mut(ix)?;
        column.width = column.width.cycled();
        Some(column.width)
    }

    pub fn set_focused_width(&mut self, width: ColumnWidth) {
        let ix = self.focus_index();
        if let Some(column) = self.columns.get_mut(ix) {
            column.width = width;
        }
    }

    pub fn focused_width(&self) -> Option<ColumnWidth> {
        self.columns.get(self.focus_index()).map(|c| c.width)
    }

    /// Every column's *drawn* width in pixels. Each column's own width,
    /// except that **the strip never leaves the window part-empty**: when the
    /// columns add up to less than the viewport, the last one stretches to
    /// meet the edge -- and a lone column is simply that rule at its limit.
    /// Ground beside the last window is a strip with nothing to scroll to.
    ///
    /// The last column, not all of them proportionally: dragging any other
    /// seam then does exactly what it did -- that column takes the width under
    /// the pointer and the last one gives or takes the difference -- where a
    /// proportional stretch would rescale the column being dragged and pull
    /// its edge out from under the cursor.
    ///
    /// Derived, never stored. A stretched column keeps its own width, so when
    /// another arrives it glides back to the size it had rather than staying
    /// wide and shoving the newcomer off screen.
    fn pixel_widths(&self, viewport: f32) -> Vec<f32> {
        // With company, no column is drawn wider than the cap: a column as
        // wide as the window has its seam on the window's edge, where it
        // cannot be grabbed, and hides that there is anything beside it. The
        // neighbours peek in instead. Alone, a column fills the window.
        let cap = if self.columns.len() > 1 {
            viewport * self.max_share
        } else {
            f32::INFINITY
        };
        let mut widths: Vec<f32> = self
            .columns
            .iter()
            .map(|c| c.width.pixels(viewport).min(cap))
            .collect();
        let total: f32 = widths.iter().sum();
        if let Some(last) = widths.last_mut() {
            if total < viewport {
                *last += viewport - total;
            }
        }
        widths
    }

    /// Puts a new column at `ix` (clamped) and focuses it. For a window dropped
    /// beside a column with the mouse, which can land anywhere on the strip,
    /// not only right of the focus.
    pub fn insert_at(&mut self, ix: usize, pane: Entity<Pane>, width: ColumnWidth) {
        let ix = ix.min(self.columns.len());
        self.columns.insert(ix, Column::new(pane, width));
        self.focus = ix;
    }


    /// The whole strip as `(share of the viewport, how it stands)`, for the
    /// navigation bar to draw as a row of segments. Width is carried through
    /// rather than flattened to equal marks, because a strip of four halves
    /// and a strip of four thirds are different places to be.
    pub fn readout(&self) -> Vec<(f32, Segment)> {
        let viewport = f32::from(self.bounds.get().size.width);
        let focus = self.focus_index();
        let mut x = 0.;
        let drawn = self.pixel_widths(viewport.max(1.));
        self.columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                // Before the first paint nothing has been laid out, so nothing
                // is off screen either -- claiming otherwise would flash the
                // whole read-out pale for a frame.
                let width = if viewport > 0. { drawn[ix] } else { 0. };
                // The share actually drawn, stretch included, so the read-out
                // shows a full bar when the strip fills the window.
                let share = if viewport > 0. {
                    width / viewport
                } else {
                    column.width.share()
                };
                // Measured against the final scroll, not the drawn one, so
                // the read-out names where you are going while the strip is
                // still on its way there. The bar says it and then the strip
                // shows it, which is the right order.
                let visible = viewport <= 0.
                    || (x + width > self.scroll + 0.5 && x < self.scroll + viewport - 0.5);
                x += width;
                let segment = if ix == focus {
                    Segment::Focus
                } else if visible {
                    Segment::Visible
                } else {
                    Segment::Offscreen
                };
                (share, segment)
            })
            .collect()
    }

    /// The display ordinals, as `(pane, 1-based index)`. The workspace applies
    /// them after any structural change.
    pub fn ordinals(&self) -> Vec<(Entity<Pane>, usize)> {
        self.columns
            .iter()
            .enumerate()
            .map(|(ix, c)| (c.pane.clone(), ix + 1))
            .collect()
    }

    /// Scrolls so the focused column sits wholly inside the viewport, moving
    /// as little as possible. Called during layout, where the viewport width
    /// is finally known.
    fn scroll_to_focus(&mut self, viewport: f32) {
        if viewport <= 0. {
            return;
        }
        let widths = self.pixel_widths(viewport);
        let focus = self.focus_index();
        let left: f32 = widths.iter().take(focus).sum();
        let right = left + widths.get(focus).copied().unwrap_or(0.);

        if left < self.scroll {
            self.scroll = left;
        } else if right > self.scroll + viewport {
            self.scroll = right - viewport;
        }

        // Never scrolled past either end: the strip is unbounded to the right
        // only as far as it actually has columns.
        let total: f32 = widths.iter().sum();
        let max = (total - viewport).max(0.);
        self.scroll = self.scroll.clamp(0., max);
    }

    /// Pins a column's right edge to the cursor. Absolute, never a delta from
    /// the handle's last-painted position: several mouse moves can arrive
    /// before the next paint, and correcting against the same stale position
    /// each time overshoots and visibly jitters.
    ///
    /// Only this column changes. Its neighbour is pushed along rather than
    /// squeezed, which is the difference between a strip and a split.
    pub(super) fn resize_column(&mut self, ix: usize, cursor_x: Pixels) {
        let bounds = self.bounds.get();
        let viewport = f32::from(bounds.size.width);
        if viewport <= 0. {
            return;
        }
        let drawn = self.pixel_widths(viewport);
        // The seam on the focused column's *left* resizes the focused column:
        // that is the window you are working in, and dragging its edge should
        // change it, not the neighbour. Its right edge stays where it is, its
        // left edge follows the pointer, and the neighbour on the left gives
        // or takes the difference -- the one place a seam changes two widths,
        // because a left edge cannot move any other way.
        if self.focus_index() == ix + 1 && ix + 1 < self.columns.len() {
            let start: f32 = drawn.iter().take(ix).sum();
            let origin = f32::from(bounds.origin.x) - self.shown_scroll.shown() + start;
            let right = origin + drawn[ix] + drawn[ix + 1];
            let cursor = f32::from(cursor_x).clamp(origin + MIN_COLUMN, right - MIN_COLUMN);
            self.columns[ix].width = ColumnWidth::Fixed(cursor - origin);
            self.columns[ix + 1].width = ColumnWidth::Fixed(right - cursor);
            self.columns[ix].snap = true;
            self.columns[ix + 1].snap = true;
            return;
        }
        let left: f32 = drawn.iter().take(ix).sum();
        // The cursor is in window coordinates; the column's left edge is the
        // strip's own origin, less how far the strip has been scrolled, plus
        // everything to the left of it.
        // The drawn scroll, not the final one: the seam the user grabbed is
        // where it was painted.
        let origin = f32::from(bounds.origin.x) - self.shown_scroll.shown() + left;
        if let Some(column) = self.columns.get_mut(ix) {
            // `pixels` clamps on the way out, so a cursor dragged past either
            // end cannot produce an unusable column.
            column.width = ColumnWidth::Fixed(f32::from(cursor_x) - origin);
            // Under the pointer, so no glide: see `Column::snap`.
            column.snap = true;
        }
    }

    pub fn render(&mut self, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
        let motion = Motion::from_settings(cx);
        let theme = cx.global::<Theme>();
        let border = theme.border;
        let accent = theme.accent;
        // The strip's own ground is a safety net for an opaque window only.
        // Every window on it paints the ground already (`Pane`), and the strip
        // is never left part-empty (the last column stretches), so on a
        // translucent window a ground here was a second layer under every
        // window: 74% over 74% reads as 93%, and the windows came out darker
        // and less see-through than the bar beside them.
        let bg = (!theme.translucent()).then_some(theme.bg);

        let viewport = f32::from(self.bounds.get().size.width);
        self.scroll_to_focus(viewport);

        let measured = self.bounds.clone();
        let mut container = div()
            .id("strip")
            .relative()
            .size_full()
            .overflow_hidden()
            .when_some(bg, |this, bg| this.bg(bg))
            .child(
                canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            );

        // The first frame has no measured width yet, so there is nothing
        // sensible to lay columns against; the canvas above fills it in and
        // the next frame draws for real.
        if viewport <= 0. {
            return container.into_any_element();
        }

        // Everything below is drawn from glides, never from `scroll` and
        // `ColumnWidth` directly -- see the module header. Aim them all, then
        // sample them all, in one pass before anything is positioned.
        //
        // A window resize re-derives every width at once from the new
        // viewport. Gliding to those would leave the strip a frame behind the
        // edge being dragged, so the whole thing snaps while the window moves.
        let resized = (viewport - self.last_viewport).abs() > 0.5;
        self.last_viewport = viewport;

        if resized {
            self.shown_scroll.jump(self.scroll);
        } else {
            self.shown_scroll.to(self.scroll, &motion);
        }
        let scroll = self.shown_scroll.sample(&motion);

        let targets = self.pixel_widths(viewport);
        let widths: Vec<f32> = self
            .columns
            .iter_mut()
            .zip(targets)
            .map(|(column, target)| {
                if resized || std::mem::take(&mut column.snap) {
                    column.shown.jump(target);
                } else {
                    column.shown.to(target, &motion);
                }
                column.shown.sample(&motion)
            })
            .collect();

        // One frame at a time, the same loop the home screen runs: whatever
        // is still moving asks for the next frame, and the window goes back
        // to painting only on change the moment nothing is. Not while the
        // window is in the background -- a strip nobody is looking at is not
        // worth sixteen milliseconds, and gpui repaints on activation anyway.
        let moving =
            self.shown_scroll.running() || self.columns.iter().any(|c| c.shown.running());
        if moving && window.is_window_active() {
            window.request_animation_frame();
        }

        let last = self.columns.len() - 1;
        let mut seams: Vec<(usize, f32)> = Vec::new();
        // Positioned as *fractions* of the strip, not pixels. The pixels were
        // worked out against last frame's width -- the only width there is
        // until this frame lays out -- so while the window was being resized
        // every frame drew the columns one resize behind, and the strip
        // jittered against its own edge. A fraction lets layout resolve
        // against this frame's width: preset columns land exactly, and only
        // a dragged (pixel) column can be a frame out, by one step's worth.
        let frac = move |pixels: f32| gpui::relative(pixels / viewport);
        let mut x = -scroll;
        for (ix, column) in self.columns.iter().enumerate() {
            let width = widths[ix];
            // Columns scrolled wholly off either side cost nothing to skip,
            // and a strip is meant to be longer than the window.
            if x + width > -1. && x < viewport + 1. {
                container = container.child(
                    div()
                        .absolute()
                        .left(frac(x))
                        .top_0()
                        .bottom_0()
                        .w(frac(width))
                        .child(column.pane.clone()),
                );
                if ix < last {
                    seams.push((ix, x + width));
                }
            }
            x += width;
        }

        // The seams go on last so they sit over both neighbours. 7px of grab
        // for a 1px line, because the neighbours are mouse-hungry -- a
        // terminal reports clicks to the remote program -- and a press a pixel
        // off the seam reads as "the column won't resize".
        // One hairline per seam, drawn after every column rather than as a
        // column's own right border. Columns are placed by fraction, and a
        // column's right edge can round onto the same device pixel as the
        // next column's left -- which then painted its ground over the
        // border, and the line between two windows came and went with the
        // widths. Always the plain hairline: focus is already marked in the
        // window header (its accent bar and FOCUS tag), and a full-height red
        // rule beside the focused column was the same news twice, louder.
        for &(_, seam) in &seams {
            container = container.child(
                div()
                    .absolute()
                    .left(frac(seam))
                    .ml(px(-1.))
                    .top_0()
                    .bottom_0()
                    .w(px(1.))
                    .bg(border),
            );
        }
        for (ix, seam) in seams {
            container = container.child(
                div()
                    .id(("seam", ix))
                    .absolute()
                    .left(frac(seam))
                    .ml(px(-5.))
                    .top_0()
                    .bottom_0()
                    .w(px(11.))
                    .cursor_col_resize()
                    // A press here is the seam's alone: without this the
                    // column underneath was hovered too, took the mouse-down
                    // and grabbed the focus while the seam started resizing.
                    .occlude()
                    .hover(|this| this.bg(accent.opacity(0.35)))
                    .on_drag(SeamDrag { ix }, |_, _, _, cx| cx.new(|_| NoPreview))
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<SeamDrag>, _window, cx| {
                            if event.drag(cx).ix != ix {
                                return;
                            }
                            this.strip.resize_column(ix, event.event.position.x);
                            cx.notify();
                        },
                    )),
            );
        }

        container.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_step_through_the_presets_and_clamp() {
        assert_eq!(ColumnWidth::Third.cycled(), ColumnWidth::Half);
        assert_eq!(ColumnWidth::Half.cycled(), ColumnWidth::TwoThirds);
        assert_eq!(ColumnWidth::TwoThirds.cycled(), ColumnWidth::Full);
        assert_eq!(ColumnWidth::Full.cycled(), ColumnWidth::Third);
        // A dragged width rejoins the presets rather than starting over.
        assert_eq!(ColumnWidth::Fixed(400.).cycled(), ColumnWidth::Half);

        assert_eq!(ColumnWidth::Half.pixels(1000.), 500.);
        assert_eq!(ColumnWidth::Full.pixels(1000.), 1000.);
        // Never narrower than a usable grid, even in a tiny window.
        assert_eq!(ColumnWidth::Third.pixels(120.), MIN_COLUMN);
    }

    #[test]
    fn a_saved_share_comes_back_as_the_nearest_preset() {
        for width in [
            ColumnWidth::Third,
            ColumnWidth::Half,
            ColumnWidth::TwoThirds,
            ColumnWidth::Full,
        ] {
            assert_eq!(ColumnWidth::from_share(width.share()), width);
        }
        // A dragged width has no share of its own and rejoins at a half.
        assert_eq!(
            ColumnWidth::from_share(ColumnWidth::Fixed(731.).share()),
            ColumnWidth::Half
        );
        // Nothing a hand-edited file can say produces a column of width zero.
        assert_eq!(ColumnWidth::from_share(0.), ColumnWidth::Half);
        assert_eq!(ColumnWidth::from_share(f32::NAN), ColumnWidth::Half);
        assert_eq!(ColumnWidth::from_share(-3.), ColumnWidth::Half);
    }

    #[test]
    fn header_width_labels() {
        assert_eq!(width_label(1. / 3.), "1/3");
        assert_eq!(width_label(0.5), "1/2");
        assert_eq!(width_label(0.666), "2/3");
        assert_eq!(width_label(1.0), crate::ui::kit::label("full"));
        assert_eq!(width_label(0.42), "42%");
    }
}
