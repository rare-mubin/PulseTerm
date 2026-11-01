use gpui::{div, Context, IntoElement, Render, Window};

/// The view gpui insists on for a drag's floating preview.
///
/// Resize handles use gpui's drag machinery only for its event delivery, not to
/// show anything under the cursor, so this renders nothing.
///
/// **Why the drag machinery at all:** `on_mouse_move` is gated on the element
/// being hovered, so a handle a few pixels wide stops receiving events the
/// instant the cursor outruns it — which is exactly what a fast drag does.
/// `on_drag_move` has no such gate and fires for every move anywhere in the
/// window, and gpui's own documentation points at it for "draggable UIs that
/// don't conform to a drag and drop style interaction, like resizing".
pub struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
