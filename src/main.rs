// The whole UI is a fixed-cell grid rendered on the GPU: a terminal, a Zed-style
// split/tab tree around it, and a status line. `windows_subsystem` keeps a
// console window from appearing behind the app in release builds -- in debug it
// stays attached so panics and logging are visible.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod host;
mod keymap;
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
    CancelClose, ContinueSession, FocusColumnLeft, FocusColumnRight, HideHome, ToggleOverview,
    Workspace,
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
        let started = std::time::Instant::now();
        theme::apply(cx);
        // The one startup step LINUX.md flagged as a risk there (it resolves
        // the font against the installed list). Debug builds only, on stderr.
        #[cfg(debug_assertions)]
        eprintln!("[startup] theme::apply {:.1?}", started.elapsed());
        #[cfg(not(debug_assertions))]
        let _ = started;

        // The bindings that are not the user's to change: keys scoped to one
        // dialog or overlay, and Tab passing through to the shell. The app's
        // commands -- all on ctrl+shift, the one region of the keyboard a
        // terminal has no encoding for -- are `keymap::install`'s, below.
        cx.bind_keys([
            // Inside the overview only: the design's own keys, which are the
            // unqualified ones because nothing else has the keyboard there.
            KeyBinding::new("escape", ToggleOverview, Some("Overview")),
            KeyBinding::new("enter", ToggleOverview, Some("Overview")),
            KeyBinding::new("left", FocusColumnLeft, Some("Overview")),
            KeyBinding::new("right", FocusColumnRight, Some("Overview")),
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
        // Every app command on ctrl+shift, from `keymap::COMMANDS` and the
        // user's `keys.json` -- after the fixed bindings above, so that is
        // the layer a rebinding is appended to (see `keymap`).
        keymap::install(cx);

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
                // Linux reads `None` as server-side decorations, which puts the
                // desktop's own titlebar above the navigation bar -- two
                // captions. Client-side is how the bar gets to be the only one;
                // the other backends ignore the request.
                #[cfg(target_os = "linux")]
                window_decorations: Some(gpui::WindowDecorations::Client),
                // The Wayland app id and X11 `WM_CLASS`: what the dock groups by
                // and what the `.desktop` file's `StartupWMClass` must match. A
                // no-op on the other backends.
                app_id: Some("pulseterm".to_string()),
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
