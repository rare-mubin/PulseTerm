//! One-shot commands and port forwards on an open connection.
//!
//! Both exist for the remote desktop helper (REMOTE_DESKTOP.md): it has no SSH
//! connection of its own, so PulseTerm runs its probes and reaches Sunshine's
//! web API for it, over the connection the terminals already share. Each is a
//! new channel on that connection -- no second handshake, no second login.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use russh::ChannelMsg;
use tokio::net::TcpListener;

use super::client::Connection;
use crate::runtime;

/// What a command left behind. `status` is `None` when the server closed the
/// channel without reporting one (killed by a signal, or a broken server).
#[derive(Clone, Debug, Default)]
pub struct ExecOutput {
    pub status: Option<u32>,
    pub stdout: String,
    pub stderr: String,
}

/// Output beyond this is dropped: a probe answers in a few lines, and a
/// command that streams forever must not grow memory without limit.
const MAX_OUTPUT: usize = 1 << 20;

impl Connection {
    /// Runs `command` through the account's shell, without a pty, and waits
    /// for it to finish. The one-shot counterpart of `ssh::pty::open`.
    pub async fn exec(&self, command: String) -> Result<ExecOutput, String> {
        let handle = self.handle.clone();
        runtime::spawn(async move {
            let mut channel = handle
                .channel_open_session()
                .await
                .map_err(|e| format!("Failed to open channel: {e}"))?;
            channel
                .exec(true, command)
                .await
                .map_err(|e| format!("Failed to run command: {e}"))?;

            let mut out = ExecOutput::default();
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            // The exit status can arrive before or after EOF, so read until
            // the channel itself is gone rather than stopping at either.
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { data } => append(&mut stdout, &data),
                    ChannelMsg::ExtendedData { data, .. } => append(&mut stderr, &data),
                    ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            out.stdout = String::from_utf8_lossy(&stdout).into_owned();
            out.stderr = String::from_utf8_lossy(&stderr).into_owned();
            Ok(out)
        })
        .await
    }

    /// Listens on `127.0.0.1` on a free port and carries every connection
    /// made to it to `127.0.0.1:remote_port` *on the host*, through a
    /// `direct-tcpip` channel. Returns the local port and a guard: dropping the
    /// guard stops accepting (connections already open run to their end).
    pub async fn forward(&self, remote_port: u16) -> Result<(u16, Forward), String> {
        let handle = self.handle.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_task = stop.clone();
        let port = runtime::spawn(async move {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| format!("Failed to listen: {e}"))?;
            let port = listener
                .local_addr()
                .map_err(|e| format!("Failed to listen: {e}"))?
                .port();
            runtime::detach(async move {
                loop {
                    // Woken at least once a second to notice the guard going,
                    // since a listener with no clients would never wake.
                    let accepted = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        listener.accept(),
                    )
                    .await;
                    if stop_task.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(Ok((mut socket, peer))) = accepted else { continue };
                    let handle = handle.clone();
                    runtime::detach(async move {
                        let channel = match handle
                            .channel_open_direct_tcpip(
                                "127.0.0.1",
                                remote_port as u32,
                                peer.ip().to_string(),
                                peer.port() as u32,
                            )
                            .await
                        {
                            Ok(channel) => channel,
                            Err(_) => return,
                        };
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    });
                }
            });
            Ok::<u16, String>(port)
        })
        .await?;
        Ok((port, Forward { stop }))
    }
}

impl Connection {
    /// Starts `command` on a channel that stays open until the returned
    /// handle is dropped (or the connection dies). Its output is discarded;
    /// `Spawned::write` feeds its stdin.
    ///
    /// Closing is the point: dropping the handle sends EOF and closes the
    /// channel, so a command written to wait on its stdin (`cat`, `read`)
    /// learns that PulseTerm has let go -- and if the connection drops
    /// instead, sshd closes that stdin for us. The remote desktop helper
    /// runs PulseTerm's Sunshine instance this way, so it can never outlive
    /// the session that started it.
    pub async fn spawn(&self, command: String) -> Result<Spawned, String> {
        let handle = self.handle.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<Vec<u8>>>();
        runtime::spawn(async move {
            let mut channel = handle
                .channel_open_session()
                .await
                .map_err(|e| format!("Failed to open channel: {e}"))?;
            channel
                .exec(true, command)
                .await
                .map_err(|e| format!("Failed to run command: {e}"))?;
            runtime::detach(async move {
                loop {
                    tokio::select! {
                        msg = rx.recv() => match msg {
                            Some(Some(data)) => {
                                if channel.data(&data[..]).await.is_err() {
                                    break;
                                }
                            }
                            // `None` from the handle, or the handle dropped.
                            _ => {
                                let _ = channel.eof().await;
                                let _ = channel.close().await;
                                break;
                            }
                        },
                        msg = channel.wait() => match msg {
                            None | Some(ChannelMsg::Close) => break,
                            _ => {}
                        },
                    }
                }
            });
            Ok::<(), String>(())
        })
        .await?;
        Ok(Spawned { tx })
    }
}

/// A command started by `Connection::spawn`. Dropping it closes the command's
/// stdin and its channel.
pub struct Spawned {
    tx: tokio::sync::mpsc::UnboundedSender<Option<Vec<u8>>>,
}

impl Spawned {
    pub fn write(&self, data: Vec<u8>) {
        let _ = self.tx.send(Some(data));
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.tx.send(None);
    }
}

fn append(buffer: &mut Vec<u8>, data: &[u8]) {
    let room = MAX_OUTPUT.saturating_sub(buffer.len());
    buffer.extend_from_slice(&data[..data.len().min(room)]);
}

/// Keeps a `Connection::forward` listening. Dropped, the listener closes
/// within a second.
pub struct Forward {
    stop: Arc<AtomicBool>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
