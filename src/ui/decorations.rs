//! The window's own frame where the platform leaves it to us: Linux with
//! client-side decorations.
//!
//! On Windows the system draws the frame and the resize border, and this is a
//! pass-through. On Linux `main.rs` asks for client-side decorations so the
//! navigation bar is the only caption, and that request comes with a bill:
//! there is no resize border unless we declare one. gpui's answer is an
//! *inset* -- an invisible margin around the window, outside the geometry the
//! compositor snaps and tiles by, where the pointer grabs an edge instead of
//! the content.
//!
//! Three rules, each from the design or the compositor:
//!
//! * **The inset paints nothing.** gpui makes a client-decorated Wayland
//!   surface transparent precisely so the margin can be see-through, so the
//!   window ground cannot come from `Root` here (it would fill the margin) and
//!   is painted by the frame instead -- once, and only on an opaque window, by
//!   the same "one translucent layer" rule as everything else
//!   (`Theme::translucent`; `theme::apply_component_theme` clears `Root`'s on
//!   Linux to match).
//! * **No shadow and no radius**, which is where gpui's own example spends the
//!   inset. A hairline on each edge that is free is the whole frame.
//! * **A tiled edge has no margin and no grab.** A maximised or snapped
//!   window's edge is against something; a resize handle there would be a
//!   strip of dead pixels and a cursor that lies.

use gpui::{AnyElement, App, Window};

/// Wraps the window's content in its frame.
#[cfg(target_os = "linux")]
pub fn wrap(content: AnyElement, window: &mut Window, cx: &App) -> AnyElement {
    linux::wrap(content, window, cx)
}

#[cfg(not(target_os = "linux"))]
pub fn wrap(content: AnyElement, _: &mut Window, _: &App) -> AnyElement {
    content
}

#[cfg(target_os = "linux")]
mod linux {
    use gpui::{
        canvas, div, point, prelude::*, px, AnyElement, App, Bounds, CursorStyle, Decorations,
        HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, ResizeEdge,
        Size, Tiling, Window,
    };

    use crate::theme::Theme;

    /// How far outside the visible window an edge can be grabbed. Not a design
    /// measurement -- nothing here is drawn -- but a pointer target, sized like
    /// the strip's own 7px seam band for the same reason: a hairline is too thin
    /// to hit.
    const INSET: Pixels = px(7.);

    pub fn wrap(content: AnyElement, window: &mut Window, cx: &App) -> AnyElement {
        let theme = cx.global::<Theme>();
        let ground = (!theme.translucent()).then_some(theme.bg);
        let border = theme.border;

        let tiling = match window.window_decorations() {
            // The compositor refused client-side decorations (some X11 window
            // managers do) and is drawing its own frame: only the ground is ours.
            Decorations::Server => {
                return div()
                    .size_full()
                    .when_some(ground, |this, ground| this.bg(ground))
                    .child(content)
                    .into_any_element();
            }
            Decorations::Client { tiling } => tiling,
        };
        window.set_client_inset(INSET);

        div()
            .id("window-frame")
            .size_full()
            .when(!tiling.top, |this| this.pt(INSET))
            .when(!tiling.bottom, |this| this.pb(INSET))
            .when(!tiling.left, |this| this.pl(INSET))
            .when(!tiling.right, |this| this.pr(INSET))
            // The cursor is set from a hitbox covering the whole window, so it
            // changes the moment the pointer crosses into the margin.
            .child(
                canvas(
                    |_, window, _| {
                        window.insert_hitbox(
                            Bounds::new(point(px(0.), px(0.)), window_size(window)),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_, hitbox, window, _| {
                        let Some(edge) =
                            resize_edge(window.mouse_position(), window_size(window), tiling)
                        else {
                            return;
                        };
                        window.set_cursor_style(cursor(edge), &hitbox);
                    },
                )
                .size_full()
                .absolute(),
            )
            // Repaint while the pointer is in the margin, so the cursor follows
            // it from edge to corner. The content stops propagation below, so
            // this costs nothing while the pointer is over the app itself.
            .on_mouse_move(|_: &MouseMoveEvent, window, _| window.refresh())
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, window, _| {
                if let Some(edge) = resize_edge(event.position, window_size(window), tiling) {
                    window.start_window_resize(edge);
                }
            })
            .child(
                div()
                    .id("window-content")
                    .size_full()
                    .flex()
                    .flex_col()
                    .cursor(CursorStyle::Arrow)
                    .when_some(ground, |this, ground| this.bg(ground))
                    .border_color(border)
                    .when(!tiling.top, |this| this.border_t_1())
                    .when(!tiling.bottom, |this| this.border_b_1())
                    .when(!tiling.left, |this| this.border_l_1())
                    .when(!tiling.right, |this| this.border_r_1())
                    .on_mouse_move(|_: &MouseMoveEvent, _, cx| cx.stop_propagation())
                    .child(content),
            )
            .into_any_element()
    }

    /// The whole surface, margin included -- the space mouse positions are in.
    ///
    /// **Not `window_bounds()`**, which is what gpui's own example uses: on
    /// Wayland that is the window *less* the inset, so measuring against it
    /// shifts every zone inward by the margin's width. The cursor hitbox then
    /// stopped short of the right and bottom margins, and worse, a click in the
    /// last 14px of the content -- a terminal's bottom row -- started a resize.
    fn window_size(window: &Window) -> Size<Pixels> {
        window.viewport_size()
    }

    /// Which edge or corner `pos` grabs, if any. Only free edges count, so a
    /// window snapped to the left half resizes from its right edge alone.
    fn resize_edge(pos: Point<Pixels>, size: Size<Pixels>, tiling: Tiling) -> Option<ResizeEdge> {
        let top = !tiling.top && pos.y < INSET;
        let bottom = !tiling.bottom && pos.y > size.height - INSET;
        let left = !tiling.left && pos.x < INSET;
        let right = !tiling.right && pos.x > size.width - INSET;
        Some(match (top, bottom, left, right) {
            (true, _, true, _) => ResizeEdge::TopLeft,
            (true, _, _, true) => ResizeEdge::TopRight,
            (_, true, true, _) => ResizeEdge::BottomLeft,
            (_, true, _, true) => ResizeEdge::BottomRight,
            (true, _, _, _) => ResizeEdge::Top,
            (_, true, _, _) => ResizeEdge::Bottom,
            (_, _, true, _) => ResizeEdge::Left,
            (_, _, _, true) => ResizeEdge::Right,
            _ => return None,
        })
    }

    fn cursor(edge: ResizeEdge) -> CursorStyle {
        match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use gpui::size;

        fn at(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
            resize_edge(point(px(x), px(y)), size(px(800.), px(600.)), tiling)
        }

        #[test]
        fn corners_and_edges() {
            let free = Tiling::default();
            assert_eq!(at(2., 2., free), Some(ResizeEdge::TopLeft));
            assert_eq!(at(798., 598., free), Some(ResizeEdge::BottomRight));
            assert_eq!(at(400., 2., free), Some(ResizeEdge::Top));
            assert_eq!(at(798., 300., free), Some(ResizeEdge::Right));
            assert_eq!(at(400., 300., free), None);
        }

        #[test]
        fn tiled_edges_do_not_grab() {
            let left_half = Tiling {
                top: true,
                bottom: true,
                left: true,
                right: false,
            };
            assert_eq!(at(2., 2., left_half), None);
            assert_eq!(at(2., 300., left_half), None);
            assert_eq!(at(798., 2., left_half), Some(ResizeEdge::Right));
            assert_eq!(at(400., 300., Tiling::tiled()), None);
        }
    }
}
