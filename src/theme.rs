use std::sync::Arc;

use gpui::{
    px, rgb, App, Font, FontFeatures, FontWeight, Global, Hsla, Pixels, SharedString,
    WindowBackgroundAppearance,
};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::{Colorize as _, Theme as ComponentTheme, ThemeMode};

use crate::store::Settings;

/// Every colour a theme defines. Opaque, apart from the selection -- window
/// transparency is applied on top of this when the `Theme` is built, so the
/// palette stays the "design" colours a colour picker shows and edits.
#[derive(Clone, Copy)]
pub struct Palette {
    // Chrome. `bg` is the window ground, `surface` the panels sitting on it,
    // `raised` the active tab / focused pane -- three steps is enough to read
    // structure without drawing more borders than a TUI would.
    pub bg: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub border_focused: Hsla,

    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub fg_subtle: Hsla,

    pub accent: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,

    pub cursor: Hsla,
    /// Painted as the cell background of selected text. Translucent on purpose:
    /// it composites over whatever colour the remote program set, so coloured
    /// output stays recognisable inside a selection.
    pub selection: Hsla,
    /// Terminal palette, indexed 0-15: black, red, green, yellow, blue,
    /// magenta, cyan, white, then the eight bright variants.
    pub ansi: [Hsla; 16],
}

impl Palette {
    /// Looks a colour up by the token name settings store overrides under.
    pub fn get(&self, key: &str) -> Option<Hsla> {
        let mut copy = *self;
        copy.get_mut(key).map(|color| *color)
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Hsla> {
        Some(match key {
            "bg" => &mut self.bg,
            "surface" => &mut self.surface,
            "raised" => &mut self.raised,
            "border" => &mut self.border,
            "border_focused" => &mut self.border_focused,
            "fg" => &mut self.fg,
            "fg_muted" => &mut self.fg_muted,
            "fg_subtle" => &mut self.fg_subtle,
            "accent" => &mut self.accent,
            "success" => &mut self.success,
            "warning" => &mut self.warning,
            "danger" => &mut self.danger,
            "cursor" => &mut self.cursor,
            "selection" => &mut self.selection,
            _ => {
                let index: usize = key.strip_prefix("ansi")?.parse().ok()?;
                return self.ansi.get_mut(index);
            }
        })
    }
}

/// The chrome colours the Settings page offers a picker for: token, label,
/// description. The 16 ANSI slots (`ansi0`..`ansi15`) are listed separately.
pub const COLOR_TOKENS: &[(&str, &str, &str)] = &[
    ("bg", "Background", "The window ground, behind terminals and editors."),
    ("surface", "Panels", "Title bar, status bar, sidebar and explorer."),
    ("raised", "Highlight", "The active tab and selected rows."),
    ("border", "Borders", "Hairlines between panes and panels."),
    ("border_focused", "Focus ring", "The border of the focused pane and field."),
    ("fg", "Text", "Primary text."),
    ("fg_muted", "Muted text", "Labels and inactive tabs."),
    ("fg_subtle", "Subtle text", "Hints, placeholders and separators."),
    ("accent", "Accent", "Links, the connect button and active markers."),
    ("success", "Success", "The connected dot and success messages."),
    ("warning", "Warning", "Connecting state and warnings."),
    ("danger", "Danger", "Errors, delete and disconnect."),
    ("cursor", "Cursor", "The terminal and editor caret."),
    ("selection", "Selection", "Selected text. Kept translucent over program colours."),
];

pub const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub light: bool,
    pub palette: Palette,
}

/// The built-in themes. Each is the scheme's own published palette mapped onto
/// our tokens: its background and panel shades for the three chrome steps, its
/// terminal colours for the ANSI slots.
pub fn presets() -> Vec<Preset> {
    let mut midnight = palette(
        0x11141c, 0x161a24, 0x1e2430, 0x2a3140, 0xc8d0e0, 0x8792a6, 0x5a6478, 0x64b5f6,
        0x64b5f6, (0x64b5f6, 0.35),
        [
            0x2a3140, 0xe07b7b, 0x84c48a, 0xdcb267, 0x64b5f6, 0xb99bd8, 0x6fc7c7, 0xc8d0e0,
            0x5a6478, 0xf09a9a, 0xa2dda8, 0xf0cc85, 0x8ccdff, 0xd0b6ee, 0x8ee0e0, 0xe8eef8,
        ],
    );
    midnight.border_focused = c(0x3f7fbf);

    // The redesign's own two, and the only ones drawn rather than adopted:
    // every other preset here is somebody else's published scheme mapped onto
    // our tokens, while these two *are* the tokens. Ink is the default.
    //
    // They are one palette inverted, not two palettes -- the same seven steps
    // read dark-on-light or light-on-dark -- which is why the grid, the
    // hairlines and the spacing need no second case.
    let ink = palette(
        0x121212, 0x191919, 0x202020, 0x2b2b2b, 0xedeae3, 0x9a958b, 0x5e5a52, 0xff2e57,
        0xff2e57, (0xff2e57, 0.3),
        [
            0x2b2b2b, 0xff2e57, 0x8fb573, 0xd1a74f, 0x6f9fd8, 0xb288c0, 0x6fb0b0, 0xedeae3,
            0x5e5a52, 0xff6b89, 0xa8c98f, 0xe0bf74, 0x92b9e6, 0xc9a6d4, 0x92c7c7, 0xfffdf8,
        ],
    );
    let paper = palette(
        0xedeae3, 0xe3dfd5, 0xf5f3ee, 0xc9c4b8, 0x121212, 0x565249, 0x948f83, 0xe3103c,
        0xe3103c, (0xe3103c, 0.18),
        [
            0xc9c4b8, 0xe3103c, 0x4a7238, 0x8a6a17, 0x2f5c96, 0x74458c, 0x1f6a6a, 0x565249,
            0x948f83, 0xb01030, 0x3c5c2c, 0x6f5512, 0x254a7a, 0x5d3570, 0x185454, 0x121212,
        ],
    );

    vec![
        Preset { id: "ink", name: "Ink", light: false, palette: ink },
        Preset { id: "paper", name: "Paper", light: true, palette: paper },
        Preset { id: "midnight", name: "Midnight", light: false, palette: midnight },
        Preset {
            id: "tokyo-night",
            name: "Tokyo Night",
            light: false,
            palette: palette(
                0x1a1b26, 0x16161e, 0x292e42, 0x2f3549, 0xc0caf5, 0xa9b1d6, 0x565f89, 0x7aa2f7,
                0xc0caf5, (0x33467c, 0.7),
                [
                    0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
                    0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
                ],
            ),
        },
        Preset {
            id: "tokyo-night-storm",
            name: "Tokyo Night Storm",
            light: false,
            palette: palette(
                0x24283b, 0x1f2335, 0x2f3549, 0x3b4261, 0xc0caf5, 0xa9b1d6, 0x565f89, 0x7aa2f7,
                0xc0caf5, (0x2e3c64, 0.8),
                [
                    0x1d202f, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
                    0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
                ],
            ),
        },
        Preset {
            id: "catppuccin-mocha",
            name: "Catppuccin Mocha",
            light: false,
            palette: palette(
                0x1e1e2e, 0x181825, 0x313244, 0x3a3c52, 0xcdd6f4, 0xa6adc8, 0x6c7086, 0x89b4fa,
                0xf5e0dc, (0x585b70, 0.6),
                [
                    0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
                    0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                ],
            ),
        },
        Preset {
            id: "catppuccin-latte",
            name: "Catppuccin Latte",
            light: true,
            palette: palette(
                0xeff1f5, 0xe6e9ef, 0xccd0da, 0xbcc0cc, 0x4c4f69, 0x6c6f85, 0x9ca0b0, 0x1e66f5,
                0xdc8a78, (0xacb0be, 0.6),
                [
                    0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
                    0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
                ],
            ),
        },
        Preset {
            id: "dracula",
            name: "Dracula",
            light: false,
            palette: palette(
                0x282a36, 0x21222c, 0x44475a, 0x363949, 0xf8f8f2, 0xb9bccb, 0x6272a4, 0xbd93f9,
                0xf8f8f2, (0x44475a, 0.9),
                [
                    0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
                    0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
                ],
            ),
        },
        Preset {
            id: "nord",
            name: "Nord",
            light: false,
            palette: palette(
                0x2e3440, 0x292e39, 0x3b4252, 0x434c5e, 0xd8dee9, 0xa3acbc, 0x616e88, 0x88c0d0,
                0xd8dee9, (0x434c5e, 0.9),
                [
                    0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
                    0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
                ],
            ),
        },
        Preset {
            id: "gruvbox-dark",
            name: "Gruvbox Dark",
            light: false,
            palette: palette(
                0x282828, 0x1d2021, 0x3c3836, 0x504945, 0xebdbb2, 0xa89984, 0x7c6f64, 0xfabd2f,
                0xebdbb2, (0x504945, 0.9),
                [
                    0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
                    0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
                ],
            ),
        },
        Preset {
            id: "one-dark",
            name: "One Dark",
            light: false,
            palette: palette(
                0x282c34, 0x21252b, 0x2c313a, 0x3e4451, 0xabb2bf, 0x828997, 0x5c6370, 0x61afef,
                0x528bff, (0x3e4451, 0.9),
                [
                    0x3f4451, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
                    0x4f5666, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd7dae0,
                ],
            ),
        },
        Preset {
            id: "solarized-dark",
            name: "Solarized Dark",
            light: false,
            palette: palette(
                0x002b36, 0x00232c, 0x073642, 0x0b4050, 0x93a1a1, 0x839496, 0x586e75, 0x268bd2,
                0x93a1a1, (0x0f4c5c, 0.9),
                [
                    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
                    0x586e75, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
                ],
            ),
        },
        Preset {
            id: "rose-pine",
            name: "Rosé Pine",
            light: false,
            palette: palette(
                0x191724, 0x1f1d2e, 0x26233a, 0x403d52, 0xe0def4, 0x908caa, 0x6e6a86, 0xc4a7e7,
                0xe0def4, (0x403d52, 0.9),
                [
                    0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
                    0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
                ],
            ),
        },
        Preset {
            id: "github-dark",
            name: "GitHub Dark",
            light: false,
            palette: palette(
                0x0d1117, 0x161b22, 0x21262d, 0x30363d, 0xc9d1d9, 0x8b949e, 0x6e7681, 0x58a6ff,
                0x58a6ff, (0x58a6ff, 0.3),
                [
                    0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
                    0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
                ],
            ),
        },
    ]
}

/// The preset with this id, falling back to the first one for an id that is
/// no longer shipped.
pub fn preset(id: &str) -> Preset {
    let mut all = presets();
    match all.iter().position(|preset| preset.id == id) {
        Some(index) => all.swap_remove(index),
        None => all.swap_remove(0),
    }
}

/// The whole UI is monospace on a fixed cell grid, so a "theme" here is closer
/// to a terminal colour scheme than to a widget stylesheet: the palette, with
/// window transparency applied to the grounds, plus the font.
///
/// Built from `Settings` by `apply`, never edited in place: every view reads it
/// at render time, so replacing the global and refreshing is the whole of a
/// live theme change.
pub struct Theme {
    pub font: SharedString,
    /// What the settings asked for, kept so `apply` can skip re-resolving the
    /// family (a walk of every installed font) when it hasn't changed.
    font_request: String,
    pub font_size: Pixels,
    pub line_height: Pixels,
    /// `line_height` as a multiple of `font_size`, for text that isn't on the
    /// terminal grid -- the workspace sets it as its inherited line height.
    pub line_height_ratio: f32,
    pub font_weight: FontWeight,
    pub ligatures: bool,

    // The palette's colours, with the three grounds (`bg`, `surface`,
    // `raised`) carrying the window opacity. Everything else stays opaque so
    // text and borders read at full strength on a see-through window.
    pub bg: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub border_focused: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub fg_subtle: Hsla,
    pub accent: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub ansi: [Hsla; 16],
    pub cursor: Hsla,
    pub selection: Hsla,

    /// The same colours, opaque and with overrides applied: what the colour
    /// pickers show, and what text drawn *in* a background colour needs.
    pub palette: Palette,
    pub light: bool,
    pub appearance: WindowBackgroundAppearance,
}

impl Global for Theme {}

impl Theme {
    /// Whether the window lets the desktop through.
    ///
    /// When it does, **every pixel is painted by exactly one translucent
    /// layer**: alpha stacks, and a panel painted over an already-painted
    /// ground reads as solid. So the workspace is painted region by region --
    /// title bar, rail, explorer, status bar and pane tab strips in `surface`;
    /// pane bodies and the dividers between them in `bg` -- and nothing nested
    /// inside those (terminal, editor, Settings page) paints a ground of its
    /// own. Highlights (`raised`) do stack, deliberately: a selected row or the
    /// active tab should read a little more solid than what's around it.
    pub fn translucent(&self) -> bool {
        self.appearance != WindowBackgroundAppearance::Opaque
    }

    /// The terminal's text font: family, weight, and ligatures off unless asked
    /// for -- in a terminal `->` and `!=` are usually meant literally.
    pub fn text_font(&self) -> Font {
        let mut font = gpui::font(self.font.clone());
        font.weight = self.font_weight;
        if !self.ligatures {
            font.features = FontFeatures::disable_ligatures();
        }
        font
    }
}

/// Builds the `Theme` global from `Settings` and pushes it everywhere that
/// paints: our views (via the global and a refresh), gpui-component's widgets
/// and `Root` (its own theme), and the window's own background mode.
///
/// Called once at startup and again after every settings change.
pub fn apply(cx: &mut App) {
    let settings = cx.global::<Settings>().clone();
    let previous = cx
        .try_global::<Theme>()
        .map(|theme| (theme.font_request.clone(), theme.font.clone(), theme.appearance));

    let font = match &previous {
        Some((requested, font, _)) if *requested == settings.font_family => font.clone(),
        _ => resolve_font(&settings.font_family, cx),
    };
    let theme = build(&settings, font);
    apply_component_theme(&theme, cx);

    // Only on a change: the composition attribute is a DWM round trip, and the
    // opacity slider calls this on every tick. At startup there is no previous
    // theme and the window is opened with the right mode instead.
    let appearance = theme.appearance;
    let appearance_changed = previous.is_some_and(|(_, _, old)| old != appearance);
    cx.set_global(theme);
    if appearance_changed {
        // Deferred: this runs from inside a settings control's handler, while
        // the window is being updated, and a window can't be re-entered from
        // its own update -- `handle.update` would just fail.
        cx.defer(move |cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| {
                    window.set_background_appearance(appearance);
                });
            }
        });
    }
    cx.refresh_windows();
}

fn build(settings: &Settings, font: SharedString) -> Theme {
    let preset = preset(&settings.theme);
    let mut palette = preset.palette;
    for (key, hex) in &settings.colors {
        let (Some(slot), Ok(color)) = (palette.get_mut(key), Hsla::parse_hex(hex)) else {
            continue;
        };
        // A picker hands back an opaque colour; keeping the slot's own alpha is
        // what leaves an overridden selection translucent.
        *slot = Hsla { a: slot.a, ..color };
    }

    let opacity = settings.opacity / 100.;
    let appearance = if opacity >= 1. {
        WindowBackgroundAppearance::Opaque
    } else if settings.blur {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Transparent
    };

    Theme {
        font,
        font_request: settings.font_family.clone(),
        font_size: px(settings.font_size),
        line_height: px(settings.line_height()),
        line_height_ratio: settings.line_height / 100.,
        font_weight: FontWeight(settings.font_weight as f32),
        ligatures: settings.ligatures,
        bg: palette.bg.opacity(opacity),
        surface: palette.surface.opacity(opacity),
        raised: palette.raised.opacity(opacity),
        border: palette.border,
        // Turning the ring off is just this: every focused border, ours and
        // gpui-component's (`ring`, below), reads this field, so it falls back
        // to the plain border everywhere at once. The palette keeps the colour,
        // so the picker still shows it and switching back restores it.
        border_focused: if settings.focus_ring {
            palette.border_focused
        } else {
            palette.border
        },
        fg: palette.fg,
        fg_muted: palette.fg_muted,
        fg_subtle: palette.fg_subtle,
        accent: palette.accent,
        success: palette.success,
        warning: palette.warning,
        danger: palette.danger,
        ansi: palette.ansi,
        cursor: palette.cursor,
        selection: palette.selection,
        palette,
        light: preset.light,
        appearance,
    }
}

/// Maps our palette onto gpui-component's theme, which its widgets and `Root`
/// paint from.
///
/// On a translucent window every pixel must be painted by exactly one
/// translucent layer -- alpha stacks, so a panel painted over a painted ground
/// reads as solid (two 70% layers are 91%). So there, `Root`'s `background` and
/// the Settings page's nested panels are transparent, and each region of the
/// workspace paints its own ground once (see `Theme::translucent`). On an opaque
/// window stacking is harmless, and `Root` keeps painting the ground as a
/// safety net under anything that doesn't.
fn apply_component_theme(ours: &Theme, cx: &mut App) {
    let mode = if ours.light { ThemeMode::Light } else { ThemeMode::Dark };
    ComponentTheme::change(mode, None, cx);

    let translucent = ours.translucent();
    let clear = gpui::transparent_black();
    let p = ours.palette;
    let theme = ComponentTheme::global_mut(cx);
    theme.font_family = ours.font.clone();
    theme.mono_font_family = ours.font.clone();
    theme.mono_font_size = ours.font_size;
    // `Root` sets this as the window's rem size, and gpui-component sizes its
    // text in rems -- labels, inputs and menus are `text_sm`, 0.875rem. Pinned
    // at 16px, the Settings page and every other gpui-component widget stayed
    // at 14px whatever font size was chosen. Scaled so `text_sm` lands exactly
    // on our font size, they follow it; rem-based spacing (ours included)
    // scales along with it, so the whole UI grows and shrinks together.
    theme.font_size = ours.font_size / 0.875;
    theme.radius = px(0.);
    theme.radius_lg = px(0.);
    theme.shadow = false;
    // The editor paints its line-number gutter in the syntax theme's editor
    // background -- #0a0a0a, or white on light -- a solid strip whatever our
    // theme or opacity. Pointed at our ground instead. Not transparent: the
    // editor doesn't clip its text to the code area, and that gutter fill is
    // what hides code scrolled sideways under the line numbers. So on a
    // translucent window the gutter is the one place two layers stack.
    let mut highlight = (*if ours.light {
        HighlightTheme::default_light()
    } else {
        HighlightTheme::default_dark()
    })
    .clone();
    highlight.style.editor_background = Some(ours.bg);
    theme.highlight_theme = Arc::new(highlight);

    let colors = &mut theme.colors;
    colors.background = if translucent { clear } else { ours.bg };
    colors.foreground = p.fg;
    colors.border = p.border;
    colors.input = p.border;
    colors.ring = ours.border_focused;
    colors.caret = p.cursor;
    colors.selection = p.selection;
    colors.muted = p.surface;
    colors.muted_foreground = p.fg_muted;
    colors.accent = p.raised;
    colors.accent_foreground = p.fg;
    // Popups stay opaque whatever the window opacity: a menu you can read the
    // desktop through is a menu you can't read.
    colors.popover = p.surface;
    colors.popover_foreground = p.fg;
    colors.list = p.surface;
    colors.list_even = p.surface;
    colors.list_head = p.surface;
    colors.list_hover = p.raised;
    colors.list_active = p.raised;
    colors.list_active_border = p.accent;
    colors.scrollbar = gpui::transparent_black();
    colors.scrollbar_thumb = p.border;
    colors.scrollbar_thumb_hover = p.fg_subtle;
    colors.window_border = p.border;
    colors.overlay = gpui::black().opacity(0.5);
    colors.primary = p.accent;
    colors.primary_hover = p.accent;
    colors.primary_active = p.accent;
    colors.primary_foreground = p.bg;
    colors.secondary = p.raised;
    colors.secondary_hover = p.border;
    colors.secondary_active = p.border;
    colors.secondary_foreground = p.fg;
    colors.link = p.accent;
    colors.info = p.accent;
    colors.danger = p.danger;
    colors.danger_foreground = p.bg;
    colors.warning = p.warning;
    colors.warning_foreground = p.bg;
    colors.success = p.success;
    colors.success_foreground = p.bg;
    // The Settings page's sidebar and group boxes are drawn *inside* a pane
    // body, which has already painted the ground. On a translucent window they
    // must add nothing, or they read as a solid block in a see-through window.
    colors.sidebar = if translucent { clear } else { p.surface };
    colors.sidebar_foreground = p.fg_muted;
    colors.sidebar_border = p.border;
    colors.sidebar_accent = ours.raised;
    colors.sidebar_accent_foreground = p.fg;
    colors.sidebar_primary = p.accent;
    colors.sidebar_primary_foreground = p.bg;
    colors.group_box = if translucent { clear } else { p.surface };
    colors.group_box_foreground = p.fg;
    colors.switch = p.border;
    colors.switch_thumb = p.fg;
    colors.slider_bar = p.accent;
    colors.slider_thumb = p.fg;
}

/// The monospace fonts that are commonly installed, best first. Used when the
/// settings don't name one, or name one that isn't installed any more.
const PREFERRED: &[&str] = &[
    "Cascadia Mono",
    "Cascadia Code",
    "JetBrains Mono",
    "Fira Code",
    "Consolas",
    "Lucida Console",
    "Courier New",
];

/// Picks the family to render with.
///
/// gpui resolves an unknown family by silently falling back to a proportional
/// face, which in a cell grid looks like the renderer is broken rather than
/// like a missing font -- so the choice is made against the real font list up
/// front instead of being left to a CSS-style fallback chain.
fn resolve_font(requested: &str, cx: &App) -> SharedString {
    let available = cx.text_system().all_font_names();
    let installed = |name: &str| available.iter().any(|family| family == name);
    if !requested.is_empty() && installed(requested) {
        return requested.to_string().into();
    }
    PREFERRED
        .iter()
        .find(|name| installed(name))
        .map(|name| SharedString::from(*name))
        .unwrap_or_else(|| "monospace".into())
}

/// Every installed family that is monospace, for the font picker.
///
/// There is no "is monospace" flag to ask for, so it is measured: a family is
/// kept when a narrow glyph and a wide one advance by the same amount. A
/// proportional font in a terminal breaks the cell grid, so it isn't offered.
pub fn monospace_fonts(cx: &App) -> Vec<SharedString> {
    let text = cx.text_system();
    let mut names = text.all_font_names();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names
        .into_iter()
        // Windows lists a vertical-writing twin of each CJK family as "@Name".
        .filter(|name| !name.starts_with('@'))
        .filter(|name| {
            let id = text.resolve_font(&gpui::font(name.clone()));
            match (text.advance(id, px(16.), 'i'), text.advance(id, px(16.), 'W')) {
                (Ok(narrow), Ok(wide)) => {
                    (f32::from(narrow.width) - f32::from(wide.width)).abs() < 0.5
                }
                _ => false,
            }
        })
        .map(SharedString::from)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn palette(
    bg: u32,
    surface: u32,
    raised: u32,
    border: u32,
    fg: u32,
    fg_muted: u32,
    fg_subtle: u32,
    accent: u32,
    cursor: u32,
    selection: (u32, f32),
    ansi: [u32; 16],
) -> Palette {
    Palette {
        bg: c(bg),
        surface: c(surface),
        raised: c(raised),
        border: c(border),
        border_focused: c(accent),
        fg: c(fg),
        fg_muted: c(fg_muted),
        fg_subtle: c(fg_subtle),
        accent: c(accent),
        // Status colours come from the scheme's own terminal colours, so a
        // theme's "green" means the same thing in the chrome as in `ls`.
        success: c(ansi[2]),
        warning: c(ansi[3]),
        danger: c(ansi[1]),
        cursor: c(cursor),
        selection: c(selection.0).opacity(selection.1),
        ansi: ansi.map(c),
    }
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}
