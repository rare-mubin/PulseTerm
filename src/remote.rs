//! The remote desktop helper, seen from PulseTerm (REMOTE_DESKTOP.md).
//!
//! The helper, `pulseterm-remote`, is a separate program built separately.
//! Nothing here links against it: PulseTerm finds the binary at run time,
//! starts it, and serves it over a pipe. It has no SSH connection of its own
//! -- its probes and its route to Sunshine's web API go through the one this
//! window already has, so a remote desktop costs no second login.
//!
//! The pipe speaks JSON, one object per line. The helper writes requests to
//! its stdout, PulseTerm answers on the helper's stdin:
//!
//! ```text
//! → {"id":1,"op":"exec","command":"command -v sunshine"}
//! ← {"id":1,"ok":{"status":0,"stdout":"/usr/bin/sunshine\n","stderr":""}}
//! → {"id":2,"op":"forward","port":47990}
//! ← {"id":2,"ok":{"local_port":40123}}
//! → {"id":3,"op":"spawn","command":"..."}   (a long-lived command)
//! ← {"id":3,"ok":{"handle":1}}
//! → {"id":4,"op":"write","handle":1,"data":"1234\n"}   (to its stdin)
//! → {"id":5,"op":"kill","handle":1}          (EOF + close its channel)
//! ← {"event":"raise"}                     (the nav icon was clicked again)
//! ```
//!
//! An error answers `{"id":n,"error":"..."}`; `write` and `kill` answer
//! `{"ok":null}`. EOF on stdin means PulseTerm let go -- disconnected or quit
//! -- and the helper should wind down. When the helper exits, every command
//! it spawned is closed; when the connection drops, sshd closes them.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use std::collections::HashMap;

use crate::ssh::exec::{Forward, Spawned};
use crate::ssh::Connection;

#[cfg(windows)]
const BINARY: &str = "pulseterm-remote.exe";
#[cfg(not(windows))]
const BINARY: &str = "pulseterm-remote";

/// Where the helper is, if it is installed: beside the PulseTerm executable
/// first, then on `PATH`. Asked every frame by the navigation bar, so the
/// answer is kept for a few seconds -- long enough to cost nothing, short
/// enough that a helper built and dropped in place shows up without a restart.
pub fn helper_path() -> Option<PathBuf> {
    static CACHE: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, path)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(5) {
            return path.clone();
        }
    }
    let found = find_helper();
    *cache = Some((Instant::now(), found.clone()));
    found
}

fn find_helper() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(BINARY)));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(BINARY));
    beside.into_iter().chain(on_path).find(|path| path.is_file())
}

/// A running helper and the pipe serving it. Dropping it closes the helper's
/// stdin (its cue to quit) and every port forward it asked for.
pub struct Session {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    alive: Arc<AtomicBool>,
    _held: Arc<Held>,
}

/// What the helper asked for that outlives a request: port forwards and
/// spawned commands. Dropped with the session, or emptied when the helper
/// exits -- which closes every one of them.
#[derive(Default)]
struct Held {
    forwards: Mutex<Vec<Forward>>,
    spawned: Mutex<HashMap<u64, Spawned>>,
    next: std::sync::atomic::AtomicU64,
}

impl Held {
    fn clear(&self) {
        self.forwards.lock().unwrap().clear();
        self.spawned.lock().unwrap().clear();
    }
}

#[derive(Deserialize)]
struct Request {
    id: u64,
    #[serde(flatten)]
    op: Op,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Exec { command: String },
    Forward { port: u16 },
    Spawn { command: String },
    Write { handle: u64, data: String },
    Kill { handle: u64 },
}

impl Session {
    /// Starts the helper for `connection`. `label` is `user@host:port` for its
    /// title bar; `address` is the host as dialled, which the stream needs to
    /// reach Sunshine's UDP ports directly.
    /// `theme` is `theme_handoff`'s JSON: the helper draws in PulseTerm's
    /// design language without sharing any of its code.
    pub fn launch(connection: Connection, theme: String) -> Result<Self, String> {
        let path = helper_path().ok_or("PulseTerm Remote is not installed")?;
        let mut child: Child = Command::new(&path)
            .arg("--label")
            .arg(&connection.label)
            .arg("--address")
            .arg(&connection.host)
            .arg("--theme")
            .arg(theme)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to start {}: {e}", path.display()))?;

        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().ok_or("helper has no stdout")?;
        let alive = Arc::new(AtomicBool::new(true));
        let held: Arc<Held> = Arc::default();

        let (reply_to, still_alive, keep) = (stdin.clone(), alive.clone(), held.clone());
        // A plain thread: the pipe is blocking IO and has no business on
        // either async executor. Each request is served on the SSH runtime and
        // answers whenever it finishes, so a slow probe never holds up a
        // quick one.
        std::thread::Builder::new()
            .name("pulseterm-remote-pipe".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let request: Request = match serde_json::from_str(&line) {
                        Ok(request) => request,
                        Err(e) => {
                            eprintln!("pulseterm-remote: bad request {line:?}: {e}");
                            continue;
                        }
                    };
                    serve(request, &connection, &reply_to, &keep);
                }
                // stdout closed: the helper is exiting. Close what it held
                // open on the host (its Sunshine goes with it), reap it so it
                // does not linger as a zombie, and let the next click start a
                // new one.
                keep.clear();
                let _ = child.wait();
                still_alive.store(false, Ordering::Relaxed);
            })
            .map_err(|e| format!("Failed to start the helper pipe: {e}"))?;

        Ok(Self { stdin, alive, _held: held })
    }

    /// False once the helper has exited (its window was closed).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Asks the helper to bring its window forward. A window belongs to the
    /// process that made it, so this is a request, not an act.
    pub fn raise(&self) {
        send(&self.stdin, json!({ "event": "raise" }));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing stdin is the goodbye; the helper decides how to wind down.
        self.stdin.lock().unwrap().take();
    }
}

fn serve(
    request: Request,
    connection: &Connection,
    reply_to: &Arc<Mutex<Option<ChildStdin>>>,
    held: &Arc<Held>,
) {
    let (connection, reply_to, held) = (connection.clone(), reply_to.clone(), held.clone());
    let id = request.id;
    crate::runtime::detach(async move {
        let answer: Result<Value, String> = match request.op {
            Op::Exec { command } => connection.exec(command).await.map(|out| {
                json!({ "status": out.status, "stdout": out.stdout, "stderr": out.stderr })
            }),
            Op::Forward { port } => connection.forward(port).await.map(|(local_port, guard)| {
                held.forwards.lock().unwrap().push(guard);
                json!({ "local_port": local_port })
            }),
            Op::Spawn { command } => connection.spawn(command).await.map(|spawned| {
                let handle = held.next.fetch_add(1, Ordering::Relaxed) + 1;
                held.spawned.lock().unwrap().insert(handle, spawned);
                json!({ "handle": handle })
            }),
            Op::Write { handle, data } => match held.spawned.lock().unwrap().get(&handle) {
                Some(spawned) => {
                    spawned.write(data.into_bytes());
                    Ok(Value::Null)
                }
                None => Err(format!("no spawned command {handle}")),
            },
            Op::Kill { handle } => {
                held.spawned.lock().unwrap().remove(&handle);
                Ok(Value::Null)
            }
        };
        send(
            &reply_to,
            match answer {
                Ok(ok) => json!({ "id": id, "ok": ok }),
                Err(error) => json!({ "id": id, "error": error }),
            },
        );
    });
}

fn send(stdin: &Arc<Mutex<Option<ChildStdin>>>, message: Value) {
    if let Some(pipe) = stdin.lock().unwrap().as_mut() {
        // A helper that has gone away makes this fail; nothing to do about it.
        let _ = writeln!(pipe, "{message}").and_then(|_| pipe.flush());
    }
}

/// The resolved design tokens, for the helper's window: colours as opaque
/// `#rrggbb` (the helper's window is never translucent), plus the font. The
/// helper links none of PulseTerm's code, so this is how it matches the
/// palette, the user's overrides and the font size.
pub fn theme_handoff(theme: &crate::theme::Theme) -> String {
    let hex = |c: gpui::Hsla| {
        let c = gpui::Rgba::from(gpui::Hsla { a: 1., ..c });
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(c.r), byte(c.g), byte(c.b))
    };
    json!({
        "font": theme.font.to_string(),
        "font_size": f32::from(theme.font_size),
        "line_height": f32::from(theme.line_height),
        "light": theme.light,
        "bg": hex(theme.palette.bg),
        "surface": hex(theme.palette.surface),
        "raised": hex(theme.palette.raised),
        "border": hex(theme.border),
        "fg": hex(theme.fg),
        "fg_muted": hex(theme.fg_muted),
        "fg_subtle": hex(theme.fg_subtle),
        "accent": hex(theme.accent),
        "success": hex(theme.success),
        "warning": hex(theme.warning),
        "danger": hex(theme.danger),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse() {
        let r: Request = serde_json::from_str(r#"{"id":3,"op":"exec","command":"true"}"#).unwrap();
        assert_eq!(r.id, 3);
        assert!(matches!(r.op, Op::Exec { ref command } if command == "true"));
        let r: Request = serde_json::from_str(r#"{"id":4,"op":"forward","port":47990}"#).unwrap();
        assert!(matches!(r.op, Op::Forward { port: 47990 }));
        let r: Request =
            serde_json::from_str(r#"{"id":6,"op":"write","handle":1,"data":"1234\n"}"#).unwrap();
        assert!(matches!(r.op, Op::Write { handle: 1, ref data } if data == "1234\n"));
        assert!(serde_json::from_str::<Request>(r#"{"id":5,"op":"rm"}"#).is_err());
    }
}
