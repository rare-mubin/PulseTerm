//! Overview: the whole strip at once.
//!
//! **The strip is longer than the window on purpose**, and this is the only
//! place it is ever shown whole. Pick a column, press escape, and you are back
//! exactly where you were -- the strip has not been rearranged, only looked at.
//!
//! **It is not a switcher.** There is no z-order, no stacking and no
//! most-recently-used list, because position on the strip is the one thing
//! there is to remember and nothing here changes it. A switcher reorders
//! itself as you use it, which means the thing you want is in a different
//! place every time; the overview is the same picture every time you open it.
//!
//! Like `home` and `nav_bar` it is a free function over the workspace rather
//! than a view: everything it draws already lives on the workspace, so an
//! entity of its own would only be a subscription to keep in sync.

use gpui::{div, prelude::*, px, AnyElement, App, Context, SharedString, Window};

use super::kit;
use crate::theme::Theme;
use crate::workspace::item::PaneItem;
use crate::workspace::Workspace;

/// One column, flattened for drawing. Read out of the entities before any of
/// them is rendered, because reading a leased entity aborts the process.
struct Card {
    ordinal: usize,
    focused: bool,
    windows: Vec<Window_>,
}

/// A window inside a card. `lines` is a hint of what is in it, not a
/// rendering of it -- see `Emulator::preview_lines`.
struct Window_ {
    title: SharedString,
    lines: Vec<String>,
}

/// `enter` is how far through its entrance the overview is, 0 to 1, which
/// the workspace samples because it owns the clock for it. It is 1 with
/// animations off, so everything below multiplies by it unconditionally.
pub fn render(
    workspace: &Workspace,
    enter: f32,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.global::<Theme>();
    let (bg, surface, border) = (theme.bg, theme.surface, theme.border);
    let (fg, fg_muted, fg_subtle) = (theme.fg, theme.fg_muted, theme.fg_subtle);
    let accent = theme.accent;
    let micro = kit::micro(theme);
    let header_height = kit::header(theme);

    let cards = collect(workspace, cx);
    let columns = cards.len();
    let windows: usize = cards.iter().map(|card| card.windows.len()).sum();

    let reading = kit::label(&format!(
        "overview \u{2014} {columns} column{} / {windows} window{}",
        if columns == 1 { "" } else { "s" },
        if windows == 1 { "" } else { "s" },
    ));

    let cap = |text: &str| {
        div()
            .px_1()
            .border_1()
            .border_color(border)
            .text_color(fg_muted)
            .child(kit::label(text))
    };

    let mut row = div().flex().flex_1().h_full().min_h_0().gap_2();
    for card in cards {
        let ordinal = card.ordinal;
        let focused = card.focused;
        let caption = card
            .windows
            .iter()
            .map(|window| window.title.to_string())
            .collect::<Vec<_>>()
            .join(" \u{b7} ");

        // Every column is the same width here, whatever its width on the
        // strip. The overview answers "what is open and where", and a column
        // squeezed to a third would be the hardest one to read -- which is
        // backwards, since a narrow column is the one you are most likely to
        // have lost track of.
        // `flex_1`, never `h_full`: the card and its caption share the
        // column's height, and a stack that took all of it would push the
        // caption off the bottom of the window.
        let mut stack = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(bg)
            .border_1()
            .border_color(if focused { accent } else { border });

        let count = card.windows.len();
        for (ix, pane_window) in card.windows.into_iter().enumerate() {
            let label = if count > 1 {
                format!("{ordinal:02}.{}", ix + 1)
            } else {
                format!("{ordinal:02}")
            };
            stack = stack.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(ix > 0, |this| this.border_t_1().border_color(border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(header_height)
                            .px_1()
                            .flex_none()
                            .bg(surface)
                            .text_size(micro)
                            .text_color(fg_subtle)
                            .child(label)
                            .when(focused && ix == 0, |this| {
                                this.child(div().text_color(accent).child(kit::label("focus")))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .p_1()
                            .text_size(micro)
                            .text_color(fg_subtle)
                            // Clipped at the card's edge rather than
                            // ellipsised. A thumbnail is a picture of a
                            // screen, and a screen is cut off by its own
                            // edge too -- a row of "..." down the card would
                            // say only that the lines are long.
                            .children(pane_window.lines.into_iter().map(|line| {
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(line)
                            })),
                    ),
            );
        }

        row = row.child(
            div()
                .id(("overview-column", ordinal))
                .flex()
                .flex_col()
                .flex_1()
                .h_full()
                .min_w_0()
                .min_h_0()
                .gap_1()
                .child(stack)
                .child(
                    div()
                        .flex_none()
                        .truncate()
                        .text_size(micro)
                        .text_color(if focused { accent } else { fg_subtle })
                        .child(kit::label(&caption)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus_column_at(ordinal.saturating_sub(1), window, cx);
                    this.toggle_overview(window, cx);
                })),
        );
    }

    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(bg)
        .p_2()
        .gap_2()
        // The cards fade up as a block. Not each one in turn: a stagger would
        // make the card you are looking for arrive last, and the overview is
        // read as one picture rather than a list.
        .when(enter < 1., |this| {
            this.opacity(enter).top(px(8. * (1. - enter)))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .flex_none()
                .text_size(micro)
                .text_color(fg)
                .child(reading)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(cap("esc"))
                        .child(div().text_color(fg_subtle).child(kit::label("back")))
                        .child(cap("left / right"))
                        .child(div().text_color(fg_subtle).child(kit::label("pick"))),
                ),
        )
        .child(row)
        .into_any_element()
}

/// Reads every column out of the strip before anything is drawn.
fn collect(workspace: &Workspace, cx: &App) -> Vec<Card> {
    let focus = workspace.strip().focus_index();
    workspace
        .strip()
        .columns()
        .iter()
        .enumerate()
        .map(|(ix, column)| {
            let pane = column.pane.read(cx);
            Card {
                ordinal: ix + 1,
                focused: ix == focus,
                windows: pane
                    .items()
                    .iter()
                    .map(|item| Window_ {
                        title: item.title(cx),
                        lines: hint(item, cx),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// What to show inside a thumbnail.
///
/// Only a terminal gets one. A terminal's content is the only thing you
/// cannot name -- two shells on one host look identical from the outside, and
/// what is on the screen is the whole difference between them. An editor, a
/// preview and the settings page are each fully described by their title,
/// which the caption under the card already carries; drawing the file name a
/// second time inside the card would be the same word twice.
fn hint(item: &PaneItem, cx: &App) -> Vec<String> {
    match item {
        PaneItem::Terminal(view) => view.read(cx).emulator().preview_lines(6),
        _ => Vec::new(),
    }
}
