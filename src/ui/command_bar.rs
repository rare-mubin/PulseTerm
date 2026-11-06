//! The command bar: one overlay for connecting, opening and running.
//!
//! **One overlay, not three.** This replaces the Connections popup, and it is
//! also where an "open file" prompt and a command list would otherwise have
//! gone. They are the same bar reading the same query -- saved hosts, a host
//! typed out in full, a path, an action -- so there is one thing to summon,
//! one thing to dismiss, and one set of keys to learn. Nothing else floats.
//!
//! **A secret is asked for inline and never stored.** Choosing a host does not
//! connect: it turns the query line into the prompt for the one thing that is
//! missing, on the same line, masked. It is held in an `InputState` for as
//! long as the connect takes and goes no further -- not to `Profiles`, not to
//! `session.json`, not into scrollback. That is the same rule `store` keeps,
//! enforced here by having nowhere to put one.
//!
//! The view stays a leaf. It **mirrors** connection state rather than reading
//! it back out of the workspace, and emits intent upward as `CommandEvent`;
//! the workspace is the only thing that knows how to act on any of it.

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, SharedString, Subscription, Window,
};

use gpui_component::input::{InputEvent, InputState};

use super::kit;
use super::motion::{Entrance, Motion};
use super::text_field::text_input;
use crate::ssh::{AuthMethod, ConnectParams};
use crate::store::{Profiles, SavedAuth};
use crate::theme::Theme;

actions!(
    pulseterm,
    [DismissCommandBar, CommandUp, CommandDown, CommandForget]
);

pub enum CommandEvent {
    Connect(ConnectParams),
    /// Work on this machine instead. No host, no credentials, nothing to get
    /// wrong -- which is why it is one row rather than a mode of the others.
    UseLocal,
    Disconnect,
    OpenSettings,
    OpenOverview,
    NewTerminal,
    OpenPath(SharedString),
    Dismiss,
}

impl EventEmitter<CommandEvent> for CommandBar {}

/// What the workspace has told us about the live connection. Mirrored rather
/// than read back out of the workspace so this view stays a plain leaf: it
/// emits intent upward and renders whatever state it was last handed.
#[derive(Clone)]
pub enum ConnectionStatus {
    /// Working on this machine. A separate state from `Connected` only so the
    /// bar can leave the disconnect row out -- there is nothing to hang up,
    /// and the workspace is still "connected" everywhere else. It carries no
    /// name because "this computer" is the name.
    Local,
    Disconnected,
    Connecting(SharedString),
    Connected(SharedString),
    Failed(SharedString),
}

/// Which action a row stands for. Carried on the row rather than matched by
/// label, so renaming one cannot quietly rewire it.
#[derive(Clone, PartialEq)]
enum Act {
    Profile(u64),
    /// A host typed out in the query that is not a saved profile yet.
    NewHost { host: String, port: u16, username: String },
    Open(SharedString),
    UseLocal,
    Disconnect,
    Settings,
    Overview,
    NewTerminal,
}

/// One line of the result list. `note` is the small right-hand word -- what
/// kind of thing this is, or the keys that reach it without the bar.
struct Row {
    act: Act,
    label: SharedString,
    note: SharedString,
}

/// Which secret is being asked for. A key needs two values, so it asks twice;
/// everything else asks once.
#[derive(Clone, Copy, PartialEq)]
enum Ask {
    Password,
    KeyPath,
    Passphrase,
}

impl Ask {
    fn label(self) -> &'static str {
        match self {
            Ask::Password => "password",
            Ask::KeyPath => "private key",
            Ask::Passphrase => "passphrase",
        }
    }

    /// A passphrase is optional -- plenty of keys have none -- and the hint
    /// has to say so, or an empty Enter looks like it was ignored.
    fn hint(self) -> &'static str {
        match self {
            Ask::Password => "enter to connect",
            Ask::KeyPath => "tab for password",
            Ask::Passphrase => "enter to skip",
        }
    }

    fn masked(self) -> bool {
        self != Ask::KeyPath
    }

    /// The field's own hint. Empty for a secret -- the label beside the field
    /// already says what goes in it -- where it used to keep saying "type to
    /// filter" while it asked for a password.
    fn placeholder(self) -> &'static str {
        match self {
            Ask::KeyPath => "path to the key file",
            Ask::Password | Ask::Passphrase => "",
        }
    }
}

/// Switches the one field between the query (`None`) and a prompt: masking
/// and hint together, so the two cannot disagree. The value is cleared too --
/// whatever was typed belonged to the other mode.
fn set_mode(state: &mut InputState, ask: Option<Ask>, window: &mut Window, cx: &mut Context<InputState>) {
    state.set_masked(ask.is_some_and(Ask::masked), window, cx);
    state.set_placeholder(ask.map_or(QUERY_HINT, Ask::placeholder), window, cx);
    state.set_value("", window, cx);
}

const QUERY_HINT: &str = "type to filter";

/// The host being connected to while the bar collects what it needs. Dropped
/// the moment the connect is emitted, which is as long as any of it lives.
struct Pending {
    host: String,
    port: u16,
    username: String,
    key_path: String,
    ask: Ask,
}

pub struct CommandBar {
    pub focus_handle: FocusHandle,
    /// The one field. It is the query while browsing and the secret prompt
    /// while connecting -- the same line, which is the whole point.
    query: Entity<InputState>,
    pending: Option<Pending>,
    selected: usize,
    error: Option<SharedString>,
    status: ConnectionStatus,
    focused_once: bool,
    /// The fade-up, restarted on the bar's first paint.
    entrance: Entrance,
    _subscriptions: Vec<Subscription>,
}

impl CommandBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(QUERY_HINT));

        let subscriptions = vec![cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.activate(window, cx),
                InputEvent::Change => {
                    // Any edit invalidates both the complaint and the
                    // selection: the row that was highlighted may not be in
                    // the list any more, and index 0 is always in it.
                    this.error = None;
                    this.selected = 0;
                    cx.notify();
                }
                _ => {}
            },
        )];

        Self {
            focus_handle: cx.focus_handle(),
            query,
            pending: None,
            selected: 0,
            error: None,
            status: ConnectionStatus::Disconnected,
            focused_once: false,
            entrance: Entrance::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_status(&mut self, status: ConnectionStatus, cx: &mut Context<Self>) {
        // A failure ends the connect attempt, so the bar goes back to being a
        // list: leaving the secret prompt up would invite retyping a password
        // into a field nothing is listening to any more.
        if let ConnectionStatus::Failed(_) = status {
            self.pending = None;
        }
        self.status = status;
        cx.notify();
    }

    /// Puts a saved profile into the query, the way clicking one used to fill
    /// the form in. It still stops short of connecting -- a password profile
    /// has no password to find, because no secret is ever written to disk.
    pub fn load_profile(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = cx.global::<Profiles>().items.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.pending = None;
        self.selected = 0;
        self.query
            .update(cx, |state, cx| state.set_value(profile.label(), window, cx));
        cx.notify();
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_string()
    }

    /// Everything the query matches, in the order it is drawn. Sections are
    /// implied by the order rather than stored: the list is short enough that
    /// grouping it twice would be the more complicated of the two.
    fn rows(&self, cx: &App) -> Vec<(Option<&'static str>, Vec<Row>)> {
        let query = self.query_text(cx).to_lowercase();
        let matches = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);

        let mut hosts: Vec<Row> = cx.global::<Profiles>()
            .items
            .iter()
            .filter(|profile| matches(&profile.label()))
            .map(|profile| Row {
                act: Act::Profile(profile.id),
                label: profile.label().into(),
                note: match profile.auth {
                    SavedAuth::Password => "password".into(),
                    SavedAuth::PrivateKey { .. } => "key".into(),
                },
            })
            .collect();

        // A host typed out in full is offered as itself, so the bar is also
        // the connect form it replaced. Only when it is not already saved --
        // otherwise the same host would be in the list twice.
        if let Some((username, host, port)) = parse_target(&self.query_text(cx)) {
            let known = cx.global::<Profiles>().items.iter().any(|profile| {
                profile.host == host && profile.port == port && profile.username == username
            });
            if !known {
                let label = if port == 22 {
                    format!("{username}@{host}")
                } else {
                    format!("{username}@{host}:{port}")
                };
                hosts.push(Row {
                    act: Act::NewHost { host, port, username },
                    label: label.into(),
                    note: "new".into(),
                });
            }
        }

        // A path is offered only while there is a host to read it from, and
        // only as an exact path: searching a remote tree for a name would
        // mean walking it, which is the one thing the budget forbids.
        let mut open = Vec::new();
        let connected = matches!(
            self.status,
            ConnectionStatus::Connected(_) | ConnectionStatus::Local
        );
        let typed = self.query_text(cx);
        if connected && looks_like_path(&typed) {
            open.push(Row {
                act: Act::Open(typed.clone().into()),
                label: typed.into(),
                note: "open".into(),
            });
        }

        let mut actions = Vec::new();
        let mut action = |act: Act, label: &'static str, note: &'static str| {
            if matches(label) {
                actions.push(Row { act, label: label.into(), note: note.into() });
            }
        };
        if connected {
            action(Act::NewTerminal, "New terminal", "ctrl+shift+t");
            action(Act::Overview, "Overview", "ctrl+shift+space");
        }
        if !matches!(self.status, ConnectionStatus::Local) {
            action(Act::UseLocal, "Use this computer", "local");
        }
        action(Act::Settings, "Open settings", "ctrl+shift+,");
        // The one row whose note is not a shortcut: hanging up is the one
        // action here you cannot undo by running it again, so it says which
        // machine it would hang up on.
        if let ConnectionStatus::Connected(host) = &self.status {
            if matches("Disconnect") {
                actions.push(Row {
                    act: Act::Disconnect,
                    label: "Disconnect".into(),
                    note: host.clone(),
                });
            }
        }

        [("hosts", hosts), ("open", open), ("actions", actions)]
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(name, rows)| (Some(name), rows))
            .collect()
    }

    fn flat(&self, cx: &App) -> Vec<Act> {
        self.rows(cx)
            .into_iter()
            .flat_map(|(_, rows)| rows.into_iter().map(|row| row.act))
            .collect()
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.flat(cx).len();
        if count == 0 {
            return;
        }
        // Wraps, unlike the strip: a list has ends, and running off one of
        // them to reach the other is what every result list does.
        let next = (self.selected as isize + delta).rem_euclid(count as isize);
        self.selected = next as usize;
        cx.notify();
    }

    /// Enter: take the selected row, or answer the prompt that is up.
    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            self.answer(window, cx);
            return;
        }
        let acts = self.flat(cx);
        let Some(act) = acts.get(self.selected).cloned() else {
            return;
        };
        self.run(act, window, cx);
    }

    fn run(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Profile(id) => {
                let Some(profile) = cx.global::<Profiles>().items.iter().find(|p| p.id == id).cloned() else {
                    return;
                };
                let (key_path, ask) = match &profile.auth {
                    SavedAuth::Password => (String::new(), Ask::Password),
                    // The path is remembered, so a saved key host asks only
                    // for what cannot be: the passphrase, which plenty of
                    // keys do not have.
                    SavedAuth::PrivateKey { path } => (path.clone(), Ask::Passphrase),
                };
                self.ask(
                    Pending {
                        host: profile.host,
                        port: profile.port,
                        username: profile.username,
                        key_path,
                        ask,
                    },
                    window,
                    cx,
                );
            }
            Act::NewHost { host, port, username } => self.ask(
                Pending { host, port, username, key_path: String::new(), ask: Ask::Password },
                window,
                cx,
            ),
            Act::Open(path) => cx.emit(CommandEvent::OpenPath(path)),
            Act::UseLocal => cx.emit(CommandEvent::UseLocal),
            Act::Disconnect => cx.emit(CommandEvent::Disconnect),
            Act::Settings => cx.emit(CommandEvent::OpenSettings),
            Act::Overview => cx.emit(CommandEvent::OpenOverview),
            Act::NewTerminal => cx.emit(CommandEvent::NewTerminal),
        }
    }

    /// Turns the query line into a prompt for one value.
    fn ask(&mut self, pending: Pending, window: &mut Window, cx: &mut Context<Self>) {
        let ask = pending.ask;
        self.pending = Some(pending);
        self.error = None;
        self.query.update(cx, |state, cx| set_mode(state, Some(ask), window, cx));
        cx.notify();
    }

    /// Enter on a prompt: either the last value needed, or the next question.
    fn answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.query.read(cx).value().to_string();
        let Some(pending) = &mut self.pending else {
            return;
        };
        match pending.ask {
            Ask::Password => {
                let host = pending.host.clone();
                let port = pending.port;
                let username = pending.username.clone();
                self.finish(host, port, username, AuthMethod::Password(value), window, cx);
            }
            Ask::KeyPath => {
                let path = value.trim().to_string();
                if path.is_empty() {
                    self.error = Some("A private key path is required.".into());
                    cx.notify();
                    return;
                }
                pending.key_path = path;
                pending.ask = Ask::Passphrase;
                self.query.update(cx, |state, cx| set_mode(state, Some(Ask::Passphrase), window, cx));
                cx.notify();
            }
            Ask::Passphrase => {
                let host = pending.host.clone();
                let port = pending.port;
                let username = pending.username.clone();
                let path = pending.key_path.clone();
                let auth = AuthMethod::PrivateKey {
                    path,
                    passphrase: (!value.is_empty()).then_some(value),
                };
                self.finish(host, port, username, auth, window, cx);
            }
        }
    }

    fn finish(
        &mut self,
        host: String,
        port: u16,
        username: String,
        auth: AuthMethod,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Cleared before the event goes out, not after: `auth` already owns
        // the secret, and the field it came from should not still be holding
        // a copy while the connect runs.
        self.query.update(cx, |state, cx| set_mode(state, None, window, cx));
        self.pending = None;
        self.error = None;
        cx.emit(CommandEvent::Connect(ConnectParams { host, port, username, auth }));
    }

    /// Delete on a saved host forgets it. The old popup had a button for
    /// this and a bar has no room for one, so it is a key -- and only ever on
    /// a `Profile` row, because nothing else here is a thing you own. The
    /// selection is left where it is: forgetting the third of five should
    /// leave you looking at the list, not at the top of it.
    fn forget(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        let Some(Act::Profile(id)) = self.flat(cx).get(self.selected).cloned() else {
            return;
        };
        cx.global_mut::<Profiles>().remove(id);
        let count = self.flat(cx).len();
        self.selected = self.selected.min(count.saturating_sub(1));
        cx.notify();
    }

    /// Escape: back out of a prompt first, and only then dismiss the bar. A
    /// single Escape that threw away both would make a mistyped password cost
    /// the whole overlay.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.take().is_some() {
            self.query.update(cx, |state, cx| set_mode(state, None, window, cx));
            cx.notify();
            return;
        }
        cx.emit(CommandEvent::Dismiss);
    }
}

impl CommandBar {
    /// Puts the caret in the query line. The bar's own focus handle is not
    /// enough: typing and Enter are the input's, so a bar focused on its
    /// container looks open and ignores every key.
    pub fn focus_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| state.focus(window, cx));
    }
}

impl Focusable for CommandBar {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus on the first *render*, never in the constructor: the bar is
        // built from `Workspace::new`, before the window has a live focus
        // tree, and a request made then is silently dropped -- which is what
        // once made the overlay impossible to dismiss.
        //
        // The entrance starts here for the same reason: the clock on a fade
        // should start when the thing is first painted, not when it was
        // built.
        if !self.focused_once {
            self.focused_once = true;
            self.entrance.restart();
            self.query.update(cx, |state, cx| state.focus(window, cx));
        }
        let motion = Motion::from_settings(cx);
        let enter = self.entrance.progress(&motion);
        if self.entrance.running() && window.is_window_active() {
            window.request_animation_frame();
        }

        let theme = cx.global::<Theme>();
        let (bg, surface, raised) = (theme.bg, theme.surface, theme.raised);
        let (fg, fg_muted, fg_subtle) = (theme.fg, theme.fg_muted, theme.fg_subtle);
        let (accent, border, danger) = (theme.accent, theme.border, theme.danger);
        let font_size = theme.font_size;
        let micro = kit::micro(theme);
        let row_height = kit::row(theme);

        let sections = self.rows(cx);
        let total: usize = sections.iter().map(|(_, rows)| rows.len()).sum();
        let prompt = self.pending.as_ref().map(|pending| pending.ask);
        // While a prompt is up the footer names who it is for -- the prompt
        // itself says only what it wants -- instead of "type to filter",
        // which there is nothing to filter at that point.
        let footer = match &self.pending {
            Some(pending) if pending.port == 22 => format!("{}@{}", pending.username, pending.host),
            Some(pending) => format!("{}@{}:{}", pending.username, pending.host, pending.port),
            None => QUERY_HINT.to_string(),
        };
        let forgettable = prompt.is_none()
            && matches!(self.flat(cx).get(self.selected), Some(Act::Profile(_)));

        let mut list = div().flex().flex_col();
        let mut index = 0usize;
        for (name, rows) in sections {
            if let Some(name) = name {
                list = list.child(
                    div()
                        .w_full()
                        .px_2()
                        .py_1()
                        .bg(surface)
                        .text_size(micro)
                        .text_color(fg_subtle)
                        .child(kit::label(name)),
                );
            }
            for row in rows {
                let ix = index;
                index += 1;
                let chosen = ix == self.selected;
                let act = row.act.clone();
                list = list.child(
                    div()
                        .id(("result", ix))
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(row_height)
                        .w_full()
                        .pr_2()
                        .when(chosen, |this| this.bg(raised))
                        // The mark is the row's left edge, 2px of accent --
                        // the same mark the focused column wears, because it
                        // means the same thing in both places.
                        .child(
                            div()
                                .w(px(2.))
                                .h_full()
                                .flex_none()
                                .bg(if chosen { accent } else { gpui::transparent_black() }),
                        )
                        .child(
                            div()
                                .pl_2()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(if chosen { fg } else { fg_muted })
                                .child(row.label),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(micro)
                                .text_color(fg_subtle)
                                .child(kit::label(&row.note)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = ix;
                            this.run(act.clone(), window, cx);
                        })),
                );
            }
        }

        // The backdrop: dismisses on a click and blocks the rest, so nothing
        // falls through to a terminal that would read it as a mouse report.
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            // Light: enough to say the window behind is not the thing to
            // type into, not so much that it stops being readable. A heavier
            // veil turns a light theme grey, and the bar has a border and a
            // ground of its own to separate it anyway.
            .bg(gpui::black().opacity(0.2))
            .block_mouse_except_scroll()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
            )
            .child(
                div()
                    .id("command-bar")
                    .track_focus(&self.focus_handle)
                    .key_context("CommandBar")
                    .on_action(cx.listener(|this, _: &DismissCommandBar, window, cx| {
                        this.dismiss(window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &CommandUp, _, cx| this.step(-1, cx)))
                    .on_action(cx.listener(|this, _: &CommandDown, _, cx| this.step(1, cx)))
                    .on_action(cx.listener(|this, _: &CommandForget, _, cx| this.forget(cx)))
                    // Tab offers the other way in to a host that is not saved.
                    // A key handler, not a binding -- and it only ever sees
                    // the key because `main.rs` cancels `Root`'s Tab binding
                    // in the `CommandBar` context; a binding is dispatched
                    // before any key handler.
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key != "tab" {
                            return;
                        }
                        let Some(pending) = &mut this.pending else {
                            return;
                        };
                        pending.ask = match pending.ask {
                            Ask::Password => Ask::KeyPath,
                            _ => Ask::Password,
                        };
                        let ask = pending.ask;
                        this.query.update(cx, |state, cx| set_mode(state, Some(ask), window, cx));
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    // A click inside is not a click on the backdrop.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // Down a fifth of the window, and the last few pixels of
                    // that are the entrance: it settles into place rather
                    // than replacing the window between two frames. The
                    // backdrop does not fade with it -- a veil that arrives
                    // gradually reads as a delay, and its job is to say "not
                    // here" immediately.
                    .mt(gpui::relative(0.18))
                    .when(enter < 1., |this| {
                        this.opacity(enter).top(px(10. * (1. - enter)))
                    })
                    .w(px(560.))
                    .max_w(gpui::relative(0.9))
                    .flex()
                    .flex_col()
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .text_size(font_size)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(border)
                            // The prompt glyph says which question is being
                            // asked. `>` is the list; a word is a secret.
                            .child(match prompt {
                                Some(ask) => div()
                                    .flex_none()
                                    .text_size(micro)
                                    .text_color(accent)
                                    .child(kit::label(ask.label())),
                                None => div().flex_none().text_color(accent).child("\u{203a}"),
                            })
                            .child(div().flex_1().min_w_0().child(text_input(&self.query, font_size)))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(micro)
                                    .text_color(fg_subtle)
                                    .child(match prompt {
                                        Some(ask) => kit::label(ask.hint()),
                                        None => kit::label(&format!(
                                            "{total} result{}",
                                            if total == 1 { "" } else { "s" }
                                        )),
                                    }),
                            ),
                    )
                    // While a secret is being asked for there is nothing to
                    // pick, so the list is not drawn: one question, one line.
                    .when(prompt.is_none(), |this| this.child(list))
                    .children(self.error.clone().map(|error| {
                        div().px_2().py_1().text_color(danger).child(error)
                    }))
                    .children(status_line(&self.status, fg_subtle, danger, accent, micro))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .py_1()
                            .border_t_1()
                            .border_color(border)
                            .text_size(micro)
                            .text_color(fg_subtle)
                            .gap_2()
                            .child(kit::label(&footer))
                            // `del` is only mentioned while it would do
                            // something. Tracked-out micro type is twice the
                            // characters it looks like, and three hints
                            // across 560px meet in the middle -- so the one
                            // that is contextual is shown contextually.
                            .child(kit::label(if forgettable {
                                "del forgets \u{b7} esc"
                            } else {
                                "esc"
                            })),
                    ),
            )
    }
}

/// The connection's own line, only when it has something to say. "Connected"
/// is left out: the navigation bar already shows it, and repeating it here
/// would be the second place to look that this overlay exists to remove.
fn status_line(
    status: &ConnectionStatus,
    fg_subtle: gpui::Hsla,
    danger: gpui::Hsla,
    accent: gpui::Hsla,
    micro: gpui::Pixels,
) -> Option<impl IntoElement> {
    let (text, colour) = match status {
        ConnectionStatus::Connecting(host) => (format!("connecting to {host}"), accent),
        ConnectionStatus::Failed(error) => (error.to_string(), danger),
        _ => return None,
    };
    Some(
        div()
            .px_2()
            .py_1()
            .text_size(micro)
            .text_color(colour)
            .child(kit::label(&text))
            .text_color(fg_subtle),
    )
}

/// `user@host`, `user@host:port`. Returns nothing for anything else, which is
/// what keeps an ordinary filter word from being offered as a machine.
pub(crate) fn parse_target(text: &str) -> Option<(String, String, u16)> {
    let (username, rest) = text.split_once('@')?;
    let username = username.trim();
    let rest = rest.trim();
    if username.is_empty() || rest.is_empty() {
        return None;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (rest, 22),
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((username.to_string(), host.to_string(), port))
}

/// Whether the query is a path rather than a word to filter by. Absolute only:
/// a bare name is almost always a filter, and offering to open it would put a
/// row that cannot work at the top of every search.
fn looks_like_path(text: &str) -> bool {
    text.starts_with('/')
        || text.starts_with('~')
        || (text.len() > 2 && text.as_bytes()[1] == b':' && text.as_bytes()[0].is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_host_spec_and_nothing_else() {
        assert_eq!(
            parse_target("rare@192.168.68.106"),
            Some(("rare".into(), "192.168.68.106".into(), 22))
        );
        assert_eq!(
            parse_target("rare@example.com:2222"),
            Some(("rare".into(), "example.com".into(), 2222))
        );
        // A filter word, a half-typed one, and a port that is not a number
        // must all stay out of the hosts section.
        assert_eq!(parse_target("settings"), None);
        assert_eq!(parse_target("rare@"), None);
        assert_eq!(parse_target("@host"), None);
        assert_eq!(parse_target("rare@host:nope"), None);
        assert_eq!(parse_target("rare@host:0"), None);
    }

    #[test]
    fn only_absolute_paths_are_offered_to_open() {
        assert!(looks_like_path("/etc/hosts"));
        assert!(looks_like_path("~/.bashrc"));
        assert!(looks_like_path("C:\\Users\\rare"));
        assert!(!looks_like_path("hosts"));
        assert!(!looks_like_path("settings"));
    }
}
