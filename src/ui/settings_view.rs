use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Corner, Entity, FocusHandle,
    Focusable, FontWeight, Hsla, Pixels, SharedString, Subscription, Window,
};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Colorize as _, Sizable as _};

use super::kit;
use crate::keymap::{self, Keys, COMMANDS, FIXED};
use crate::store::Settings;
use crate::theme::{self, Theme, ANSI_NAMES, COLOR_TOKENS};
use crate::workspace::item::ItemId;

type Pickers = Rc<Vec<(SharedString, Entity<ColorPickerState>)>>;

/// The font list's first entry, standing for "no family chosen".
const AUTOMATIC: &str = "Automatic";


/// Below this the page itself (the tab less its section list and padding)
/// stacks each row's control under its label.
const NARROW_ROWS: f32 = 640.;

/// Below this the section list leaves the side and runs across the top: a
/// 260px sidebar in a narrow column leaves the page a letter wide.
const TABS_BELOW: f32 = 640.;

/// The section list's width, as the design draws it (frame 08).
const SIDEBAR: f32 = 260.;

const WEIGHTS: &[(&str, &str)] = &[
    ("300", "Light"),
    ("400", "Regular"),
    ("500", "Medium"),
    ("600", "Semibold"),
    ("700", "Bold"),
];

/// The Settings tab.
///
/// The page itself -- the section sidebar, search, grouped rows with the label
/// on the left and the control on the right -- is gpui-component's `Settings`
/// element. Each field is a pair of closures reading and writing our `Settings`
/// global through `Settings::update`, which saves and re-applies the theme, so
/// every change is live and there is no separate "apply" step or state to keep
/// in sync here.
///
/// What this view does own is the handful of controls that need a stateful
/// entity of their own: a colour picker per colour, the searchable font list
/// and the opacity slider.
pub struct SettingsView {
    pub item_id: ItemId,
    pub focus_handle: FocusHandle,
    /// Every monospace family installed, "Automatic" first; measured once,
    /// when the tab opens.
    font_names: Vec<SharedString>,
    /// The font list is open, and what it is filtered by.
    font_open: bool,
    font_query: Entity<InputState>,
    opacity: Entity<SliderState>,
    /// Whether the opacity slider has had its settling frame; see `window_page`.
    opacity_settled: Rc<Cell<bool>>,
    /// One picker per colour token: the chrome colours in `COLOR_TOKENS` order,
    /// then `ansi0`..`ansi15`.
    pickers: Pickers,
    /// This tab's width as of the last paint, which picks the row layout. A pane
    /// can be narrow in a wide window, so the window size is no guide.
    width: Rc<Cell<Pixels>>,
    /// The section on show.
    page: Page,
    /// The command whose keys the next chord replaces, while a Keys row is
    /// recording. Shared with the keystroke interceptor registered in `new`.
    recording: Rc<Cell<Option<&'static str>>>,
    /// The Keys page's find box.
    keys_find: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// The design's sections, in its order (frame 08's sidebar).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Interface,
    Appearance,
    Terminal,
    Tiling,
    Files,
    Editor,
    Keys,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Interface,
        Page::Appearance,
        Page::Terminal,
        Page::Tiling,
        Page::Files,
        Page::Editor,
        Page::Keys,
    ];

    fn name(self) -> &'static str {
        match self {
            Page::Interface => "Interface",
            Page::Appearance => "Appearance",
            Page::Terminal => "Terminal",
            Page::Tiling => "Tiling",
            Page::Files => "Files",
            Page::Editor => "Editor",
            Page::Keys => "Keys",
        }
    }
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let palette = cx.global::<Theme>().palette;
        let tokens = COLOR_TOKENS
            .iter()
            .map(|(token, _, _)| SharedString::from(*token))
            .chain((0..16).map(|index| SharedString::from(format!("ansi{index}"))));
        let mut pickers = Vec::new();
        for token in tokens {
            let value = opaque(palette.get(&token).unwrap_or_default());
            let picker = cx.new(|cx| ColorPickerState::new(window, cx).default_value(value));
            subscriptions.push(cx.subscribe_in(&picker, window, {
                let token = token.clone();
                move |_, _, event: &ColorPickerEvent, _, cx| {
                    if let ColorPickerEvent::Change(Some(color)) = event {
                        let hex = color.to_hex();
                        Settings::update(cx, |settings| {
                            settings.colors.insert(token.to_string(), hex);
                        });
                    }
                }
            }));
            pickers.push((token, picker));
        }

        let mut fonts = vec![SharedString::from(AUTOMATIC)];
        fonts.extend(theme::monospace_fonts(cx));
        // The font list is ours, not gpui-component's Select: its dropdown
        // paints with the theme's window background, which is transparent on
        // Linux (the resize margin) and on any translucent window -- so the
        // list came out see-through. Ours is opaque and drawn like every
        // other list in the app.
        let font_query = cx.new(|cx| InputState::new(window, cx).placeholder("font"));
        subscriptions.push(cx.subscribe_in(&font_query, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));

        let current_opacity = cx.global::<Settings>().opacity;
        let opacity = cx.new(|_| {
            SliderState::new()
                .min(30.)
                .max(100.)
                .step(1.)
                .default_value(current_opacity)
        });
        subscriptions.push(cx.subscribe_in(
            &opacity,
            window,
            |_, _, event: &SliderEvent, _, cx| match event {
                SliderEvent::Change(value) => {
                    let value = value.start().round();
                    if cx.global::<Settings>().opacity != value {
                        Settings::update(cx, |settings| settings.opacity = value);
                    }
                }
            },
        ));

        let keys_find = cx.new(|cx| InputState::new(window, cx).placeholder("command or key"));
        subscriptions.push(cx.subscribe_in(&keys_find, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        // Recording a chord: an interceptor runs before any binding resolves,
        // so the chord being recorded is captured as typed instead of doing
        // what it currently does -- recording ctrl+shift+t must not open a
        // column. Escape cancels; delete or backspace unbinds.
        let recording: Rc<Cell<Option<&'static str>>> = Rc::new(Cell::new(None));
        let weak = cx.entity().downgrade();
        let armed = recording.clone();
        subscriptions.push(cx.intercept_keystrokes(move |event, _window, cx| {
            let Some(id) = armed.get() else {
                return;
            };
            cx.stop_propagation();
            match event.keystroke.key.as_str() {
                "escape" => {}
                "backspace" | "delete" => keymap::assign(cx, id, None),
                _ => keymap::assign(cx, id, Some(event.keystroke.unparse())),
            }
            armed.set(None);
            if let Some(view) = weak.upgrade() {
                view.update(cx, |_, cx| cx.notify());
            }
        }));

        Self {
            item_id: ItemId::next(),
            focus_handle: cx.focus_handle(),
            font_names: fonts,
            font_open: false,
            font_query,
            opacity,
            opacity_settled: Rc::new(Cell::new(false)),
            pickers: Rc::new(pickers),
            width: Rc::new(Cell::new(px(f32::MAX))),
            page: Page::Interface,
            recording,
            keys_find,
            _subscriptions: subscriptions,
        }
    }

}

/// One row of a page: a label and what it does, and the control that sets it.
struct Row {
    label: &'static str,
    description: &'static str,
    control: AnyElement,
    /// The control takes the row's whole width under its label -- a palette
    /// list, a grid of colours -- instead of sitting beside it.
    wide: bool,
}

fn row(label: &'static str, description: &'static str, control: impl IntoElement) -> Row {
    Row {
        label,
        description,
        control: control.into_any_element(),
        wide: false,
    }
}

fn wide(label: &'static str, description: &'static str, control: impl IntoElement) -> Row {
    Row {
        wide: true,
        ..row(label, description, control)
    }
}

/// The colours every control is drawn in, read once per frame.
#[derive(Clone, Copy)]
struct Ink {
    border: Hsla,
    fg: Hsla,
    fg_muted: Hsla,
    fg_subtle: Hsla,
    bg: Hsla,
    raised: Hsla,
    accent: Hsla,
    micro: Pixels,
}

impl Ink {
    fn read(cx: &App) -> Self {
        let t = cx.global::<Theme>();
        Self {
            border: t.border,
            fg: t.fg,
            fg_muted: t.fg_muted,
            fg_subtle: t.fg_subtle,
            bg: t.palette.bg,
            raised: t.raised,
            accent: t.accent,
            micro: kit::micro(t),
        }
    }
}

/// The design's choice control: a row of boxed words, the chosen one solid.
/// Every setting with a handful of values is one of these -- the bar's edge,
/// the theme, a column width, on and off -- so the page reads as one kind of
/// thing rather than switches, dropdowns and radios side by side.
fn segmented(
    id: &'static str,
    options: &[(&'static str, &'static str)],
    current: &str,
    set: fn(&mut Settings, &str),
    ink: Ink,
) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .children(options.iter().enumerate().map(|(ix, (value, label))| {
            let chosen = *value == current;
            let value = *value;
            div()
                .id((id, ix))
                .px(px(16.))
                .py(px(6.))
                .border_1()
                .border_color(if chosen { ink.fg } else { ink.border })
                // One hairline between neighbours, not two: every box after
                // the first gives up its left edge, and nothing is drawn over
                // anything -- an overlap let the next box's edge paint over
                // the chosen one's.
                .when(ix > 0, |this| this.border_l_0())
                .bg(if chosen { ink.fg } else { gpui::transparent_black() })
                .text_size(ink.micro)
                .text_color(if chosen { ink.bg } else { ink.fg_muted })
                .hover(move |this| if chosen { this } else { this.text_color(ink.fg) })
                .child(kit::label(label))
                .on_click(move |_: &ClickEvent, _, cx| {
                    Settings::update(cx, |settings| set(settings, value));
                })
        }))
        .into_any_element()
}

/// On and off, drawn the design's way: a two-word choice.
fn toggle(id: &'static str, on: bool, set: fn(&mut Settings, bool), ink: Ink) -> AnyElement {
    div()
        .flex()
        .children([(true, "on"), (false, "off")].into_iter().enumerate().map(
            move |(ix, (value, label))| {
                let chosen = value == on;
                div()
                    .id((id, ix))
                    .px(px(16.))
                    .py(px(6.))
                    .border_1()
                    .border_color(if chosen { ink.fg } else { ink.border })
                    .when(ix > 0, |this| this.border_l_0())
                    .bg(if chosen { ink.fg } else { gpui::transparent_black() })
                    .text_size(ink.micro)
                    .text_color(if chosen { ink.bg } else { ink.fg_muted })
                    .child(kit::label(label))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        Settings::update(cx, |settings| set(settings, value));
                    })
            },
        ))
        .into_any_element()
}

/// The design's number control: minus, the value, plus. Clamped by
/// `Settings::clamp` on the way in, like every other change.
fn stepper(
    id: &'static str,
    value: f32,
    step: f32,
    unit: &'static str,
    set: fn(&mut Settings, f32),
    ink: Ink,
) -> AnyElement {
    // The minus and plus look like small boxes and click like big ones: each
    // box sits in a transparent margin that takes the press too, laid out
    // with a matching negative margin so the row is no wider than it looks.
    const REACH: f32 = 8.;
    let button = move |ix: usize, text: &'static str, delta: f32, left: bool| {
        // The reach goes on the outer side and above and below -- never
        // between the box and the value, which the design joins.
        div()
            .id((id, ix))
            .py(px(REACH))
            .my(px(-REACH))
            .when(left, |this| this.pl(px(REACH)).ml(px(-REACH)))
            .when(!left, |this| this.pr(px(REACH)).mr(px(-REACH)))
            .group("step")
            .on_click(move |_: &ClickEvent, _, cx| {
                Settings::update(cx, |settings| set(settings, value + delta));
            })
            .child(
                div()
                    .min_w(px(32.))
                    .px(px(12.))
                    .py(px(6.))
                    .flex()
                    .justify_center()
                    .border_1()
                    // The edge it shares with the value box is the value
                    // box's to draw, in its own colour.
                    .when(left, |this| this.border_r_0())
                    .when(!left, |this| this.border_l_0())
                    .border_color(ink.border)
                    .text_size(ink.micro)
                    .text_color(ink.fg_muted)
                    .group_hover("step", move |this| this.text_color(ink.fg))
                    .child(text),
            )
    };
    div()
        .flex()
        .items_center()
        .child(button(0, "-", -step, true))
        .child(
            div()
                .min_w(px(72.))
                .px(px(12.))
                .py(px(6.))
                .flex()
                .justify_center()
                // All four of its own edges; nothing overlaps it, so none
                // of them is painted over.
                .border_1()
                .border_color(ink.fg)
                .text_size(ink.micro)
                .text_color(ink.fg)
                .child(format!("{value}{unit}")),
        )
        .child(button(2, "+", step, false))
        .into_any_element()
}

impl SettingsView {
    /// The font choice: a box naming the family in use, and on click an
    /// opaque list of every installed monospace family -- each drawn in
    /// itself -- with a find line over it.
    fn font_picker(&self, cx: &mut Context<Self>, ink: Ink) -> AnyElement {
        let current = cx.global::<Settings>().font_family.clone();
        let shown = if current.is_empty() { AUTOMATIC.to_string() } else { current.clone() };
        let open = self.font_open;
        let query = self.font_query.read(cx).value().to_lowercase();
        let row_h = kit::row(cx.global::<Theme>());

        let button = div()
            .id("font-button")
            .w(px(300.))
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py(px(6.))
            .border_1()
            .border_color(if open { ink.fg_subtle } else { ink.border })
            .text_color(ink.fg)
            .hover(move |this| this.border_color(ink.fg_subtle))
            .child(div().truncate().child(shown))
            .child(div().text_color(ink.fg_subtle).child(if open { "\u{25b4}" } else { "\u{25be}" }))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.font_open = !this.font_open;
                if this.font_open {
                    this.font_query.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                }
                cx.notify();
            }));

        let list = open.then(|| {
            let rows = self
                .font_names
                .iter()
                .filter(|name| query.is_empty() || name.to_lowercase().contains(&query))
                .enumerate()
                .map(|(ix, name)| {
                    let chosen = if current.is_empty() {
                        name.as_ref() == AUTOMATIC
                    } else {
                        name.as_ref() == current
                    };
                    let family = if name.as_ref() == AUTOMATIC { String::new() } else { name.to_string() };
                    div()
                        .id(("font-row", ix))
                        .relative()
                        .flex_none()
                        .flex()
                        .items_center()
                        .h(row_h)
                        .px_3()
                        .text_color(if chosen { ink.fg } else { ink.fg_muted })
                        .when(name.as_ref() != AUTOMATIC, |this| this.font_family(name.clone()))
                        .when(chosen, |this| {
                            this.bg(ink.raised).child(
                                div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(ink.accent),
                            )
                        })
                        .hover(move |this| this.bg(ink.raised).text_color(ink.fg))
                        .child(div().truncate().child(name.clone()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let family = family.clone();
                            Settings::update(cx, |settings| settings.font_family = family);
                            this.font_open = false;
                            cx.notify();
                        }))
                })
                .collect::<Vec<_>>();
            gpui::deferred(
                gpui::anchored().snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id("font-menu")
                        .w(px(300.))
                        .h(px(320.))
                        .mt_1()
                        .flex()
                        .flex_col()
                        .bg(ink.bg)
                        .border_1()
                        .border_color(ink.fg_subtle)
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.font_open = false;
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            if event.keystroke.key == "escape" {
                                this.font_open = false;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .border_b_1()
                                .border_color(ink.border)
                                .child(div().text_size(ink.micro).text_color(ink.fg_subtle).child(kit::label("find")))
                                .child(div().flex_1().child(Input::new(&self.font_query).appearance(false).xsmall())),
                        )
                        .child(
                            div()
                                .id("font-list")
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
            )
        });

        div().flex().flex_col().child(button).children(list).into_any_element()
    }

    fn interface_rows(&self, window: &mut Window, cx: &mut Context<Self>, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>().clone();
        let opacity = self.opacity.clone();
        // The slider positions its thumb from bounds it records during
        // prepaint without asking for a redraw, so the first frame it is on
        // screen its thumb sits at the left whatever the value. One extra
        // frame, once, lets it catch up.
        if !self.opacity_settled.replace(true) {
            window.request_animation_frame();
        }
        let configured = s.plugins_dir.clone();
        let plugins_dir = crate::plugins::resolve_dir(&configured);
        let plugins = crate::plugins::catalog().entries().len();
        vec![
            row(
                "Navigation bar edge",
                "Which side of the window the one bar lives on. On a side it drops the clock and the host name.",
                segmented(
                    "edge",
                    &[("top", "top"), ("bottom", "bottom"), ("left", "left"), ("right", "right")],
                    &s.nav_edge,
                    |s, v| s.nav_edge = v.into(),
                    ink,
                ),
            ),
            row(
                "Navigation bar",
                "Auto-hide slides it in over the windows while the pointer is at its edge. Hidden or not, that edge still moves the window.",
                segmented(
                    "nav-mode",
                    &[("show", "show"), ("auto", "auto-hide"), ("hide", "hide")],
                    &s.nav_mode,
                    |s, v| s.nav_mode = v.into(),
                    ink,
                ),
            ),
            row(
                "Window buttons",
                "Minimise, maximise and close in the bar. Off, Alt+F4 and the taskbar still close the window.",
                toggle("window-buttons", s.window_buttons, |s, v| s.window_buttons = v, ink),
            ),
            row(
                "Font family",
                "Any monospace font installed here. One family for chrome and grid.",
                self.font_picker(cx, ink),
            ),
            row(
                "Font size",
                "Chrome and terminal scale together. Open terminals reflow.",
                stepper("font-size", s.font_size, 1., "", |s, v| s.font_size = v, ink),
            ),
            row(
                "Default column width",
                "What a new column takes on the strip.",
                segmented(
                    "width",
                    &[("third", "1/3"), ("half", "1/2"), ("two_thirds", "2/3"), ("full", "full")],
                    &s.default_width,
                    |s, v| s.default_width = v.into(),
                    ink,
                ),
            ),
            row(
                "Widest column",
                "With more than one column, none is drawn wider than this share of the window, so the next one always peeks in and its edge can be grabbed. A lone column fills the window regardless.",
                stepper("max-width", s.max_column_width, 5., "%", |s, v| s.max_column_width = v, ink),
            ),
            row(
                "Window opacity",
                "How much of the ground is painted. Text, borders and menus stay solid.",
                div()
                    .w(px(300.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(Slider::new(&opacity)))
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .text_right()
                            .text_color(ink.fg)
                            .child(format!("{:.0}%", s.opacity)),
                    ),
            ),
            // Directly under opacity, because it only means something below
            // 100%. It was on Appearance, a page away from the one setting it
            // depends on, and read as missing.
            row(
                "Blur behind",
                if s.opacity < 100. {
                    "Frosted glass behind the translucent window, where the desktop supports it."
                } else {
                    "Frosted glass behind the window -- shows once opacity is below 100%."
                },
                toggle("blur", s.blur, |s, v| s.blur = v, ink),
            ),
            row(
                "On launch",
                "Home asks for nothing. Last session reopens the layout you left -- a remote one still needs its password, which is never stored. Local goes straight to a shell here.",
                segmented(
                    "startup",
                    &[("home", "home"), ("resume", "last session"), ("local", "local")],
                    &s.startup,
                    |s, v| s.startup = v.into(),
                    ink,
                ),
            ),
            row(
                "Plugins folder",
                "Where file-type support is read from. Only the index is read at launch.",
                // The path on a line of its own, the count and the button
                // under it. All three on one 300px line left a normal
                // `C:\Users\...` path a few characters, which ran into the
                // count. Shortened from the left, because the end of a path is
                // the part that says which folder it is.
                div()
                    .w(px(300.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(ink.fg_muted)
                            .child(tail_to_fit(
                                &plugins_dir.to_string_lossy(),
                                // Monospace: an advance is ~0.6 of the size.
                                (300. / (s.font_size * 0.62)) as usize,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ink.micro)
                                    .text_color(ink.fg_subtle)
                                    .child(kit::label(&format!("{plugins} installed"))),
                            )
                            .child(
                                div()
                                    .id("plugins-choose")
                                    .flex_none()
                                    .px_2()
                                    .py(px(4.))
                                    .border_1()
                                    .border_color(ink.border)
                                    .text_size(ink.micro)
                                    .text_color(ink.fg_muted)
                                    .hover(move |this| this.text_color(ink.fg))
                                    .child(kit::label("choose"))
                                    .on_click(|_: &ClickEvent, _, cx| choose_plugins_folder(cx)),
                            ),
                    ),
            ),
        ]
    }

    fn appearance_rows(&self, cx: &App, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>();
        let pickers = self.pickers.clone();
        let overrides = s.colors.len();
        let ansi: Vec<_> = self.pickers[COLOR_TOKENS.len()..].to_vec();
        vec![
            wide(
                "Palette",
                "System follows the desktop's light or dark; Ink and Paper are the design's own; the rest are guests in the same layout. Picking one clears your colour overrides.",
                palette_list(&self.pickers, cx, ink),
            ),
            wide(
                "Colours",
                "Override any one colour of the palette. A dot marks the ones you have changed.",
                colour_grid(&self.pickers, cx, ink),
            ),
            wide(
                "Terminal palette",
                "The 16 colours programs draw with, normal over bright. A program can still set its own.",
                ansi_grid(&ansi, cx, ink),
            ),
            row(
                "Reset colours",
                "Back to the palette's own colours.",
                div()
                    .id("reset-colors")
                    .px(px(16.))
                    .py(px(6.))
                    .border_1()
                    .border_color(ink.border)
                    .text_size(ink.micro)
                    .text_color(if overrides == 0 { ink.fg_subtle } else { ink.fg })
                    .child(kit::label(&if overrides == 0 {
                        "nothing to reset".to_string()
                    } else {
                        format!("reset {overrides}")
                    }))
                    .when(overrides > 0, |this| {
                        this.hover(move |this| this.border_color(ink.fg_subtle))
                            .on_click(move |_: &ClickEvent, window, cx| {
                                Settings::update(cx, |settings| settings.colors.clear());
                                sync_pickers(&pickers, window, cx);
                            })
                    }),
            ),
            row(
                "Font weight",
                "How heavy the one family is drawn, chrome and grid alike.",
                segmented(
                    "weight",
                    WEIGHTS,
                    &s.font_weight.to_string(),
                    |s, v| s.font_weight = v.parse().unwrap_or(400),
                    ink,
                ),
            ),
            row(
                "Line height",
                "Row height as a share of the font size. Box drawing joins up around 130-150.",
                stepper("line-height", s.line_height, 5., "%", |s, v| s.line_height = v, ink),
            ),
            row(
                "Ligatures",
                "Draw -> and != as single glyphs in terminals, for fonts that have them.",
                toggle("ligatures", s.ligatures, |s, v| s.ligatures = v, ink),
            ),
            row(
                "Focus ring",
                "Outline the focused field in the accent. Off, it keeps the ordinary hairline.",
                toggle("focus-ring", s.focus_ring, |s, v| s.focus_ring = v, ink),
            ),
            wide("Preview", "", font_preview(cx)),
        ]
    }

    fn terminal_rows(cx: &App, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>();
        // Probed when the page is drawn rather than listed in source: offering
        // a shell that is not installed opens a tab that dies at once.
        let shells = crate::local::shells();
        let current = s.local_shell.clone();
        let shell_choice = div()
            .flex()
            .flex_wrap()
            .children(
                std::iter::once((String::new(), "default".to_string()))
                    .chain(shells.into_iter().map(|shell| (shell.command, shell.label)))
                    .enumerate()
                    .map(|(ix, (command, label))| {
                        let chosen = command == current;
                        div()
                            .id(("shell", ix))
                            .px(px(16.))
                            .py(px(6.))
                            .border_1()
                            .border_color(if chosen { ink.fg } else { ink.border })
                            .when(ix > 0, |this| this.border_l_0())
                            .bg(if chosen { ink.fg } else { gpui::transparent_black() })
                            .text_size(ink.micro)
                            .text_color(if chosen { ink.bg } else { ink.fg_muted })
                            .child(kit::label(&label))
                            .on_click(move |_: &ClickEvent, _, cx| {
                                let command = command.clone();
                                Settings::update(cx, |settings| settings.local_shell = command);
                            })
                    }),
            );
        vec![
            row(
                "Cursor shape",
                "When a program has not asked for one. vim switches to a bar in insert mode by itself.",
                segmented(
                    "cursor",
                    &[("block", "block"), ("bar", "bar"), ("underline", "underline")],
                    &s.cursor_shape,
                    |s, v| s.cursor_shape = v.into(),
                    ink,
                ),
            ),
            row(
                "Local shell",
                "What a local terminal runs; only shells found here are offered. Remote tabs start the account's own.",
                shell_choice,
            ),
            row(
                "Scrollback",
                "Lines of history kept per terminal.",
                stepper(
                    "scrollback",
                    s.scrollback as f32,
                    1000.,
                    "",
                    |s, v| s.scrollback = v.max(0.) as usize,
                    ink,
                ),
            ),
            row(
                "Padding",
                "Space between a terminal's edge and its text, on every side. Open terminals reflow.",
                stepper(
                    "terminal-padding",
                    s.terminal_padding,
                    2.,
                    "px",
                    |s, v| s.terminal_padding = v,
                    ink,
                ),
            ),
            row(
                "Copy on select",
                "Copy to the clipboard the moment a selection ends.",
                toggle("copy-on-select", s.copy_on_select, |s, v| s.copy_on_select = v, ink),
            ),
        ]
    }

    fn tiling_rows(cx: &App, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>();
        vec![
            row(
                "Animate",
                "The strip slides, columns grow and the overlays rise instead of cutting. Off, everything lands the frame it changes.",
                toggle("animate", s.animations, |s, v| s.animations = v, ink),
            ),
            row(
                "Duration",
                "How long a move takes. Much past 200 ms and the app feels like it is thinking.",
                stepper("duration", s.animation_ms, 10., " ms", |s, v| s.animation_ms = v, ink),
            ),
            row(
                "Curve",
                "Ease out settles; linear does not ease; spring overshoots a little and comes back.",
                segmented(
                    "curve",
                    &[("ease", "ease out"), ("linear", "linear"), ("spring", "spring")],
                    &s.animation_curve,
                    |s, v| s.animation_curve = v.into(),
                    ink,
                ),
            ),
            row(
                "Window headers",
                "Each window's number, title and close button, above or below what it shows.",
                segmented(
                    "header-position",
                    &[("top", "top"), ("bottom", "bottom")],
                    &s.header_position,
                    |s, v| s.header_position = v.into(),
                    ink,
                ),
            ),
            row(
                "Header visibility",
                "Auto-hide draws a header over its window while the pointer is on it, so nothing resizes. Hidden, a window is moved and closed from the keyboard.",
                segmented(
                    "header-mode",
                    &[("show", "show"), ("auto", "auto-hide"), ("hide", "hide")],
                    &s.header_mode,
                    |s, v| s.header_mode = v.into(),
                    ink,
                ),
            ),
        ]
    }

    fn files_rows(cx: &App, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>();
        vec![
            row(
                "Show hidden files",
                "Dotfiles such as .bashrc and .config.",
                toggle("hidden", s.show_hidden, |s, v| s.show_hidden = v, ink),
            ),
            row(
                "Open on connect",
                "Dock the file tree as soon as a connection succeeds. Otherwise ctrl+shift+b.",
                toggle("open-on-connect", s.explorer_on_connect, |s, v| s.explorer_on_connect = v, ink),
            ),
        ]
        .into_iter()
        // Windows only. The switch shows what the registry says, not what
        // was last saved, so a menu removed by hand reads truthfully; a
        // failed write leaves it off.
        .chain(crate::shell_menu::SUPPORTED.then(|| {
            row(
                "File Explorer menu",
                "\"Open with PulseTerm\" on files and folders: a file opens in the editor, a folder in a shell there. For this user only; on Windows 11 under Show more options.",
                toggle(
                    "explorer-menu",
                    crate::shell_menu::is_registered(),
                    // `set` first: it has to run for off too.
                    |s, v| s.explorer_menu = crate::shell_menu::set(v).is_ok() && v,
                    ink,
                ),
            )
        }))
        .collect()
    }

    fn editor_rows(cx: &App, ink: Ink) -> Vec<Row> {
        let s = cx.global::<Settings>();
        vec![
            row(
                "Line numbers",
                "",
                toggle("line-numbers", s.editor_line_numbers, |s, v| s.editor_line_numbers = v, ink),
            ),
            row(
                "Soft wrap",
                "Wrap long lines at the window's edge instead of scrolling sideways.",
                toggle("soft-wrap", s.editor_soft_wrap, |s, v| s.editor_soft_wrap = v, ink),
            ),
            row(
                "Tab size",
                "Applies to files opened from now on.",
                segmented(
                    "tab-size",
                    &[("2", "2"), ("4", "4"), ("8", "8")],
                    &s.editor_tab_size.to_string(),
                    |s, v| s.editor_tab_size = v.parse().unwrap_or(4),
                    ink,
                ),
            ),
        ]
    }

    /// The Keys page (frame 12): find, the counts, restore defaults, then
    /// every command by group with its keys. Click a row to record a new
    /// chord for it.
    fn keys_page(&self, cx: &mut Context<Self>, ink: Ink, surface: Hsla) -> Vec<AnyElement> {
        let keys = cx.global::<Keys>().clone();
        let query = self.keys_find.read(cx).value().to_lowercase();
        let recording = self.recording.get();
        let changed = COMMANDS.iter().filter(|c| keys.is_changed(c)).count();
        let conflicts = keys.conflicts();

        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .py(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .max_w(px(280.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .border_1()
                    .border_color(ink.border)
                    .child(div().text_size(ink.micro).text_color(ink.fg_subtle).child(kit::label("find")))
                    .child(div().flex_1().child(Input::new(&self.keys_find).appearance(false).xsmall())),
            )
            .child(
                div()
                    .px_2()
                    .py(px(4.))
                    .border_1()
                    .border_color(if conflicts > 0 { ink.accent } else { ink.border })
                    .text_size(ink.micro)
                    .text_color(if conflicts > 0 { ink.accent } else { ink.fg_subtle })
                    .child(kit::label(&format!("conflicts {conflicts}"))),
            )
            .child(
                div()
                    .px_2()
                    .py(px(4.))
                    .border_1()
                    .border_color(if changed > 0 { ink.fg } else { ink.border })
                    .text_size(ink.micro)
                    .text_color(if changed > 0 { ink.fg } else { ink.fg_subtle })
                    .child(kit::label(&format!("changed {changed}"))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("keys-restore")
                    .px_2()
                    .py(px(4.))
                    .border_1()
                    .border_color(ink.border)
                    .text_size(ink.micro)
                    .text_color(ink.fg_muted)
                    .hover(move |this| this.text_color(ink.fg))
                    .child(kit::label("restore defaults"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.recording.set(None);
                        keymap::reset(cx, None);
                        cx.notify();
                    })),
            );

        let matches = |label: &str, id: &str, list: &[String]| {
            query.is_empty()
                || label.to_lowercase().contains(&query)
                || id.contains(&query)
                || list.iter().any(|k| k.contains(&query))
        };

        let mut out = vec![toolbar.into_any_element()];
        for group in ["tiling", "window", "session", "text & view"] {
            let mut rows: Vec<AnyElement> = Vec::new();
            for command in COMMANDS.iter().filter(|c| c.group == group) {
                let list = keys.keys_for(command);
                if !matches(command.label, command.id, &list) {
                    continue;
                }
                let is_changed = keys.is_changed(command);
                let is_recording = recording == Some(command.id);
                let id = command.id;
                // What the row is doing goes on its second line, under the
                // label, so the right side only ever holds keys: in a narrow
                // window that is the half there is no room to lose.
                let note: Option<(String, Hsla)> = if is_recording {
                    Some(("recording \u{b7} esc cancels \u{b7} delete unbinds".into(), ink.accent))
                } else if is_changed {
                    Some((
                        format!(
                            "changed \u{b7} default {}",
                            command.defaults.first().map(|k| k.replace('-', " ")).unwrap_or_default()
                        ),
                        ink.fg_subtle,
                    ))
                } else {
                    None
                };
                let right: AnyElement = if is_recording {
                    div()
                        .px_2()
                        .py(px(2.))
                        .border_1()
                        .border_color(ink.accent)
                        .text_size(ink.micro)
                        .text_color(ink.accent)
                        .child(kit::label("press keys"))
                        .into_any_element()
                } else if list.is_empty() {
                    div()
                        .px_2()
                        .py(px(2.))
                        .border_1()
                        .border_color(ink.border)
                        .text_size(ink.micro)
                        .text_color(ink.fg_subtle)
                        .child(kit::label("not bound \u{2014} click to set"))
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(chords(&list, ink, true))
                        .when(is_changed, |this| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("keys-reset-{id}")))
                                    .text_size(ink.micro)
                                    .text_color(ink.fg_subtle)
                                    .hover(move |this| this.text_color(ink.fg))
                                    .child(kit::label("reset"))
                                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        keymap::reset(cx, Some(id));
                                        cx.notify();
                                    })),
                            )
                        })
                        .into_any_element()
                };
                rows.push(
                    div()
                        .id(SharedString::from(format!("keys-row-{id}")))
                        .flex()
                        .items_center()
                        .gap_4()
                        .px(px(12.))
                        .py(px(6.))
                        .border_b_1()
                        .border_color(ink.border)
                        .when(is_changed || is_recording, |this| this.bg(ink.raised))
                        .hover(move |this| this.bg(ink.raised))
                        // The label and its id stacked, giving way before the
                        // keys do: the keys are what the row is for.
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_color(ink.fg)
                                        .when(is_changed, |this| {
                                            this.font_weight(FontWeight::SEMIBOLD)
                                                .child(div().size(px(5.)).flex_none().bg(ink.accent))
                                        })
                                        .child(div().min_w_0().truncate().child(command.label)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(ink.micro)
                                        .text_color(ink.fg_subtle)
                                        .child(command.id),
                                )
                                .children(note.map(|(text, color)| {
                                    div()
                                        .truncate()
                                        .text_size(ink.micro)
                                        .text_color(color)
                                        .child(kit::label(&text))
                                })),
                        )
                        .child(div().flex_none().child(right))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.recording.set(Some(id));
                            window.focus(&this.focus_handle);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            if group == "text & view" {
                for (label, id, list) in FIXED {
                    if !matches(label, id, &[list.to_string()]) {
                        continue;
                    }
                    rows.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .px(px(12.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(ink.border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().truncate().text_color(ink.fg_muted).child(*label))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(ink.micro)
                                            .text_color(ink.fg_subtle)
                                            .child(*id),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(div().text_size(ink.micro).text_color(ink.fg_subtle).child(kit::label("fixed")))
                                    .child(chords(
                                        &list.split(" / ").map(str::to_string).collect::<Vec<_>>(),
                                        ink,
                                        false,
                                    )),
                            )
                            .into_any_element(),
                    );
                }
            }
            if rows.is_empty() {
                continue;
            }
            out.push(
                div()
                    .mt(px(12.))
                    .px(px(12.))
                    .py(px(6.))
                    .bg(surface)
                    .text_size(ink.micro)
                    .text_color(ink.fg_muted)
                    .child(kit::label(group))
                    .into_any_element(),
            );
            out.extend(rows);
        }
        out
    }
}

impl SettingsView {
    /// Where the section list's right edge falls, while it is a sidebar --
    /// so the window header above can carry the same hairline up to the top
    /// of the window instead of the line stopping short under the header.
    pub fn sidebar_edge(&self) -> Option<f32> {
        (f32::from(self.width.get()) >= TABS_BELOW).then_some(SIDEBAR)
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = Ink::read(cx);
        let head = kit::head(cx.global::<Theme>());
        let tabs = f32::from(self.width.get()) < TABS_BELOW;
        // Rows go side by side only when the *page* has room, not the tab: a
        // 260px sidebar beside an 840px tab leaves the rows under 500px, and
        // a label and a four-way choice do not share that.
        let page_width =
            f32::from(self.width.get()) - if tabs { 0. } else { SIDEBAR } - 112.;
        let narrow = page_width < NARROW_ROWS;
        let width = self.width.clone();
        let page = self.page;

        let surface = cx.global::<Theme>().palette.surface;
        let keys_page = (page == Page::Keys).then(|| self.keys_page(cx, ink, surface));
        let rows = match page {
            Page::Interface => self.interface_rows(window, cx, ink),
            Page::Appearance => self.appearance_rows(cx, ink),
            Page::Terminal => Self::terminal_rows(cx, ink),
            Page::Tiling => Self::tiling_rows(cx, ink),
            Page::Files => Self::files_rows(cx, ink),
            Page::Editor => Self::editor_rows(cx, ink),
            Page::Keys => Vec::new(),
        };
        let count = if page == Page::Keys {
            COMMANDS.len() + FIXED.len()
        } else {
            rows.len()
        };
        let noun = if page == Page::Keys { "command" } else { "setting" };
        let number = Page::ALL.iter().position(|p| *p == page).unwrap_or(0) + 1;

        // The section list: the chosen one raised, with the accent rule on its
        // leading edge -- the same mark a focused window header carries.
        let sidebar = div()
            .flex_none()
            .flex()
            .when(tabs, |this| this.flex_row().flex_wrap().w_full().border_b_1())
            .when(!tabs, |this| {
                this.w(px(SIDEBAR)).h_full().flex_col().border_r_1().pt(px(24.))
            })
            .border_color(ink.border)
            .children(Page::ALL.iter().enumerate().map(|(ix, p)| {
                let chosen = *p == page;
                let target = *p;
                div()
                    .id(("settings-page", ix))
                    .px(px(22.))
                    .py(px(8.))
                    .border_l_2()
                    .border_color(if chosen { ink.accent } else { gpui::transparent_black() })
                    .when(chosen, |this| this.bg(ink.raised))
                    .text_color(if chosen { ink.fg } else { ink.fg_muted })
                    .when(chosen, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .hover(move |this| this.text_color(ink.fg))
                    .child(p.name())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.page = target;
                        this.recording.set(None);
                        cx.notify();
                    }))
            }))
            .when(!tabs, |this| this.child(div().flex_1()))
            .when(!tabs, |this| this.child(
                div()
                    .px(px(22.))
                    .pb(px(20.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(ink.micro)
                    .text_color(ink.fg_subtle)
                    .child(kit::label("config"))
                    .child(
                        div()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            // From the left, like the plugins folder: the end
                            // of the path is the part that names it.
                            .child(tail_to_fit(
                                &crate::store::data_dir().to_string_lossy(),
                                ((SIDEBAR - 44.) / (f32::from(ink.micro) * 0.62)) as usize,
                            )),
                    ),
            ));

        let body = div()
            .id("settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px(px(if narrow { 24. } else { 56. }))
            .pt(px(36.))
            .pb(px(48.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pb(px(18.))
                    .border_b_1()
                    .border_color(ink.border)
                    .child(
                        div()
                            .text_size(head * 1.5)
                            .font_weight(FontWeight::BOLD)
                            .text_color(ink.fg)
                            .child(page.name().to_uppercase()),
                    )
                    .child(
                        div()
                            .text_size(ink.micro)
                            .text_color(ink.fg_subtle)
                            .child(kit::label(&format!(
                                "{number:02} \u{2014} {count} {noun}{}",
                                if count == 1 { "" } else { "s" }
                            ))),
                    ),
            )
            .children(keys_page.into_iter().flatten())
            .children(rows.into_iter().map(|row| {
                let stacked = narrow || row.wide;
                div()
                    .flex()
                    .when(stacked, |this| this.flex_col().gap_3())
                    .when(!stacked, |this| this.items_center().gap_6())
                    .py(px(20.))
                    .border_b_1()
                    .border_color(ink.border)
                    .child(
                        div()
                            // Grows along the row, never down it: in a
                            // stacked row `flex_1` took spare height and left
                            // a gap under the label.
                            .when(!stacked, |this| this.flex_1())
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(ink.fg)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(row.label),
                            )
                            .when(!row.description.is_empty(), |this| {
                                this.child(div().text_color(ink.fg_subtle).child(row.description))
                            }),
                    )
                    .child(
                        div()
                            .when(row.wide, |this| this.w_full())
                            .when(!row.wide, |this| this.flex_none().max_w_full())
                            .child(row.control),
                    )
            }));

        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            // Escape closes the font list wherever the keyboard is in this
            // page -- in its find line or on the page itself. On the list
            // alone it only heard keys while the find line had focus.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.font_open && event.keystroke.key == "escape" {
                    this.font_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .relative()
            .size_full()
            .flex()
            .when(tabs, |this| this.flex_col())
            .when(!tabs, |this| this.flex_row())
            .child(sidebar)
            .child(body.when(tabs, |this| this.w_full().min_h_0().flex_1()))
            // Measures the tab for the next frame's layout choice, and asks for
            // that frame only when the choice flips.
            .child(
                canvas(
                    move |bounds, window, _| {
                        // Several layout choices hang off this width, so any
                        // change asks for the frame that applies them. It
                        // only changes while something is being resized.
                        let changed = (f32::from(width.get()) - f32::from(bounds.size.width)).abs() > 0.5;
                        width.set(bounds.size.width);
                        if changed {
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}

/// Picks a plugins folder and switches to it. The catalogue is reinstalled on
/// the spot, so a folder chosen here is in force without a restart.
/// The end of a path, kept within `chars` characters: `…\PulseTerm\plugins`.
/// gpui truncates only from the right, which keeps the drive and drops the
/// folder's own name -- the one part worth seeing.
fn tail_to_fit(path: &str, chars: usize) -> String {
    let count = path.chars().count();
    if count <= chars || chars < 2 {
        return path.to_string();
    }
    let tail: String = path.chars().skip(count - (chars - 1)).collect();
    format!("…{tail}")
}

fn choose_plugins_folder(cx: &mut App) {
    let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(dir) = paths.into_iter().next() else {
            return;
        };
        let _ = cx.update(|cx| {
            Settings::update(cx, |settings| {
                settings.plugins_dir = dir.to_string_lossy().to_string();
            });
            crate::plugins::install_from_settings(cx);
        });
    })
    .detach();
}

/// Pickers hold their own copy of the colour, so after anything that changes
/// colours wholesale -- a theme switch, a reset -- they are pointed back at the
/// theme. `set_value` doesn't emit a change, so this can't loop back into
/// settings as a pile of overrides.
fn sync_pickers(pickers: &Pickers, window: &mut Window, cx: &mut App) {
    let palette = cx.global::<Theme>().palette;
    for (token, picker) in pickers.iter() {
        let color = opaque(palette.get(token).unwrap_or_default());
        picker.update(cx, |picker, cx| picker.set_value(color, window, cx));
    }
}

fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1., ..color }
}

/// Every preset as a row, in the app's own list language: a strip of its
/// colours, its name, and whether it is light or dark. The chosen one is
/// raised with the accent rule on its leading edge -- the mark a selected
/// file, a chosen Settings section and a focused window all carry.
fn palette_list(pickers: &Pickers, cx: &App, ink: Ink) -> AnyElement {
    let current = cx.global::<Settings>().theme.clone();
    // Following the desktop is a palette choice like any other, so it sits at
    // the head of the list: Paper's chips and Ink's side by side, because it
    // is whichever of the two the desktop asks for.
    let system = {
        let chosen = current == "system";
        let paper = theme::preset("paper").palette;
        let ink_p = theme::preset("ink").palette;
        let pickers = pickers.clone();
        div()
            .id("palette-system")
            .relative()
            .flex()
            .items_center()
            .gap_4()
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(ink.border)
            .when(chosen, |this| {
                this.bg(ink.raised).child(
                    div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(ink.accent),
                )
            })
            .hover(move |this| this.bg(ink.raised))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .border_1()
                    .border_color(ink.border)
                    .child(
                        div().flex().p(px(3.)).gap(px(2.)).bg(paper.bg).children(
                            [paper.surface, paper.accent, paper.ansi[1], paper.ansi[4]]
                                .map(|color| div().size(px(10.)).bg(color)),
                        ),
                    )
                    .child(
                        div().flex().p(px(3.)).gap(px(2.)).bg(ink_p.bg).children(
                            [ink_p.surface, ink_p.accent, ink_p.ansi[1], ink_p.ansi[4]]
                                .map(|color| div().size(px(10.)).bg(color)),
                        ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if chosen { ink.fg } else { ink.fg_muted })
                    .when(chosen, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child("System"),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ink.micro)
                    .text_color(ink.fg_subtle)
                    .child(kit::label("follows desktop")),
            )
            .on_click(move |_: &ClickEvent, window, cx| {
                Settings::update(cx, |settings| {
                    settings.theme = "system".into();
                    settings.colors.clear();
                });
                sync_pickers(&pickers, window, cx);
            })
    };
    div()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(ink.border)
        .child(system)
        .children(theme::presets().into_iter().enumerate().map(|(ix, preset)| {
            let chosen = preset.id == current;
            let p = preset.palette;
            let id = preset.id;
            let pickers = pickers.clone();
            div()
                .id(("palette", ix))
                .flex()
                .items_center()
                .gap_4()
                .px(px(12.))
                .py(px(8.))
                .relative()
                .border_b_1()
                .border_color(ink.border)
                .when(chosen, |this| {
                    // gpui has one border colour per box, so the accent
                    // rule is a 2px bar of its own on the leading edge.
                    this.bg(ink.raised).child(
                        div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(ink.accent),
                    )
                })
                .hover(move |this| this.bg(ink.raised))
                .child(
                    // The palette's own ground behind its accent and six of
                    // its ANSI colours: enough to tell two dark themes apart.
                    div()
                        .flex()
                        .flex_none()
                        .p(px(3.))
                        .gap(px(2.))
                        .bg(p.bg)
                        .border_1()
                        .border_color(ink.border)
                        .children(
                            [p.surface, p.accent, p.ansi[1], p.ansi[2], p.ansi[3], p.ansi[4], p.ansi[5], p.ansi[6]]
                                .map(|color| div().size(px(10.)).bg(color)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if chosen { ink.fg } else { ink.fg_muted })
                        .when(chosen, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(preset.name),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(ink.micro)
                        .text_color(ink.fg_subtle)
                        .child(kit::label(if preset.light { "light" } else { "dark" })),
                )
                .on_click(move |_: &ClickEvent, window, cx| {
                    Settings::update(cx, |settings| {
                        settings.theme = id.to_string();
                        settings.colors.clear();
                    });
                    sync_pickers(&pickers, window, cx);
                })
        }))
        .into_any_element()
}

/// A colour picker's swatch with a hairline round it, so a dark colour on a
/// dark page -- the ground itself -- is still a visible box.
fn swatch(picker: &Entity<ColorPickerState>, ink: Ink) -> AnyElement {
    div()
        .flex_none()
        .p(px(2.))
        .border_1()
        .border_color(ink.fg_subtle)
        .child(ColorPicker::new(picker).small().anchor(Corner::TopRight))
        .into_any_element()
}

/// The chrome colours as a compact grid -- swatch, name, hex, and a dot when
/// overridden -- rather than a full row each: thirteen rows of one small
/// square apiece was a page of scrolling for what is one table.
fn colour_grid(pickers: &Pickers, cx: &App, ink: Ink) -> AnyElement {
    let s = cx.global::<Settings>();
    let palette = cx.global::<Theme>().palette;
    div()
        .flex()
        .flex_wrap()
        .gap_x_6()
        .children(COLOR_TOKENS.iter().zip(pickers.iter()).map(|((token, label, _), (_, picker))| {
            let changed = s.colors.contains_key(*token);
            let hex = palette.get(token).map(|c| opaque(c).to_hex()).unwrap_or_default();
            div()
                .w(px(232.))
                .flex()
                .items_center()
                .gap_3()
                .py(px(6.))
                .border_b_1()
                .border_color(ink.border)
                .child(swatch(picker, ink))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(ink.fg)
                        .child(*label),
                )
                .when(changed, |this| this.child(div().size(px(5.)).flex_none().bg(ink.accent)))
                .child(
                    div()
                        .flex_none()
                        .text_size(ink.micro)
                        .text_color(ink.fg_subtle)
                        .child(hex.to_uppercase()),
                )
        }))
        .into_any_element()
}

/// The 16 ANSI slots, normal over bright -- the way every terminal documents
/// its palette. Each row wraps, so a narrow tab gets two lines of four rather
/// than eight swatches running off the edge.
fn ansi_grid(pickers: &[(SharedString, Entity<ColorPickerState>)], _cx: &App, ink: Ink) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .children([("normal", 0), ("bright", 8)].map(|(label, start)| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_size(ink.micro).text_color(ink.fg_subtle).child(kit::label(label)))
                .child(div().flex().flex_wrap().gap_3().children((0..8).map(|index| {
                    let (_, picker) = &pickers[start + index];
                    // Plain lower case, not tracked: "magenta" spelled out
                    // letter by letter does not fit under a swatch.
                    div()
                        .w(px(52.))
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap_1()
                        .child(swatch(picker, ink))
                        .child(
                            div()
                                .text_size(ink.micro)
                                .text_color(ink.fg_subtle)
                                .child(ANSI_NAMES[index]),
                        )
                })))
        }))
        .into_any_element()
}

/// A few lines of fake shell session in the current font and colours.
fn font_preview(cx: &App) -> impl IntoElement {
    let t = cx.global::<Theme>();
    let span = |text: &'static str, color: Hsla| div().text_color(color).child(text);
    let line = || div().flex().flex_wrap();

    div()
        .w_full()
        .overflow_hidden()
        .p_3()
        .bg(t.palette.bg)
        .border_1()
        .border_color(t.border)
        .font_family(t.font.clone())
        .font_weight(t.font_weight)
        .text_size(t.font_size)
        .line_height(t.line_height)
        .child(
            line()
                .child(span("rare@orion", t.ansi[2]))
                .child(span(":", t.fg))
                .child(span("~/src", t.ansi[4]))
                .child(span("$ ", t.fg))
                .child(span("ls -la | grep -> != >= 0O 1lI", t.fg)),
        )
        .child(
            line()
                .child(span("drwxr-xr-x ", t.fg_muted))
                .child(span("config  ", t.ansi[4]))
                .child(span("build.sh  ", t.ansi[2]))
                .child(span("error.log  ", t.ansi[1]))
                .child(span("notes.md", t.ansi[3])),
        )
        .child(span("┌─┬─┐ │ ├─┼─┤ │ └─┴─┘  the quick brown fox", t.fg_subtle))
}

/// One or more chords as the design draws them (frame 12): every key of a
/// chord boxed on its own -- CTRL, SHIFT, T -- and chords separated by a
/// slash. Parsed with gpui's own parser, so `ctrl--` is ctrl and minus rather
/// than two dashes.
fn chords(list: &[String], ink: Ink, strong: bool) -> AnyElement {
    let cap = move |text: String| {
        div()
            .px(px(6.))
            .py(px(2.))
            .border_1()
            .border_color(if strong { ink.fg_subtle } else { ink.border })
            .text_size(ink.micro)
            .text_color(if strong { ink.fg } else { ink.fg_muted })
            .child(text)
    };
    // Whole chords wrap, never the keys inside one: wrapping chip by chip
    // broke `CTRL SHIFT TAB` across two lines at a larger font. Each chord is
    // a group that does not wrap, and the `/` rides with the chord after it so
    // it never dangles at the end of a line.
    let mut row = div().flex().flex_wrap().items_center().justify_end().gap_1();
    for (ix, chord) in list.iter().enumerate() {
        let mut group = div().flex().flex_none().items_center().gap_1();
        if ix > 0 {
            group = group.child(div().px_1().text_color(ink.fg_subtle).child("/"));
        }
        match gpui::Keystroke::parse(chord) {
            Err(_) => group = group.child(cap(chord.to_uppercase())),
            Ok(keystroke) => {
                let m = keystroke.modifiers;
                for (on, name) in [
                    (m.control, "CTRL"),
                    (m.alt, "ALT"),
                    (m.shift, "SHIFT"),
                    (m.platform, "SUPER"),
                ] {
                    if on {
                        group = group.child(cap(name.into()));
                    }
                }
                group = group.child(cap(keystroke.key.to_uppercase()));
            }
        }
        row = row.child(group);
    }
    row.into_any_element()
}
