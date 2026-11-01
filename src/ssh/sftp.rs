use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::host::{FileEntry, Progress, Properties, MAX_EDITABLE_BYTES, TRANSFER_CHUNK};
use crate::runtime;

/// The SFTP subsystem for a connection.
///
/// One session is opened alongside the shell at connect time and shared by
/// everything that browses files -- it is a channel on the same SSH connection,
/// exactly like the terminals, so the explorer costs no extra handshake.
///
/// `FileEntry`, `Properties` and `Progress` are `host`'s, not this module's:
/// the local backend answers in the same vocabulary.
#[derive(Clone)]
pub struct Sftp {
    session: Arc<SftpSession>,
}

impl Sftp {
    pub(super) fn new(session: SftpSession) -> Self {
        Self {
            session: Arc::new(session),
        }
    }

    /// Lists a directory, folders first and then case-insensitively by name —
    /// the order a file explorer is expected to show.
    pub async fn read_dir(&self, path: String) -> Result<Vec<FileEntry>, String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let dir = session.read_dir(&path).await.map_err(|e| e.to_string())?;
            let mut entries: Vec<FileEntry> = dir
                .filter_map(|entry| {
                    let name = entry.file_name();
                    if name == "." || name == ".." {
                        return None;
                    }
                    let attrs = entry.metadata();
                    Some(FileEntry {
                        name,
                        is_dir: attrs.is_dir(),
                        is_symlink: attrs.is_symlink(),
                        size: attrs.size.unwrap_or(0),
                    })
                })
                .collect();
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(entries)
        })
        .await
    }

    /// The absolute path of the login directory, used as the explorer's root.
    pub async fn home(&self) -> Result<String, String> {
        let session = self.session.clone();
        runtime::spawn(async move { session.canonicalize(".").await.map_err(|e| e.to_string()) })
            .await
    }

    pub async fn create_dir(&self, path: String) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move { session.create_dir(&path).await.map_err(|e| e.to_string()) })
            .await
    }

    /// Deletes a file, or a directory and everything under it.
    ///
    /// SFTP's `remove_dir` only unlinks an empty directory, so a folder has to
    /// be walked and emptied from the leaves up. There is no server-side
    /// recursive delete to defer to.
    pub async fn remove(&self, path: String, is_dir: bool) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move { remove_inner(session, path, is_dir).await }).await
    }

    /// Creates an empty file.
    ///
    /// Deliberately not `SftpSession::write`: despite its name that opens with
    /// `OpenFlags::WRITE` alone, with no `CREATE`, so the server refuses with
    /// "No such file" whenever the path doesn't already exist. `create` is the
    /// one that sends `CREATE | TRUNCATE | WRITE`.
    pub async fn create_file(&self, path: String) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let mut file = session.create(&path).await.map_err(|e| e.to_string())?;
            // Closes the handle; dropping it would leave the file open on the
            // server until the session ends.
            file.shutdown().await.map_err(|e| e.to_string())
        })
        .await
    }

    /// Reads one path's attributes for the Properties panel.
    pub async fn properties(&self, path: String) -> Result<Properties, String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let attrs = session.metadata(&path).await.map_err(|e| e.to_string())?;
            Ok(Properties {
                size: attrs.size.unwrap_or(0),
                is_dir: attrs.is_dir(),
                is_symlink: attrs.is_symlink(),
                permissions: attrs.permissions,
                modified: attrs.mtime,
                accessed: attrs.atime,
                user: attrs.user.clone(),
                group: attrs.group.clone(),
                uid: attrs.uid,
                gid: attrs.gid,
            })
        })
        .await
    }

    /// Copies a local file to the remote host.
    ///
    /// Streamed rather than read into memory: an upload should not be limited by
    /// how much of the file fits in RAM.
    pub async fn upload(
        &self,
        local: PathBuf,
        remote: String,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let mut source = tokio::fs::File::open(&local)
                .await
                .map_err(|e| format!("{}: {e}", local.display()))?;
            progress.set_total(source.metadata().await.map(|meta| meta.len()).unwrap_or(0));
            let mut destination = session.create(&remote).await.map_err(|e| e.to_string())?;

            // Chunked by hand rather than `tokio::io::copy`, which reports
            // nothing until it returns -- a 200 MB upload would sit at 0%.
            let mut buffer = vec![0u8; TRANSFER_CHUNK];
            loop {
                let read = source.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                destination
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|e| e.to_string())?;
                progress.advance(read as u64);
            }

            // Without an explicit shutdown the last buffered chunk can be lost;
            // dropping the handle does not flush it.
            destination.shutdown().await.map_err(|e| e.to_string())
        })
        .await
    }

    /// Copies a remote file to the local disk.
    pub async fn download(
        &self,
        remote: String,
        local: PathBuf,
        progress: Arc<Progress>,
    ) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            // A stat the server may refuse; 0 just means the line shows bytes
            // and speed without a percentage.
            let total = session
                .metadata(&remote)
                .await
                .ok()
                .and_then(|attrs| attrs.size)
                .unwrap_or(0);
            progress.set_total(total);

            let mut source = session.open(&remote).await.map_err(|e| e.to_string())?;
            let mut destination = tokio::fs::File::create(&local)
                .await
                .map_err(|e| format!("{}: {e}", local.display()))?;

            let mut buffer = vec![0u8; TRANSFER_CHUNK];
            loop {
                let read = source.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                destination
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|e| e.to_string())?;
                progress.advance(read as u64);
            }

            destination.shutdown().await.map_err(|e| e.to_string())
        })
        .await
    }

    /// Reads a whole remote file as text, for the editor.
    ///
    /// Refuses anything over 5 MB or that isn't valid UTF-8, as RemoteTTY did:
    /// the editor holds the whole document in memory, and a binary file opened
    /// as text is garbage that would be destroyed by the first save.
    pub async fn read_text(&self, path: String) -> Result<String, String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let attrs = session.metadata(&path).await.map_err(|e| e.to_string())?;
            let size = attrs.size.unwrap_or(0);
            if size > MAX_EDITABLE_BYTES {
                return Err(format!(
                    "{:.1} MB is too large to edit (limit 5 MB)",
                    size as f64 / (1024. * 1024.)
                ));
            }
            let bytes = session.read(&path).await.map_err(|e| e.to_string())?;
            String::from_utf8(bytes).map_err(|_| "not a UTF-8 text file".to_string())
        })
        .await
    }

    /// Reads a whole remote file as bytes, for the image viewer.
    ///
    /// Capped like `read_text`: the bytes are decoded into a texture in one go,
    /// and a video or a camera raw opened by a mistaken double-click shouldn't
    /// try. The cap is the caller's, since "too big" differs by use.
    pub async fn read_bytes(&self, path: String, max_bytes: u64) -> Result<Vec<u8>, String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let attrs = session.metadata(&path).await.map_err(|e| e.to_string())?;
            let size = attrs.size.unwrap_or(0);
            if size > max_bytes {
                return Err(format!(
                    "{:.1} MB is too large to preview (limit {} MB)",
                    size as f64 / (1024. * 1024.),
                    max_bytes / (1024 * 1024)
                ));
            }
            session.read(&path).await.map_err(|e| e.to_string())
        })
        .await
    }

    /// Replaces a remote file's contents.
    ///
    /// `create` truncates and rewrites in place, so the file keeps its mode and
    /// ownership. The alternative -- write a temp file and rename it over the
    /// original -- would be atomic, but SFTP v3 `rename` refuses to replace an
    /// existing file, so it would need a delete in between and stop being atomic
    /// anyway.
    pub async fn write_text(&self, path: String, content: String) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move {
            let mut file = session.create(&path).await.map_err(|e| e.to_string())?;
            file.write_all(content.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            file.shutdown().await.map_err(|e| e.to_string())
        })
        .await
    }

    pub async fn exists(&self, path: String) -> bool {
        let session = self.session.clone();
        runtime::spawn(async move { session.try_exists(path).await.unwrap_or(false) }).await
    }

    /// Copies a file, or a directory tree.
    ///
    /// SFTP has no copy verb at all, so a file copy is a read/write stream
    /// between two open handles on the *server's* filesystem -- the bytes cross
    /// the wire twice but never touch this machine's disk.
    pub async fn copy(&self, from: String, to: String, is_dir: bool) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move { copy_inner(session, from, to, is_dir).await }).await
    }

    pub async fn rename(&self, from: String, to: String) -> Result<(), String> {
        let session = self.session.clone();
        runtime::spawn(async move { session.rename(&from, &to).await.map_err(|e| e.to_string()) })
            .await
    }
}

/// Async recursion needs an explicit boxed future; `async fn` cannot call itself.
type Task = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

fn remove_inner(session: Arc<SftpSession>, path: String, is_dir: bool) -> Task {
    Box::pin(async move {
        if !is_dir {
            return session.remove_file(&path).await.map_err(|e| e.to_string());
        }

        let dir = session.read_dir(&path).await.map_err(|e| e.to_string())?;
        for entry in dir {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = join(&path, &name);
            remove_inner(session.clone(), child, entry.metadata().is_dir()).await?;
        }
        session.remove_dir(&path).await.map_err(|e| e.to_string())
    })
}

fn copy_inner(session: Arc<SftpSession>, from: String, to: String, is_dir: bool) -> Task {
    Box::pin(async move {
        if !is_dir {
            let mut source = session.open(&from).await.map_err(|e| e.to_string())?;
            let mut destination = session.create(&to).await.map_err(|e| e.to_string())?;
            tokio::io::copy(&mut source, &mut destination)
                .await
                .map_err(|e| e.to_string())?;
            // Without an explicit shutdown the last buffered chunk can be lost,
            // since dropping the handle does not flush it.
            destination.shutdown().await.map_err(|e| e.to_string())?;
            return Ok(());
        }

        session.create_dir(&to).await.map_err(|e| e.to_string())?;
        let dir = session.read_dir(&from).await.map_err(|e| e.to_string())?;
        for entry in dir {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            copy_inner(
                session.clone(),
                join(&from, &name),
                join(&to, &name),
                entry.metadata().is_dir(),
            )
            .await?;
        }
        Ok(())
    })
}

/// Joins a remote path. Remote paths are POSIX regardless of what this client
/// runs on, so `std::path` is deliberately not involved -- it would produce
/// backslashes on Windows.
pub fn join(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// The parent of a remote path, or `None` at the root.
pub fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => Some("/"),
        Some(index) => Some(&trimmed[..index]),
        None => None,
    }
}
