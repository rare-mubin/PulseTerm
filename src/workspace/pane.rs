use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, EventEmitter, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, Window,
};

use super::item::{ItemId, ItemKind, PaneItem};
use crate::theme::Theme;
use crate::ui::icon::{icon, small};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PaneId(pub u64);

/// Emitted upward to the workspace, which owns the split tree and is the only
/// thing allowed to restructure it.
pub enum PaneEvent {
    Focused,
    /// Every item is gone; the workspace should collapse this pane out of the
    /// tree (unless it is the last one left).
    Empty,
    /// The "+" affordance in this column's header.
    NewTerminal,
    /// A close was asked for on a tab that would lose work. A pane cannot ask
    /// the question -- dialogs belong to the workspace -- so it reports the
    /// intent and waits to be told, through `force_close_item`.
    ConfirmClose {
        item: ItemId,
    },
}

impl EventEmitter<PaneEvent> for Pane {}

pub struct Pane {
    pub id: PaneId,
    pub focus_handle: FocusHandle,
    items: Vec<PaneItem>,
    /// The focused window within this column. Every window is on screen, so
    /// this selects which one has the keyboard, not which one is visible.
    active_ix: usize,
    /// Where this column sits on the strip, 1-based, for its window headers.
    /// The strip sets it on every structural change: a pane cannot know where
    /// it sits and has no reason to.
    pub ordinal: usize,
}

impl Pane {
    pub fn new(id: PaneId, cx: &mut Context<Self>) -> Self {
        Self {
            id,
            focus_handle: cx.focus_handle(),
            items: Vec::new(),
            active_ix: 0,
            ordinal: 1,
        }
    }

    pub fn items(&self) -> &[PaneItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Which tab is in front, for the session file.
    pub fn active_ix(&self) -> usize {
        self.active_ix
    }

    pub fn active_item(&self) -> Option<&PaneItem> {
        self.items.get(self.active_ix)
    }

    pub fn index_of(&self, item_id: ItemId, cx: &App) -> Option<usize> {
        self.items.iter().position(|i| i.id(cx) == item_id)
    }

    pub fn add_item(
        &mut self,
        item: PaneItem,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.items.push(item);
        self.active_ix = self.items.len() - 1;
        if focus {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    pub fn insert_item(
        &mut self,
        item: PaneItem,
        index: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index.min(self.items.len());
        self.items.insert(index, item);
        self.active_ix = index;
        if focus {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    /// Detaches a window without closing it -- the caller is taking ownership,
    /// typically to hand it to another column. Emits `Empty` if that was the
    /// last one, so the workspace can collapse this column off the strip.
    pub fn take_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) -> Option<PaneItem> {
        let ix = self.index_of(item_id, cx)?;
        let item = self.items.remove(ix);
        self.active_ix = self.active_ix.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() {
            cx.emit(PaneEvent::Empty);
        }
        cx.notify();
        Some(item)
    }

    pub fn activate_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.items.len() {
            self.active_ix = ix;
            self.focus_active_item(window, cx);
            cx.notify();
        }
    }

    pub fn activate_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let next = (self.active_ix + 1) % self.items.len();
        self.activate_item(next, window, cx);
    }

    pub fn activate_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let prev = (self.active_ix + self.items.len() - 1) % self.items.len();
        self.activate_item(prev, window, cx);
    }

    /// Every tab close -- the X, middle-click, ctrl-shift-w -- comes through
    /// here, which is the whole reason the unsaved check fits in one place.
    pub fn close_item_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(ix) else {
            return;
        };
        // An editor whose buffer differs from the file gets a question first.
        // Closing it is otherwise silent and unrecoverable -- the text only
        // ever lived in this process.
        if let PaneItem::Editor(view) = item {
            if view.read(cx).is_dirty() {
                let item = item.id(cx);
                cx.emit(PaneEvent::ConfirmClose { item });
                return;
            }
        }
        self.remove_item_at(ix, window, cx);
    }

    /// Closes a tab that was asked about, now that the answer is in. Takes an
    /// `ItemId` rather than an index because the dialog outlives a frame, and
    /// a tab can be dragged elsewhere while it is up -- an index would then
    /// close whatever had slid into that slot.
    pub fn force_close_item(&mut self, item: ItemId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.items.iter().position(|i| i.id(cx) == item) {
            self.remove_item_at(ix, window, cx);
        }
    }

    fn remove_item_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.items.len() {
            return;
        }
        // Dropping the item drops the terminal view, which drops its `Pty`,
        // whose `Drop` closes the SSH channel. Closing a tab really does free
        // the remote side -- there is no separate teardown call to forget.
        self.items.remove(ix);
        self.active_ix = self.active_ix.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() {
            cx.emit(PaneEvent::Empty);
        } else {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    /// Closes every tab of a given kind at once, used when the connection goes
    /// away and its terminals should go with it. A Settings tab survives, since
    /// it is not tied to a session.
    pub fn close_items_of_kind(
        &mut self,
        kind: ItemKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.items.len();
        self.items.retain(|item| item.kind() != kind);
        if self.items.len() == before {
            return;
        }
        self.active_ix = self.active_ix.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() {
            window.focus(&self.focus_handle);
            cx.emit(PaneEvent::Empty);
        } else {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    pub fn close_active_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_item_at(self.active_ix, window, cx);
    }

    fn focus_active_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(self.active_ix) {
            let handle = item.focus_handle(cx);
            window.focus(&handle);
        } else {
            window.focus(&self.focus_handle);
        }
    }

}

impl Focusable for Pane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Pane {
    /// A column. Every window in it is on screen, stacked, each under its own
    /// header -- there are no tabs, because the strip replaced them and a
    /// window you cannot see is a window you have to remember.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let bg = theme.bg;
        let surface = theme.surface;
        let fg = theme.fg;
        let fg_muted = theme.fg_muted;
        let border = theme.border;
        let accent = theme.accent;
        let font = theme.font.clone();
        let font_size = theme.font_size;
        let fg_subtle = theme.fg_subtle;
        // 26px at the default font, and it grows with the type like
        // everything else.
        let header_height = crate::ui::kit::header(theme);

        let column_focused = self.focus_handle.contains_focused(window, cx);
        let active = self.active_ix;
        let ordinal = self.ordinal;
        let count = self.items.len();

        let windows: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let focused = column_focused && ix == active;
                let title = item.title(cx);
                let glyph = item.icon(cx);
                // Unsaved work, and a shell that has ended. Both are states of
                // the window rather than of its title, so both are marks the
                // header carries rather than words in the name.
                let dirty = item.is_dirty(cx);
                let exited = item.exit_reason(cx);
                // The column's number on the left -- the same one the strip
                // read-out counts -- and, only when there is a stack, which of
                // its windows this is, on the right. One number names a place
                // you can navigate to; the other says how deep the pile is,
                // and they belong at opposite ends.
                let label = format!("{ordinal:02}");
                let depth = (count > 1).then(|| format!("{}/{}", ix + 1, count));
                // An ended shell greys out whole. There is nothing live to
                // read, only scrollback, and the header should say so before
                // you try typing into it.
                let ink = if exited.is_some() {
                    fg_subtle
                } else if focused {
                    fg
                } else {
                    fg_muted
                };

                div()
                    .id(("window", ix))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    // One hairline between stacked windows, drawn by every
                    // window but the first. The column's outer edges belong to
                    // the strip, which draws the seam between columns.
                    .when(ix > 0, |this| this.border_t_1().border_color(border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pr_2()
                            .h(header_height)
                            .flex_none()
                            .bg(surface)
                            .border_b_1()
                            .border_color(border)
                            // Focus is a bar on the leading edge, never a frame:
                            // a frame would double the hairlines this header
                            // already sits between.
                            .child(
                                div()
                                    .w(px(2.))
                                    .h_full()
                                    .flex_none()
                                    .when(focused, |this| this.bg(accent)),
                            )
                            .child(div().pl_1().text_color(fg_subtle).child(label))
                            .child(icon(glyph, small(), ink))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(ink)
                                    .child(match &exited {
                                        Some(reason) => format!("{title} -- {reason}"),
                                        None => title.to_string(),
                                    }),
                            )
                            // A 5px dot, the same one the navigation bar uses
                            // for the session: unsaved is a state, not a word.
                            .when(dirty, |this| {
                                this.child(div().size(px(5.)).flex_none().bg(accent))
                            })
                            .children(
                                depth.map(|depth| {
                                    div().flex_none().text_color(fg_subtle).child(depth)
                                }),
                            )
                            // Only on the bottom-most header: it stacks
                            // another window in this column, and one header
                            // per column saying so is enough -- repeating it
                            // down a stack would be the same button four
                            // times.
                            .when(ix + 1 == count, |this| {
                                this.child(
                                    div()
                                        .id(("stack", ix))
                                        .px_1()
                                        .text_color(fg_muted)
                                        .hover(|this| this.text_color(fg))
                                        .child("+")
                                        .on_click(cx.listener(
                                            |_, _: &ClickEvent, _window, cx| {
                                                cx.stop_propagation();
                                                cx.emit(PaneEvent::NewTerminal);
                                            },
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .id(("close", ix))
                                    .px_1()
                                    .text_color(fg_muted)
                                    .hover(|this| this.text_color(fg))
                                    .child("x")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            // Or the header underneath would
                                            // also fire and focus what is being
                                            // closed.
                                            cx.stop_propagation();
                                            this.close_item_at(ix, window, cx);
                                        },
                                    )),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                    this.activate_item(ix, window, cx);
                                    cx.emit(PaneEvent::Focused);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .bg(bg)
                            .child(item.to_any_view().into_any_element()),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            // Scopes every element id inside this column: two columns both
            // numbering their windows from zero would otherwise collide.
            .id(("pane", self.id.0))
            .flex()
            .flex_col()
            .size_full()
            .font_family(font)
            .text_size(font_size)
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _window, cx| {
                    cx.emit(PaneEvent::Focused);
                    cx.notify();
                }),
            )
            .children(windows)
            // The last column is kept even when empty -- somewhere has to
            // receive the next window -- so it still has to paint a ground,
            // and offer the one thing you can do with it. Without this the
            // only way out of an empty workspace is a shortcut you have to
            // know.
            .when(count == 0, |this| {
                this.child(
                    div()
                        .id("empty-column")
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(bg)
                        .text_color(fg_muted)
                        .hover(|this| this.text_color(fg))
                        .child("+  new window")
                        .on_click(cx.listener(|_, _: &ClickEvent, _window, cx| {
                            cx.emit(PaneEvent::NewTerminal);
                        })),
                )
            })
    }
}
