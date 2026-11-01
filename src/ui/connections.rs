use gpui::{
    actions, div, prelude::*, px, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, SharedString, Subscription, Window,
};

use gpui_component::input::{InputEvent, InputState};

use super::text_field::text_input;
use crate::ssh::{AuthMethod, ConnectParams};
use crate::store::{Profiles, SavedAuth};
use super::icon::{icon, small, Icon};
use crate::theme::Theme;

actions!(pulseterm, [DismissConnections]);

pub enum ConnectionsEvent {
    Connect(ConnectParams),
    /// Work on this machine instead. No form, no credentials, nothing to get
    /// wrong -- which is why it is one button rather than a mode of the one
    /// below it.
    UseLocal,
    Disconnect,
    Dismiss,
}

impl EventEmitter<ConnectionsEvent> for ConnectionsView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    Password,
    Key,
}

/// What the workspace has told us about the live connection. Mirrored rather
/// than read back out of the workspace so this view stays a plain leaf: it emits
/// intent upward and renders whatever state it was last handed.
#[derive(Clone)]
pub enum ConnectionStatus {
    /// Working on this machine. A separate state from `Connected` only so the
    /// popup can leave the disconnect button out -- there is nothing to hang
    /// up, and the workspace is still "connected" everywhere else.
    Local(SharedString),
    Disconnected,
    Connecting(SharedString),
    Connected(SharedString),
    Failed(SharedString),
}

/// The Connections popup: saved profiles on top, the connect form below.
///
/// It is a modal overlay rather than a tab. It was briefly a tab, which avoided
/// the two things that made the *original* modal unusable — it never took focus,
/// so Escape went nowhere, and clicks fell through to the terminal behind it.
/// Both are fixed here (focus on first render, `block_mouse_except_scroll` on the
/// backdrop), so the popup keeps its advantage: connecting is a brief, modal act,
/// and it shouldn't cost a permanent tab.
pub struct ConnectionsView {
    pub focus_handle: FocusHandle,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    key_path: Entity<InputState>,
    passphrase: Entity<InputState>,
    auth: AuthKind,
    error: Option<SharedString>,
    status: ConnectionStatus,
    focused_once: bool,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut input = |placeholder: &'static str, masked: bool| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(masked)
            })
        };
        let host = input("hostname or IP", false);
        let port = input("22", false);
        let username = input("username", false);
        let password = input("password", true);
        let key_path = input("path to private key", false);
        let passphrase = input("key passphrase (optional)", true);

        // Enter submits from whichever field has focus, which is how every login
        // form is expected to behave. Escape needs no handling here: the input
        // lets it propagate, so the `Connections` binding still dismisses.
        let subscriptions = [&host, &port, &username, &password, &key_path, &passphrase]
            .into_iter()
            .map(|field| {
                cx.subscribe_in(field, window, |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    InputEvent::Change => {
                        if this.error.take().is_some() {
                            cx.notify();
                        }
                    }
                    _ => {}
                })
            })
            .collect();

        Self {
            focus_handle: cx.focus_handle(),
            host,
            port,
            username,
            password,
            key_path,
            passphrase,
            auth: AuthKind::Password,
            error: None,
            status: ConnectionStatus::Disconnected,
            focused_once: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_status(&mut self, status: ConnectionStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    /// Fills the form in from a saved profile. Stops short of connecting: a
    /// password profile still needs its password, because no secret is ever
    /// written to disk.
    pub fn load_profile(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = cx
            .global::<Profiles>()
            .items
            .iter()
            .find(|p| p.id == id)
            .cloned()
        else {
            return;
        };

        self.host
            .update(cx, |f, cx| f.set_value(profile.host.clone(), window, cx));
        self.port
            .update(cx, |f, cx| f.set_value(profile.port.to_string(), window, cx));
        self.username
            .update(cx, |f, cx| f.set_value(profile.username.clone(), window, cx));

        let focus = match &profile.auth {
            SavedAuth::Password => {
                self.auth = AuthKind::Password;
                self.password.clone()
            }
            SavedAuth::PrivateKey { path } => {
                self.auth = AuthKind::Key;
                self.key_path
                    .update(cx, |f, cx| f.set_value(path.clone(), window, cx));
                self.passphrase.clone()
            }
        };
        self.error = None;
        focus.update(cx, |f, cx| f.focus(window, cx));
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let host = self.host.read(cx).value().trim().to_string();
        if host.is_empty() {
            self.error = Some("A host is required.".into());
            cx.notify();
            return;
        }

        let port_text = self.port.read(cx).value().trim().to_string();
        let port = if port_text.is_empty() {
            22
        } else {
            match port_text.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.error = Some("Port must be a number between 1 and 65535.".into());
                    cx.notify();
                    return;
                }
            }
        };

        let username = self.username.read(cx).value().trim().to_string();
        if username.is_empty() {
            self.error = Some("A username is required.".into());
            cx.notify();
            return;
        }

        let auth = match self.auth {
            AuthKind::Password => AuthMethod::Password(self.password.read(cx).value().to_string()),
            AuthKind::Key => {
                let path = self.key_path.read(cx).value().trim().to_string();
                if path.is_empty() {
                    self.error = Some("A private key path is required.".into());
                    cx.notify();
                    return;
                }
                let passphrase = self.passphrase.read(cx).value().to_string();
                AuthMethod::PrivateKey {
                    path,
                    passphrase: (!passphrase.is_empty()).then_some(passphrase),
                }
            }
        };

        self.error = None;
        cx.emit(ConnectionsEvent::Connect(ConnectParams {
            host,
            port,
            username,
            auth,
        }));
    }
}

impl Focusable for ConnectionsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_once {
            self.focused_once = true;
            self.host.update(cx, |f, cx| f.focus(window, cx));
        }

        // Snapshot the palette: the borrow comes from `cx`, and every
        // `cx.listener` below needs `cx` mutably.
        let (bg, surface, raised, border, accent, fg, fg_muted, fg_subtle, danger, success, warning, font_size) = {
            let t = cx.global::<Theme>();
            (
                t.bg, t.surface, t.raised, t.border, t.accent, t.fg, t.fg_muted, t.fg_subtle,
                t.danger, t.success, t.warning, t.font_size,
            )
        };

        let auth = self.auth;
        // `connected` drives the disconnect button, `busy` everything that
        // offers a *new* host: local counts as busy without being something to
        // disconnect from, which is the whole difference between the two.
        let (status_text, status_color, connected, busy) = match &self.status {
            ConnectionStatus::Disconnected => ("not connected".to_string(), fg_subtle, false, false),
            ConnectionStatus::Connecting(label) => {
                (format!("connecting to {label}…"), warning, false, true)
            }
            ConnectionStatus::Connected(label) => {
                (format!("connected — {label}"), success, true, true)
            }
            ConnectionStatus::Local(label) => (format!("local — {label}"), accent, false, true),
            ConnectionStatus::Failed(error) => (error.to_string(), danger, false, false),
        };

        let profiles: Vec<_> = cx
            .global::<Profiles>()
            .items
            .iter()
            .map(|p| (p.id, p.label(), matches!(p.auth, SavedAuth::Password)))
            .collect();

        let heading = move |text: &'static str| {
            div()
                .pb_1()
                .text_color(fg_subtle)
                .child(format!("── {text} ──────────────────────────"))
        };

        let field = move |label: &'static str, input: Entity<InputState>| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .w_full()
                .child(div().w(px(104.)).flex_none().text_color(fg_muted).child(label))
                .child(div().flex_1().min_w_0().child(text_input(&input, font_size)))
        };

        let profile_rows: Vec<_> = profiles
            .into_iter()
            .enumerate()
            .map(|(ix, (id, label, is_password))| {
                div()
                    .id(("profile", ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .text_color(fg_muted)
                    .hover(|s| s.bg(raised).text_color(fg))
                    .child(icon(Icon::Server, small(), fg_subtle))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .text_color(fg_subtle)
                            .child(if is_password { "password" } else { "key" }),
                    )
                    .child(
                        div()
                            .id(("forget", ix))
                            .px_1()
                            .text_color(fg_subtle)
                            .hover(|s| s.text_color(danger))
                            .child(icon(Icon::Trash2, small(), fg_subtle))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.update_global::<Profiles, _>(|p, _| p.remove(id));
                                cx.notify();
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.load_profile(id, window, cx);
                    }))
            })
            .collect();
        let has_profiles = !profile_rows.is_empty();

        let auth_tab = move |label: &'static str, kind: AuthKind, cx: &mut Context<Self>| {
            let selected = auth == kind;
            div()
                .id(label)
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(if selected { accent } else { border })
                .bg(if selected { raised } else { surface })
                .text_color(if selected { fg } else { fg_muted })
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.auth = kind;
                    this.error = None;
                    cx.notify();
                }))
        };

        div()
            .id("connections")
            .track_focus(&self.focus_handle)
            // Scoped so Escape closes the popup without being taken from the
            // terminal, where it is a key vim and every TUI needs.
            .key_context("Connections")
            .on_action(cx.listener(|_, _: &DismissConnections, _, cx| {
                cx.emit(ConnectionsEvent::Dismiss);
            }))
            // The popup floats over the terminal; without this, clicks that miss
            // a field fall through and focus whatever is behind it.
            .block_mouse_except_scroll()
            .w(px(560.))
            .max_h(px(620.))
            .overflow_y_scroll()
            .bg(bg)
            .border_1()
            .border_color(border)
            .text_size(font_size)
            .text_color(fg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w_full()
                    .p_4()
                    .bg(surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pb_1()
                            .border_b_1()
                            .border_color(border)
                            .child(icon(Icon::Plug, small(), accent))
                            .child(div().flex_1().text_color(fg).child("connections"))
                            .child(
                                div()
                                    .id("dismiss")
                                    .p_1()
                                    .text_color(fg_muted)
                                    .hover(|s| s.text_color(fg))
                                    .child(icon(Icon::X, small(), fg_muted))
                                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                        cx.emit(ConnectionsEvent::Dismiss);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_color(status_color).child("●"))
                            .child(div().flex_1().child(status_text))
                            .when(connected, |this| {
                                this.child(
                                    div()
                                        .id("disconnect")
                                        .px_2()
                                        .py_0p5()
                                        .border_1()
                                        .border_color(danger)
                                        .text_color(danger)
                                        .hover(|s| s.bg(danger.opacity(0.2)))
                                        .child("disconnect")
                                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                            cx.emit(ConnectionsEvent::Disconnect);
                                        })),
                                )
                            }),
                    )
                    .when(!busy, |this| {
                        this.child(
                            div()
                                .id("use-local")
                                .mt_1()
                                .px_2()
                                .py_0p5()
                                .flex()
                                .gap_2()
                                .items_center()
                                .border_1()
                                .border_color(border)
                                .text_color(fg)
                                .hover(|s| s.bg(raised))
                                .child(icon(Icon::Monitor, small(), fg_muted))
                                .child("use this computer")
                                .child(
                                    div()
                                        .flex_1()
                                        .text_right()
                                        .text_color(fg_subtle)
                                        .child("local"),
                                )
                                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                    cx.emit(ConnectionsEvent::UseLocal);
                                })),
                        )
                    })
                    .child(heading("saved"))
                    .when(!has_profiles, |this| {
                        this.child(
                            div()
                                .px_2()
                                .text_color(fg_subtle)
                                .child("nothing saved yet — a connection is remembered once it succeeds"),
                        )
                    })
                    .children(profile_rows)
                    .child(heading("connect to"))
                    .child(field("host", self.host.clone()))
                    .child(field("port", self.port.clone()))
                    .child(field("user", self.username.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(104.)).flex_none().text_color(fg_muted).child("auth"))
                            .child(auth_tab("password", AuthKind::Password, cx))
                            .child(auth_tab("public key", AuthKind::Key, cx)),
                    )
                    .when(auth == AuthKind::Password, |this| {
                        this.child(field("password", self.password.clone()))
                    })
                    .when(auth == AuthKind::Key, |this| {
                        this.child(field("key file", self.key_path.clone()))
                            .child(field("passphrase", self.passphrase.clone()))
                    })
                    .when_some(self.error.clone(), |this, error| {
                        this.child(div().text_color(danger).child(error))
                    })
                    .child(
                        div().flex().justify_end().pt_1().child(
                            div()
                                .id("connect")
                                .px_3()
                                .py_0p5()
                                .bg(raised)
                                .border_1()
                                .border_color(accent)
                                .text_color(fg)
                                .hover(|s| s.bg(accent.opacity(0.25)))
                                .child("connect [⏎]")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.submit(cx)
                                })),
                        ),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_color(fg_subtle)
                            .child("passwords and passphrases are never saved to disk"),
                    ),
            )
    }
}
