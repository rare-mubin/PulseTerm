pub mod item;
pub mod pane;
pub mod strip;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{
    actions, div, prelude::*, App, Context, Entity, FocusHandle, Focusable, SharedString,
    Subscription, Task, Window,
};

use self::item::{ItemId, ItemKind, PaneItem};
use self::pane::{DropZone, Pane, PaneEvent, PaneId};
use self::strip::{ColumnWidth, Strip};
use crate::session::{SavedItem, SavedMember, SavedTarget, Session};
use crate::host::Host;
use crate::pty::PtyProgram;
use crate::ssh::ConnectParams;
use crate::store::Profiles;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use crate::ui::command_bar::{CommandBar, CommandEvent, ConnectionStatus};
use crate::ui::explorer::ExplorerView;
use crate::ui::settings_view::SettingsView;
use crate::ui::nav_bar;

actions!(
    pulseterm,
    [
        NewTerminal,
        StackTerminal,
        CloseTab,
        FocusColumnLeft,
        FocusColumnRight,
        FocusWindowUp,
        FocusWindowDown,
        MoveColumnLeft,
        MoveColumnRight,
        MoveWindowUp,
        MoveWindowDown,
        ExpelWindow,
        ConsumeWindow,
        CycleColumnWidth,
        ToggleOverview,
        OpenConnections,
        OpenSettings,
        ToggleExplorer,
        Disconnect,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        CancelClose,
        ContinueSession,
        HideHome,
    ]
);

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

/// The docked file tree's starting width, the narrowest it can be dragged to,
/// and the most of the window it may take -- past that there is no strip left
/// to work in.
const EXPLORER_WIDTH: f32 = 280.;
const EXPLORER_MIN: f32 = 160.;
const EXPLORER_MAX_SHARE: f32 = 0.6;

/// The payload for dragging the file tree's edge. There is only one, so it
/// carries nothing.
#[derive(Clone)]
struct ExplorerSeam;

fn next_pane_id() -> PaneId {
    PaneId(NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed))
}

/// Where the app is in its connection lifecycle. There is exactly one host per
/// window for now: either one SSH connection, whose channels every tab
/// multiplexes, or this machine, where a tab is a pseudo-console of its own.
///
/// Local mode still passes through `Connecting` -- for a frame, since there is
/// nothing to negotiate -- rather than having a state of its own. One path
/// means the explorer, the status line and the session file need no second
/// case, and `Disconnected` stays the honest name for "nothing is open".
pub enum ConnectionState {
    Disconnected,
    Connecting(SharedString),
    Connected(Host),
    Failed(SharedString),
}

pub struct Workspace {
    focus_handle: FocusHandle,
    strip: Strip,
    /// The focused column's pane. The strip owns the focus as an index; this
    /// is the same column by another name, kept because nearly everything that
    /// acts on "here" wants the entity rather than the position. `focus_pane`
    /// is the only thing that may set it, so the two cannot drift apart.
    active_pane: Entity<Pane>,
    connection: ConnectionState,
    /// Keyed by pane so a collapsed pane's subscription is dropped with it.
    /// Held in a growable collection long enough and every split/close cycle
    /// would leak one.
    pane_subscriptions: HashMap<PaneId, Subscription>,
    /// The command bar, when it is up. A modal rather than a tab:
    /// connecting is a brief act and shouldn't cost a permanent tab.
    connections: Option<Entity<CommandBar>>,
    /// The overview's fade-up. It lives here rather than in `ui::overview`
    /// because that is a free function with nowhere to keep a clock, and
    /// because opening the overview is a workspace event.
    overview_entrance: crate::ui::motion::Entrance,
    /// Replaced, not appended: the popup can be closed and reopened any number
    /// of times.
    connections_subscription: Option<Subscription>,
    /// The file explorer panel. Built once and kept, so its expanded folders and
    /// cached listings survive being hidden.
    explorer: Entity<ExplorerView>,
    /// Whether the file tree is docked open beside the strip.
    explorer_docked: bool,
    /// Its width in pixels, set by dragging its edge.
    explorer_width: f32,
    /// Its laid-out bounds as of the last paint, for the edge drag to measure
    /// against -- the cursor's distance from the panel's left edge is the
    /// width, which keeps the drag absolute.
    explorer_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Up when closing was asked for while a session is live.
    confirm_close: bool,
    /// Set once the answer is "close anyway", so the window's own close hook
    /// stops asking and lets it through.
    closing: bool,
    /// The dialog's focus, so Escape reaches it. Escape is deliberately not
    /// bound in the `Workspace` context -- it would be taken from vim. Shared
    /// with the tab dialog below: only one of the two is ever on screen.
    close_focus: FocusHandle,
    /// A single *tab* close waiting on an answer, because it would lose work.
    confirm_tab: Option<PendingClose>,
    /// Last run's layout, waiting for a connection it belongs to. Taken at the
    /// first successful connect, whether or not it matched, so a restore can
    /// happen at most once per launch.
    pending_session: Option<Session>,
    /// Where the live connection points. `Connection` carries a display label
    /// and a label is not an identity -- this is what a saved layout is matched
    /// against next time.
    target: Option<SavedTarget>,
    /// The layout as `track_layout_changes` last saw it. Zero until the first
    /// frame, which is never a real fingerprint, so the first comparison always
    /// counts as a change.
    layout_print: u64,
    /// True while the home screen has the window. It is what the app opens on,
    /// and the rail's house button brings it back.
    home_open: bool,
    /// Whether the strip is being shown whole. Not a mode of the strip: the
    /// strip is untouched while this is up, which is what lets escape put
    /// you back exactly where you were. See `ui::overview`.
    overview_open: bool,
    /// Home's focus, so space and escape reach the `Home` key context.
    home_focus: FocusHandle,
    /// The home screen's dial panel as laid out last frame. The dials are
    /// positioned by arithmetic against this box, and home repaints every
    /// frame, so a frame-old measurement is never visible.
    home_dials: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Debounce for `session.json`: a write lands a moment after the last
    /// change rather than on every one. Holding the `Task` is what cancels the
    /// previous timer -- gpui drops a task when its handle goes.
    session_save: Option<Task<()>>,
    /// Keeps the clock in the navigation bar honest. One wake a minute, on the
    /// minute, and a `notify` -- which is a repaint the window would not
    /// otherwise do, and the whole price of showing the time.
    #[allow(dead_code)]
    clock: Task<()>,
}

/// A tab close that has been asked about and is waiting for the answer.
///
/// The tab is remembered by `ItemId`, not by index: the dialog outlives the
/// frame that raised it, and a tab can be dragged to another slot -- or another
/// pane -- while it is up.
struct PendingClose {
    pane: Entity<Pane>,
    item: ItemId,
    name: SharedString,
    /// True once "save and close" was picked and the write is in flight.
    saving: bool,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
        let pane_id = pane.read(cx).id;
        let subscription = Self::subscribe_to_pane(&pane, window, cx);
        let session = Session::load();

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            strip: Strip::new(pane.clone()),
            active_pane: pane,
            connection: ConnectionState::Disconnected,
            pane_subscriptions: HashMap::from([(pane_id, subscription)]),
            connections: None,
            overview_entrance: crate::ui::motion::Entrance::new(),
            connections_subscription: None,
            explorer: cx.new(|cx| ExplorerView::new(window, cx)),
            explorer_docked: false,
            explorer_width: EXPLORER_WIDTH,
            explorer_bounds: Default::default(),
            confirm_close: false,
            closing: false,
            close_focus: cx.focus_handle(),
            confirm_tab: None,
            pending_session: Some(session),
            target: None,
            layout_print: 0,
            session_save: None,
            home_open: true,
            overview_open: false,
            home_focus: cx.focus_handle(),
            home_dials: Default::default(),
            clock: Self::tick_clock(cx),
        };

        // Alt-F4, the taskbar, the OS -- every close that isn't our own X ends
        // up here. Returning false vetoes it and puts the dialog up instead.
        let handle = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            handle
                .update(cx, |this, cx| this.request_close(window, cx))
                .unwrap_or(true)
        });
        // The desktop switched between light and dark. Only a `system` theme
        // cares, but re-applying is cheap and keeps the check in one place
        // (`theme::apply`).
        cx.observe_window_appearance(window, |_, _, cx| {
            if cx.global::<crate::store::Settings>().theme == "system" {
                crate::theme::apply(cx);
            }
        })
        .detach();
        // Double-clicking a file in the explorer opens it in an editor tab.
        // Detached rather than stored: the explorer lives exactly as long as
        // the workspace, so there is nothing to tear down separately.
        let explorer = this.explorer.clone();
        cx.subscribe_in(
            &explorer,
            window,
            |this, _, event: &crate::ui::explorer::ExplorerEvent, window, cx| match event {
                // An image isn't text, and the editor refuses it -- so a
                // double-click on one opens the viewer instead. Everything
                // else, Markdown included, opens to be edited; the explorer's
                // "Preview" entry is how you ask for the rendered version.
                crate::ui::explorer::ExplorerEvent::OpenFile(path) => {
                    this.open_file(path.clone().into(), window, cx)
                }
                crate::ui::explorer::ExplorerEvent::PreviewFile(path) => {
                    this.open_preview(path.clone(), window, cx)
                }
                crate::ui::explorer::ExplorerEvent::OpenTerminal(path) => {
                    this.open_terminal_column_with(PtyProgram::ShellIn(path.clone()), window, cx)
                }
            },
        )
        .detach();

        // What the window opens on is a setting. Every branch of it has to
        // reach the first frame without waiting on anything: none of them dial,
        // read a file or spawn a shell before returning.
        match cx.global::<crate::store::Settings>().startup.as_str() {
            "local" => this.start_local(false, window, cx),
            "resume" => this.resume_at_launch(window, cx),
            // Home asks for nothing and is the fallback for an unknown value.
            _ => this.show_home(window, cx),
        }
        this
    }

    /// Wakes on each minute boundary rather than every sixty seconds, so the
    /// bar changes when the clock does instead of up to a minute afterwards.
    /// It never ends: the task is dropped with the workspace.
    fn tick_clock(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                use chrono::Timelike as _;
                let left = 60u64.saturating_sub(chrono::Local::now().second() as u64);
                cx.background_executor()
                    .timer(Duration::from_secs(left.max(1)))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
    }

    /// Opens a remote file in an editor tab, or raises the tab it is already
    /// open in -- the same file in two tabs would be two diverging copies of
    /// it, and whichever saved last would silently win.
    pub fn open_editor(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let ConnectionState::Connected(connection) = &self.connection else {
            return;
        };
        let connection = connection.clone();

        for pane in self.strip.panes() {
            let index = pane.read(cx).items().iter().position(|item| {
                matches!(item, PaneItem::Editor(view) if view.read(cx).path == path)
            });
            if let Some(index) = index {
                pane.update(cx, |pane, cx| pane.activate_item(index, window, cx));
                self.focus_pane(pane);
                cx.notify();
                return;
            }
        }

        let item = self.new_editor(connection, path, window, cx);
        self.add_item(item, window, cx);
    }

    /// Builds an editor tab and wires its events up. Both `open_editor` and the
    /// session restore go through here, so a tab that came back from last run
    /// behaves exactly like one just opened.
    fn new_editor(
        &mut self,
        host: Host,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneItem {
        let view = cx.new(|cx| crate::ui::editor::EditorView::new(host, path, window, cx));
        // The editor's Preview button, and the result of a "save and close".
        // Detached rather than stored: the subscription ends when the tab's
        // view is dropped, so there is no per-tab entry to forget to remove.
        cx.subscribe_in(
            &view,
            window,
            |this, editor, event: &crate::ui::editor::EditorEvent, window, cx| {
                let item = editor.read(cx).item_id;
                match event {
                    crate::ui::editor::EditorEvent::Preview => {
                        let path = editor.read(cx).path.clone();
                        this.open_preview(path, window, cx);
                    }
                    // Both of these only matter while "save and close" is
                    // waiting on this very tab; `tab_save_finished` checks that
                    // before it touches anything.
                    crate::ui::editor::EditorEvent::Saved => {
                        this.tab_save_finished(item, true, window, cx)
                    }
                    crate::ui::editor::EditorEvent::SaveFailed => {
                        this.tab_save_finished(item, false, window, cx)
                    }
                }
            },
        )
        .detach();
        PaneItem::Editor(view)
    }

    /// Opens a read-only preview -- Markdown rendered, or an image drawn -- or
    /// raises the tab already showing that file. Deduplicated by path like an
    /// editor, though a preview and an editor of the same file coexist: they
    /// are different kinds, which is the point of being able to split them.
    pub fn open_preview(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let ConnectionState::Connected(connection) = &self.connection else {
            return;
        };
        let connection = connection.clone();

        for pane in self.strip.panes() {
            let index = pane.read(cx).items().iter().position(|item| {
                matches!(item, PaneItem::Preview(view) if view.read(cx).path == path)
            });
            if let Some(index) = index {
                pane.update(cx, |pane, cx| pane.activate_item(index, window, cx));
                self.focus_pane(pane);
                cx.notify();
                return;
            }
        }

        let view = cx.new(|cx| crate::ui::preview::PreviewView::new(connection, path, cx));
        self.add_item(PaneItem::Preview(view), window, cx);
    }

    /// Raises an existing singleton tab if there is one, and reports whether it
    /// found one. Opening Settings twice should focus the tab you already have,
    /// not stack a second copy of it.
    fn activate_existing(
        &mut self,
        kind: ItemKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for pane in self.strip.panes() {
            let index = pane
                .read(cx)
                .items()
                .iter()
                .position(|item| item.kind() == kind);
            if let Some(index) = index {
                pane.update(cx, |pane, cx| pane.activate_item(index, window, cx));
                self.focus_pane(pane);
                cx.notify();
                return true;
            }
        }
        false
    }

    /// Opens the command bar, or focuses it if it is already up.
    pub fn open_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home_open = false;
        if let Some(view) = &self.connections {
            let handle = view.read(cx).focus_handle.clone();
            window.focus(&handle);
            cx.notify();
            return;
        }
        let view = cx.new(|cx| CommandBar::new(window, cx));
        self.connections_subscription = Some(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &CommandEvent, window, cx| match event {
                CommandEvent::Connect(params) => this.connect(params.clone(), window, cx),
                CommandEvent::UseLocal => this.connect_local(window, cx),
                CommandEvent::Disconnect => this.disconnect(window, cx),
                CommandEvent::Dismiss => this.close_connections(window, cx),
                // The rest close the bar first: every one of them puts
                // something else in front of you, and an overlay still up
                // over it would be the second thing to dismiss.
                CommandEvent::OpenSettings => {
                    this.close_connections(window, cx);
                    this.open_settings(window, cx);
                }
                CommandEvent::OpenOverview => {
                    this.close_connections(window, cx);
                    this.toggle_overview(window, cx);
                }
                CommandEvent::NewTerminal => {
                    this.close_connections(window, cx);
                    this.open_terminal_column(window, cx);
                }
                CommandEvent::OpenPath(path) => {
                    let path = path.clone();
                    this.close_connections(window, cx);
                    this.open_file(path, window, cx);
                }
            },
        ));
        self.connections = Some(view);
        self.sync_connections_view(cx);
        cx.notify();
    }

    /// Dismissing the popup with no session open returns to home rather than
    /// leaving an empty workspace with nothing on it: home is where the ways in
    /// are, and it is where the dismissal came from.
    pub fn close_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_connected() && self.connections.is_some() {
            self.connections = None;
            self.connections_subscription = None;
            self.show_home(window, cx);
            return;
        }
        self.connections = None;
        self.connections_subscription = None;
        self.focus_active_item(window, cx);
        cx.notify();
    }

    fn focus_active_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self
            .active_pane
            .read(cx)
            .active_item()
            .map(|item| item.focus_handle(cx));
        match handle {
            Some(handle) => window.focus(&handle),
            None => window.focus(&self.focus_handle),
        }
    }

    /// Whether the file tree is docked open. The navigation bar asks, to
    /// light its own button.
    pub fn explorer_open(&self, _cx: &App) -> bool {
        self.explorer_docked
    }

    /// Docks the file tree open beside the strip, or closes it again.
    ///
    /// **A panel, not a window.** It was a column on the strip for a while --
    /// resized at its seam, scrolled past, carried along like any other -- and
    /// that is exactly what was wrong with it: a file tree is somewhere you
    /// start from, and one that scrolls off the side or gets stacked under a
    /// shell is one you have to go looking for. So it is pinned to the left of
    /// the strip, outside it: never scrolled, never moved, never dropped into,
    /// and resized from its own edge. Closing it keeps the entity, so
    /// reopening is a repaint rather than a round trip per folder.
    pub fn toggle_explorer(&mut self, _: &ToggleExplorer, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_session(window, cx);
        self.home_open = false;
        self.explorer_docked = !self.explorer_docked;
        if self.explorer_docked {
            let handle = self.explorer.read(cx).focus_handle.clone();
            window.focus(&handle);
        } else {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // From home with nothing running, Settings used to open onto a
        // workspace with no host behind it -- terminals and the file tree then
        // had nothing to run on. This machine is the default.
        self.ensure_session(window, cx);
        self.home_open = false;
        if self.activate_existing(ItemKind::Settings, window, cx) {
            return;
        }
        let view = cx.new(|cx| SettingsView::new(window, cx));
        self.add_item(PaneItem::Settings(view), window, cx);
        // A full-strip window, as the design has it (frames 04 and 08): a
        // page of rows wants the width, and it is a place you visit rather
        // than something you work beside. Only when it got a column of its
        // own -- stacked into an empty one, it keeps that column's width.
        if self.active_pane.read(cx).items().len() == 1 {
            self.strip.set_focused_width(ColumnWidth::Full);
        }
    }

    /// Whether the window may close now. `false` puts the confirmation up.
    ///
    /// A live *SSH* session is worth a question: closing drops the connection,
    /// and with it every shell on it -- the remote side hangs them up, so
    /// whatever was running stops. A local one is not: those shells are this
    /// machine's own processes, and closing the window is the ordinary way to
    /// end them -- the same reason local mode offers no disconnect. An editor
    /// with unsaved changes is worth a question either way, since that text has
    /// never existed anywhere but this process. Closing a single tab asks the
    /// same question separately, through `PendingClose`.
    fn request_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return true;
        }
        // The last word on the session, written before anything is torn down:
        // the debounced save may still be pending, and divider positions reach
        // the file by this route alone -- a drag moves weights the panes never
        // hear about.
        self.save_session(cx);
        let connected = matches!(self.connection, ConnectionState::Connected(_)) && !self.is_local();
        if !connected && self.unsaved_files(cx).is_empty() {
            return true;
        }
        self.confirm_close = true;
        cx.notify();
        false
    }

    /// The names of editor tabs with unsaved changes, in tab order.
    fn unsaved_files(&self, cx: &App) -> Vec<SharedString> {
        self.strip
            .panes()
            .iter()
            .flat_map(|pane| pane.read(cx).items().to_vec())
            .filter_map(|item| match item {
                PaneItem::Editor(view) => {
                    let view = view.read(cx);
                    view.is_dirty()
                        .then(|| SharedString::from(view.name().to_string()))
                }
                _ => None,
            })
            .collect()
    }

    /// The title bar's X. Same question as the OS close, asked the same way.
    pub fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.request_close(window, cx) {
            window.remove_window();
        }
    }

    fn cancel_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Escape is bound once, in the `ConfirmClose` context both dialogs
        // declare, so it dismisses whichever of them is up.
        self.confirm_close = false;
        self.confirm_tab = None;
        self.focus_active_item(window, cx);
        cx.notify();
    }

    fn cancel_tab_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_tab = None;
        self.focus_active_item(window, cx);
        cx.notify();
    }

    /// A pane wants to close a tab that would lose work, and can't ask itself.
    ///
    /// The pane doesn't remove anything until it is told to; if this can't find
    /// the tab it named, nothing happens and the tab simply stays open.
    fn confirm_tab_close(&mut self, pane: Entity<Pane>, item: ItemId, cx: &mut Context<Self>) {
        let Some(editor) = Self::editor_in(&pane, item, cx) else {
            return;
        };
        let name = SharedString::from(editor.read(cx).name().to_string());
        self.confirm_tab = Some(PendingClose {
            pane,
            item,
            name,
            saving: false,
        });
        cx.notify();
    }

    fn editor_in(
        pane: &Entity<Pane>,
        item: ItemId,
        cx: &App,
    ) -> Option<Entity<crate::ui::editor::EditorView>> {
        pane.read(cx).items().iter().find_map(|i| match i {
            PaneItem::Editor(view) if view.read(cx).item_id == item => Some(view.clone()),
            _ => None,
        })
    }

    /// "close without saving" -- the answer the pane has been waiting for.
    fn discard_and_close_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.confirm_tab.take() else {
            return;
        };
        pending
            .pane
            .update(cx, |pane, cx| pane.force_close_item(pending.item, window, cx));
        cx.notify();
    }

    /// "save and close": write first, close when the write lands.
    ///
    /// The tab stays open and the dialog says "saving…" meanwhile. Closing now
    /// and letting the write finish in the background would be a lie in the one
    /// case that matters -- a failed save would take the text with it.
    fn save_and_close_tab(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.confirm_tab.as_mut() else {
            return;
        };
        pending.saving = true;
        let (pane, item) = (pending.pane.clone(), pending.item);
        if let Some(editor) = Self::editor_in(&pane, item, cx) {
            editor.update(cx, |editor, cx| editor.save_now(cx));
        }
        cx.notify();
    }

    /// A save finished while a dialog was waiting on it. On success the tab
    /// goes; on failure the dialog does, leaving the editor's own error banner
    /// to explain and the text still there to retry from.
    fn tab_save_finished(
        &mut self,
        item: ItemId,
        ok: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = matches!(&self.confirm_tab, Some(p) if p.saving && p.item == item);
        if !waiting {
            return;
        }
        if ok {
            self.discard_and_close_tab(window, cx);
        } else {
            self.confirm_tab = None;
            cx.notify();
        }
    }

    /// "Disconnect and close": hang the session up properly, *then* go.
    ///
    /// The goodbye is awaited rather than fired and forgotten, or the process
    /// would exit first and the server would see a dropped socket. Two seconds
    /// is the cap -- a server that never answers must not trap the app open.
    fn close_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing = true;
        cx.notify();

        let host = match &self.connection {
            ConnectionState::Connected(host) => Some(host.clone()),
            _ => None,
        };
        cx.spawn_in(window, async move |this, cx| {
            if let Some(connection) = host {
                let goodbye = Box::pin(connection.disconnect_and_wait());
                let deadline = Box::pin(cx.background_executor().timer(Duration::from_secs(2)));
                futures::future::select(goodbye, deadline).await;
            }
            let _ = this.update_in(cx, |_, window, _| window.remove_window());
        })
        .detach();
    }

    /// The "still connected" dialog: an overlay over everything, including the
    /// title bar.
    fn render_close_confirm(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (surface, border, fg, fg_muted, warning, danger) = {
            let t = cx.global::<Theme>();
            (t.surface, t.border, t.fg, t.fg_muted, t.warning, t.danger)
        };
        let connected = matches!(self.connection, ConnectionState::Connected(_));
        let label = match &self.connection {
            ConnectionState::Connected(connection) => connection.label.clone(),
            _ => String::new(),
        };
        let unsaved = self.unsaved_files(cx);
        let in_progress = self.closing;

        // Focus follows the dialog so Escape reaches its context. Checked
        // first: focusing unconditionally would steal it back every frame.
        if !self.close_focus.is_focused(window) {
            window.focus(&self.close_focus);
        }

        let button = move |id: &'static str, text: &'static str, tint: gpui::Hsla| {
            div()
                .id(id)
                .px_3()
                .py_0p5()
                .border_1()
                .border_color(tint)
                .text_color(tint)
                .hover(move |s| s.bg(tint.opacity(0.2)))
                .child(text)
        };

        div()
            .id("confirm-close")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.5))
            // Or a click meant for the dialog lands in the terminal behind it.
            .block_mouse_except_scroll()
            .track_focus(&self.close_focus)
            .key_context("ConfirmClose")
            .on_action(cx.listener(|this, _: &CancelClose, window, cx| {
                this.cancel_close(window, cx)
            }))
            // Clicking the backdrop is the same as cancelling.
            .on_click(cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                this.cancel_close(window, cx)
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(gpui::px(460.))
                    .p_4()
                    .bg(surface)
                    .border_1()
                    .border_color(border)
                    // Clicks inside the card aren't a dismissal.
                    .occlude()
                    .child(div().text_color(fg).child("close PulseTerm?"))
                    .when(connected, |this| {
                        this.child(div().text_color(fg_muted).child(format!(
                            "{label} is still connected. Closing hangs the session up, \
                             and anything still running in its tabs stops."
                        )))
                    })
                    .when(!unsaved.is_empty(), |this| {
                        this.child(
                            div()
                                .text_color(warning)
                                .child(format!("Unsaved changes in {}.", name_list(&unsaved))),
                        )
                    })
                    .child(if in_progress {
                        // The disconnect is a round trip, so say what's holding
                        // things up rather than leaving a dead dialog.
                        div()
                            .flex()
                            .justify_end()
                            .pt_1()
                            .text_color(fg_muted)
                            .child(if connected { "disconnecting…" } else { "closing…" })
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .pt_1()
                            .child(button("close-cancel", "cancel", fg_muted).on_click(
                                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                                    this.cancel_close(window, cx)
                                }),
                            ))
                            .child(
                                button(
                                    "close-anyway",
                                    if connected {
                                        "disconnect and close"
                                    } else {
                                        "close anyway"
                                    },
                                    danger,
                                )
                                .on_click(cx.listener(
                                    |this, _: &gpui::ClickEvent, window, cx| {
                                        this.close_anyway(window, cx)
                                    },
                                )),
                            )
                            .into_any_element()
                    }),
            )
    }

    /// The unsaved-tab dialog. Same shape and same key context as the window's,
    /// so Escape behaves identically; only one of the two is ever rendered.
    fn render_tab_close_confirm(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (surface, border, fg, fg_muted, warning, danger, accent) = {
            let t = cx.global::<Theme>();
            (
                t.surface,
                t.border,
                t.fg,
                t.fg_muted,
                t.warning,
                t.danger,
                t.accent,
            )
        };
        let Some(pending) = self.confirm_tab.as_ref() else {
            // Unreachable: the caller renders this only when there is one. The
            // id keeps the two arms the same type.
            return div().id("confirm-tab-close");
        };
        let name = pending.name.clone();
        let saving = pending.saving;

        if !self.close_focus.is_focused(window) {
            window.focus(&self.close_focus);
        }

        let button = move |id: &'static str, text: &'static str, tint: gpui::Hsla| {
            div()
                .id(id)
                .px_3()
                .py_0p5()
                .border_1()
                .border_color(tint)
                .text_color(tint)
                .hover(move |s| s.bg(tint.opacity(0.2)))
                .child(text)
        };

        div()
            .id("confirm-tab-close")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.5))
            .block_mouse_except_scroll()
            .track_focus(&self.close_focus)
            .key_context("ConfirmClose")
            .on_action(
                cx.listener(|this, _: &CancelClose, window, cx| this.cancel_tab_close(window, cx)),
            )
            .on_click(
                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                    this.cancel_tab_close(window, cx)
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(gpui::px(460.))
                    .p_4()
                    .bg(surface)
                    .border_1()
                    .border_color(border)
                    .occlude()
                    .child(div().text_color(fg).child(format!("close {name}?")))
                    .child(
                        div()
                            .text_color(warning)
                            .child("It has unsaved changes. Closing the tab discards them."),
                    )
                    .child(if saving {
                        div()
                            .flex()
                            .justify_end()
                            .pt_1()
                            .text_color(fg_muted)
                            .child("saving…")
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .pt_1()
                            .child(button("tab-cancel", "cancel", fg_muted).on_click(
                                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                                    this.cancel_tab_close(window, cx)
                                }),
                            ))
                            .child(button("tab-discard", "close without saving", danger).on_click(
                                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                                    this.discard_and_close_tab(window, cx)
                                }),
                            ))
                            .child(button("tab-save", "save and close", accent).on_click(
                                cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                                    this.save_and_close_tab(cx)
                                }),
                            ))
                            .into_any_element()
                    }),
            )
    }

    /// Steps the font size. It lives in `Settings`, so one step reaches every
    /// terminal, editor and panel at once, and is saved.
    ///
    /// Deliberately *not* per-pane: an editor tab takes its size from
    /// gpui-component's theme, which is global, so a per-pane size would work
    /// for terminals and quietly do nothing for editors. `Settings::update`
    /// clamps to the 8-32px range the Settings page offers.
    /// The docked file tree: a panel of its own width, a hairline on its strip
    /// side, and a 7px grab band over that hairline -- the same seam the
    /// columns have, because a press a pixel off a 1px line reads as "it won't
    /// resize". The band is absolute, so it costs the layout nothing.
    fn render_explorer_dock(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let border = theme.border;
        let accent = theme.accent;
        let bg = theme.bg;
        let measured = self.explorer_bounds.clone();
        div()
            .relative()
            .flex_none()
            .h_full()
            .w(gpui::px(self.explorer_width))
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .child(
                gpui::canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(div().size_full().overflow_hidden().child(self.explorer.clone()))
            .child(
                div()
                    .id("explorer-seam")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(gpui::px(-4.))
                    .w(gpui::px(7.))
                    .cursor_col_resize()
                    .hover(move |this| this.bg(accent.opacity(0.35)))
                    .on_drag(ExplorerSeam, |_, _, _, cx| {
                        cx.new(|_| crate::ui::resize::NoPreview)
                    })
                    .on_drag_move(cx.listener(
                        |this, event: &gpui::DragMoveEvent<ExplorerSeam>, window, cx| {
                            let left = f32::from(this.explorer_bounds.get().origin.x);
                            let most = f32::from(window.viewport_size().width) * EXPLORER_MAX_SHARE;
                            this.explorer_width = (f32::from(event.event.position.x) - left)
                                .clamp(EXPLORER_MIN, most.max(EXPLORER_MIN));
                            cx.notify();
                        },
                    )),
            )
            .into_any_element()
    }

    fn adjust_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        crate::store::Settings::update(cx, |settings| settings.font_size += delta);
    }

    fn reset_font_size(&mut self, cx: &mut Context<Self>) {
        let default = crate::store::Settings::default().font_size;
        crate::store::Settings::update(cx, |settings| settings.font_size = default);
    }

    /// Opens an item in a column of its own -- Settings, an editor, a preview,
    /// the file tree. Stacking into the focused column is the deliberate
    /// gesture (`^⇧D`, a header's "+"), never the default: a new window
    /// splitting the one you were reading in half is exactly what the strip
    /// exists to avoid.
    fn add_item(&mut self, item: PaneItem, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.column_for_new_window(window, cx);
        pane.update(cx, |pane, cx| pane.add_item(item, true, window, cx));
        cx.notify();
    }

    /// Where a new window goes: the focused column if it is empty -- a fresh
    /// workspace starts with one, and putting the first window beside it
    /// would leave a column of nothing on screen -- and otherwise a new column
    /// right of the focus, at the focused column's width. Focuses it either way.
    fn column_for_new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        if self.active_pane.read(cx).is_empty() {
            return self.active_pane.clone();
        }
        // Any other column left empty -- say, from a restored layout -- has no
        // reason to stay now that there is a window to look at. Collapsed
        // *before* the new one goes in: the new column is empty too until the
        // caller fills it, and collapsing after would remove it, leaving the
        // window in a column that is no longer on the strip.
        self.collapse_empty_columns(cx);
        let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
        self.register_pane(&pane, window, cx);
        // The setting, not the focused column's width: the design makes it a
        // choice (Settings -> Interface -> Default column width).
        let width = match cx.global::<crate::store::Settings>().default_width.as_str() {
            "third" => ColumnWidth::Third,
            "two_thirds" => ColumnWidth::TwoThirds,
            "full" => ColumnWidth::Full,
            _ => ColumnWidth::Half,
        };
        self.strip.insert_after_focus(pane.clone(), width);
        self.active_pane = pane.clone();
        self.renumber_columns(cx);
        pane
    }

    /// Pushes the live connection state into the connections popup, which mirrors
    /// it rather than reaching back into the workspace to read it. Status only:
    /// this also runs every time the popup opens, so it must not touch anything
    /// that a repeat of the same state would reset.
    fn sync_connections_view(&mut self, cx: &mut Context<Self>) {
        let status = match &self.connection {
            ConnectionState::Disconnected => ConnectionStatus::Disconnected,
            ConnectionState::Connecting(label) => ConnectionStatus::Connecting(label.clone()),
            ConnectionState::Connected(host) if host.is_local() => {
                ConnectionStatus::Local
            }
            ConnectionState::Connected(host) => ConnectionStatus::Connected(host.label.clone().into()),
            ConnectionState::Failed(error) => ConnectionStatus::Failed(error.clone()),
        };
        if let Some(view) = self.connections.clone() {
            view.update(cx, |view, cx| view.set_status(status, cx));
        }
    }

    /// Every connection transition goes through here, so the popup, the
    /// explorer and the status bar can't disagree about it.
    ///
    /// The explorer is only told on a real transition: `set_connection` resets
    /// it to the home directory, and it once ran on every popup open, which
    /// threw away wherever you had browsed to.
    fn set_connection(&mut self, state: ConnectionState, cx: &mut Context<Self>) {
        self.connection = state;
        self.sync_connections_view(cx);
        let connection = match &self.connection {
            ConnectionState::Connected(connection) => Some(connection.clone()),
            _ => None,
        };
        self.explorer
            .update(cx, |explorer, cx| explorer.set_host(connection, cx));
        cx.notify();
    }

    /// Tears the host down. Dropping the terminals drops their `Pty`s, which
    /// closes each SSH channel (or each pseudo-console); dropping the last
    /// `Connection` clone ends an SSH session itself.
    ///
    /// Local mode offers no disconnect -- there is no session to hang up -- but
    /// this still works if something reaches it, and leaves an empty workspace.
    pub fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let ConnectionState::Connected(host) = &self.connection {
            host.disconnect();
        }
        // Synced here rather than left to `open_connections` below: that returns
        // early when the popup is already up -- which it always is when its own
        // disconnect button was the trigger -- so the popup went on showing
        // "connected" and the explorer its stale tree.
        self.set_connection(ConnectionState::Disconnected, cx);
        // Nothing to restore onto either: a layout is only meaningful against
        // the host it was arranged on, and there is no longer one.
        self.target = None;
        // Terminals and editors go -- both run over this connection's channels,
        // and an editor left open could never save again. A Settings tab isn't
        // tied to a session, so it stays.
        for pane in self.strip.panes() {
            pane.update(cx, |pane, cx| {
                pane.close_items_of_kind(ItemKind::Terminal, window, cx);
                pane.close_items_of_kind(ItemKind::Editor, window, cx);
                pane.close_items_of_kind(ItemKind::Preview, window, cx);
            });
        }
        self.collapse_empty_columns(cx);
        self.open_connections(window, cx);
        cx.notify();
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        pane.update(cx, |pane, cx| pane.close_active_item(window, cx));
        self.collapse_empty_columns(cx);
        // The window that had the keyboard is gone. If it was its column's
        // last, the column went with it, and focus would otherwise stay on a
        // view that is no longer drawn -- every key after that goes nowhere.
        self.focus_active_item(window, cx);
        cx.notify();
    }

    /// Points the focus at a column -- both halves of it, the strip's index and
    /// the workspace's entity. Nothing else may set `active_pane`: the strip
    /// scrolls to its index while every action acts on the entity, and the two
    /// disagreeing means acting on a column that is not the one on screen.
    fn focus_pane(&mut self, pane: Entity<Pane>) {
        self.strip.focus_on(&pane);
        self.active_pane = pane;
    }

    /// Hands every column its position on the strip, 1-based. A window header
    /// shows it (`02`, `02.2`), which is how columns of identical shells are
    /// told apart -- and a pane has no way of knowing where it sits, so the
    /// strip tells it after anything that inserts, removes or reorders.
    /// Hands every column the width its headers print. Run each frame, from
    /// the top of `render`, because the share drawn depends on the viewport --
    /// a window resize, a seam drag or the fill-the-window stretch all change
    /// it without any structural change to hook. Updates only what differs,
    /// so a still frame costs a comparison per column and no repaint.
    fn sync_width_labels(&mut self, cx: &mut Context<Self>) {
        let shares = self.strip.readout();
        for (pane, (share, _)) in self.strip.panes().into_iter().zip(shares) {
            let label = SharedString::from(strip::width_label(share));
            pane.update(cx, |pane, cx| {
                if pane.width_label != label {
                    pane.width_label = label;
                    cx.notify();
                }
            });
        }
    }

    fn renumber_columns(&mut self, cx: &mut Context<Self>) {
        for (pane, ordinal) in self.strip.ordinals() {
            pane.update(cx, |pane, cx| {
                if pane.ordinal != ordinal {
                    pane.ordinal = ordinal;
                    cx.notify();
                }
            });
        }
    }

    /// Moves focus one column over and takes the keyboard with it. Nothing
    /// happens at either end: the strip does not wrap, because where you are
    /// on it is the only thing you have to keep in your head.
    fn focus_column(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.strip.focus_step(delta) {
            self.active_pane = pane;
            self.focus_active_item(window, cx);
            cx.notify();
        }
    }

    /// Carries the focused column past its neighbour, focus travelling with it.
    fn move_column(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.strip.move_focused(delta) {
            self.renumber_columns(cx);
            cx.notify();
        }
    }

    /// Steps the focused column through the width presets -- a third, a half,
    /// two thirds, the whole viewport -- and touches no other column. That is
    /// what the strip is for: widening one window never squeezes the one
    /// beside it, it pushes it along.
    pub fn cycle_column_width(&mut self, cx: &mut Context<Self>) {
        if self.strip.cycle_focused_width().is_some() {
            cx.notify();
        }
    }

    /// Reorders the focused window inside its column. It never leaves the
    /// column -- `expel_window` is the way out.
    fn move_window(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        let (item_id, to) = {
            let column = pane.read(cx);
            let to = column.active_ix() as isize + delta;
            if to < 0 || to as usize >= column.items().len() {
                return;
            }
            match column.active_item() {
                Some(item) => (item.id(cx), to as usize),
                None => return,
            }
        };
        pane.update(cx, |column, cx| {
            if let Some(item) = column.take_item(item_id, cx) {
                column.insert_item(item, to, true, window, cx);
            }
        });
        cx.notify();
    }

    /// Pushes the focused window out of its column into a new one of its own,
    /// right of it -- niri calls it expelling, and it is the way back out of a
    /// stack that has grown too tall.
    ///
    /// Refused on a column of one: taking the window out empties the column,
    /// the new one receives it, and `collapse_empty_columns` removes the
    /// original -- a lot of churn to land exactly where you started.
    fn expel_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.active_pane.clone();
        let item_id = {
            let column = source.read(cx);
            if column.items().len() <= 1 {
                return;
            }
            match column.active_item() {
                Some(item) => item.id(cx),
                None => return,
            }
        };
        let Some(item) = source.update(cx, |column, cx| column.take_item(item_id, cx)) else {
            return;
        };
        let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
        self.register_pane(&pane, window, cx);
        let width = self.strip.focused_width().unwrap_or(ColumnWidth::Half);
        self.strip.insert_after_focus(pane.clone(), width);
        pane.update(cx, |column, cx| column.add_item(item, true, window, cx));
        self.active_pane = pane;
        self.renumber_columns(cx);
        cx.notify();
    }

    /// Pulls the top window of the next column into the focused one, which is
    /// how a stack gets built out of what is already on the strip. A column
    /// emptied by this collapses itself, through the `Empty` it emits.
    fn consume_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.strip.at(self.strip.focus_index() + 1) else {
            return;
        };
        let item_id = match source.read(cx).items().first() {
            Some(item) => item.id(cx),
            None => return,
        };
        let Some(item) = source.update(cx, |column, cx| column.take_item(item_id, cx)) else {
            return;
        };
        self.active_pane
            .update(cx, |column, cx| column.add_item(item, true, window, cx));
        self.collapse_empty_columns(cx);
        cx.notify();
    }

    /// Lands a window dragged by its header: into `onto`'s column above or below
    /// window `target`, or into a new column beside it. The keyboard has had
    /// all of this for a while (expel, consume, carry); this is the mouse.
    ///
    /// The item is re-parented with `take_item` / `insert_item`, never rebuilt,
    /// so a terminal keeps its shell and an editor its unsaved text. A column
    /// the window leaves empty collapses itself through the `Empty` it emits.
    #[allow(clippy::too_many_arguments)]
    fn drop_window(
        &mut self,
        from: Entity<Pane>,
        item: ItemId,
        onto: Entity<Pane>,
        target: usize,
        zone: DropZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_ix) = from.read(cx).index_of(item, cx) else {
            return;
        };
        let alone = from.read(cx).items().len() == 1;
        match zone {
            DropZone::Above | DropZone::Below => {
                let mut at = target + usize::from(zone == DropZone::Below);
                if from == onto {
                    // Taking it out shifts everything after it up by one.
                    if source_ix < at {
                        at -= 1;
                    }
                    if source_ix == at {
                        return;
                    }
                }
                let Some(taken) = from.update(cx, |pane, cx| pane.take_item(item, cx)) else {
                    return;
                };
                onto.update(cx, |pane, cx| pane.insert_item(taken, at, true, window, cx));
                self.focus_pane(onto);
            }
            DropZone::Left | DropZone::Right => {
                // The only window of a column, dropped beside that same column,
                // would leave and arrive in the same place.
                if from == onto && alone {
                    return;
                }
                let Some(column) = self.strip.index_of(&onto) else {
                    return;
                };
                let width = self
                    .strip
                    .columns()
                    .get(column)
                    .map(|c| c.width)
                    .unwrap_or(ColumnWidth::Half);
                let Some(taken) = from.update(cx, |pane, cx| pane.take_item(item, cx)) else {
                    return;
                };
                let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
                self.register_pane(&pane, window, cx);
                let at = column + usize::from(zone == DropZone::Right);
                self.strip.insert_at(at, pane.clone(), width);
                pane.update(cx, |pane, cx| pane.add_item(taken, true, window, cx));
                self.active_pane = pane;
            }
        }
        // `collapse_empty_columns` renumbers on its way out, which covers the
        // column this may have added as well as any it emptied.
        self.collapse_empty_columns(cx);
        cx.notify();
    }

    /// Moves the keyboard between the windows stacked in the focused column.
    /// All of them are on screen, so this chooses which one types, not which
    /// one you can see.
    fn focus_window(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane.update(cx, |pane, cx| {
            if delta < 0 {
                pane.activate_prev(window, cx);
            } else {
                pane.activate_next(window, cx);
            }
        });
        cx.notify();
    }

    /// Pane events are all requests to restructure the strip, which only the
    /// workspace is allowed to do -- panes never reach into each other.
    /// `subscribe_in` rather than `subscribe` because every one of these needs
    /// a `Window` to move focus.
    fn subscribe_to_pane(
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            pane,
            window,
            |this, pane, event: &PaneEvent, window, cx| match event {
                PaneEvent::Focused => {
                    if this.active_pane != *pane {
                        this.focus_pane(pane.clone());
                        cx.notify();
                    }
                }
                PaneEvent::Empty => {
                    this.collapse_empty_columns(cx);
                    // Same reason as in `close_active_tab`: the emptied column
                    // may have held the focus, and it is no longer drawn.
                    this.focus_active_item(window, cx);
                    cx.notify();
                }
                // The "+" in a column's own header stacks another window in
                // that column, whichever one it was.
                PaneEvent::NewTerminal => {
                    this.focus_pane(pane.clone());
                    this.open_terminal(PtyProgram::LoginShell, window, cx);
                }
                PaneEvent::ConfirmClose { item } => {
                    this.confirm_tab_close(pane.clone(), *item, cx);
                }
                PaneEvent::DropWindow {
                    from,
                    item,
                    target,
                    zone,
                } => {
                    this.drop_window(from.clone(), *item, pane.clone(), *target, *zone, window, cx);
                }
            },
        )
    }

    fn register_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let id = pane.read(cx).id;
        let subscription = Self::subscribe_to_pane(pane, window, cx);
        self.pane_subscriptions.insert(id, subscription);
    }

    /// Notices that the layout changed and marks the session file dirty.
    ///
    /// Called from `render`, which looks like the wrong place until you try the
    /// obvious one: `cx.observe` never fires for a pane. gpui's `notify` on an
    /// entity that a window renders takes the invalidation path instead and
    /// pushes no `Effect::Notify` at all, so nothing observing a view is ever
    /// told (`App::notify`). The frame is the signal that is left -- and the
    /// comparison is a hash of ids and weights, so an unchanged layout costs no
    /// allocation and no write.
    ///
    /// Must run before any pane is rendered, which is why it is the first thing
    /// in `render`: reading a leased entity aborts the process.
    fn track_layout_changes(&mut self, cx: &mut Context<Self>) {
        let print = self.layout_fingerprint(cx);
        if self.layout_print != print {
            self.layout_print = print;
            self.schedule_session_save(cx);
        }
    }

    fn layout_fingerprint(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.target.is_some().hash(&mut hasher);
        // The explorer's directory, because it is saved with its window and
        // nothing else notices it changing. Its width is the column's now,
        // and that is already in the widths below.
        self.explorer.read(cx).root().hash(&mut hasher);
        self.explorer_docked.hash(&mut hasher);
        self.explorer_width.to_bits().hash(&mut hasher);
        self.strip.focus_index().hash(&mut hasher);
        // Ids and widths only. What a window *is* never changes after it is
        // built -- an editor keeps its path for life -- so an id standing in
        // for its contents is enough to notice everything the session file
        // records.
        for column in self.strip.columns() {
            column.width.share().to_bits().hash(&mut hasher);
            let pane = column.pane.read(cx);
            pane.id.0.hash(&mut hasher);
            pane.active_ix().hash(&mut hasher);
            for item in pane.items() {
                item.id(cx).0.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Marks the session file dirty. Batched behind a short timer: one gesture
    /// can change the layout several frames running, and each of those would
    /// otherwise be a write.
    fn schedule_session_save(&mut self, cx: &mut Context<Self>) {
        self.session_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| this.save_session(cx));
        }));
    }

    /// Writes `session.json` now, cancelling any pending debounce.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        // Last run's session is still unused, so nothing here may overwrite it:
        // the first frame after launch would otherwise replace a saved layout
        // with the empty workspace that is about to restore it.
        if self.pending_session.is_some() {
            return;
        }
        self.session_save = None;
        self.snapshot_session(cx).save();
    }

    fn snapshot_session(&self, cx: &App) -> Session {
        Session {
            // Without a target there is nothing to restore onto, which is
            // exactly what quitting from a disconnected workspace should leave
            // behind: no layout, no resume, a clean start next time.
            layout: self.target.is_some().then(|| self.snapshot_strip(cx)),
            target: self.target.clone(),
            explorer: self.target.is_some().then(|| crate::session::SavedExplorer {
                open: self.explorer_docked,
                path: self.explorer.read(cx).root().map(str::to_string),
                width: Some(self.explorer_width),
            }),
            saved_at: None,
        }
    }

    /// The strip, written in the shape the session file already had: one
    /// horizontal axis of panes *is* a strip, so last run's file still loads
    /// and this run's stays readable. `widths` carries each column's share of
    /// the viewport; `flexes`, the old tree's divider weights, goes out empty.
    fn snapshot_strip(&self, cx: &App) -> SavedMember {
        SavedMember::Axis {
            vertical: false,
            members: self
                .strip
                .columns()
                .iter()
                .map(|column| {
                    let pane = column.pane.read(cx);
                    SavedMember::Pane {
                        items: pane
                            .items()
                            .iter()
                            .map(|item| Self::snapshot_item(item, cx))
                            .collect(),
                        active: pane.active_ix(),
                    }
                })
                .collect(),
            flexes: Vec::new(),
            widths: self
                .strip
                .columns()
                .iter()
                .map(|column| column.width.share())
                .collect(),
        }
    }

    fn snapshot_item(item: &PaneItem, cx: &App) -> SavedItem {
        match item {
            PaneItem::Terminal(_) => SavedItem::Terminal,
            PaneItem::Settings(_) => SavedItem::Settings,
            PaneItem::Editor(view) => SavedItem::Editor {
                path: view.read(cx).path.clone(),
            },
            PaneItem::Preview(view) => SavedItem::Preview {
                path: view.read(cx).path.clone(),
            },
        }
    }

    /// Decides whether last run's layout belongs to the connection being made,
    /// and if it does, queues the explorer state that goes with it and hands
    /// the layout back to be rebuilt.
    ///
    /// Refuses when anything is already open: tabs opened while disconnected
    /// are real work, and a restore that threw them away would be worse than no
    /// restore at all. Runs *before* the host is published, because
    /// `ExplorerView::set_host` heads for the home directory the instant it
    /// arrives and the saved directory has to be queued first.
    fn take_matching_session(
        &mut self,
        target: &SavedTarget,
        cx: &mut Context<Self>,
    ) -> Option<SavedMember> {
        let session = self.pending_session.take()?;
        let workspace_untouched =
            self.strip.len() == 1 && self.active_pane.read(cx).is_empty();
        if !session.matches(target) || !workspace_untouched {
            return None;
        }
        // Queued before the host is published: `set_host` heads for the home
        // directory the instant it arrives, and the saved path has to already
        // be in place to be preferred over it.
        // A file from before the tree was docked carries it as a window in the
        // layout; that still means "docked, at this path".
        let legacy = session.layout.as_ref().and_then(explorer_path);
        let saved = session.explorer.clone().unwrap_or_default();
        self.explorer_docked = saved.open || legacy.is_some();
        if let Some(width) = saved.width {
            self.explorer_width = width.max(EXPLORER_MIN);
        }
        let path = saved.path.or(legacy);
        self.explorer.update(cx, |explorer, _| explorer.restore(path));
        session.layout
    }

    /// Rebuilds the saved layout as a strip. The old one is dropped wholesale
    /// -- it is one empty column by the time we get here, which
    /// `take_matching_session` has already insisted on.
    ///
    /// A file written while the split tree still existed describes a nest, so
    /// the panes in it are *flattened* in reading order rather than refused: a
    /// tree of four panes comes back as four columns. Nothing is lost that was
    /// worth keeping -- a vertical split had no width to carry over, and the
    /// shells all die with their session either way.
    fn restore_layout(
        &mut self,
        layout: SavedMember,
        host: &Host,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_subscriptions.clear();
        let mut saved = Vec::new();
        let mut widths = Vec::new();
        Self::flatten_member(layout, &mut saved, &mut widths);

        let mut columns = Vec::new();
        for (ix, (items, active)) in saved.into_iter().enumerate() {
            let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
            self.register_pane(&pane, window, cx);
            for item in items {
                // A legacy file-tree window: docked by `take_matching_session`.
                if matches!(item, SavedItem::Files { .. }) {
                    continue;
                }
                let item = self.build_item(item, host, window, cx);
                // Not focused here: focus is placed once at the end, or every
                // restored window would fight for it in turn.
                pane.update(cx, |pane, cx| pane.add_item(item, false, window, cx));
            }
            // A column that comes back with nothing in it is a column of
            // nothing on screen. (Terminals save nothing but do come back, as
            // fresh shells; this is a column that was empty when saved.)
            if pane.read(cx).is_empty() {
                self.pane_subscriptions.remove(&pane.read(cx).id);
                continue;
            }
            pane.update(cx, |pane, cx| pane.activate_item(active, window, cx));
            let width = widths
                .get(ix)
                .copied()
                .map(ColumnWidth::from_share)
                .unwrap_or(ColumnWidth::Half);
            columns.push((pane, width));
        }

        // A hand-edited file can describe a layout with no panes in it at all,
        // and a workspace with nowhere to put a window is unusable. One empty
        // column is what a fresh launch starts from.
        if columns.is_empty() {
            let pane = cx.new(|cx| Pane::new(next_pane_id(), cx));
            self.register_pane(&pane, window, cx);
            columns.push((pane, ColumnWidth::Half));
        }

        self.strip = Strip::restored(columns);
        if let Some(first) = self.strip.focused() {
            self.active_pane = first;
        }
        // A layout with no windows at all restores to a bare strip, which is a
        // connection with nothing to show for it. Give it what a fresh
        // connect would have had.
        if self.active_pane.read(cx).is_empty() {
            self.open_terminal(PtyProgram::LoginShell, window, cx);
        }
        self.renumber_columns(cx);
        self.focus_active_item(window, cx);
        cx.notify();
    }

    /// Reads a saved layout -- of any shape -- as a flat list of columns, with
    /// the width shares that go with them when the file has one per column.
    ///
    /// The shares are only taken from a row of plain panes. A nest has no
    /// answer for what a column's width should be, and half a list of shares
    /// would silently hand one column the width meant for another.
    fn flatten_member(
        saved: SavedMember,
        out: &mut Vec<(Vec<SavedItem>, usize)>,
        shares: &mut Vec<f32>,
    ) {
        match saved {
            SavedMember::Pane { items, active } => out.push((items, active)),
            SavedMember::Axis {
                members, widths, ..
            } => {
                let flat = members
                    .iter()
                    .all(|member| matches!(member, SavedMember::Pane { .. }));
                if shares.is_empty() && flat && widths.len() == members.len() {
                    *shares = widths;
                }
                for member in members {
                    Self::flatten_member(member, out, shares);
                }
            }
        }
    }

    fn build_item(
        &mut self,
        saved: SavedItem,
        host: &Host,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneItem {
        match saved {
            // A fresh shell: the old one died with last run's session, and
            // nothing about it survives to be resumed.
            SavedItem::Terminal => {
                let label: SharedString = host.label.clone().into();
                PaneItem::Terminal(cx.new(|cx| {
                    TerminalView::new(host.clone(), label, PtyProgram::LoginShell, window, cx)
                }))
            }
            SavedItem::Settings => PaneItem::Settings(cx.new(|cx| SettingsView::new(window, cx))),
            SavedItem::Editor { path } => self.new_editor(host.clone(), path, window, cx),
            SavedItem::Preview { path } => PaneItem::Preview(
                cx.new(|cx| crate::ui::preview::PreviewView::new(host.clone(), path, cx)),
            ),
            // Never built: the file tree is a docked panel now, and the
            // restore loop skips these (see `take_matching_session`).
            SavedItem::Files { .. } => unreachable!("the restore loop skips file-tree windows"),
        }
    }

    /// Points the connections popup at the host the saved layout belongs to, so
    /// resuming is one password away. Nothing secret is stored, so this can get
    /// no further by itself.
    fn prefill_saved_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SavedTarget::Ssh {
            host,
            port,
            username,
        }) = self.pending_session.as_ref().and_then(|s| s.target.clone())
        else {
            return;
        };
        let profile = cx
            .global::<Profiles>()
            .items
            .iter()
            .find(|p| p.host == host && p.port == port && p.username == username)
            .map(|p| p.id);
        let (Some(id), Some(view)) = (profile, self.connections.clone()) else {
            return;
        };
        view.update(cx, |view, cx| view.load_profile(id, window, cx));
    }


    fn connect(&mut self, params: ConnectParams, window: &mut Window, cx: &mut Context<Self>) {
        // Connecting somewhere while already somewhere -- most likely leaving a
        // local session for a remote one -- is a move, not an addition. The old
        // host's tabs cannot follow it: their shells and their open files
        // belong to the machine being left.
        if matches!(self.connection, ConnectionState::Connected(_)) {
            self.disconnect(window, cx);
        }
        let label: SharedString = format!("{}@{}", params.username, params.host_id()).into();
        self.set_connection(ConnectionState::Connecting(label.clone()), cx);

        let remembered = params.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::ssh::connect(params).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(connection) => {
                    // Only remember a target that actually accepted us, so the
                    // saved list never fills with typos. No secret is stored --
                    // see `store::SavedAuth`.
                    cx.update_global::<Profiles, _>(|profiles, _| profiles.remember(&remembered));
                    this.target = Some(SavedTarget::ssh(&remembered));
                    // Decided before the connection is published: the explorer
                    // leaves for the home directory as soon as it has one.
                    let layout = this.take_matching_session(&SavedTarget::ssh(&remembered), cx);
                    let host = Host::remote(connection);
                    this.home_open = false;
                    this.set_connection(ConnectionState::Connected(host.clone()), cx);
                    this.close_connections(window, cx);
                    match layout {
                        // A restored layout brings its own terminals.
                        Some(layout) => this.restore_layout(layout, &host, window, cx),
                        None => this.open_terminal(PtyProgram::LoginShell, window, cx),
                    }
                    cx.notify();
                }
                Err(error) => this.set_connection(ConnectionState::Failed(error.into()), cx),
            });
        })
        .detach();
    }

    /// Puts the home screen up. It takes the whole window: the title bar, rail,
    /// status line and every pane stop being rendered, which is what makes it
    /// somewhere to *be* rather than a panel to look past.
    ///
    /// A session behind it keeps running -- its shells are not rendered, but
    /// nothing about a pty depends on being on screen.
    pub fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_open {
            return;
        }
        self.home_open = true;
        window.focus(&self.home_focus);
        // Nothing to start: the screen animates, and `home::render` asks for
        // the next frame itself for as long as it is the thing being rendered.
        cx.notify();
    }

    /// Steps back off home into the session behind it. Does nothing when there
    /// is none: escape would otherwise drop you on an empty workspace with no
    /// way back except the rail you just left.
    pub fn hide_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_connected() {
            return;
        }
        self.leave_home(window, cx);
    }

    fn leave_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home_open = false;
        self.focus_active_item(window, cx);
        cx.notify();
    }

    /// Space on the home screen: pick up where the last run left off.
    ///
    /// A local session goes straight back -- there is nothing to authenticate.
    /// A remote one cannot: no password is stored, by design, so the best this
    /// can do is come up on that host with the form filled in.
    pub fn continue_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_connected() {
            self.leave_home(window, cx);
            return;
        }
        match self.saved_target() {
            Some(SavedTarget::Local) => self.connect_local(window, cx),
            Some(SavedTarget::Ssh { .. }) | None => {
                self.open_connections(window, cx);
                self.prefill_saved_target(window, cx);
            }
        }
    }

    pub fn home_focus(&self) -> &FocusHandle {
        &self.home_focus
    }

    pub fn home_dials(&self) -> std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>> {
        self.home_dials.clone()
    }

    /// What the home screen's RESUME block says. The live session when there
    /// is one -- home is then a screen you step off -- and otherwise last
    /// run's layout, read out of the pending session without restoring it.
    pub fn resume(&self, cx: &App) -> Option<crate::ui::home::Resume> {
        use crate::ui::home::Resume;
        if self.is_connected() {
            let shares = self
                .strip
                .readout()
                .into_iter()
                .map(|(share, segment)| (share, segment == strip::Segment::Focus))
                .collect();
            let windows = self
                .strip
                .panes()
                .iter()
                .map(|pane| pane.read(cx).items().len())
                .sum();
            return Some(Resume {
                target: self.saved_target()?,
                columns: self.strip.len(),
                windows,
                shares,
                closed: None,
                live: true,
            });
        }
        let session = self.pending_session.as_ref()?;
        let target = session.target.clone()?;
        let mut saved = Vec::new();
        let mut widths = Vec::new();
        if let Some(layout) = session.layout.clone() {
            Self::flatten_member(layout, &mut saved, &mut widths);
        }
        // Columns that come back empty are dropped on restore, so they are not
        // counted here either; nor is a legacy file-tree window, now docked.
        let counted: Vec<usize> = saved
            .iter()
            .map(|(items, _)| {
                items
                    .iter()
                    .filter(|item| !matches!(item, SavedItem::Files { .. }))
                    .count()
            })
            .collect();
        let columns = counted.iter().filter(|n| **n > 0).count().max(1);
        let shares = counted
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(ix, _)| (widths.get(ix).copied().unwrap_or(0.5), ix == 0))
            .collect();
        Some(Resume {
            target,
            columns,
            windows: counted.iter().sum::<usize>().max(1),
            shares,
            closed: session.saved_at,
            live: false,
        })
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionState::Connected(_))
    }

    /// The host last run ended on, for home's "continue" line. Read from the
    /// pending session rather than the live one: on the home screen there is
    /// usually no live one yet.
    pub fn saved_target(&self) -> Option<SavedTarget> {
        self.pending_session
            .as_ref()
            .and_then(|session| session.target.clone())
            .or_else(|| self.target.clone())
    }

    /// Works against this machine instead of a remote one.
    ///
    /// There is nothing to negotiate and nothing to authenticate, so this is
    /// the whole of it: build the host, publish it, restore or open a shell.
    /// It is also why local mode has no disconnect -- there is no session to
    /// hang up, and closing the tabs is the only teardown there is.
    pub fn connect_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_local(true, window, cx);
    }

    /// Picks up where the last run ended, as far as it can without asking.
    ///
    /// A remote session needs a password that is deliberately never stored, so
    /// the honest resume for one is the connect form already pointed at that
    /// host. Only a local session can truly come back by itself.
    fn resume_at_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.saved_target() {
            Some(SavedTarget::Local) => self.start_local(true, window, cx),
            Some(SavedTarget::Ssh { .. }) => {
                self.open_connections(window, cx);
                self.prefill_saved_target(window, cx);
            }
            None => self.show_home(window, cx),
        }
    }

    /// `restore` is false for "open a new local terminal": the layout is
    /// deliberately not rebuilt, just a shell.
    /// Starts a session on this machine if there is none -- nothing
    /// connected and nothing on its way -- and reports whether it did. Local
    /// is the default host: anything that needs one (a terminal, the file
    /// tree, Settings opened from home) gets it rather than doing nothing.
    fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(
            self.connection,
            ConnectionState::Disconnected | ConnectionState::Failed(_)
        ) {
            return false;
        }
        self.start_local(true, window, cx);
        true
    }

    fn start_local(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.connection, ConnectionState::Connected(_)) {
            return;
        }
        let host = Host::local();
        self.target = Some(SavedTarget::Local);
        // Same order as the remote path: the saved directory has to be queued
        // before the explorer is handed a host, or it leaves for home.
        let layout = if restore {
            self.take_matching_session(&SavedTarget::Local, cx)
        } else {
            // Nothing will be restored, so last run's file has served its
            // purpose. Dropping it here is also what lets this session start
            // saving at all -- `save_session` refuses to write over a session
            // that has not been used yet.
            self.pending_session = None;
            None
        };
        // Only when there is no layout coming: a restored one already says
        // whether the tree was open, and opening it on top would put a second
        // column of files beside the one being rebuilt.
        let open_explorer = layout.is_none()
            && cx.global::<crate::store::Settings>().explorer_on_connect;
        self.set_connection(ConnectionState::Connected(host.clone()), cx);
        self.close_connections(window, cx);
        // Home has served its purpose the moment there is something behind it.
        self.home_open = false;
        match layout {
            Some(layout) => self.restore_layout(layout, &host, window, cx),
            None => self.open_terminal(PtyProgram::LoginShell, window, cx),
        }
        if open_explorer {
            self.toggle_explorer(&ToggleExplorer, window, cx);
        }
        cx.notify();
    }

    /// Whether the host is this machine, which is what hides the disconnect
    /// affordances: there is nothing to disconnect from.
    pub fn is_local(&self) -> bool {
        matches!(&self.connection, ConnectionState::Connected(host) if host.is_local())
    }

    pub fn open_terminal(
        &mut self,
        program: PtyProgram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ConnectionState::Connected(connection) = &self.connection else {
            return;
        };
        let connection = connection.clone();
        let label: SharedString = match &program {
            PtyProgram::LoginShell => connection.label.clone().into(),
            PtyProgram::Command(cmd) => cmd.clone().into(),
            PtyProgram::ShellIn(_) => connection.label.clone().into(),
        };

        let terminal =
            cx.new(|cx| TerminalView::new(connection, label, program, window, cx));
        let item = PaneItem::Terminal(terminal);
        self.active_pane.update(cx, |pane, cx| {
            pane.add_item(item, true, window, cx);
        });
        cx.notify();
    }

    /// Opens a terminal in a new column, immediately right of the focus. This
    /// is the strip's whole promise: the columns already on screen keep the
    /// width they had, and the strip simply gets longer. It inherits the
    /// focused column's width, so repeating it gives an even row rather than
    /// a ladder of sizes.
    pub fn open_terminal_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_terminal_column_with(PtyProgram::LoginShell, window, cx);
    }

    /// A terminal in a new column, running `program` -- the login shell, or
    /// the shell started in a directory (the file tree's "open in terminal").
    pub fn open_terminal_column_with(
        &mut self,
        program: PtyProgram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No session: starting the local one already opens a shell (or
        // brings last run's local layout back), which is all a plain "new
        // terminal" asked for.
        if self.ensure_session(window, cx) && matches!(program, PtyProgram::LoginShell) {
            return;
        }
        if !self.is_connected() {
            return;
        }
        self.column_for_new_window(window, cx);
        self.renumber_columns(cx);
        self.open_terminal(program, window, cx);
        cx.notify();
    }

    pub fn connection_state(&self) -> &ConnectionState {
        &self.connection
    }

    /// The strip as the navigation bar's read-out draws it: one entry per
    /// column, its share of the viewport and whether it is the focus, on
    /// screen, or scrolled off. It is the only place the app says how long
    /// the strip is -- the old status line's counts are gone, because a row
    /// of segments says the same thing and says where you are in it too.
    pub fn strip_readout(&self) -> Vec<(f32, strip::Segment)> {
        self.strip.readout()
    }

    /// Focuses a column by position, which is what clicking a read-out
    /// segment means. Out of range is ignored rather than clamped: the
    /// read-out is drawn from the same list, so a miss means the strip
    /// changed under the click.
    pub fn focus_column_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.strip.at(ix) else {
            return;
        };
        self.focus_pane(pane);
        self.focus_active_item(window, cx);
        cx.notify();
    }

    /// The strip, for the overview to draw whole. Read-only: the overview
    /// looks at the strip and never rearranges it.
    pub fn strip(&self) -> &Strip {
        &self.strip
    }

    /// Shows the whole strip, or puts it away again.
    ///
    /// There is nothing to save and restore: the strip is not touched while
    /// the overview is up, so coming back is just drawing it again.
    pub fn toggle_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overview_open = !self.overview_open;
        if self.overview_open {
            self.overview_entrance.restart();
            // The overview needs the keyboard for its own left/right/escape,
            // so it takes the workspace's handle -- the one the `Workspace`
            // key context already hangs off. Giving it back on the way out is
            // what makes escape land you in the column you picked, typing.
            window.focus(&self.focus_handle);
        } else {
            self.focus_active_item(window, cx);
        }
        cx.notify();
    }

    /// Opens a path the way a double-click in the explorer does -- images to
    /// the viewer, everything else to the editor. One rule, in one place, so
    /// the command bar and the explorer cannot disagree about what a file is.
    pub fn open_file(&mut self, path: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.to_string();
        if crate::ui::preview::image_format(&path).is_some() {
            self.open_preview(path, window, cx);
        } else {
            self.open_editor(path, window, cx);
        }
    }

    /// Drops columns with no windows left in them. The last column always
    /// stays, even empty -- there has to be somewhere for the next window to
    /// land.
    fn collapse_empty_columns(&mut self, cx: &mut Context<Self>) {
        loop {
            let empty = self
                .strip
                .panes()
                .into_iter()
                .find(|pane| pane.read(cx).is_empty());
            let Some(pane) = empty else { break };
            let removed_id = pane.read(cx).id;
            // False means it was the last one; leaving it is the point, and
            // looping again would never end.
            if !self.strip.remove(&pane) {
                break;
            }
            self.pane_subscriptions.remove(&removed_id);
            if self.active_pane == pane {
                if let Some(next) = self.strip.focused() {
                    self.active_pane = next;
                }
            }
        }
        self.renumber_columns(cx);
    }
}

/// The directory a saved layout had the file tree open at, if it had one.
///
/// Read out of the layout rather than from a field beside it, because the
/// explorer is a window now and a window's state belongs to the window. It
/// has to be found before the layout is rebuilt: `set_host` leaves for the
/// home directory the moment a connection arrives.
fn explorer_path(member: &SavedMember) -> Option<String> {
    match member {
        SavedMember::Pane { items, .. } => items.iter().find_map(|item| match item {
            SavedItem::Files { path } => path.clone(),
            _ => None,
        }),
        SavedMember::Axis { members, .. } => members.iter().find_map(explorer_path),
    }
}

/// Up to three names, then a count: a confirmation dialog shouldn't scroll.
fn name_list(names: &[SharedString]) -> String {
    const SHOWN: usize = 3;
    let mut text = names
        .iter()
        .take(SHOWN)
        .map(|name| name.as_ref())
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    text
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything below renders a pane: this reads them, and reading a
        // leased entity aborts the process.
        self.track_layout_changes(cx);
        // Every app shortcut is bound in the `Workspace` context, so they all
        // die the moment keyboard focus is somewhere outside it -- on nothing,
        // or on a view that was closed and is no longer drawn. Whatever got it
        // there, the keyboard comes back to the active window. Home, the
        // command bar and the dialogs keep focus of their own and are left
        // alone; only a background window is not worth correcting.
        if !self.home_open
            && window.is_window_active()
            && !self.focus_handle.contains_focused(window, cx)
            && !self.focus_handle.is_focused(window)
        {
            self.focus_active_item(window, cx);
        }
        self.strip.max_share = cx.global::<crate::store::Settings>().max_column_width / 100.;
        self.sync_width_labels(cx);

        // Home takes the window rather than floating over it: no title bar, no
        // rail, no status line, no panes. Actions still need somewhere to land,
        // so the `Workspace` context stays wrapped around the `Home` one that
        // `home::render` declares inside it.
        if self.home_open {
            let home = div()
                .key_context("Workspace")
                .size_full()
                .on_action(cx.listener(|this, _: &ContinueSession, window, cx| {
                    this.continue_session(window, cx)
                }))
                .on_action(cx.listener(|this, _: &HideHome, window, cx| this.hide_home(window, cx)))
                .on_action(cx.listener(|this, _: &OpenConnections, window, cx| {
                    this.open_connections(window, cx)
                }))
                .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                    this.open_settings(window, cx)
                }))
                .on_action(cx.listener(|this, _: &IncreaseFontSize, _, cx| {
                    this.adjust_font_size(1., cx);
                }))
                .on_action(cx.listener(|this, _: &DecreaseFontSize, _, cx| {
                    this.adjust_font_size(-1., cx);
                }))
                .on_action(cx.listener(|this, _: &ResetFontSize, _, cx| {
                    this.reset_font_size(cx);
                }))
                // The design keeps the navigation bar on home (frame 06): it
                // is the window's caption and its buttons, and the state word
                // says IDLE. Home is the body beside it.
                .child({
                    let theme = cx.global::<Theme>();
                    let font = theme.font.clone();
                    let edge = nav_bar::Edge::from_settings(cx);
                    let bar = nav_bar::render(self, window, cx);
                    let body = div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .font_family(font)
                        .child(crate::ui::home::render(self, window, cx));
                    let frame = div()
                        .flex()
                        .size_full()
                        .when(edge.is_vertical(), |this| this.flex_row())
                        .when(!edge.is_vertical(), |this| this.flex_col());
                    match edge {
                        nav_bar::Edge::Left | nav_bar::Edge::Top => frame.child(bar).child(body),
                        nav_bar::Edge::Right | nav_bar::Edge::Bottom => frame.child(body).child(bar),
                    }
                })
                .into_any_element();
            return crate::ui::decorations::wrap(home, window, cx);
        }

        let theme = cx.global::<Theme>();
        let font = theme.font.clone();
        let font_size = theme.font_size;
        let font_weight = theme.font_weight;
        let line_height = gpui::relative(theme.line_height_ratio);

        // One bar, on whichever edge the setting names. There is deliberately
        // no second: see `ui::nav_bar`.
        let edge = nav_bar::Edge::from_settings(cx);
        let bar = nav_bar::render(self, window, cx);
        // The overview takes the body and leaves the bar: it is a way of
        // looking at the strip, not a way of leaving the window.
        // Sampled before the overview is drawn, because the workspace owns
        // the clock for it and `overview::render` only takes the reading.
        let motion = crate::ui::motion::Motion::from_settings(cx);
        let entering = self.overview_entrance.progress(&motion);
        if self.overview_entrance.running() && window.is_window_active() {
            window.request_animation_frame();
        }
        let center = if self.overview_open {
            // Its own key context, so plain left/right/escape work here and
            // nowhere else -- unqualified arrows belong to whatever has focus
            // the rest of the time, which is usually a shell.
            div()
                .key_context("Overview")
                .flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(crate::ui::overview::render(self, entering, window, cx))
                .into_any_element()
        } else {
            self.strip.render(window, cx)
        };

        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            // Positions the close confirmation, which covers the whole window
            // including the title bar -- the X up there is one way in.
            .relative()
            // No background: `Root` paints the window ground, once. Every layer
            // that repeats `theme.bg` over it multiplies a translucent window's
            // opacity back up towards solid.
            .font_family(font)
            .font_weight(font_weight)
            .text_size(font_size)
            // Relative, so it suits any text size below: the line-height setting
            // then reaches prose too (Settings descriptions, explorer rows), not
            // only the terminal grid.
            .line_height(line_height)
            // A new window goes in a new column; stacking into the one you
            // are in is the deliberate second gesture, not the default.
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.open_terminal_column(window, cx);
            }))
            .on_action(cx.listener(|this, _: &StackTerminal, window, cx| {
                this.open_terminal(PtyProgram::LoginShell, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusColumnLeft, window, cx| {
                this.focus_column(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusColumnRight, window, cx| {
                this.focus_column(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusWindowUp, window, cx| {
                this.focus_window(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusWindowDown, window, cx| {
                this.focus_window(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveColumnLeft, _, cx| {
                this.move_column(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveColumnRight, _, cx| {
                this.move_column(1, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveWindowUp, window, cx| {
                this.move_window(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveWindowDown, window, cx| {
                this.move_window(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExpelWindow, window, cx| {
                this.expel_window(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ConsumeWindow, window, cx| {
                this.consume_window(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleOverview, window, cx| {
                this.toggle_overview(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleColumnWidth, _, cx| {
                this.cycle_column_width(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenConnections, window, cx| {
                this.open_connections(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Disconnect, window, cx| {
                this.disconnect(window, cx);
            }))
            .on_action(cx.listener(|this, _: &IncreaseFontSize, _, cx| {
                this.adjust_font_size(1., cx);
            }))
            .on_action(cx.listener(|this, _: &DecreaseFontSize, _, cx| {
                this.adjust_font_size(-1., cx);
            }))
            .on_action(cx.listener(|this, _: &ResetFontSize, _, cx| {
                this.reset_font_size(cx);
            }))
            .on_action(cx.listener(Self::toggle_explorer))
            .child({
                // The docked file tree, then the strip and whatever floats
                // over it.
                let body = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(self.explorer_docked, |this| {
                        this.child(self.render_explorer_dock(cx))
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(center)
                            // Bare: the command bar paints its own backdrop,
                            // because the backdrop is where its click-to-dismiss
                            // and its mouse blocking live.
                            .children(self.connections.clone()),
                    );
                // The bar and the rest of the window, in whichever order and
                // along whichever axis the chosen edge implies. Four cases,
                // written out, because two of them also flip the bar's own
                // border to its inner side.
                let frame = div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(edge.is_vertical(), |this| this.flex_row())
                    .when(!edge.is_vertical(), |this| this.flex_col());
                match edge {
                    nav_bar::Edge::Left | nav_bar::Edge::Top => frame.child(bar).child(body),
                    nav_bar::Edge::Right | nav_bar::Edge::Bottom => frame.child(body).child(bar),
                }
            })
            .children(self.confirm_close.then(|| self.render_close_confirm(window, cx)))
            // Never both at once: they share a focus handle and a key context,
            // and the window's question is the one that outranks a tab's.
            .children(
                (!self.confirm_close && self.confirm_tab.is_some())
                    .then(|| self.render_tab_close_confirm(window, cx)),
            )
            // Boxed because the home screen above returns a different element
            // entirely, and the two arms have to agree on a type.
            .into_any_element();
        // Linux's client-side frame and resize margin; a pass-through elsewhere.
        crate::ui::decorations::wrap(root, window, cx)
    }
}
