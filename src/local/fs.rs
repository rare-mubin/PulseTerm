//! This machine's files, behind the same API as `ssh::sftp`.
//!
//! Every method mirrors one of Sftp's, down to the `String` paths and the
//! `Result<_, String>` errors, so `host::Files` can hand either to the explorer,
//! the editor and the preview without any of them knowing which they have.
//!
//! Paths here are Windows paths -- backslashes, drive letters -- which is why
//! `join` and `parent` are not the hand-written POSIX ones `sftp` needs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::runtime;
use crate::host::{FileEntry, Progress, Properties, MAX_EDITABLE_BYTES, TRANSFER_CHUNK};

/// The local filesystem. Nothing to hold -- it is a unit -- but it is a type of
/// its own so `Files` can be an enum over the two backends.
#[derive(Clone, Default)]
pub struct LocalFs;

/// Everything runs on the shared tokio runtime rather than inline: these are
/// awaited from gpui's executor, and a blocking read there would stall the
/// frame that asked for it.
async fn run<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    runtime::spawn(fut).await
}

fn io(path: &str, error: std::io::Error) -> String {
    format!("{path}: {error}")
}

/// Seconds since the Unix epoch, which is what `Properties` carries -- it was
/// shaped for SFTP, where that is the only form a timestamp comes in.
fn unix_seconds(time: std::io::Result<std::time::SystemTime>) -> Option<u32> {
    time.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
}

impl LocalFs {
    /// Folders first, then case-insensitively by name: the order a file
    /// explorer is expected to show, and the same one `Sftp::read_dir` applies.
    pub async fn read_dir(&self, path: String) -> Result<Vec<FileEntry>, String> {
        run(async move {
            let mut dir = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| io(&path, e))?;
            let mut entries = Vec::new();
            while let Some(entry) = dir.next_entry().await.map_err(|e| io(&path, e))? {
                let name = entry.file_name().to_string_lossy().to_string();
                // `file_type` follows no link, so a junction or symlink to a
                // directory is reported as a link *and* as a directory, which
                // is what the tree needs to know whether it can be expanded.
                let link = entry
                    .file_type()
                    .await
                    .map(|t| t.is_symlink())
                    .unwrap_or(false);
                let meta = tokio::fs::metadata(entry.path()).await.ok();
                entries.push(FileEntry {
                    name,
                    is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                    is_symlink: link,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                });
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(entries)
        })
        .await
    }

    pub async fn home(&self) -> Result<String, String> {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| "No home directory".to_string())
    }

    pub async fn create_dir(&self, path: String) -> Result<(), String> {
        run(async move {
            tokio::fs::create_dir(&path)
                .await
                .map_err(|e| io(&path, e))
        })
        .await
    }

    /// Recursive for a directory, like `sftp::remove` -- and for the same
    /// reason the tree offers it at all: deleting a folder one entry at a time
    /// from the UI is not a feature.
    pub async fn remove(&self, path: String, is_dir: bool) -> Result<(), String> {
        run(async move {
            if is_dir {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            }
            .map_err(|e| io(&path, e))
        })
        .await
    }

    /// Fails if the file is already there rather than truncating it: this is
    /// reached from "New file" in the explorer, where hitting an existing name
    /// must not silently empty it.
    pub async fn create_file(&self, path: String) -> Result<(), String> {
        run(async move {
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .map(|_| ())
                .map_err(|e| io(&path, e))
        })
        .await
    }

    pub async fn properties(&self, path: String) -> Result<Properties, String> {
        run(async move {
            let link = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|e| io(&path, e))?;
            let meta = tokio::fs::metadata(&path).await.unwrap_or(link.clone());
            Ok(Properties {
                size: meta.len(),
                is_dir: meta.is_dir(),
                is_symlink: link.file_type().is_symlink(),
                // Windows has no mode bits, and read-only is not one. The
                // Properties panel leaves the row out rather than invent a
                // number that looks like a Unix mode and isn't.
                permissions: None,
                modified: unix_seconds(meta.modified()),
                accessed: unix_seconds(meta.accessed()),
                user: None,
                group: None,
                uid: None,
                gid: None,
            })
        })
        .await
    }

    /// Copies a file *into* the browsed tree. Named for the remote case, where
    /// it is an upload; locally it is a copy, and the explorer's progress line
    /// says so.
    pub async fn upload(
        &self,
        local: PathBuf,
        remote: String,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        run(async move { copy_with_progress(local, PathBuf::from(remote), progress).await }).await
    }

    /// The other direction: out of the tree to a path the user picked.
    pub async fn download(
        &self,
        remote: String,
        local: PathBuf,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        run(async move { copy_with_progress(PathBuf::from(remote), local, progress).await }).await
    }

    pub async fn read_text(&self, path: String) -> Result<String, String> {
        run(async move {
            let bytes = read_capped(&path, MAX_EDITABLE_BYTES).await?;
            String::from_utf8(bytes).map_err(|_| {
                "This file isn't UTF-8 text. Opening it as text would destroy it on save."
                    .to_string()
            })
        })
        .await
    }

    pub async fn read_bytes(&self, path: String, max_bytes: u64) -> Result<Vec<u8>, String> {
        run(async move { read_capped(&path, max_bytes).await }).await
    }

    pub async fn write_text(&self, path: String, content: String) -> Result<(), String> {
        run(async move {
            tokio::fs::write(&path, content.as_bytes())
                .await
                .map_err(|e| io(&path, e))
        })
        .await
    }

    pub async fn exists(&self, path: String) -> bool {
        run(async move { tokio::fs::symlink_metadata(&path).await.is_ok() }).await
    }

    pub async fn copy(&self, from: String, to: String, is_dir: bool) -> Result<(), String> {
        run(async move {
            if is_dir {
                copy_dir(PathBuf::from(from), PathBuf::from(to)).await
            } else {
                tokio::fs::copy(&from, &to)
                    .await
                    .map(|_| ())
                    .map_err(|e| io(&from, e))
            }
        })
        .await
    }

    pub async fn rename(&self, from: String, to: String) -> Result<(), String> {
        run(async move {
            tokio::fs::rename(&from, &to)
                .await
                .map_err(|e| io(&from, e))
        })
        .await
    }
}

async fn read_capped(path: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let meta = tokio::fs::metadata(path).await.map_err(|e| io(path, e))?;
    if meta.len() > max_bytes {
        return Err(format!(
            "File is too large to open ({:.1} MB).",
            meta.len() as f64 / (1024. * 1024.)
        ));
    }
    tokio::fs::read(path).await.map_err(|e| io(path, e))
}

/// A chunked copy rather than `tokio::fs::copy`, for the same reason the SFTP
/// transfers are chunked: the one-shot call reports nothing until it is done,
/// so a large file would sit at 0% and then jump to finished.
async fn copy_with_progress(
    from: PathBuf,
    to: PathBuf,
    progress: Arc<Progress>,
) -> Result<(), String> {
    let mut source = tokio::fs::File::open(&from)
        .await
        .map_err(|e| format!("{}: {e}", from.display()))?;
    if let Ok(meta) = source.metadata().await {
        progress.set_total(meta.len());
    }
    let mut target = tokio::fs::File::create(&to)
        .await
        .map_err(|e| format!("{}: {e}", to.display()))?;

    let mut buffer = vec![0u8; TRANSFER_CHUNK];
    loop {
        let read = source
            .read(&mut buffer)
            .await
            .map_err(|e| format!("{}: {e}", from.display()))?;
        if read == 0 {
            break;
        }
        target
            .write_all(&buffer[..read])
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        progress.advance(read as u64);
    }
    target
        .flush()
        .await
        .map_err(|e| format!("{}: {e}", to.display()))
}

/// Recursive directory copy. `std::fs` has no such thing either, so this is the
/// local twin of `sftp::copy`'s recursion -- and it is written as a loop over an
/// explicit stack because an `async fn` cannot call itself without boxing.
async fn copy_dir(from: PathBuf, to: PathBuf) -> Result<(), String> {
    let mut pending = vec![(from, to)];
    while let Some((from, to)) = pending.pop() {
        tokio::fs::create_dir_all(&to)
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        let mut dir = tokio::fs::read_dir(&from)
            .await
            .map_err(|e| format!("{}: {e}", from.display()))?;
        while let Some(entry) = dir
            .next_entry()
            .await
            .map_err(|e| format!("{}: {e}", from.display()))?
        {
            let source = entry.path();
            let target = to.join(entry.file_name());
            let is_dir = tokio::fs::metadata(&source)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if is_dir {
                pending.push((source, target));
            } else {
                tokio::fs::copy(&source, &target)
                    .await
                    .map_err(|e| format!("{}: {e}", source.display()))?;
            }
        }
    }
    Ok(())
}

/// `sftp::join`'s local counterpart. `Path::join` is right here for exactly the
/// reason it is wrong there: this side really does want the platform's
/// separator, and on Windows that is a backslash.
pub fn join(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).to_string_lossy().to_string()
}

pub fn parent(path: &str) -> Option<&str> {
    Path::new(path).parent().and_then(|p| p.to_str())
}
