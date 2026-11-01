// The whole UI is a fixed-cell grid rendered on the GPU: a terminal, a Zed-style
// split/tab tree around it, and a status line. `windows_subsystem` keeps a
// console window from appearing behind the app in release builds -- in debug it
// stays attached so panics and logging are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod local;
mod plugins;
mod pty;
mod runtime;
mod session;
mod ssh;
mod store;
mod terminal;
mod theme;
mod ui;
mod workspace;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, KeyBinding, NoAction, TitlebarOptions,
    WindowBounds, WindowOptions,
};
use gpui_component::Root;

use store::{Profiles, Settings};
use theme::Theme;
use ui::command_bar::{CommandDown, CommandForget, CommandUp, DismissCommandBar};
use ui::editor::SaveFile;
use ui::icon::Assets;
use workspace::{
    CancelClose, CloseTab, ContinueSession, DecreaseFontSize, Disconnect, HideHome,
    ConsumeWindow, CycleColumnWidth, ExpelWindow, FocusColumnLeft, FocusColumnRight,
    FocusWindowDown, FocusWindowUp, IncreaseFontSize, MoveColumnLeft, MoveColumnRight,
    MoveWindowDown, MoveWindowUp, NewTerminal, OpenConnections, OpenSettings, ResetFontSize,
    StackTerminal, ToggleExplorer, ToggleOverview, Workspace,
};

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        // gpui-component supplies the code editor and the Settings page's
        // widgets. Its init has to run before `theme::apply`, which maps our
        // palette onto its theme -- otherwise its defaults leak through.
        gpui_component::init(cx);
        // The editor's syntax-highlighting languages. gpui-component's own set
        // is switched off to keep its grammars out of the binary; see there.
        ui::languages::register();
        cx.set_global(Settings::load());
        // Plugins: the cached index and nothing more. Manifests, queries and
        // grammars are read the first time a file wants them -- see `plugins`.
        plugins::install_from_settings(cx);
        cx.set_global(Profiles::load());
        // Builds the `Theme` global from the saved settings. Every later
        // settings change goes through the same function.
        theme::apply(cx);

        // App shortcuts sit on ctrl+shift, the one region of the keyboard a
        // terminal has no encoding for -- see `terminal::keymap`. Anything on
        // plain ctrl would be stolen from the remote shell.
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-t", NewTerminal, Some("Workspace")),
            KeyBinding::new("ctrl-shift-w", CloseTab, Some("Workspace")),
            // The strip is the tab bar now, so the tab keys walk it. Columns
            // left and right, windows up and down within a column: the
            // directions the layout actually has.
            KeyBinding::new("ctrl-shift-tab", FocusColumnLeft, Some("Workspace")),
            KeyBinding::new("ctrl-tab", FocusColumnRight, Some("Workspace")),
            KeyBinding::new("ctrl-shift-left", FocusColumnLeft, Some("Workspace")),
            KeyBinding::new("ctrl-shift-right", FocusColumnRight, Some("Workspace")),
            KeyBinding::new("ctrl-shift-up", FocusWindowUp, Some("Workspace")),
            KeyBinding::new("ctrl-shift-down", FocusWindowDown, Some("Workspace")),
            KeyBinding::new("ctrl-shift-alt-left", MoveColumnLeft, Some("Workspace")),
            KeyBinding::new("ctrl-shift-alt-right", MoveColumnRight, Some("Workspace")),
            KeyBinding::new("ctrl-shift-alt-up", MoveWindowUp, Some("Workspace")),
            KeyBinding::new("ctrl-shift-alt-down", MoveWindowDown, Some("Workspace")),
            // Out of the stack into a column of its own, and back in from the
            // column next door -- niri's expel and consume. Not ctrl-shift-i
            // for the second: gpui binds that to its own inspector in a debug
            // build, and the inspector wins.
            KeyBinding::new("ctrl-shift-o", ExpelWindow, Some("Workspace")),
            KeyBinding::new("ctrl-shift-u", ConsumeWindow, Some("Workspace")),
            KeyBinding::new("ctrl-shift-r", CycleColumnWidth, Some("Workspace")),
            // The whole strip at once. Space, because it is the one key that
            // means "show me everything" in every tiling window manager that
            // has an overview, and ctrl+shift keeps it off the shell.
            KeyBinding::new("ctrl-shift-space", ToggleOverview, Some("Workspace")),
            // Inside the overview only: the design's own keys, which are the
            // unqualified ones because nothing else has the keyboard there.
            KeyBinding::new("escape", ToggleOverview, Some("Overview")),
            KeyBinding::new("enter", ToggleOverview, Some("Overview")),
            KeyBinding::new("left", FocusColumnLeft, Some("Overview")),
            KeyBinding::new("right", FocusColumnRight, Some("Overview")),
            KeyBinding::new("ctrl-shift-d", StackTerminal, Some("Workspace")),
            KeyBinding::new("ctrl-shift-k", OpenConnections, Some("Workspace")),
            KeyBinding::new("ctrl-shift-,", OpenSettings, Some("Workspace")),
            KeyBinding::new("ctrl-shift-q", Disconnect, Some("Workspace")),
            KeyBinding::new("ctrl-shift-b", ToggleExplorer, Some("Workspace")),
            // The one deliberate exception to the ctrl+shift rule: zoom keys.
            // Every app has them on plain ctrl and no shell binds them, so
            // they're safe to take. Bound in the `Workspace` context, gpui
            // dispatches them before the terminal's own key handler, so the
            // remote side never sees them. `ctrl-shift-=` is the same key as
            // `ctrl-+` on most layouts.
            KeyBinding::new("ctrl-=", IncreaseFontSize, Some("Workspace")),
            // Both spellings of the same physical key: on this layout shift-=
            // arrives as `+`, not as a shifted `=`, so binding only the latter
            // misses every `ctrl-+` press.
            KeyBinding::new("ctrl-+", IncreaseFontSize, Some("Workspace")),
            KeyBinding::new("ctrl-shift-=", IncreaseFontSize, Some("Workspace")),
            KeyBinding::new("ctrl--", DecreaseFontSize, Some("Workspace")),
            KeyBinding::new("ctrl-0", ResetFontSize, Some("Workspace")),
            // Scoped to the popup so Escape is not taken from vim.
            KeyBinding::new("escape", DismissCommandBar, Some("CommandBar")),
            // Moving the selection is bound in the bar's own context, never
            // wider: up and down belong to whatever has focus everywhere else.
            KeyBinding::new("up", CommandUp, Some("CommandBar")),
            KeyBinding::new("down", CommandDown, Some("CommandBar")),
            // Forgetting a saved host. The old connect dialog had a button
            // for it; a one-line bar has nowhere to put one, and Delete is
            // what a list of things you own answers to everywhere else.
            KeyBinding::new("delete", CommandForget, Some("CommandBar")),
            // Same reasoning: only while the close confirmation is up.
            KeyBinding::new("escape", CancelClose, Some("ConfirmClose")),
            // The home screen owns the whole window while it is up, so these
            // two are safe to take: no terminal is on screen to want them.
            KeyBinding::new("space", ContinueSession, Some("Home")),
            KeyBinding::new("escape", HideHome, Some("Home")),
            KeyBinding::new("ctrl-s", SaveFile, Some("Editor")),
            // `Root` binds tab / shift-tab to focus cycling, and it is an ancestor
            // of everything -- the terminal included, where Tab has to reach the
            // shell for completion. gpui dispatches a matched binding *before* an
            // element's own key handler, and stops there once it is handled, so
            // without these the terminal would never see Tab. A `NoAction` in the
            // deeper `Terminal` context cancels the inherited binding, and the
            // keystroke falls through to the terminal's own encoder.
            KeyBinding::new("tab", NoAction, Some("Terminal")),
            KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        ]);

        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        // Opened in the saved background mode rather than switched after, so a
        // transparent window never flashes opaque on launch.
        let window_background = cx.global::<Theme>().appearance;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("PulseTerm".into()),
                    // Hides the system caption so `ui::nav_bar` can draw its
                    // own and host the tab strip. Dragging, snapping and the
                    // window buttons go through `ui::window_controls`.
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_background,
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                // The editor calls `Root::read` while painting, which `expect`s
                // the window's root view to *be* a `Root` -- so every window is
                // one, with the workspace inside it.
                cx.new(|cx| Root::new(workspace, window, cx))
            },
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
