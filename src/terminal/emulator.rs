use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor};

/// Grid geometry. alacritty models scrollback as lines above line 0, so
/// `total_lines` is the viewport plus history -- but `Term` only reads this
/// trait to size itself, and it derives history from `Config`, so reporting
/// the viewport height for both is what alacritty's own `TermSize` does too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl TermSize {
    pub fn new(cols: usize, rows: usize) -> Self {
        // A zero-width or zero-height grid panics inside alacritty's indexing,
        // and both are reachable transiently while a pane is being laid out or
        // dragged, so clamp rather than trusting the caller.
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// alacritty pushes side effects out through an `EventListener` rather than
/// returning them. We buffer them and let the caller drain after each feed,
/// which keeps everything on one thread and avoids a channel per terminal.
#[derive(Clone, Default)]
pub struct EventCollector(Rc<RefCell<Vec<TermEvent>>>);

impl EventListener for EventCollector {
    fn send_event(&self, event: TermEvent) {
        self.0.borrow_mut().push(event);
    }
}

/// What the view has to act on after feeding bytes in.
pub enum TermAction {
    /// Bytes the emulator wants written back up the pty -- cursor position
    /// reports, device attributes, bracketed-paste acknowledgements. Dropping
    /// these is what makes some TUI programs hang waiting for a reply.
    Write(Vec<u8>),
    TitleChanged(String),
    Bell,
    Exited,
}

pub struct Emulator {
    term: Term<EventCollector>,
    parser: Processor,
    listener: EventCollector,
    size: TermSize,
    cell_size: (u16, u16),
    /// The settings last handed to `configure`, so it can no-op when nothing
    /// changed -- it is called every frame.
    configured: Option<(usize, CursorShape)>,
}

impl Emulator {
    pub fn new(size: TermSize, scrollback: usize) -> Self {
        let listener = EventCollector::default();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &size, listener.clone());
        Self {
            term,
            parser: Processor::new(),
            listener,
            size,
            cell_size: (8, 16),
            configured: None,
        }
    }

    /// Applies the user's terminal settings to a live terminal.
    ///
    /// `Term::set_options` resizes the history in place and swaps the default
    /// cursor style; a shape the remote program asked for itself (DECSCUSR, as
    /// vim does in insert mode) still wins over this default.
    pub fn configure(&mut self, scrollback: usize, cursor: CursorShape) {
        if self.configured == Some((scrollback, cursor)) {
            return;
        }
        self.configured = Some((scrollback, cursor));
        self.term.set_options(Config {
            scrolling_history: scrollback,
            default_cursor_style: CursorStyle {
                shape: cursor,
                blinking: false,
            },
            ..Config::default()
        });
    }

    pub fn size(&self) -> TermSize {
        self.size
    }

    pub fn term(&self) -> &Term<EventCollector> {
        &self.term
    }

    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_size = (width.max(1), height.max(1));
    }

    pub fn resize(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.resize(size);
    }

    /// Feeds raw bytes from the SSH channel through the VT parser and returns
    /// whatever the emulator wants the outside world to do about it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TermAction> {
        self.parser.advance(&mut self.term, bytes);
        self.drain_events()
    }

    fn drain_events(&mut self) -> Vec<TermAction> {
        let events: Vec<_> = self.listener.0.borrow_mut().drain(..).collect();
        let mut actions = Vec::new();
        for event in events {
            match event {
                TermEvent::PtyWrite(text) => actions.push(TermAction::Write(text.into_bytes())),
                TermEvent::Title(title) => actions.push(TermAction::TitleChanged(title)),
                TermEvent::ResetTitle => actions.push(TermAction::TitleChanged(String::new())),
                TermEvent::Bell => actions.push(TermAction::Bell),
                TermEvent::Exit | TermEvent::ChildExit(_) => actions.push(TermAction::Exited),
                TermEvent::TextAreaSizeRequest(format) => {
                    let reply = format(WindowSize {
                        num_lines: self.size.rows as u16,
                        num_cols: self.size.cols as u16,
                        cell_width: self.cell_size.0,
                        cell_height: self.cell_size.1,
                    });
                    actions.push(TermAction::Write(reply.into_bytes()));
                }
                TermEvent::ColorRequest(index, format) => {
                    // Report whatever the emulator currently has pinned for
                    // that slot; an unset slot means "still the default", and
                    // answering with black is better than not answering at all.
                    let rgb = self.term.colors()[index].unwrap_or(alacritty_terminal::vte::ansi::Rgb {
                        r: 0,
                        g: 0,
                        b: 0,
                    });
                    actions.push(TermAction::Write(format(rgb).into_bytes()));
                }
                // Clipboard round-trips (OSC 52) and the cursor-blink /
                // mouse-shape hints aren't wired up yet; ignoring them is
                // inert, not lossy.
                _ => {}
            }
        }
        actions
    }

    pub fn set_selection(&mut self, selection: Option<alacritty_terminal::selection::Selection>) {
        self.term.selection = selection;
    }

    pub fn update_selection(
        &mut self,
        point: alacritty_terminal::index::Point,
        side: alacritty_terminal::index::Side,
    ) {
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    pub fn scroll(&mut self, lines: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        use alacritty_terminal::grid::Scroll;
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// The last few non-blank lines of the screen, as plain text, for the
    /// overview's thumbnails.
    ///
    /// The *last* few rather than the first: a screen is written downward, so
    /// what was just printed is at the bottom, and a thumbnail showing the top
    /// of an 80x24 grid would show the shell banner for the rest of the
    /// session. Blank rows are dropped rather than kept, because a program
    /// that clears the screen and draws near the bottom would otherwise
    /// thumbnail as nothing at all.
    ///
    /// Colour and attributes are thrown away here on purpose. A thumbnail is
    /// read as a shape at a glance; carrying the cell attributes through would
    /// mean a second, smaller copy of `element.rs` to paint them.
    pub fn preview_lines(&self, count: usize) -> Vec<String> {
        let grid = self.term.grid();
        let mut lines: Vec<String> = Vec::new();
        for row in (0..grid.screen_lines() as i32).rev() {
            if lines.len() == count {
                break;
            }
            let line = alacritty_terminal::index::Line(row);
            let text: String = (0..grid.columns())
                .map(|column| grid[line][alacritty_terminal::index::Column(column)].c)
                .collect();
            let text = text.trim_end().to_string();
            if !text.is_empty() {
                lines.push(text);
            }
        }
        lines.reverse();
        lines
    }
}
