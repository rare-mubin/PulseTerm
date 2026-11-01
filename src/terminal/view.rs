use alacritty_terminal::index::{Column, Point as AlacPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{viewport_to_point, TermMode};
use gpui::{
    div, prelude::*, App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, SharedString, Task, Window,
};

use super::element::TerminalElement;
use super::emulator::{Emulator, TermAction, TermSize};
use super::keymap;
use super::mouse::{self, MouseEventKind, MouseIntent};
use crate::host::Host;
use crate::pty::{Pty, PtyEvent, PtyProgram};
use crate::store::Settings;
use crate::theme::Theme;
use crate::workspace::item::ItemId;

pub enum TerminalEvent {
    TitleChanged,
    Closed,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

pub struct TerminalView {
    pub item_id: ItemId,
    pub focus_handle: FocusHandle,
    emulator: Emulator,
    pty: Option<Pty>,
    /// Title set by the remote program via OSC 0/2, if it set one.
    remote_title: Option<SharedString>,
    /// Fallback label, e.g. `me@host:22` or `tty1`.
    label: SharedString,
    exit_message: Option<SharedString>,
    focused: bool,
    /// Where the grid was last painted, captured during the element's prepaint.
    /// Mouse positions arrive in window coordinates, and this is the only thing
    /// that can turn them back into cells.
    layout: Option<GridLayout>,
    /// True between mouse-down and mouse-up while dragging out a selection.
    selecting: bool,
    /// The button whose press we forwarded to the remote program, if any. Drag
    /// and release are gated on this rather than on hover: once a program has
    /// been told about a press it must get the matching release, even if the
    /// cursor has since left the pane.
    reported_button: Option<MouseButton>,
    _pump: Option<Task<()>>,
}

#[derive(Clone, Copy)]
struct GridLayout {
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    cell_height: Pixels,
}

impl TerminalView {
    /// Opens a pty on `host` and wires it to a fresh emulator.
    ///
    /// The initial 80x24 is a placeholder: the real geometry is not known until
    /// the element gets its bounds, at which point `set_grid_size` resizes both
    /// the emulator and the remote pty. Starting from a sane default rather
    /// than 0x0 means the shell's first prompt is never written into a
    /// degenerate grid.
    pub fn new(
        host: Host,
        label: SharedString,
        program: PtyProgram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = TermSize::new(80, 24);
        // Which shell a *local* terminal runs. Read here rather than inside the
        // task, so changing the setting reaches the next tab opened and leaves
        // running ones alone -- the same rule as the scrollback below. A remote
        // host ignores it and starts the account's own login shell.
        let shell = crate::local::resolve_shell(&cx.global::<Settings>().local_shell);
        let mut this = Self {
            item_id: ItemId::next(),
            focus_handle: cx.focus_handle(),
            // Read once, at creation: changing the setting later applies to
            // terminals opened from then on, not to running ones.
            emulator: Emulator::new(size, cx.global::<Settings>().scrollback),
            pty: None,
            remote_title: None,
            label,
            exit_message: None,
            focused: false,
            layout: None,
            selecting: false,
            reported_button: None,
            _pump: None,
        };

        this._pump = Some(cx.spawn(async move |view, cx| {
            let pty = match host
                .open_pty(size.cols as u32, size.rows as u32, program, shell)
                .await
            {
                Ok(pty) => pty,
                Err(error) => {
                    let _ = view.update(cx, |this, cx| {
                        this.exit_message = Some(error.into());
                        cx.notify();
                    });
                    return;
                }
            };

            let events = pty.events.clone();
            if view
                .update(cx, |this, cx| {
                    // Push the geometry the element already settled on; between
                    // spawning this task and the channel opening, the pane has
                    // usually been laid out at least once.
                    let size = this.emulator.size();
                    pty.resize(size.cols as u32, size.rows as u32);
                    this.pty = Some(pty);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            while let Ok(event) = events.recv_async().await {
                let keep_going = view.update(cx, |this, cx| this.handle_pty_event(event, cx));
                match keep_going {
                    Ok(true) => {}
                    _ => break,
                }
            }
        }));

        let _ = window;
        this
    }

    fn handle_pty_event(&mut self, event: PtyEvent, cx: &mut Context<Self>) -> bool {
        match event {
            PtyEvent::Output(bytes) => {
                let actions = self.emulator.feed(&bytes);
                self.apply(actions, cx);
                cx.notify();
                true
            }
            PtyEvent::Closed { error } => {
                // The reason alone, in as few words as a header can carry. It
                // used to be a sentence pinned across the bottom of the pane
                // ending "press any key to close" -- which was never true:
                // `on_key` returns the moment `pty` is `None`, so every key
                // was swallowed by the promise. The column header says it
                // instead, beside the title it belongs to.
                self.exit_message = Some(match error {
                    Some(error) => error.into(),
                    None => "exited".into(),
                });
                self.pty = None;
                cx.emit(TerminalEvent::Closed);
                cx.notify();
                false
            }
        }
    }

    fn apply(&mut self, actions: Vec<TermAction>, cx: &mut Context<Self>) {
        for action in actions {
            match action {
                TermAction::Write(bytes) => {
                    if let Some(pty) = &self.pty {
                        pty.write(bytes);
                    }
                }
                TermAction::TitleChanged(title) => {
                    self.remote_title = (!title.is_empty()).then(|| title.into());
                    cx.emit(TerminalEvent::TitleChanged);
                }
                // A visible bell would mean flashing the pane; until there is a
                // setting for it, silently ignoring is the polite default.
                TermAction::Bell => {}
                TermAction::Exited => {
                    self.pty = None;
                    cx.emit(TerminalEvent::Closed);
                }
            }
        }
    }

    pub fn emulator(&self) -> &Emulator {
        &self.emulator
    }

    /// Why the shell behind this window ended, if it has: `exited` for one
    /// that was asked to go, the transport's own words for one that was not.
    /// The window stays -- its scrollback is still worth reading -- and its
    /// header says so.
    ///
    /// No exit code: `PtyEvent::Closed` carries an optional error string and
    /// nothing else, so there is none to show without a change to the
    /// transport on both sides.
    pub fn exit_reason(&self) -> Option<SharedString> {
        self.exit_message.clone()
    }

    pub fn is_focused_for_cursor(&self) -> bool {
        self.focused
    }

    pub fn title(&self) -> SharedString {
        match &self.remote_title {
            Some(title) => shorten_title(title),
            None => self.label.clone(),
        }
    }

    /// Called from the element's prepaint once the real pixel bounds are known.
    /// Resizing the emulator and the remote pty in the same pass keeps the two
    /// grids from disagreeing for a frame.
    /// Passes the terminal settings through to the emulator; see
    /// `Emulator::configure`.
    pub fn configure(
        &mut self,
        scrollback: usize,
        cursor: alacritty_terminal::vte::ansi::CursorShape,
    ) {
        self.emulator.configure(scrollback, cursor);
    }

    pub fn set_layout(
        &mut self,
        bounds: Bounds<Pixels>,
        size: TermSize,
        cell_width: Pixels,
        cell_height: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.layout = Some(GridLayout {
            bounds,
            cell_width,
            cell_height,
        });
        self.emulator
            .set_cell_size(f32::from(cell_width) as u16, f32::from(cell_height) as u16);
        if size == self.emulator.size() {
            return;
        }
        self.emulator.resize(size);
        if let Some(pty) = &self.pty {
            pty.resize(size.cols as u32, size.rows as u32);
        }
        cx.notify();
    }

    /// Turns a window-space position into a buffer point plus which half of the
    /// cell it landed in -- the side is what makes a selection feel right when
    /// you drag left-to-right versus right-to-left.
    fn grid_point(&self, position: Point<Pixels>) -> Option<(AlacPoint, Side)> {
        let layout = self.layout?;
        let size = self.emulator.size();

        let x = f32::from(position.x - layout.bounds.origin.x) / f32::from(layout.cell_width);
        let y = f32::from(position.y - layout.bounds.origin.y) / f32::from(layout.cell_height);

        // Clamped, not rejected: dragging past the edge of the pane should
        // extend the selection to that edge rather than stop updating.
        let col = (x.floor().max(0.) as usize).min(size.cols.saturating_sub(1));
        let row = (y.floor().max(0.) as usize).min(size.rows.saturating_sub(1));
        let side = if x - x.floor() > 0.5 { Side::Right } else { Side::Left };

        // Selections live in buffer coordinates so they stay put when the
        // viewport scrolls under them.
        let point = viewport_to_point(
            self.emulator.display_offset(),
            AlacPoint::new(row, Column(col)),
        );
        Some((point, side))
    }

    /// The same hit test, but left in viewport coordinates -- which is what the
    /// mouse-reporting protocols want, and the opposite of what selection wants.
    fn viewport_cell(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let layout = self.layout?;
        let size = self.emulator.size();
        let x = f32::from(position.x - layout.bounds.origin.x) / f32::from(layout.cell_width);
        let y = f32::from(position.y - layout.bounds.origin.y) / f32::from(layout.cell_height);
        let col = (x.floor().max(0.) as usize).min(size.cols.saturating_sub(1));
        let row = (y.floor().max(0.) as usize).min(size.rows.saturating_sub(1));
        Some((row, col))
    }

    fn report_mouse(
        &mut self,
        kind: MouseEventKind,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: &gpui::Modifiers,
    ) -> bool {
        let Some(pty) = &self.pty else { return false };
        let Some((row, col)) = self.viewport_cell(position) else {
            return false;
        };
        let mode = *self.emulator.term().mode();
        match mouse::encode(kind, button, mouse::viewport_point(row, col), modifiers, mode) {
            Some(bytes) => {
                pty.write(bytes);
                true
            }
            // The mode declined this particular event (motion without drag
            // tracking, say). It is still the program's mouse, so don't fall
            // back to starting a selection underneath it.
            None => true,
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);

        // A program that turned mouse tracking on owns the mouse; shift takes it
        // back so text is still selectable out of htop or tmux.
        let mode = *self.emulator.term().mode();
        if mouse::intent(mode, &event.modifiers) == MouseIntent::Report {
            self.reported_button = Some(event.button);
            self.report_mouse(
                MouseEventKind::Press,
                event.button,
                event.position,
                &event.modifiers,
            );
            // Clear any leftover local selection so it can't sit on top of a
            // program that is now drawing its own highlight.
            self.emulator.set_selection(None);
            cx.notify();
            return;
        }

        if event.button != MouseButton::Left {
            return;
        }

        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        // Click count picks the granularity, the way every terminal does it:
        // drag by character, double-click a word, triple-click a line.
        let kind = match event.click_count {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.selecting = true;
        self.emulator.set_selection(Some(Selection::new(kind, point, side)));
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mode = *self.emulator.term().mode();
        if mouse::intent(mode, &event.modifiers) == MouseIntent::Report {
            if let Some(button) = self.reported_button {
                self.report_mouse(
                    MouseEventKind::Drag,
                    button,
                    event.position,
                    &event.modifiers,
                );
            }
            return;
        }

        // `on_mouse_move` is dispatched window-wide, so this has to gate on our
        // own drag state rather than assume the cursor is over us.
        if !self.selecting || !event.dragging() {
            return;
        }
        if let Some((point, side)) = self.grid_point(event.position) {
            self.emulator.update_selection(point, side);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.reported_button.take() {
            self.report_mouse(
                MouseEventKind::Release,
                button,
                event.position,
                &event.modifiers,
            );
            return;
        }
        if self.selecting {
            self.selecting = false;
            if cx.global::<Settings>().copy_on_select {
                self.copy(cx);
            }
            cx.notify();
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.emulator.selected_text() {
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }

        // A remote program in bracketed-paste mode wants the payload fenced so
        // it can tell pasted text from typing -- that is what stops an editor
        // auto-indenting every line of a pasted block.
        let mode = *self.emulator.term().mode();
        if mode.contains(TermMode::BRACKETED_PASTE) {
            pty.write(b"\x1b[200~".to_vec());
            pty.write(text.replace('\r', "").into_bytes());
            pty.write(b"\x1b[201~".to_vec());
        } else {
            // Bare CR, not CRLF: a terminal sends what the Return key sends, and
            // a pasted LF would show up as an extra blank line in the shell.
            pty.write(
                text.replace("\r\n", "\r")
                    .replace('\n', "\r")
                    .into_bytes(),
            );
        }
        self.emulator.scroll_to_bottom();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;

        // Clipboard lives on ctrl+shift, the same region app shortcuts use and
        // the one a terminal has no encoding for. `keymap::encode` returns None
        // for it, so nothing reaches the shell either way.
        if mods.control && mods.shift {
            match keystroke.key.as_str() {
                "c" => {
                    self.copy(cx);
                    return;
                }
                "v" => {
                    self.paste(cx);
                    return;
                }
                _ => {}
            }
        }

        let Some(pty) = &self.pty else {
            return;
        };
        let mode = *self.emulator.term().mode();
        let Some(bytes) = keymap::encode(&event.keystroke, mode) else {
            return;
        };
        // Typing replaces the selection the way it does everywhere else.
        self.emulator.set_selection(None);
        // Typing always snaps back to the live view, the way every terminal
        // behaves when you're scrolled up in history and start typing.
        self.emulator.scroll_to_bottom();
        pty.write(bytes);
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let line_height = cx.global::<Theme>().line_height;
        let delta = event.delta.pixel_delta(line_height).y;
        let lines = (f32::from(delta) / f32::from(line_height)).round() as i32;
        if lines == 0 {
            return;
        }

        let mode = *self.emulator.term().mode();
        if mouse::intent(mode, &event.modifiers) == MouseIntent::Report {
            let kind = if lines > 0 {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            };
            // One report per line, the way a wheel actually behaves -- a program
            // counting ticks should see the same number we did.
            for _ in 0..lines.unsigned_abs() {
                self.report_mouse(kind, MouseButton::Left, event.position, &event.modifiers);
            }
            return;
        }

        // On the alternate screen there is no scrollback to move, so a wheel
        // tick becomes arrow keys instead -- that is what makes the wheel work
        // in `less` and `man`.
        if let Some(bytes) = mouse::alternate_scroll(mode, lines) {
            if let Some(pty) = &self.pty {
                pty.write(bytes);
            }
            return;
        }

        self.emulator.scroll(lines);
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Recorded rather than queried at paint time: the element needs to know
        // focus state while laying the cursor out, and it only has `&App`.
        self.focused = self.focus_handle.is_focused(window);

        let theme = cx.global::<Theme>();
        let font = theme.font.clone();
        let font_size = theme.font_size;

        div()
            .id("terminal")
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .size_full()
            // No background: `Root` paints the window ground once, and a second
            // layer here would double up the opacity of a translucent window.
            .p_1()
            .font_family(font)
            .text_size(font_size)
            // Plain arrow, always. An I-beam is the convention for a text field
            // you click into; a terminal is a grid you type at, and half the
            // time a program has the mouse anyway, so the pointer stays the
            // ordinary one rather than flickering between shapes.
            .cursor(gpui::CursorStyle::Arrow)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            // Middle-click pastes, unless a program is listening for it.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let mode = *this.emulator.term().mode();
                    if mouse::intent(mode, &event.modifiers) == MouseIntent::Report {
                        this.on_mouse_down(event, window, cx);
                    } else {
                        this.paste(cx);
                    }
                }),
            )
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(TerminalElement::new(cx.entity()))
    }
}

/// Titles the tab with something that fits.
///
/// A Windows shell sets its console title to the executable's full path --
/// `C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe` -- which as a tab
/// label is 55 characters of directory and one word of information. Anything
/// that is a path to a program is shown as the program; everything else, and
/// every title a remote program sets deliberately, is left exactly as sent.
fn shorten_title(title: &SharedString) -> SharedString {
    let looks_like_a_program = title.ends_with(".exe") && title.contains('\\');
    if !looks_like_a_program {
        return title.clone();
    }
    title
        .rsplit('\\')
        .next()
        .map(|name| SharedString::from(name.trim_end_matches(".exe").to_string()))
        .unwrap_or_else(|| title.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_path_becomes_the_program() {
        let long =
            SharedString::from("C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        assert_eq!(shorten_title(&long), SharedString::from("powershell"));
    }

    #[test]
    fn anything_a_program_chose_is_left_alone() {
        for title in ["vim README.md", "rare@server: ~/src", "htop"] {
            let title = SharedString::from(title);
            assert_eq!(shorten_title(&title), title);
        }
    }
}
