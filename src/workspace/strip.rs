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

    /// Puts a new column at the head of the strip and focuses it. Only the
    /// file tree uses this: it is where you start from, and a place belongs
    /// at the beginning rather than wherever the focus happened to be.
    pub fn insert_first(&mut self, pane: Entity<Pane>, width: ColumnWidth) {
        self.columns.insert(0, Column::new(pane, width));
        self.focus = 0;
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

    pub fn focused_width(&self) -> Option<ColumnWidth> {
        self.columns.get(self.focus_index()).map(|c| c.width)
    }


    /// The whole strip as `(share of the viewport, how it stands)`, for the
    /// navigation bar to draw as a row of segments. Width is carried through
    /// rather than flattened to equal marks, because a strip of four halves
    /// and a strip of four thirds are different places to be.
    pub fn readout(&self) -> Vec<(f32, Segment)> {
        let viewport = f32::from(self.bounds.get().size.width);
        let focus = self.focus_index();
        let mut x = 0.;
        self.columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                // Before the first paint nothing has been laid out, so nothing
                // is off screen either -- claiming otherwise would flash the
                // whole read-out pale for a frame.
                let width = if viewport > 0. {
                    column.width.pixels(viewport)
                } else {
                    0.
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
                (column.width.share(), segment)
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
        let widths: Vec<f32> = self
            .columns
            .iter()
            .map(|c| c.width.pixels(viewport))
            .collect();
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
        let left: f32 = self
            .columns
            .iter()
            .take(ix)
            .map(|c| c.width.pixels(viewport))
            .sum();
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
        let bg = theme.bg;

        let viewport = f32::from(self.bounds.get().size.width);
        self.scroll_to_focus(viewport);

        let measured = self.bounds.clone();
        let mut container = div()
            .id("strip")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(bg)
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

        let widths: Vec<f32> = self
            .columns
            .iter_mut()
            .map(|column| {
                let target = column.width.pixels(viewport);
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

        let focus = self.focus_index();
        let last = self.columns.len() - 1;
        let mut seams: Vec<(usize, f32)> = Vec::new();
        let mut x = -scroll;
        for (ix, column) in self.columns.iter().enumerate() {
            let width = widths[ix];
            // Columns scrolled wholly off either side cost nothing to skip,
            // and a strip is meant to be longer than the window.
            if x + width > -1. && x < viewport + 1. {
                container = container.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top_0()
                        .bottom_0()
                        .w(px(width))
                        // One hairline per seam, drawn by the column on its
                        // right edge -- the same rule the dividers followed.
                        // The focused column tints both of its own.
                        .when(ix < last, |this| {
                            this.border_r_1()
                                .border_color(if ix == focus { accent } else { border })
                        })
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
        for (ix, seam) in seams {
            container = container.child(
                div()
                    .id(("seam", ix))
                    .absolute()
                    .left(px(seam - 3.))
                    .top_0()
                    .bottom_0()
                    .w(px(7.))
                    .cursor_col_resize()
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

        // A tick at the edge the strip continues past, so a column clipped off
        // screen is visibly off screen rather than simply absent.
        let total: f32 = widths.iter().sum();
        if scroll > 0.5 {
            container = container.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(accent.opacity(0.5)),
            );
        }
        if total - scroll > viewport + 0.5 {
            container = container.child(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(accent.opacity(0.5)),
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
}
