use gpui::{
    actions, div, prelude::*, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, SharedString, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState, TabSize};

use crate::host::Host;
use crate::store::Settings;
use crate::theme::Theme;
use crate::workspace::item::ItemId;

actions!(pulseterm, [SaveFile]);

/// What an editor asks the workspace for. Only the workspace can open tabs, so
/// the button emits and the workspace acts -- the same shape as the explorer's
/// events.
pub enum EditorEvent {
    Preview,
    /// The file reached the server. The workspace watches for this so "save and
    /// close" can close the tab once the write is actually done, rather than
    /// hoping and closing over an in-flight request.
    Saved,
    /// It didn't. The error is already on screen in this tab's own banner; the
    /// event exists so a dialog waiting on the save stops waiting.
    SaveFailed,
}

impl EventEmitter<EditorEvent> for EditorView {}

/// A remote file open for editing.
///
/// The editing itself -- rope, cursor, selection, undo, search, IME and syntax
/// highlighting -- is gpui-component's `InputState` in code-editor mode. This
/// view only owns the *file*: where it lives, what was last saved, and moving
/// the bytes over SFTP.
pub struct EditorView {
    pub item_id: ItemId,
    pub path: String,
    /// The editor's own focus handle, so focusing the tab puts the caret in the
    /// text rather than on a wrapper around it.
    pub focus_handle: FocusHandle,
    name: SharedString,
    host: Host,
    state: Entity<InputState>,
    /// The text as it last came from, or went to, the server. Dirtiness is a
    /// comparison against this rather than a flag set on every keystroke, so
    /// typing a change and then undoing it leaves the tab clean again.
    saved: SharedString,
    dirty: bool,
    loaded: bool,
    saving: bool,
    error: Option<SharedString>,
    /// (line numbers, soft wrap) as last pushed into the editor, so `render`
    /// only calls the setters when the Settings page actually changed one.
    applied: (bool, bool),
    _subscription: Subscription,
}

impl EditorView {
    pub fn new(
        host: Host,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Both separators: a remote path is POSIX, a local one on Windows is
        // not, and splitting on '/' alone named the window after the whole
        // `C:\...` path.
        let name: SharedString = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path.as_str())
            .to_string()
            .into();
        // Always a name that is actually registered: for anything else the
        // highlighter registry falls back to JSON. See `ui::languages`.
        let language = super::languages::for_file_name(&name);

        let (line_numbers, soft_wrap, tab_size) = {
            let settings = cx.global::<Settings>();
            (
                settings.editor_line_numbers,
                settings.editor_soft_wrap,
                settings.editor_tab_size,
            )
        };
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(line_numbers)
                .soft_wrap(soft_wrap)
                .tab_size(TabSize {
                    tab_size,
                    hard_tabs: false,
                })
        });
        let focus_handle = state.read(cx).focus_handle(cx);

        let subscription = cx.subscribe_in(
            &state,
            window,
            |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.loaded {
                    let dirty = state.read(cx).value() != this.saved;
                    if dirty != this.dirty {
                        this.dirty = dirty;
                        // The tab strip reads this view while rendering, which
                        // is what makes gpui repaint it on this notify -- no
                        // separate title event needed.
                        cx.notify();
                    }
                }
            },
        );

        let mut this = Self {
            item_id: ItemId::next(),
            path,
            focus_handle,
            name,
            host,
            state,
            saved: SharedString::default(),
            dirty: false,
            loaded: false,
            saving: false,
            error: None,
            applied: (line_numbers, soft_wrap),
            _subscription: subscription,
        };
        this.load(window, cx);
        this
    }

    pub fn title(&self) -> SharedString {
        if self.dirty {
            format!("{} ●", self.name).into()
        } else {
            self.name.clone()
        }
    }

    /// Whether the buffer differs from what was last saved. The workspace asks
    /// about these before the window closes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.host.files.clone();
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = files.read_text(path).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(text) => {
                        // Recorded before the text goes in, so the `Change` that
                        // `set_value` raises compares equal and the tab opens
                        // clean.
                        this.saved = text.clone().into();
                        this.loaded = true;
                        this.state
                            .update(cx, |state, cx| state.set_value(text, window, cx));
                        window.focus(&this.focus_handle);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        self.save_now(cx);
    }

    /// `ctrl-s` by another route: the close dialog's "save and close" needs the
    /// same write, and an action can only be dispatched at a focused element --
    /// the dialog holds focus at that moment, not the editor.
    pub fn save_now(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || self.saving {
            return;
        }
        let text = self.state.read(cx).value();
        self.saving = true;
        self.error = None;
        cx.notify();

        let files = self.host.files.clone();
        let path = self.path.clone();
        let content = text.to_string();
        cx.spawn(async move |this, cx| {
            let result = files.write_text(path, content).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.saved = text;
                        // Clean only if nothing was typed while the write was in
                        // flight; otherwise those keystrokes are still unsaved.
                        this.dirty = this.state.read(cx).value() != this.saved;
                        cx.emit(EditorEvent::Saved);
                    }
                    Err(error) => {
                        this.error = Some(format!("save failed: {error}").into());
                        cx.emit(EditorEvent::SaveFailed);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Reconciled here rather than pushed from the Settings page, which has
        // no idea which editors are open. Tab size has no setter in this
        // version of the editor, so that one only reaches new tabs.
        let wanted = {
            let settings = cx.global::<Settings>();
            (settings.editor_line_numbers, settings.editor_soft_wrap)
        };
        if wanted != self.applied {
            self.applied = wanted;
            self.state.update(cx, |state, cx| {
                state.set_line_number(wanted.0, window, cx);
                state.set_soft_wrap(wanted.1, window, cx);
            });
        }

        let (border, fg, fg_muted, fg_subtle, raised, danger, accent) = {
            let t = cx.global::<Theme>();
            (
                t.border,
                t.fg,
                t.fg_muted,
                t.fg_subtle,
                t.raised,
                t.danger,
                t.accent,
            )
        };
        // Only Markdown has a rendered form worth a button; the header still
        // carries the path for everything else.
        let previewable = crate::ui::preview::can_preview(&self.name);
        let path = self.path.clone();

        let banner = move |text: SharedString, color: gpui::Hsla| {
            div()
                .px_2()
                .py_0p5()
                .flex_none()
                .border_b_1()
                .border_color(border)
                .text_color(color)
                .child(text)
        };

        div()
            // `ctrl-s` is bound to `SaveFile` in this context. The editor's own
            // "Input" context sits beneath it on the dispatch path and doesn't
            // bind `ctrl-s`, so the action bubbles up to here.
            .key_context("Editor")
            .on_action(cx.listener(Self::save))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .flex_none()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(fg_muted)
                            .child(path),
                    )
                    .when(previewable, |this| {
                        this.child(
                            div()
                                .id("editor-preview")
                                .px_2()
                                .py_0p5()
                                .flex_none()
                                .border_1()
                                .border_color(border)
                                .text_color(fg_muted)
                                .hover(|s| s.bg(raised).text_color(fg))
                                .child("preview")
                                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                    cx.emit(EditorEvent::Preview);
                                })),
                        )
                    }),
            )
            .when_some(self.error.clone(), |this, error| this.child(banner(error, danger)))
            .when(self.saving, |this| this.child(banner("saving…".into(), accent)))
            .when(!self.loaded && self.error.is_none(), |this| {
                this.child(div().p_2().text_color(fg_subtle).child("loading…"))
            })
            .when(self.loaded, |this| {
                this.child(
                    div().flex_1().min_h_0().child(
                        Input::new(&self.state)
                            .h_full()
                            // Its own chrome off: the pane already supplies the
                            // frame, and a second border inside it would break
                            // the hairline TUI look.
                            .appearance(false)
                            .bordered(false),
                    ),
                )
            })
    }
}
