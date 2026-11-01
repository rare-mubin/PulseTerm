use russh::ChannelMsg;
use tokio::sync::mpsc;

use super::client::Connection;
use crate::pty::{Pty, PtyEvent, PtyMsg, PtyProgram};
use crate::runtime;

/// Opens a new interactive shell channel on an existing connection and spawns
/// the task that pumps data both ways. One SSH connection hosts many of these
/// concurrently -- that's channel multiplexing, and it's what gives tmux-like
/// multi-tab terminals without a connection or a remote agent per tab.
pub async fn open(
    connection: &Connection,
    cols: u32,
    rows: u32,
    program: PtyProgram,
) -> Result<Pty, String> {
    let handle = connection.handle.clone();

    let (tx, rx) = mpsc::unbounded_channel::<PtyMsg>();
    // Bounded so a terminal that stops draining (a hidden tab spewing output)
    // exerts backpressure on the pump task instead of growing without limit.
    let (event_tx, events) = flume::bounded::<PtyEvent>(512);

    runtime::spawn(async move {
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| format!("Failed to open channel: {e}"))?;

        channel
            .request_pty(false, "xterm-256color", cols, rows, 0, 0, &[])
            .await
            .map_err(|e| format!("Failed to request pty: {e}"))?;

        match program {
            PtyProgram::Command(cmd) => channel
                .exec(true, cmd)
                .await
                .map_err(|e| format!("Failed to run command: {e}"))?,
            PtyProgram::LoginShell => channel
                .request_shell(true)
                .await
                .map_err(|e| format!("Failed to start shell: {e}"))?,
            // There is no "start the shell here" request in SSH, so the
            // server's shell is asked to change directory and then replace
            // itself with the account's login shell. Single-quoted, with any
            // quote inside the path closed, escaped and reopened, so a path
            // with spaces or quotes cannot break out of the argument.
            PtyProgram::ShellIn(path) => {
                let quoted = format!("'{}'", path.replace('\'', "'\\''"));
                channel
                    .exec(true, format!("cd {quoted} && exec \"${{SHELL:-/bin/sh}}\" -l"))
                    .await
                    .map_err(|e| format!("Failed to start shell: {e}"))?
            }
        }

        runtime::detach(pump(channel, rx, event_tx));
        Ok::<(), String>(())
    })
    .await?;

    Ok(Pty { tx, events })
}

async fn pump(
    mut channel: russh::Channel<russh::client::Msg>,
    mut rx: mpsc::UnboundedReceiver<PtyMsg>,
    events: flume::Sender<PtyEvent>,
) {
    let mut close_error: Option<String> = None;

    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data })
                | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    // `send_async` rather than `send`: if the terminal view is
                    // behind, block this task instead of dropping output.
                    if events.send_async(PtyEvent::Output(data.to_vec())).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    if exit_status != 0 {
                        close_error = Some(format!("process exited with status {exit_status}"));
                    }
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            incoming = rx.recv() => match incoming {
                Some(PtyMsg::Data(bytes)) => {
                    if channel.data_bytes(bytes).await.is_err() {
                        break;
                    }
                }
                Some(PtyMsg::Resize(cols, rows)) => {
                    let _ = channel.window_change(cols, rows, 0, 0).await;
                }
                Some(PtyMsg::Close) | None => {
                    let _ = channel.eof().await;
                    let _ = channel.close().await;
                    break;
                }
            },
        }
    }

    let _ = events.send_async(PtyEvent::Closed { error: close_error }).await;
}
