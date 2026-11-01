//! What the workspace is working against: a remote machine over SSH, or this one.
//!
//! Everything above the transport -- terminals, explorer, editor, preview --
//! talks to a `Host` and never learns which it got. That is the whole point:
//! local mode is not a second UI, it is the same one pointed somewhere else.
//!
//! The vocabulary the two backends share lives here too (`FileEntry`,
//! `Properties`, `Progress`), rather than under `ssh`, because `local::fs` is
//! just as entitled to it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::local::fs::LocalFs;
use crate::pty::{Pty, PtyProgram};
use crate::ssh::{self, Connection};

/// How far a transfer has got, shared with whoever started it.
///
/// Counters rather than a channel or a callback: the copy runs on the transport
/// runtime and the panel renders on gpui's, so anything pushed would need a
/// receiver drained on the other side. The panel reads these while it paints
/// instead, and a transfer nobody is watching costs two atomic stores per chunk.
#[derive(Default)]
pub struct Progress {
    transferred: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    pub fn transferred(&self) -> u64 {
        self.transferred.load(Ordering::Relaxed)
    }

    /// Zero when the size isn't known -- the bar then stays empty rather than
    /// inventing a position.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub(crate) fn set_total(&self, bytes: u64) {
        self.total.store(bytes, Ordering::Relaxed);
    }

    pub(crate) fn advance(&self, bytes: u64) {
        self.transferred.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Copy buffer. Big enough that the per-chunk round trip isn't the bottleneck,
/// small enough that the progress line moves smoothly on a slow link.
pub(crate) const TRANSFER_CHUNK: usize = 64 * 1024;

/// A file opened as text is loaded whole, and a binary opened by mistake is
/// destroyed by the first save, so both backends refuse above this.
pub(crate) const MAX_EDITABLE_BYTES: u64 = 5 * 1024 * 1024;

/// One entry in a directory listing.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// The tree has no size column; Properties reads size from `properties()`
    /// instead, which is a fresh stat rather than a cached listing.
    #[allow(dead_code)]
    pub size: u64,
}

/// Everything the Properties panel shows about one path. Shaped for SFTP, where
/// several of these are the only form the server offers; the local backend
/// leaves the Unix-only ones `None` rather than inventing them.
#[derive(Clone, Debug, Default)]
pub struct Properties {
    pub size: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Unix mode bits, when the server reports them.
    pub permissions: Option<u32>,
    pub modified: Option<u32>,
    pub accessed: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// The file half of a host. Every method mirrors one of `Sftp`'s, so the
/// explorer, the editor and the preview are written once.
#[derive(Clone)]
pub enum Files {
    Remote(ssh::sftp::Sftp),
    Local(LocalFs),
}

impl Files {
    pub fn is_local(&self) -> bool {
        matches!(self, Files::Local(_))
    }

    /// Path arithmetic, and the one place the two really differ in shape.
    /// Remote paths are POSIX whatever this client runs on; local ones are this
    /// platform's, so `std::path` is right for one and wrong for the other.
    pub fn join(&self, parent: &str, name: &str) -> String {
        match self {
            Files::Remote(_) => ssh::sftp::join(parent, name),
            Files::Local(_) => crate::local::fs::join(parent, name),
        }
    }

    pub fn parent<'a>(&self, path: &'a str) -> Option<&'a str> {
        match self {
            Files::Remote(_) => ssh::sftp::parent(path),
            Files::Local(_) => crate::local::fs::parent(path),
        }
    }

    pub async fn read_dir(&self, path: String) -> Result<Vec<FileEntry>, String> {
        match self {
            Files::Remote(fs) => fs.read_dir(path).await,
            Files::Local(fs) => fs.read_dir(path).await,
        }
    }

    pub async fn home(&self) -> Result<String, String> {
        match self {
            Files::Remote(fs) => fs.home().await,
            Files::Local(fs) => fs.home().await,
        }
    }

    pub async fn create_dir(&self, path: String) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.create_dir(path).await,
            Files::Local(fs) => fs.create_dir(path).await,
        }
    }

    pub async fn remove(&self, path: String, is_dir: bool) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.remove(path, is_dir).await,
            Files::Local(fs) => fs.remove(path, is_dir).await,
        }
    }

    pub async fn create_file(&self, path: String) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.create_file(path).await,
            Files::Local(fs) => fs.create_file(path).await,
        }
    }

    pub async fn properties(&self, path: String) -> Result<Properties, String> {
        match self {
            Files::Remote(fs) => fs.properties(path).await,
            Files::Local(fs) => fs.properties(path).await,
        }
    }

    pub async fn upload(
        &self,
        local: PathBuf,
        remote: String,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.upload(local, remote, progress).await,
            Files::Local(fs) => fs.upload(local, remote, progress).await,
        }
    }

    pub async fn download(
        &self,
        remote: String,
        local: PathBuf,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.download(remote, local, progress).await,
            Files::Local(fs) => fs.download(remote, local, progress).await,
        }
    }

    pub async fn read_text(&self, path: String) -> Result<String, String> {
        match self {
            Files::Remote(fs) => fs.read_text(path).await,
            Files::Local(fs) => fs.read_text(path).await,
        }
    }

    pub async fn read_bytes(&self, path: String, max_bytes: u64) -> Result<Vec<u8>, String> {
        match self {
            Files::Remote(fs) => fs.read_bytes(path, max_bytes).await,
            Files::Local(fs) => fs.read_bytes(path, max_bytes).await,
        }
    }

    pub async fn write_text(&self, path: String, content: String) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.write_text(path, content).await,
            Files::Local(fs) => fs.write_text(path, content).await,
        }
    }

    pub async fn exists(&self, path: String) -> bool {
        match self {
            Files::Remote(fs) => fs.exists(path).await,
            Files::Local(fs) => fs.exists(path).await,
        }
    }

    pub async fn copy(&self, from: String, to: String, is_dir: bool) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.copy(from, to, is_dir).await,
            Files::Local(fs) => fs.copy(from, to, is_dir).await,
        }
    }

    pub async fn rename(&self, from: String, to: String) -> Result<(), String> {
        match self {
            Files::Remote(fs) => fs.rename(from, to).await,
            Files::Local(fs) => fs.rename(from, to).await,
        }
    }
}

/// A machine to work on. Cheaply cloneable, and every clone of a remote host
/// shares one SSH session -- which is what makes every tab a multiplexed
/// channel rather than a new TCP connection.
#[derive(Clone)]
pub struct Host {
    /// `user@host` either way, so a tab title reads the same in both modes.
    pub label: String,
    pub files: Files,
    kind: Kind,
}

#[derive(Clone)]
enum Kind {
    Remote(Connection),
    Local,
}

impl Host {
    pub fn remote(connection: Connection) -> Self {
        Self {
            label: connection.label.clone(),
            files: Files::Remote(connection.sftp.clone()),
            kind: Kind::Remote(connection),
        }
    }

    pub fn local() -> Self {
        Self {
            label: local_label(),
            files: Files::Local(LocalFs),
            kind: Kind::Local,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.kind, Kind::Local)
    }

    /// Opens a terminal on this host. `shell` is the command a *local* terminal
    /// runs, read from Settings by the caller so changing it reaches the next
    /// tab opened; a remote host ignores it and starts the account's own login
    /// shell, which is the server's business, not ours.
    pub async fn open_pty(
        &self,
        cols: u32,
        rows: u32,
        program: PtyProgram,
        shell: String,
    ) -> Result<Pty, String> {
        match &self.kind {
            Kind::Remote(connection) => ssh::pty::open(connection, cols, rows, program).await,
            Kind::Local => crate::local::open_pty(cols, rows, program, shell),
        }
    }

    /// Hangs an SSH session up. A local host has nothing to hang up -- the
    /// shells die with their tabs -- which is why local mode offers no
    /// disconnect at all.
    pub fn disconnect(&self) {
        if let Kind::Remote(connection) = &self.kind {
            connection.disconnect();
        }
    }

    /// The same, awaited, for the path that is about to exit the process. See
    /// `Connection::disconnect_and_wait`.
    pub async fn disconnect_and_wait(&self) {
        if let Kind::Remote(connection) = &self.kind {
            connection.disconnect_and_wait().await;
        }
    }
}

/// `user@machine`, from the environment Windows always sets. Falling back to
/// something neutral rather than "unknown@unknown": this string is a tab title.
fn local_label() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let machine = std::env::var("COMPUTERNAME").unwrap_or_default();
    match (user.is_empty(), machine.is_empty()) {
        (false, false) => format!("{user}@{machine}"),
        (false, true) => user,
        _ => "this computer".to_string(),
    }
}
