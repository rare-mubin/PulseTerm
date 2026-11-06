//! This machine's files, behind the same API as `ssh::sftp`.
//!
//! Every method mirrors one of Sftp's, down to the `String` paths and the
//! `Result<_, String>` errors, so `host::Files` can hand either to the explorer,
//! the editor and the preview without any of them knowing which they have.
//!
//! Paths here are this platform's own -- backslashes and drive letters on
//! Windows, plain POSIX on Linux -- which is why `join` and `parent` go through
//! `std::path` rather than the hand-written POSIX ones `sftp` needs.

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
        #[cfg(windows)]
        if path == THIS_PC {
            return Ok(drives());
        }
        let path = normalize(&path);
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
        refuse_roots(&path)?;
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
        refuse_roots(&path)?;
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
        refuse_roots(&path)?;
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
        refuse_roots(&to)?;
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
        refuse_roots(&from)?;
        refuse_roots(&to)?;
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
    #[cfg(windows)]
    if parent == THIS_PC {
        // The drives list's entries are drive names, and each is its own root.
        return normalize(name);
    }
    Path::new(&normalize(parent)).join(name).to_string_lossy().to_string()
}

pub fn parent(path: &str) -> Option<&str> {
    #[cfg(windows)]
    {
        // Above a drive root is the list of drives, the way Explorer has
        // "This PC" above C:\ -- without it the tree could never leave the
        // drive it started on except by typing a whole path.
        if path == THIS_PC {
            return None;
        }
        if is_drive_root(path) {
            return Some(THIS_PC);
        }
    }
    Path::new(path).parent().and_then(|p| p.to_str())
}

/// A path as the tree should hold it. On Windows a bare `D:` is *not* the
/// root of D but "the current folder on D", so typing it into the path bar
/// listed whatever folder this process last used there, and joined names on
/// as `D:name`. It means `D:\`.
pub fn normalize(path: &str) -> String {
    #[cfg(windows)]
    {
        let bytes = path.as_bytes();
        if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return format!("{}\\", path.to_ascii_uppercase());
        }
    }
    path.to_string()
}

/// The virtual folder above every drive root: its entries are the drives.
#[cfg(windows)]
pub const THIS_PC: &str = "This PC";

/// `C:\`, `D:` and the like -- the top of a drive, which has no parent folder
/// and must never be deleted or renamed from the tree.
#[cfg(windows)]
fn is_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    (2..=3).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_none_or(|&c| c == b'\\' || c == b'/')
}

/// Refuses a change that would land on the drives list or on a drive root.
/// The drives are listed as folders, so the context menu offers Delete and
/// Rename on them like any other row -- and a recursive delete of `C:\` must
/// not be one confirmation away.
fn refuse_roots(path: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if path == THIS_PC || path.starts_with(&format!("{THIS_PC}\\")) {
            return Err(format!("{THIS_PC} is the list of drives; open a drive first"));
        }
        if is_drive_root(path) {
            return Err(format!("{path} is a drive, and cannot be deleted or renamed here"));
        }
    }
    let _ = path;
    Ok(())
}

/// The drives this machine has, as the entries of `THIS_PC`.
#[cfg(windows)]
fn drives() -> Vec<FileEntry> {
    // A bit per letter; asking for the list touches no drive, so an empty
    // card reader or a sleeping network drive cannot stall the tree.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| FileEntry {
            name: format!("{}:\\", (b'A' + i) as char),
            is_dir: true,
            is_symlink: false,
            size: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn drive_roots_lead_to_the_drives_list() {
        assert_eq!(normalize("d:"), "D:\\");
        assert_eq!(normalize("D:\\Projects"), "D:\\Projects");
        assert_eq!(parent("C:\\"), Some(THIS_PC));
        assert_eq!(parent("C:\\Users"), Some("C:\\"));
        assert_eq!(parent(THIS_PC), None);
        assert_eq!(join(THIS_PC, "D:\\"), "D:\\");
        assert_eq!(join("D:", "Projects"), "D:\\Projects");
    }

    #[cfg(windows)]
    #[test]
    fn drives_and_the_list_cannot_be_changed() {
        assert!(refuse_roots("C:\\").is_err());
        assert!(refuse_roots("D:").is_err());
        assert!(refuse_roots(THIS_PC).is_err());
        assert!(refuse_roots(&format!("{THIS_PC}\\New folder")).is_err());
        assert!(refuse_roots("C:\\Users\\rare\\notes.txt").is_ok());
    }
}
