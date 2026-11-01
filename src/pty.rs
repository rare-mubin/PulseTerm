//! The UI-side half of a pseudo-terminal, whichever side of the wire it runs on.
//!
//! An SSH channel (`ssh::pty`) and a local console (`local::open_pty`: ConPTY
//! on Windows, a Unix pty on Linux)
//! have nothing in common below this point and everything in common above it:
//! bytes in, bytes out, a size, and an end. A terminal view is handed one of
//! these and never learns which it got.

use tokio::sync::mpsc;

/// Messages pushed into the background task that owns the pty. That task is the
/// only thing that touches the channel or the console handles, so every write
/// from the UI goes through this queue.
pub(crate) enum PtyMsg {
    Data(Vec<u8>),
    Resize(u32, u32),
    Close,
}

/// Output from a pty, consumed by the terminal view that owns it.
#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Closed { error: Option<String> },
}

/// One open pty. Unlike the Tauri build -- which broadcast every byte to every
/// window and made the frontend filter by id -- each pty gets its own event
/// channel, so a terminal view only ever sees its own output.
pub struct Pty {
    pub(crate) tx: mpsc::UnboundedSender<PtyMsg>,
    pub events: flume::Receiver<PtyEvent>,
}

impl Pty {
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(PtyMsg::Data(bytes));
    }

    pub fn resize(&self, cols: u32, rows: u32) {
        let _ = self.tx.send(PtyMsg::Resize(cols, rows));
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.tx.send(PtyMsg::Close);
    }
}

/// What a new pty should run: the shell, or a specific command in place of it.
#[derive(Clone, Debug, Default)]
pub enum PtyProgram {
    #[default]
    LoginShell,
    /// Plumbed for the TTY1-4 `sudo conspy N` console shortcuts; only the UI to
    /// trigger them is still missing.
    #[allow(dead_code)]
    Command(String),
    /// The login shell, started in this directory -- the file tree's "open
    /// in terminal". A path on the host the terminal runs on.
    ShellIn(String),
}
