use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, App, Bounds, ClickEvent, Context,
    CursorStyle, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    MouseButton,
    MouseDownEvent, Pixels, Point, SharedString, Window,
};

use super::item::{ItemId, ItemKind, PaneItem};
use crate::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PaneId(pub u64);

/// A window being carried by its header. Carries the column it came from,
/// because `on_drag_move` and `on_drop` fire on whichever column it is over,
/// and that column has to tell the workspace where to take it *from*.
#[derive(Clone)]
pub struct WindowDrag {
    pub from: Entity<Pane>,
    pub item: ItemId,
    pub title: SharedString,
}

/// Where a dragged window lands, relative to the window it is dropped on: into
/// that column above or below it, or out into a new column beside it.
///
/// The sides are a quarter of the width each and the rest is split top and
/// bottom. Sides are the narrower target on purpose -- stacking is the gesture
/// that keeps you where you are, a new column is the one that reshapes the
/// strip, and the bigger target should be the cheaper mistake.
/// `Settings::header_mode`, read once per render.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderMode {
    Show,
    Hide,
    Auto,
}

/// Where a window's header can go: in the column's flow above or below the
/// content, or laid over it (auto-hide).
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderSlot {
    Before,
    After,
    Overlay,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropZone {
    Above,
    Below,
    Left,
    Right,
}

impl DropZone {
    /// The zone `position` falls in, or `None` when it is outside `bounds` --
    /// `on_drag_move` reports every drag to every listener in the window, so
    /// being told about one is no sign it is over you.
    pub fn locate(position: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<Self> {
        if !bounds.contains(&position) {
            return None;
        }
        let w = f32::from(bounds.size.width).max(1.);
        let h = f32::from(bounds.size.height).max(1.);
        let x = f32::from(position.x - bounds.origin.x) / w;
        let y = f32::from(position.y - bounds.origin.y) / h;
        Some(if x < 0.25 {
            DropZone::Left
        } else if x > 0.75 {
            DropZone::Right
        } else if y < 0.5 {
            DropZone::Above
        } else {
            DropZone::Below
        })
    }
}

/// The payload for dragging the boundary between two stacked windows. Like
/// the strip's `SeamDrag`, it names which boundary it is -- in which column --
/// because every listener for the type hears every drag.
#[derive(Clone)]
struct RowSeam {
    pane: PaneId,
    /// The boundary under window `ix`, between it and `ix + 1`.
    ix: usize,
}

/// The smallest height a stacked window can be dragged to: its header and a
/// couple of lines under it. A window squeezed to nothing cannot be grabbed
/// back.
const MIN_ROW: f32 = 64.;

/// What follows the pointer while a window is dragged: its title in a box,
/// drawn like a header. Colours are taken once, at the start of the drag.
pub struct DragGhost {
    title: SharedString,
    fg: Hsla,
    surface: Hsla,
    accent: Hsla,
}

impl DragGhost {
    fn new(title: SharedString, cx: &App) -> Self {
        let theme = cx.global::<Theme>();
        Self {
            title,
            fg: theme.fg,
            surface: theme.palette.surface,
            accent: theme.accent,
        }
    }
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(self.surface)
            .border_1()
            .border_color(self.accent)
            .text_color(self.fg)
            .child(self.title.clone())
    }
}

/// Emitted upward to the workspace, which owns the split tree and is the only
/// thing allowed to restructure it.
pub enum PaneEvent {
    Focused,
    /// A window header was pressed: the strip holds its scroll until the
    /// button comes up (`Strip::hold_scroll`), in case this is a drag.
    HoldScroll,
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
    /// A window was dropped on this column's window `target`. Only the
    /// workspace can move a window between columns, so this is the request.
    DropWindow {
        from: Entity<Pane>,
        item: ItemId,
        target: usize,
        zone: DropZone,
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
    /// The column's width as the header prints it -- `1/3`, `1/2`, `FULL` --
    /// set by the workspace for the same reason as `ordinal`: the width
    /// belongs to the strip, and a pane cannot see it.
    pub width_label: SharedString,
    /// The window a drag is over and the zone it would land in, for the
    /// indicator. Cleared when no drag is live: gpui has no "the drag left
    /// me" event, so `has_active_drag` going false is the reset.
    drop_hover: Option<(usize, DropZone)>,
    /// Each stacked window's share of the column's height, as a flex weight.
    /// Keyed by `ItemId` rather than kept as a parallel `Vec`, so it follows
    /// a window through every insert, close and drag without bookkeeping; a
    /// window not in here weighs 1, which is an even split.
    row_weights: HashMap<ItemId, f32>,
    /// Each window's laid-out bounds as of the last paint, recorded by a
    /// `canvas`, for the row seams to measure against.
    row_bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl Pane {
    pub fn new(id: PaneId, cx: &mut Context<Self>) -> Self {
        Self {
            id,
            focus_handle: cx.focus_handle(),
            items: Vec::new(),
            active_ix: 0,
            ordinal: 1,
            width_label: SharedString::default(),
            drop_hover: None,
            row_weights: HashMap::new(),
            row_bounds: Rc::new(RefCell::new(Vec::new())),
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

    /// Swaps the window at `ix` for `item` in place: same slot in the stack,
    /// same share of the column's height, and focused. The old window is
    /// dropped. Not `take_item` + `insert_item`: taking the only window out of
    /// a column empties it, and the workspace collapses an empty column off
    /// the strip before the new one could go in.
    pub fn replace_item(
        &mut self,
        ix: usize,
        item: PaneItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.items.len() {
            return;
        }
        let old = self.items[ix].id(cx);
        if let Some(weight) = self.row_weights.remove(&old) {
            self.row_weights.insert(item.id(cx), weight);
        }
        self.items[ix] = item;
        self.active_ix = ix;
        self.focus_active_item(window, cx);
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

    /// Moves the boundary under window `ix` to `cursor_y`. Absolute, never a
    /// delta: the two windows' combined span does not move while one of their
    /// shared edges does, so the cursor's place in that span *is* the split,
    /// however many mouse moves arrive between paints. Only those two windows
    /// change; the rest of the stack keeps its height.
    fn resize_row(&mut self, ix: usize, cursor_y: Pixels, cx: &App) {
        let bounds = self.row_bounds.borrow();
        let (Some(upper), Some(lower)) = (bounds.get(ix), bounds.get(ix + 1)) else {
            return;
        };
        let top = f32::from(upper.origin.y);
        let span = f32::from(lower.origin.y + lower.size.height) - top;
        if span <= MIN_ROW * 2. {
            return;
        }
        let at = (f32::from(cursor_y) - top).clamp(MIN_ROW, span - MIN_ROW) / span;
        let (Some(a), Some(b)) = (self.items.get(ix), self.items.get(ix + 1)) else {
            return;
        };
        let (a, b) = (a.id(cx), b.id(cx));
        let weight = |id| self.row_weights.get(&id).copied().unwrap_or(1.);
        let pair = weight(a) + weight(b);
        drop(bounds);
        self.row_weights.insert(a, pair * at);
        self.row_weights.insert(b, pair * (1. - at));
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
        // Opaque: an auto-hidden header is drawn over the content, and a
        // translucent one would show the terminal's text through its labels.
        let header_ground = theme.palette.bg;
        let settings = cx.global::<crate::store::Settings>();
        let header_bottom = settings.header_position == "bottom";
        let header_mode = match settings.header_mode.as_str() {
            "hide" => HeaderMode::Hide,
            "auto" => HeaderMode::Auto,
            _ => HeaderMode::Show,
        };
        let micro = crate::ui::kit::micro(theme);
        let width_label = self.width_label.clone();

        if self.drop_hover.is_some() && !cx.has_active_drag() {
            self.drop_hover = None;
        }
        let drop_hover = self.drop_hover;
        let this_pane = cx.entity();
        let pane_id = self.id;
        // Forget weights for windows that have left; the rest stay put.
        let present: Vec<ItemId> = self.items.iter().map(|item| item.id(cx)).collect();
        self.row_weights.retain(|id, _| present.contains(id));
        self.row_bounds.borrow_mut().resize(self.items.len(), Bounds::default());
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
                // Unsaved work, and a shell that has ended. Both are states of
                // the window rather than of its title, so both are marks the
                // header carries rather than words in the name.
                let dirty = item.is_dirty(cx);
                let exited = item.exit_reason(cx);
                let divider = item.header_divider(cx);
                let drag = WindowDrag {
                    from: this_pane.clone(),
                    item: item.id(cx),
                    title: title.clone(),
                };
                let zone = drop_hover.and_then(|(at, zone)| (at == ix).then_some(zone));
                let weight = self.row_weights.get(&drag.item).copied().unwrap_or(1.);
                let measured = self.row_bounds.clone();
                // The design's header: the column's number on the left -- `02`,
                // and `02.2` for the second window of a stack, so every window
                // has an address -- and on the right what the window is to its
                // column: the column's width on the first window, `STACKED` on
                // the rest. The width is the column's, so saying it once per
                // stack is enough.
                let label = if ix == 0 {
                    format!("{ordinal:02}")
                } else {
                    format!("{ordinal:02}.{}", ix + 1)
                };
                let role = if ix == 0 {
                    width_label.clone()
                } else {
                    SharedString::from(crate::ui::kit::label("stacked"))
                };
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

                // The header, built once and put where the settings say: above
                // or below the content, drawn over it (auto-hide: invisible
                // until the pointer reaches it, so the content never reflows),
                // or not at all.
                let header = div()
                            .id(("header", ix))
                            .flex()
                            .items_center()
                            .gap_2()
                            .pr_2()
                            .h(header_height)
                            // The header is the handle. Dropped on another
                            // window it stacks or opens a column -- see
                            // `DropZone` -- and the terminal behind it is moved,
                            // never recreated, so its shell survives the trip.
                            .on_drag(drag, |drag, _, _, cx| {
                                let ghost = DragGhost::new(drag.title.clone(), cx);
                                cx.new(|_| ghost)
                            })
                            .flex_none()
                            // No hairline under it: the header is the top of
                            // the window, not a band laid across it.
                            // Focus is a bar on the leading edge plus the tag
                            // on the right, never a frame: a frame would double
                            // the hairlines this header already sits between.
                            .child(
                                div()
                                    .w(px(2.))
                                    .h_full()
                                    .flex_none()
                                    .when(focused, |this| this.bg(accent)),
                            )
                            .child(
                                div()
                                    .pl_1()
                                    .flex_none()
                                    .text_size(micro)
                                    .text_color(fg_subtle)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(ink)
                                    .when(focused, |this| this.font_weight(FontWeight::SEMIBOLD))
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
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(micro)
                                    .text_color(if focused { fg } else { fg_subtle })
                                    .child(role),
                            )
                            .when(focused, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .border_1()
                                        .border_color(accent)
                                        .text_size(micro)
                                        .text_color(accent)
                                        .child(crate::ui::kit::label("focus")),
                                )
                            })
                            // The design closes windows from the keyboard; the
                            // one button kept is this, because without it a
                            // window can only be closed by a shortcut you have
                            // to know.
                            .child(
                                // Drawn small, clicked large: the target is a
                                // square the header's full height, reaching
                                // into the header's right padding, so the
                                // small glyph does not have to be hit exactly.
                                div()
                                    .id(("close", ix))
                                    .flex_none()
                                    .h(header_height)
                                    .w(header_height)
                                    .mr(px(-8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(micro)
                                    .text_color(fg_subtle)
                                    .hover(|this| this.text_color(fg).bg(fg.opacity(0.08)))
                                    // The header around it is a drag handle,
                                    // and gpui starts a drag after 2px of
                                    // movement with the button down -- so a
                                    // press on the x that wobbled a little
                                    // picked the window up instead of closing
                                    // it. Stopping the press here keeps it from
                                    // the header; this element's own click was
                                    // armed by then (its listener runs first),
                                    // so the close still happens on release.
                                    .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .child("\u{00d7}")
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
                                    // First, so the scroll is held before the
                                    // focus change below would move it.
                                    cx.emit(PaneEvent::HoldScroll);
                                    this.activate_item(ix, window, cx);
                                    cx.emit(PaneEvent::Focused);
                                }),
                            )
                .into_any_element();
                let mut header = Some(header);
                let mut header_at = |slot: HeaderSlot| {
                    let wanted = match (header_mode, header_bottom) {
                        (HeaderMode::Hide, _) => None,
                        (HeaderMode::Show, false) => Some(HeaderSlot::Before),
                        (HeaderMode::Show, true) => Some(HeaderSlot::After),
                        (HeaderMode::Auto, _) => Some(HeaderSlot::Overlay),
                    };
                    if wanted != Some(slot) {
                        return None;
                    }
                    let header = header.take()?;
                    Some(match slot {
                        HeaderSlot::Overlay => div()
                            .id(("header-auto", ix))
                            .absolute()
                            .left_0()
                            .right_0()
                            .when(!header_bottom, |this| this.top_0())
                            .when(header_bottom, |this| this.bottom_0())
                            .bg(header_ground)
                            .opacity(0.)
                            .hover(|this| this.opacity(1.))
                            .child(header)
                            .into_any_element(),
                        _ => header,
                    })
                };

                let mut frame = div()
                    .id(("window", ix))
                    .relative()
                    // The ground, once, behind header and content alike: the
                    // header is part of the window, not a band laid over it,
                    // so it blends -- only a hairline divides them.
                    .bg(bg)
                    .flex()
                    .flex_col()
                    .flex_basis(relative(0.))
                    .min_h_0();
                // Its share of the column, set by dragging the boundary under
                // it. gpui's `flex_grow()` only ever means 1, so the weight
                // goes into the style directly.
                frame.style().flex_grow = Some(weight);
                frame
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                if let Some(slot) = measured.borrow_mut().get_mut(ix) {
                                    *slot = bounds;
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    // Window-wide, like every `on_drag_move`: work out whether
                    // the drag is over *this* window before claiming it, and
                    // let go of the claim when it moves elsewhere.
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<WindowDrag>, _window, cx| {
                            let here = DropZone::locate(event.event.position, event.bounds);
                            let next = match here {
                                Some(zone) => Some((ix, zone)),
                                None if this.drop_hover.is_some_and(|(at, _)| at == ix) => None,
                                None => return,
                            };
                            if this.drop_hover != next {
                                this.drop_hover = next;
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(move |this, drag: &WindowDrag, _window, cx| {
                        let zone = match this.drop_hover.take() {
                            Some((at, zone)) if at == ix => zone,
                            // The hover is a frame behind the pointer at worst;
                            // a drop with no zone yet stacks underneath.
                            _ => DropZone::Below,
                        };
                        cx.emit(PaneEvent::DropWindow {
                            from: drag.from.clone(),
                            item: drag.item,
                            target: ix,
                            zone,
                        });
                        cx.notify();
                    }))
                    // One hairline between stacked windows, drawn by every
                    // window but the first. The column's outer edges belong to
                    // the strip, which draws the seam between columns.
                    .when(ix > 0, |this| this.border_t_1().border_color(border))
                    .children(header_at(HeaderSlot::Before))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(item.to_any_view().into_any_element()),
                    )
                    .children(header_at(HeaderSlot::After))
                    .children(header_at(HeaderSlot::Overlay))
                    // The content's own divider, carried up through the
                    // header so it runs to the top of the window.
                    .children(divider.filter(|_| header_mode == HeaderMode::Show).map(|x| {
                        div()
                            .absolute()
                            .when(!header_bottom, |this| this.top_0())
                            .when(header_bottom, |this| this.bottom_0())
                            .left(px(x - 1.))
                            .w(px(1.))
                            .h(header_height)
                            .bg(border)
                    }))
                    .children(zone.map(|zone| drop_indicator(zone, accent)))
                    // The grab band for the boundary under this window, if a
                    // window follows it: 7px over the hairline, like a column
                    // seam, because the windows either side are mouse-hungry.
                    .when(ix + 1 < count, |this| {
                        this.child(
                            div()
                                .id(("row-seam", ix))
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(-6.))
                                .h(px(11.))
                                .cursor(CursorStyle::ResizeRow)
                                // The press is the seam's alone, or the window
                                // underneath takes it and the focus with it.
                                .occlude()
                                .hover(|this| this.bg(accent.opacity(0.35)))
                                .on_drag(RowSeam { pane: pane_id, ix }, |_, _, _, cx| {
                                    cx.new(|_| crate::ui::resize::NoPreview)
                                })
                                .on_drag_move(cx.listener(
                                    move |this, event: &DragMoveEvent<RowSeam>, _window, cx| {
                                        let seam = event.drag(cx);
                                        if seam.pane != pane_id || seam.ix != ix {
                                            return;
                                        }
                                        this.resize_row(ix, event.event.position.y, cx);
                                        cx.notify();
                                    },
                                )),
                        )
                    })
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

/// Where a dragged window would land: the half (or the quarter, for a new
/// column) it would take, tinted, with a 2px accent rule on the edge it would
/// arrive at. Painted over the window, never beside it, so showing it moves
/// nothing.
fn drop_indicator(zone: DropZone, accent: Hsla) -> AnyElement {
    let base = div().absolute().bg(accent.opacity(0.12)).border_color(accent);
    match zone {
        DropZone::Above => base.top_0().left_0().right_0().h_1_2().border_t_2(),
        DropZone::Below => base.bottom_0().left_0().right_0().h_1_2().border_b_2(),
        DropZone::Left => base.top_0().bottom_0().left_0().w_1_4().border_l_2(),
        DropZone::Right => base.top_0().bottom_0().right_0().w_1_4().border_r_2(),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn drop_zones() {
        let bounds = Bounds::new(point(px(100.), px(100.)), size(px(400.), px(200.)));
        let at = |x: f32, y: f32| DropZone::locate(point(px(x), px(y)), bounds);
        assert_eq!(at(120., 150.), Some(DropZone::Left));
        assert_eq!(at(480., 150.), Some(DropZone::Right));
        assert_eq!(at(300., 120.), Some(DropZone::Above));
        assert_eq!(at(300., 280.), Some(DropZone::Below));
        // Elsewhere in the window is no business of this one's.
        assert_eq!(at(50., 150.), None);
        assert_eq!(at(300., 320.), None);
    }
}
