//! The pipe to PulseTerm: how this helper reaches the host.
//!
//! The helper has no SSH connection of its own. PulseTerm starts it with
//! stdin/stdout piped and serves its requests over the connection the
//! terminals already share (PulseTerm's `src/remote.rs` has the protocol). So
//! **nothing but protocol lines may ever be written to stdout** -- logging
//! goes to stderr.
//!
//! `--local` swaps the pipe for this machine: commands run through `sh -c`
//! and a "forward" is the port itself. That is for developing the launcher
//! without an SSH host, nothing more.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::{mpsc, oneshot};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default)]
pub struct ExecOutput {
    #[allow(dead_code)] // the control actions (start/stop Sunshine) read it
    pub status: Option<u32>,
    pub stdout: String,
    #[allow(dead_code)]
    pub stderr: String,
}

/// What PulseTerm can tell the helper without being asked.
pub enum Event {
    /// The navigation bar icon was clicked while this window was open.
    Raise,
    /// stdin closed: PulseTerm disconnected or quit.
    Closed,
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

#[derive(Clone)]
pub struct Bridge {
    kind: Kind,
}

#[derive(Clone)]
enum Kind {
    Pipe { next: Arc<AtomicU64>, pending: Arc<Pending> },
    Local { children: Arc<Mutex<HashMap<u64, std::process::Child>>>, next: Arc<AtomicU64> },
}

impl Bridge {
    /// The real thing: requests out on stdout, answers and events in on stdin.
    pub fn pipe() -> (Self, mpsc::UnboundedReceiver<Event>) {
        let pending: Arc<Pending> = Arc::default();
        let (events, receiver) = mpsc::unbounded();
        let answers = pending.clone();
        std::thread::Builder::new()
            .name("remote-pipe".into())
            .spawn(move || {
                for line in std::io::stdin().lock().lines() {
                    let Ok(line) = line else { break };
                    let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                    if message.get("event").and_then(Value::as_str) == Some("raise") {
                        let _ = events.unbounded_send(Event::Raise);
                        continue;
                    }
                    let Some(id) = message.get("id").and_then(Value::as_u64) else { continue };
                    let Some(reply) = answers.lock().unwrap().remove(&id) else { continue };
                    let _ = reply.send(match message.get("error").and_then(Value::as_str) {
                        Some(error) => Err(error.to_string()),
                        None => Ok(message.get("ok").cloned().unwrap_or(Value::Null)),
                    });
                }
                // Every request still waiting is answered, so nothing hangs.
                for (_, reply) in answers.lock().unwrap().drain() {
                    let _ = reply.send(Err("PulseTerm closed the connection".into()));
                }
                let _ = events.unbounded_send(Event::Closed);
            })
            .expect("failed to start the pipe thread");
        (Self { kind: Kind::Pipe { next: Arc::new(AtomicU64::new(1)), pending } }, receiver)
    }

    pub fn local() -> Self {
        Self { kind: Kind::Local { children: Arc::default(), next: Arc::new(AtomicU64::new(1)) } }
    }

    async fn request(&self, mut op: Value) -> Result<Value, String> {
        let Kind::Pipe { next, pending } = &self.kind else {
            unreachable!("local mode answers without the pipe")
        };
        let id = next.fetch_add(1, Ordering::Relaxed);
        op["id"] = json!(id);
        let (reply, answer) = oneshot::channel();
        pending.lock().unwrap().insert(id, reply);
        {
            let mut out = std::io::stdout().lock();
            writeln!(out, "{op}")
                .and_then(|_| out.flush())
                .map_err(|e| format!("PulseTerm is gone: {e}"))?;
        }
        answer.await.unwrap_or_else(|_| Err("PulseTerm closed the connection".into()))
    }

    /// Runs `command` through the host account's shell and waits for it.
    pub async fn exec(&self, command: &str) -> Result<ExecOutput, String> {
        match &self.kind {
            Kind::Local { .. } => {
                let command = command.to_string();
                // Blocking, so on a thread of its own rather than the UI's.
                let (reply, answer) = oneshot::channel();
                std::thread::spawn(move || {
                    let out = std::process::Command::new("sh").arg("-c").arg(&command).output();
                    let _ = reply.send(out);
                });
                let out = answer
                    .await
                    .map_err(|_| "command thread died".to_string())?
                    .map_err(|e| e.to_string())?;
                Ok(ExecOutput {
                    status: out.status.code().map(|c| c as u32),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
            Kind::Pipe { .. } => {
                let ok = self.request(json!({ "op": "exec", "command": command })).await?;
                Ok(ExecOutput {
                    status: ok.get("status").and_then(Value::as_u64).map(|s| s as u32),
                    stdout: ok.get("stdout").and_then(Value::as_str).unwrap_or("").to_string(),
                    stderr: ok.get("stderr").and_then(Value::as_str).unwrap_or("").to_string(),
                })
            }
        }
    }

    /// A local port that reaches `127.0.0.1:port` on the host.
    #[allow(dead_code)] // phase 2: Sunshine's web API for pairing
    pub async fn forward(&self, port: u16) -> Result<u16, String> {
        match &self.kind {
            Kind::Local { .. } => Ok(port),
            Kind::Pipe { .. } => {
                let ok = self.request(json!({ "op": "forward", "port": port })).await?;
                ok.get("local_port")
                    .and_then(Value::as_u64)
                    .map(|p| p as u16)
                    .ok_or_else(|| "PulseTerm sent no port".to_string())
            }
        }
    }

    /// Starts a long-lived command on the host and returns its handle. It
    /// runs until `kill`, until this helper exits, or until the SSH
    /// connection drops -- each of which closes its stdin.
    pub async fn spawn(&self, command: &str) -> Result<u64, String> {
        match &self.kind {
            Kind::Local { children, next } => {
                let child = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| e.to_string())?;
                let handle = next.fetch_add(1, Ordering::Relaxed);
                children.lock().unwrap().insert(handle, child);
                Ok(handle)
            }
            Kind::Pipe { .. } => {
                let ok = self.request(json!({ "op": "spawn", "command": command })).await?;
                ok.get("handle").and_then(Value::as_u64).ok_or_else(|| "PulseTerm sent no handle".into())
            }
        }
    }

    /// Writes to a spawned command's stdin.
    pub async fn write(&self, handle: u64, data: &str) -> Result<(), String> {
        match &self.kind {
            Kind::Local { children, .. } => {
                let mut children = children.lock().unwrap();
                let child = children.get_mut(&handle).ok_or("no such command")?;
                let stdin = child.stdin.as_mut().ok_or("stdin closed")?;
                stdin.write_all(data.as_bytes()).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
            }
            Kind::Pipe { .. } => {
                self.request(json!({ "op": "write", "handle": handle, "data": data })).await.map(|_| ())
            }
        }
    }

    /// Closes a spawned command's stdin and channel.
    pub async fn kill(&self, handle: u64) -> Result<(), String> {
        match &self.kind {
            Kind::Local { children, .. } => {
                if let Some(mut child) = children.lock().unwrap().remove(&handle) {
                    drop(child.stdin.take());
                    // Reaped off the UI thread; the wrapper takes a moment.
                    std::thread::spawn(move || child.wait());
                }
                Ok(())
            }
            Kind::Pipe { .. } => self.request(json!({ "op": "kill", "handle": handle })).await.map(|_| ()),
        }
    }
}
