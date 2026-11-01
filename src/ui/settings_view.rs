use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, rems, App, Axis, Context, Corner, Div, Entity, FocusHandle, Focusable,
    Hsla, Pixels, SharedString, Subscription, Window,
};
use gpui_component::button::Button;
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::group_box::GroupBoxVariant;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::setting::{
    NumberFieldOptions, RenderOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsPanel,
};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Colorize as _, Disableable as _, IndexPath, Sizable as _};

use crate::store::Settings;
use crate::theme::{self, Theme, ANSI_NAMES, COLOR_TOKENS};
use crate::workspace::item::ItemId;

type FontSelect = SelectState<SearchableVec<SharedString>>;
type Pickers = Rc<Vec<(SharedString, Entity<ColorPickerState>)>>;

/// The font list's first entry, standing for "no family chosen".
const AUTOMATIC: &str = "Automatic";

// Every size below is in rems, not pixels. The rem follows the font-size
// setting (see `theme::apply_component_theme`), and so does the text inside
// these controls; a pixel width stays put while its text grows, which is how
// "Tokyo Night Storm" came to run off its card at a large font. At the default
// 13px font a rem is about 14.9px.

/// Below this width (the whole tab, sidebar included) every row puts its
/// control under its label instead of beside it. Side by side needs roughly a
/// label column, a `CONTROL_WIDTH_REMS` control, and the page and group padding
/// around them; any narrower and the control is clipped at the group's edge.
const NARROW_BELOW_REMS: f32 = 51.25;

/// Width of a custom control beside its label. Number inputs get a minimum
/// instead: gpui-component sizes them at 8rem, which clips a six-digit value.
const CONTROL_WIDTH_REMS: f32 = 16.25;
const NUMBER_MIN_WIDTH_REMS: f32 = 10.625;

const WEIGHTS: &[(&str, &str)] = &[
    ("300", "Light"),
    ("400", "Regular"),
    ("500", "Medium"),
    ("600", "Semibold"),
    ("700", "Bold"),
];

const SHORTCUTS: &[(&str, &str)] = &[
    ("New terminal, in a new column", "ctrl-shift-t"),
    ("Stack a terminal in this column", "ctrl-shift-d"),
    ("Close window", "ctrl-shift-w"),
    (
        "Focus column left / right",
        "ctrl-shift-left / right, ctrl-tab",
    ),
    ("Focus window up / down", "ctrl-shift-up / down"),
    ("Move column along the strip", "ctrl-shift-alt-left / right"),
    ("Move window within its column", "ctrl-shift-alt-up / down"),
    ("Expel window into its own column", "ctrl-shift-o"),
    ("Consume the next column's window", "ctrl-shift-u"),
    ("Cycle column width", "ctrl-shift-r"),
    ("Connections", "ctrl-shift-k"),
    ("Settings", "ctrl-shift-,"),
    ("Disconnect", "ctrl-shift-q"),
    ("Toggle file explorer", "ctrl-shift-b"),
    ("Save file (in an editor)", "ctrl-s"),
    ("Bigger / smaller font", "ctrl-= / ctrl--"),
    ("Reset font size", "ctrl-0"),
    ("Copy / paste in a terminal", "ctrl-shift-c / ctrl-shift-v"),
    ("Select inside a mouse-driven TUI", "shift + drag"),
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
    fonts: Entity<FontSelect>,
    opacity: Entity<SliderState>,
    /// Whether the opacity slider has had its settling frame; see `window_page`.
    opacity_settled: Rc<Cell<bool>>,
    /// One picker per colour token: the chrome colours in `COLOR_TOKENS` order,
    /// then `ansi0`..`ansi15`.
    pickers: Pickers,
    /// This tab's width as of the last paint, which picks the row layout. A pane
    /// can be narrow in a wide window, so the window size is no guide.
    width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
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
        let current = cx.global::<Settings>().font_family.clone();
        let selected = fonts
            .iter()
            .position(|font| {
                if current.is_empty() {
                    font.as_ref() == AUTOMATIC
                } else {
                    font.as_ref() == current
                }
            })
            .map(IndexPath::new);
        let fonts = cx.new(|cx| {
            SelectState::new(SearchableVec::new(fonts), selected, window, cx).searchable(true)
        });
        subscriptions.push(cx.subscribe_in(
            &fonts,
            window,
            |_, _, event: &SelectEvent<SearchableVec<SharedString>>, _, cx| {
                if let SelectEvent::Confirm(Some(font)) = event {
                    let family = if font.as_ref() == AUTOMATIC {
                        String::new()
                    } else {
                        font.to_string()
                    };
                    Settings::update(cx, |settings| settings.font_family = family);
                }
            },
        ));

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

        Self {
            item_id: ItemId::next(),
            focus_handle: cx.focus_handle(),
            fonts,
            opacity,
            opacity_settled: Rc::new(Cell::new(false)),
            pickers: Rc::new(pickers),
            width: Rc::new(Cell::new(px(f32::MAX))),
            _subscriptions: subscriptions,
        }
    }

    fn appearance_page(&self, axis: Axis) -> SettingPage {
        let chrome = COLOR_TOKENS
            .iter()
            .zip(self.pickers.iter())
            .map(|((_, label, description), (_, picker))| {
                let picker = picker.clone();
                SettingItem::new(
                    *label,
                    SettingField::render(move |_, _, _| {
                        ColorPicker::new(&picker).small().anchor(Corner::TopRight)
                    }),
                )
                .description(*description)
                .layout(axis)
            });
        let ansi: Vec<_> = self.pickers[COLOR_TOKENS.len()..].to_vec();

        SettingPage::new("Appearance")
            .description("Pick a theme, then fine-tune any of its colours. Every change applies immediately.")
            .resettable(false)
            .group(SettingGroup::new().title("Theme").item(SettingItem::render({
                let pickers = self.pickers.clone();
                move |_, _, cx| theme_cards(&pickers, cx)
            })))
            .group(
                SettingGroup::new()
                    .title("Interface colors")
                    .description("Overrides on top of the theme. Picking a theme above clears them.")
                    .item(reset_colors_item(self.pickers.clone(), axis))
                    .item(
                        SettingItem::new(
                            "Show focus ring",
                            switch(|s| s.focus_ring, |s, v| s.focus_ring = v).default_value(true),
                        )
                        .description("Outline the focused pane and text field in the focus-ring colour. Off, they keep the ordinary border.")
                        .layout(axis),
                    )
                    .items(chrome),
            )
            .group(
                SettingGroup::new()
                    .title("Terminal palette")
                    .description("The 16 colours programs draw with. A program can still set its own.")
                    .item(SettingItem::render(move |_, _, cx| ansi_grid(&ansi, cx))),
            )
    }

    fn font_page(&self, axis: Axis) -> SettingPage {
        let fonts = self.fonts.clone();
        let defaults = Settings::default();

        SettingPage::new("Font")
            .description("The font the terminals, the editor and the app's chrome are drawn in.")
            .group(
                SettingGroup::new()
                    .item(
                        SettingItem::new(
                            "Font family",
                            SettingField::render(move |options, _, _| {
                                control(options).child(
                                    Select::new(&fonts)
                                        .small()
                                        .search_placeholder("Search fonts"),
                                )
                            }),
                        )
                        .description("Monospace fonts installed on this PC. Automatic picks Cascadia Mono, JetBrains Mono, Fira Code or Consolas, whichever is there.")
                        .layout(axis),
                    )
                    .item(
                        SettingItem::new(
                            "Font size",
                            number(8., 32., 1., |s| s.font_size as f64, |s, v| s.font_size = v as f32)
                                .default_value(defaults.font_size as f64),
                        )
                        .description("In pixels. Open terminals reflow to the new size.")
                        .layout(axis),
                    )
                    .item(
                        SettingItem::new(
                            "Line height",
                            number(100., 200., 5., |s| s.line_height as f64, |s, v| s.line_height = v as f32)
                                .default_value(defaults.line_height as f64),
                        )
                        .description("Row height as a percentage of the font size. Box-drawing characters in TUIs join up cleanly around 130-150.")
                        .layout(axis),
                    )
                    .item(
                        SettingItem::new(
                            "Font weight",
                            dropdown(WEIGHTS, |s| s.font_weight.to_string(), |s, v| {
                                s.font_weight = v.parse().unwrap_or(400)
                            })
                            .default_value(defaults.font_weight.to_string()),
                        )
                        .layout(axis),
                    )
                    .item(
                        SettingItem::new(
                            "Ligatures",
                            switch(|s| s.ligatures, |s, v| s.ligatures = v)
                                .default_value(defaults.ligatures),
                        )
                        .description("Draw sequences like -> and != as single glyphs in terminals, for fonts that have them (Cascadia Code, Fira Code, JetBrains Mono).")
                        .layout(axis),
                    ),
            )
            .group(
                SettingGroup::new()
                    .title("Preview")
                    .item(SettingItem::render(|_, _, cx| font_preview(cx))),
            )
    }
}

/// Picks a plugins folder and switches to it. The catalogue is reinstalled on
/// the spot, so a folder chosen here is in force without a restart.
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

/// What the app does before you have touched it.
fn general_page(axis: Axis) -> SettingPage {
    let defaults = Settings::default();
    SettingPage::new("General")
        .description("What PulseTerm does when it opens.")
        .group(
            SettingGroup::new()
                .title("Plugins")
                .item(
                    SettingItem::new(
                        "Plugins folder",
                        SettingField::render(move |options, _window, cx| {
                            let configured = cx.global::<Settings>().plugins_dir.clone();
                            let dir = crate::plugins::resolve_dir(&configured);
                            let count = crate::plugins::catalog().entries().len();
                            control(options)
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(dir.to_string_lossy().to_string()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .child(format!("{count} installed")),
                                )
                                .child(
                                    div()
                                        .id("plugins-choose")
                                        .flex_none()
                                        .px_2()
                                        .border_1()
                                        .border_color(cx.global::<Theme>().border)
                                        .child("Choose")
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            |_, _window, cx| choose_plugins_folder(cx),
                                        ),
                                )
                        }),
                    )
                    .description("Where file-type support and other plugins are read from. Only the index is read at launch; a plugin's manifest and queries are read the first time a file it claims is opened.")
                    .layout(axis),
                ),
        )
        .group(
            SettingGroup::new().title("Startup").item(
                SettingItem::new(
                    "On launch",
                    dropdown(
                        &[
                            ("home", "Home screen"),
                            ("resume", "Last session"),
                            ("local", "New local terminal"),
                        ],
                        |s| s.startup.clone(),
                        |s, v| s.startup = v.to_string(),
                    )
                    .default_value(defaults.startup.clone()),
                )
                .description("Home asks for nothing and is the quickest. Last session reopens the local layout you left, or comes up on the host you were using -- a remote one still needs its password, which is never stored. New local terminal goes straight to a shell on this machine.")
                .layout(axis),
            ),
        )
}

fn window_page(opacity: Entity<SliderState>, settled: Rc<Cell<bool>>, axis: Axis) -> SettingPage {
    let defaults = Settings::default();
    SettingPage::new("Window")
        .description("How the window itself is drawn.")
        .group(
            SettingGroup::new().title("Navigation").item(
                SettingItem::new(
                    "Bar edge",
                    dropdown(
                        &[
                            ("left", "Left"),
                            ("right", "Right"),
                            ("top", "Top"),
                            ("bottom", "Bottom"),
                        ],
                        |s| s.nav_edge.clone(),
                        |s, v| s.nav_edge = v.to_string(),
                    )
                    .default_value(defaults.nav_edge.clone()),
                )
                .description("Which edge the navigation bar sits on. There is only ever one bar, and it carries the window buttons, so this also moves them -- on the left or right it goes narrow and drops the clock and the host name, which do not survive being stacked a letter at a time.")
                .layout(axis),
            ),
        )
        .group(
            SettingGroup::new()
                .title("Motion")
                .description("The strip is longer than the window, so where a column went matters as much as where it is. One speed and one curve for the whole app.")
                .item(
                    SettingItem::new(
                        "Animate",
                        switch(|s| s.animations, |s, v| s.animations = v).default_value(true),
                    )
                    .description("Scrolling the strip, resizing a column and opening the command bar or the overview all move instead of cutting. Off, everything lands on its final value the frame it changes.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Duration",
                        number(
                            40.,
                            600.,
                            10.,
                            |s| s.animation_ms as f64,
                            |s, v| s.animation_ms = v as f32,
                        )
                        .default_value(defaults.animation_ms as f64),
                    )
                    .description("Milliseconds a move takes. Short enough that holding the focus key still walks the strip at the speed you press it; much past 200 and the app starts feeling like it is thinking.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Curve",
                        dropdown(
                            &[
                                ("ease", "Ease out"),
                                ("linear", "Linear"),
                                ("spring", "Spring"),
                            ],
                            |s| s.animation_curve.clone(),
                            |s, v| s.animation_curve = v.to_string(),
                        )
                        .default_value(defaults.animation_curve.clone()),
                    )
                    .description("Ease out is fast off the mark and settles; linear does not ease at all; spring passes the target by a little and comes back.")
                    .layout(axis),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Transparency")
                .item(
                    SettingItem::new(
                        "Opacity",
                        SettingField::render(move |options, window, cx| {
                            // The slider positions its thumb from bounds it
                            // records during prepaint, without asking for a
                            // redraw -- so the first frame it is on screen it
                            // still has zero width and the thumb sits at the
                            // left end whatever the value. One extra frame,
                            // once, lets it catch up.
                            if !settled.replace(true) {
                                window.request_animation_frame();
                            }
                            let value = cx.global::<Settings>().opacity;
                            control(options)
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().flex_1().min_w_0().child(Slider::new(&opacity)))
                                .child(
                                    div()
                                        .w(px(40.))
                                        .flex_none()
                                        .text_right()
                                        .child(format!("{value:.0}%")),
                                )
                        }),
                    )
                    .description("How much of the background is painted. Text, borders and menus stay solid; only the grounds let the desktop through.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Blur behind",
                        switch(|s| s.blur, |s, v| s.blur = v).default_value(false),
                    )
                    .description("Frosted-glass blur of whatever is behind the window. Only visible below 100% opacity.")
                    .layout(axis),
                ),
        )
}

fn terminal_page(axis: Axis) -> SettingPage {
    let defaults = Settings::default();
    SettingPage::new("Terminal")
        .description("Terminal behaviour. Applies to open terminals as well as new ones.")
        .group(
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "Cursor shape",
                        dropdown(
                            &[("block", "Block"), ("bar", "Bar"), ("underline", "Underline")],
                            |s| s.cursor_shape.clone(),
                            |s, v| s.cursor_shape = v.to_string(),
                        )
                        .default_value(defaults.cursor_shape.clone()),
                    )
                    .description("The shape when a program hasn't asked for one. vim, for one, switches to a bar in insert mode on its own.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Shell",
                        shell_dropdown(),
                    )
                    .description("What a local terminal runs. Only shells found on this machine are listed; remote tabs always start the account's own login shell.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Scrollback",
                        number(0., 200_000., 1000., |s| s.scrollback as f64, |s, v| {
                            s.scrollback = v as usize
                        })
                        .default_value(defaults.scrollback as f64),
                    )
                    .description("Lines of history kept per terminal.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Copy on select",
                        switch(|s| s.copy_on_select, |s, v| s.copy_on_select = v)
                            .default_value(defaults.copy_on_select),
                    )
                    .description("Copy text to the clipboard as soon as you finish selecting it.")
                    .layout(axis),
                ),
        )
}

fn explorer_page(axis: Axis) -> SettingPage {
    let defaults = Settings::default();
    SettingPage::new("File explorer")
        .description("The remote file tree.")
        .group(
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "Show hidden files",
                        switch(|s| s.show_hidden, |s, v| s.show_hidden = v)
                            .default_value(defaults.show_hidden),
                    )
                    .description("Dotfiles such as .bashrc and .config.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Open on connect",
                        switch(|s| s.explorer_on_connect, |s, v| s.explorer_on_connect = v)
                            .default_value(defaults.explorer_on_connect),
                    )
                    .description("Show the explorer as soon as a connection succeeds. Otherwise it opens from the sidebar or ctrl-shift-b.")
                    .layout(axis),
                ),
        )
}

fn editor_page(axis: Axis) -> SettingPage {
    let defaults = Settings::default();
    SettingPage::new("Editor")
        .description("Tabs opened from the file explorer.")
        .group(
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "Line numbers",
                        switch(|s| s.editor_line_numbers, |s, v| s.editor_line_numbers = v)
                            .default_value(defaults.editor_line_numbers),
                    )
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Soft wrap",
                        switch(|s| s.editor_soft_wrap, |s, v| s.editor_soft_wrap = v)
                            .default_value(defaults.editor_soft_wrap),
                    )
                    .description("Wrap long lines at the edge of the tab instead of scrolling sideways.")
                    .layout(axis),
                )
                .item(
                    SettingItem::new(
                        "Tab size",
                        dropdown(
                            &[("2", "2 spaces"), ("4", "4 spaces"), ("8", "8 spaces")],
                            |s| s.editor_tab_size.to_string(),
                            |s, v| s.editor_tab_size = v.parse().unwrap_or(4),
                        )
                        .default_value(defaults.editor_tab_size.to_string()),
                    )
                    .description("Applies to files opened from now on.")
                    .layout(axis),
                ),
        )
}

fn shortcuts_page(axis: Axis) -> SettingPage {
    SettingPage::new("Shortcuts")
        .description("App shortcuts use ctrl+shift, so plain ctrl keys still reach the remote shell.")
        .resettable(false)
        .group(SettingGroup::new().items(SHORTCUTS.iter().map(move |(label, keys)| {
            SettingItem::new(*label, SettingField::render(move |_, _, cx| keycap(keys, cx)))
                .layout(axis)
        })))
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let threshold = f32::from(window.rem_size()) * NARROW_BELOW_REMS;
        let narrow = f32::from(self.width.get()) < threshold;
        let axis = if narrow { Axis::Vertical } else { Axis::Horizontal };
        let width = self.width.clone();

        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .child(
                SettingsPanel::new("settings-panel")
                    .sidebar_width(px(210.))
                    .with_group_variant(GroupBoxVariant::Outline)
                    .page(general_page(axis))
                    .page(self.appearance_page(axis))
                    .page(self.font_page(axis))
                    .page(window_page(self.opacity.clone(), self.opacity_settled.clone(), axis))
                    .page(terminal_page(axis))
                    .page(explorer_page(axis))
                    .page(editor_page(axis))
                    .page(shortcuts_page(axis)),
            )
            // Measures the tab for the next frame's layout choice, and asks for
            // that frame only when the choice flips -- a resize that stays on one
            // side of the threshold costs nothing.
            .child(
                canvas(
                    move |bounds, window, _| {
                        let was_narrow = f32::from(width.get()) < threshold;
                        width.set(bounds.size.width);
                        if (f32::from(bounds.size.width) < threshold) != was_narrow {
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

/// The box a custom control sits in: fixed width beside its label, full width
/// under it.
fn control(options: &RenderOptions) -> Div {
    if options.layout == Axis::Vertical {
        div().w_full()
    } else {
        div().w(rems(CONTROL_WIDTH_REMS))
    }
}

/// A switch bound to one `Settings` field.
fn switch(get: fn(&Settings) -> bool, set: fn(&mut Settings, bool)) -> SettingField<bool> {
    SettingField::switch(
        move |cx| get(cx.global::<Settings>()),
        move |value, cx| Settings::update(cx, |settings| set(settings, value)),
    )
}

/// A number input bound to one `Settings` field.
fn number(
    min: f64,
    max: f64,
    step: f64,
    get: fn(&Settings) -> f64,
    set: fn(&mut Settings, f64),
) -> SettingField<f64> {
    SettingField::number_input(
        NumberFieldOptions { min, max, step },
        move |cx| get(cx.global::<Settings>()),
        move |value, cx| Settings::update(cx, |settings| set(settings, value)),
    )
    // Applied after gpui-component's own `w_32`, so it wins.
    .min_w(rems(NUMBER_MIN_WIDTH_REMS))
}

/// A dropdown bound to one `Settings` field. Options are (stored value, label).
fn dropdown(
    options: &[(&'static str, &'static str)],
    get: fn(&Settings) -> String,
    set: fn(&mut Settings, &str),
) -> SettingField<SharedString> {
    SettingField::dropdown(
        options
            .iter()
            .map(|(value, label)| (SharedString::from(*value), SharedString::from(*label)))
            .collect(),
        move |cx| get(cx.global::<Settings>()).into(),
        move |value, cx| Settings::update(cx, |settings| set(settings, &value)),
    )
}

/// The installed shells, as a dropdown. Options discovered at runtime rather
/// than listed in source: which shells exist differs per machine, and offering
/// one that isn't there produces a tab that opens and immediately dies.
///
/// The empty value is a real option -- "whatever is best here" -- so a machine
/// without the shell a settings file names still opens terminals.
fn shell_dropdown() -> SettingField<SharedString> {
    let mut options = vec![(SharedString::from(""), SharedString::from("Default"))];
    options.extend(crate::local::shells().into_iter().map(|shell| {
        (
            SharedString::from(shell.command),
            SharedString::from(shell.label),
        )
    }));
    SettingField::dropdown(
        options,
        |cx| cx.global::<Settings>().local_shell.clone().into(),
        |value, cx| {
            Settings::update(cx, |settings| settings.local_shell = value.to_string());
        },
    )
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

/// One card per preset, drawn in that preset's own colours so the grid is a
/// preview as well as a picker.
fn theme_cards(pickers: &Pickers, cx: &App) -> impl IntoElement {
    let current = cx.global::<Settings>().theme.clone();
    let accent = cx.global::<Theme>().accent;

    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .children(theme::presets().into_iter().enumerate().map(|(ix, preset)| {
            let selected = preset.id == current;
            let p = preset.palette;
            let id = preset.id;
            let pickers = pickers.clone();

            div()
                .id(("theme-preset", ix))
                .w(rems(10.75))
                .flex()
                .flex_col()
                .gap_1p5()
                .p_2()
                .bg(p.bg)
                .border_1()
                .border_color(if selected { accent } else { p.border })
                .hover(move |style| style.border_color(accent))
                .cursor_pointer()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_1()
                        .text_color(p.fg)
                        // Truncated as a backstop: the card is sized in rems so
                        // the longest name fits, but a wide font could still
                        // overrun it.
                        .child(div().min_w_0().truncate().child(preset.name))
                        .when(selected, |this| {
                            this.child(div().flex_none().text_color(p.accent).child("●"))
                        }),
                )
                .child(
                    div().flex().gap_0p5().children(
                        [
                            p.surface, p.raised, p.accent, p.ansi[1], p.ansi[2], p.ansi[3],
                            p.ansi[4], p.ansi[5], p.ansi[6],
                        ]
                        .map(|color| div().size(px(12.)).bg(color)),
                    ),
                )
                .child(
                    div()
                        .text_color(p.fg_subtle)
                        .child(if preset.light { "light" } else { "dark" }),
                )
                .on_click(move |_, window, cx| {
                    Settings::update(cx, |settings| {
                        settings.theme = id.to_string();
                        settings.colors.clear();
                    });
                    sync_pickers(&pickers, window, cx);
                })
        }))
}

fn reset_colors_item(pickers: Pickers, axis: Axis) -> SettingItem {
    SettingItem::new(
        "Reset colors",
        SettingField::render(move |_, _, cx| {
            let pickers = pickers.clone();
            let overrides = cx.global::<Settings>().colors.len();
            Button::new("reset-colors")
                .label(if overrides == 0 {
                    "nothing to reset".to_string()
                } else {
                    format!("reset {overrides} override(s)")
                })
                .small()
                .outline()
                .disabled(overrides == 0)
                .on_click(move |_, window, cx| {
                    Settings::update(cx, |settings| settings.colors.clear());
                    sync_pickers(&pickers, window, cx);
                })
        }),
    )
    .description("Go back to the theme's own colours.")
    .layout(axis)
}

/// The 16 ANSI slots, normal over bright -- the way every terminal documents
/// its palette. Each row wraps, so a narrow tab gets two lines of four rather
/// than eight swatches running off the edge.
fn ansi_grid(pickers: &[(SharedString, Entity<ColorPickerState>)], cx: &App) -> impl IntoElement {
    let (fg_muted, fg_subtle) = {
        let t = cx.global::<Theme>();
        (t.fg_muted, t.fg_subtle)
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .children([("normal", 0), ("bright", 8)].map(|(label, start)| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(fg_muted).child(label))
                .child(div().flex().flex_wrap().gap_3().children((0..8).map(|index| {
                    let (_, picker) = &pickers[start + index];
                    div()
                        .w(rems(3.5))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_0p5()
                        .child(ColorPicker::new(picker).small())
                        .child(div().text_xs().text_color(fg_subtle).child(ANSI_NAMES[index]))
                })))
        }))
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

fn keycap(keys: &'static str, cx: &App) -> impl IntoElement {
    let t = cx.global::<Theme>();
    div()
        .px_2()
        .py_0p5()
        .border_1()
        .border_color(t.border)
        .bg(t.palette.surface)
        .text_color(t.fg)
        .child(keys)
}
