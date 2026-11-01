//! A local terminal, on a Unix pseudo-terminal.
//!
//! The Linux counterpart of `conpty`: same `Pty` out the top, same reader
//! thread and exit watcher underneath, so nothing above `local::open_pty`
//! learns which one it got. The kernel's pty already speaks VT, so the bytes
//! go into `alacritty_terminal` exactly as an SSH channel's do.
//!
//! The master comes from `posix_openpt` + `grantpt` + `unlockpt` +
//! `ptsname_r` rather than `openpty`: `openpty` lives in libutil on glibc
//! older than 2.34 and the `libc` crate declares it without a `#[link]`, so it
//! links on a current distro and fails on an older one. `ptsname_r` is the one
//! Linux-ism in the set, which is why this module is `target_os = "linux"`
//! rather than `unix`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::pty::{Pty, PtyEvent, PtyMsg, PtyProgram};
use crate::runtime;

/// The child, and whether it has been reaped yet.
///
/// The flag exists so the hangup in `pump` can never hit a recycled pid: a
/// process that has exited but not been waited for is a zombie, and a zombie's
/// pid -- and so its process group id -- cannot be reused. `wait_loop` holds
/// this lock across the reap, and `pump` holds it across the `killpg`, so the
/// signal only goes out while the id still names our process group.
struct Reaped {
    pid: libc::pid_t,
    done: Mutex<bool>,
}

/// Starts `shell` (or `program`) on a new pty and wires it to a `Pty`.
///
/// Synchronous, like `conpty::open`: there is no round trip to wait for, and
/// everything that can fail does so before the first byte moves.
pub fn open(cols: u32, rows: u32, program: PtyProgram, shell: String) -> Result<Pty, String> {
    let master = open_master().map_err(|e| format!("Failed to open a pty: {e}"))?;
    set_size(master.as_raw_fd(), cols, rows);
    let child = spawn(&master, program, &shell)?;

    let master = Arc::new(master);
    let reaped = Arc::new(Reaped {
        pid: child.id() as libc::pid_t,
        done: Mutex::new(false),
    });

    let (tx, rx) = mpsc::unbounded_channel::<PtyMsg>();
    // Bounded like the SSH side, so a tab that stops draining exerts
    // backpressure instead of growing without limit.
    let (event_tx, events) = flume::bounded::<PtyEvent>(512);

    // A dedicated OS thread, not a runtime task: `read` on the master blocks
    // until the child writes. One thread per tab, as on Windows.
    let reader = master.clone();
    let output = event_tx.clone();
    std::thread::Builder::new()
        .name("pulseterm-pty".into())
        .spawn(move || read_loop(reader, output))
        .map_err(|e| format!("Failed to start the reader thread: {e}"))?;

    // And a second one owning the child. Without it every closed tab leaves a
    // zombie behind, and the exit is reported promptly rather than whenever
    // the last holder of the slave lets go of it.
    let watcher = reaped.clone();
    let exit = event_tx.clone();
    std::thread::Builder::new()
        .name("pulseterm-pty-wait".into())
        .spawn(move || wait_loop(child, watcher, exit))
        .map_err(|e| format!("Failed to start the exit watcher: {e}"))?;

    runtime::detach(pump(master, reaped, rx, event_tx));
    Ok(Pty { tx, events })
}

fn open_master() -> io::Result<OwnedFd> {
    unsafe {
        // `O_CLOEXEC` so the master never leaks into another tab's shell; the
        // slave std opens below is close-on-exec too, until `Command` dup2s it
        // onto the child's 0, 1 and 2.
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(fd);
        if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(master)
    }
}

fn slave_path(master: RawFd) -> io::Result<String> {
    let mut buffer = [0 as libc::c_char; 128];
    let rc = unsafe { libc::ptsname_r(master, buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Ok(name.to_string_lossy().into_owned())
}

fn set_size(master: RawFd, cols: u32, rows: u32) {
    // Clamped for the same reason as `TermSize::new`: zero is reachable while
    // a column is laid out, and a 0x0 pty confuses everything that asks.
    let size = libc::winsize {
        ws_row: rows.clamp(1, u16::MAX as u32) as u16,
        ws_col: cols.clamp(1, u16::MAX as u32) as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(master, libc::TIOCSWINSZ, &size);
    }
}

fn spawn(master: &OwnedFd, program: PtyProgram, shell: &str) -> Result<Child, String> {
    let path = slave_path(master.as_raw_fd()).map_err(|e| format!("Failed to name the pty: {e}"))?;
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)
        .map_err(|e| format!("Failed to open {path}: {e}"))?;

    // Where the shell starts: home, unless it was asked to start somewhere.
    let start_in = match &program {
        PtyProgram::ShellIn(path) => Some(std::path::PathBuf::from(path)),
        _ => dirs::home_dir(),
    };
    let mut command = match program {
        // The setting stores a path, so this is the program, not a line to parse.
        PtyProgram::LoginShell | PtyProgram::ShellIn(_) => Command::new(shell),
        // Unlike Windows, Linux always has /bin/sh, so a command line is handed
        // to it to parse rather than split here with a guess at the quoting.
        PtyProgram::Command(line) => {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(line);
            command
        }
    };
    let stdio = |file: &File| -> Result<Stdio, String> {
        file.try_clone()
            .map(Stdio::from)
            .map_err(|e| format!("Failed to share the pty: {e}"))
    };
    command
        .stdin(stdio(&slave)?)
        .stdout(stdio(&slave)?)
        .stderr(stdio(&slave)?)
        // What the SSH side asks the server for, so a local tab and a remote
        // one are the same terminal to every program.
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env_remove("COLUMNS")
        .env_remove("LINES");
    if let Some(dir) = start_in {
        command.current_dir(dir);
    }
    unsafe {
        // Only async-signal-safe calls between fork and exec. A new session,
        // then claim the pty as its controlling terminal: without both, job
        // control is dead -- ctrl-c reaches nothing and /dev/tty fails.
        command.pre_exec(|| {
            // Every signal back to its default. An *ignored* disposition
            // survives exec, and this process may well have some: launched
            // with `nohup`, or with `&` from a script, it ignores SIGHUP,
            // SIGINT and SIGQUIT -- and so would every program in the shell,
            // so ctrl-c reached `sleep` and did nothing, and closing the tab
            // left it running. std already clears the signal *mask*; this is
            // the other half. `signal` is async-signal-safe; SIGKILL and
            // SIGSTOP simply refuse, which is fine.
            for signal in 1..32 {
                libc::signal(signal, libc::SIG_DFL);
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|e| format!("Failed to start {shell}: {e}"))?;
    // Our copy of the slave has to go now. While it stays open the kernel never
    // reports the far side closed, the read never ends, and a dead shell looks
    // exactly like a live one.
    drop(slave);
    Ok(child)
}

/// Blocking reads until the slave side is gone.
///
/// A Linux master reports that as `EIO`, not as a zero-length read -- it is
/// the normal end, not an error worth showing. Unlike ConPTY this read *does*
/// end with the child (once nothing else holds the slave open); `wait_loop`
/// still reports the exit, because a background job can keep the slave alive
/// after the shell itself has gone.
fn read_loop(master: Arc<OwnedFd>, events: flume::Sender<PtyEvent>) {
    // `File` for `Read`'s EINTR retry; `ManuallyDrop` because the fd belongs
    // to the `Arc`, not to this wrapper.
    let mut file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(master.as_raw_fd()) });
    let mut buffer = [0u8; 8 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                // The blocking send is deliberate: it is this thread that waits
                // when the view is behind, never the child's output that is lost.
                if events.send(PtyEvent::Output(buffer[..read].to_vec())).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = events.send(PtyEvent::Closed { error: None });
    drop(master);
}

/// Waits for the child, reaps it, and reports the exit.
///
/// Waits *without* reaping first (`WNOWAIT`), so the reap itself can happen
/// under the lock `pump` checks before signalling -- see `Reaped`. The pause
/// after is the same tenth of a second `conpty::wait_loop` takes, for the
/// output the child wrote on its way out.
fn wait_loop(mut child: Child, reaped: Arc<Reaped>, events: flume::Sender<PtyEvent>) {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                reaped.pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    {
        let mut done = reaped.done.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.wait();
        *done = true;
    }
    std::thread::sleep(Duration::from_millis(100));
    let _ = events.send(PtyEvent::Closed { error: None });
}

/// Writes and resizes, then the hangup. Holding the master here is what keeps
/// the pty alive exactly as long as the `Pty` that owns this queue.
async fn pump(
    master: Arc<OwnedFd>,
    reaped: Arc<Reaped>,
    mut rx: mpsc::UnboundedReceiver<PtyMsg>,
    events: flume::Sender<PtyEvent>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            // Straight out, as on Windows: a keystroke is a few bytes into a
            // buffer the child drains continuously.
            PtyMsg::Data(bytes) => {
                if write_all(master.as_raw_fd(), &bytes).is_err() {
                    break;
                }
            }
            PtyMsg::Resize(cols, rows) => set_size(master.as_raw_fd(), cols, rows),
            PtyMsg::Close => break,
        }
    }
    // The counterpart of `ClosePseudoConsole`. Closing the master would send
    // SIGHUP too, but only once the *last* descriptor goes, and the reader
    // thread holds one of its own -- so the hangup is sent explicitly, to the
    // whole process group `setsid` made (pgid == pid).
    {
        let done = reaped.done.lock().unwrap_or_else(|e| e.into_inner());
        if !*done {
            unsafe {
                libc::killpg(reaped.pid, libc::SIGHUP);
            }
        }
    }
    // A hangup can be lost. bash's line editor swaps its own signal handlers
    // in and out around every prompt, and a SIGHUP landing in that swap is
    // delivered and dropped: the shell sits there alive, idle and orphaned.
    // So the hangup is repeated once, and a shell that still has not gone is
    // killed outright. Each step checks the reap first, under the same lock,
    // so a recycled pid is never signalled.
    let watched = reaped.clone();
    let _ = std::thread::Builder::new()
        .name("pulseterm-pty-hangup".into())
        .spawn(move || {
            for (wait, signal) in [(500, libc::SIGHUP), (2500, libc::SIGKILL)] {
                std::thread::sleep(Duration::from_millis(wait));
                let done = watched.done.lock().unwrap_or_else(|e| e.into_inner());
                if *done {
                    return;
                }
                unsafe {
                    libc::killpg(watched.pid, signal);
                }
            }
        });
    drop(master);
    drop(events);
}

fn write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the pty says until it closes (or the deadline passes).
    fn drain(pty: &Pty, until: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut out = String::new();
        while std::time::Instant::now() < deadline {
            match pty.events.recv_timeout(Duration::from_millis(200)) {
                Ok(PtyEvent::Output(bytes)) => {
                    out.push_str(&String::from_utf8_lossy(&bytes));
                    if out.contains(until) {
                        break;
                    }
                }
                Ok(PtyEvent::Closed { .. }) => break,
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        out
    }

    #[test]
    fn runs_a_command_on_a_real_tty() {
        // `tty` proves the slave is the child's terminal; `stty size` proves
        // the initial size reached it; `ps -o tpgid` proves it is the
        // controlling terminal (job control), via a non-zero foreground group.
        let pty = open(
            100,
            30,
            PtyProgram::Command("tty; stty size; echo DONE".into()),
            String::new(),
        )
        .expect("open");
        let out = drain(&pty, "DONE");
        assert!(out.contains("/dev/pts/"), "{out:?}");
        assert!(out.contains("30 100"), "{out:?}");
    }

    #[test]
    fn a_shell_can_start_in_a_given_directory() {
        // The file tree's "open in terminal": the shell starts there, not home.
        let dir = std::env::temp_dir();
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let pty = open(
            80,
            24,
            PtyProgram::ShellIn(dir.to_string_lossy().into_owned()),
            "/bin/sh".into(),
        )
        .expect("open");
        pty.write(b"pwd; echo M''ARK\n".to_vec());
        let out = drain(&pty, "MARK\r\n");
        assert!(out.contains(&*dir.to_string_lossy()), "{out:?}");
    }

    #[test]
    fn ignored_signals_are_not_inherited() {
        // Ignore SIGHUP here, the way `nohup` would have; the child must still
        // start with nothing ignored. (Harmless for the test process itself.)
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let pty = open(
            80,
            24,
            PtyProgram::Command("grep SigIgn /proc/self/status; echo DONE".into()),
            String::new(),
        )
        .expect("open");
        // Back to the default straight away: tests share one process, and a
        // SIGHUP left ignored here is inherited by every other test's world.
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
        }
        let out = drain(&pty, "DONE");
        assert!(out.contains("SigIgn:\t0000000000000000"), "{out:?}");
    }

    #[test]
    fn resize_reaches_the_child_and_close_hangs_up() {
        let pty = open(80, 24, PtyProgram::LoginShell, "/bin/sh".into()).expect("open");
        pty.resize(132, 43);
        // Give the pump a moment to apply the resize before asking.
        std::thread::sleep(Duration::from_millis(200));
        // Quoted so the typed line, which the tty echoes, does not itself
        // contain the marker.
        pty.write(b"stty size; echo M''ARK\n".to_vec());
        let out = drain(&pty, "MARK\r\n");
        assert!(out.contains("43 132"), "{out:?}");

        // Dropping the Pty must end the shell: the event stream then closes.
        let events = pty.events.clone();
        drop(pty);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut closed = false;
        while std::time::Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(200)) {
                Ok(PtyEvent::Closed { .. }) | Err(flume::RecvTimeoutError::Disconnected) => {
                    closed = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(closed, "shell outlived its Pty");
    }
}
