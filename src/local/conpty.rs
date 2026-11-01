//! A local terminal, on Windows' own pseudo-console.
//!
//! ConPTY is the platform primitive here: it gives a child process a real
//! console to talk to, translates its console API calls into VT, and hands us
//! the bytes -- which is exactly what `alacritty_terminal` already parses on
//! the SSH side. Nothing about the emulator, the grid or the key encoder
//! changes for a local tab; only where the bytes come from.
//!
//! The pieces Windows requires, in order: two pipes, `CreatePseudoConsole` over
//! their far ends, a process-thread attribute list carrying the resulting
//! `HPCON`, then `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT`. Miss the
//! attribute and the child gets a plain console of its own, detached from the
//! pipes, and the terminal stays blank.

use std::ffi::c_void;
use std::sync::Arc;

use tokio::sync::mpsc;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTUPINFOEXW, STARTUPINFOW,
};

use crate::pty::{Pty, PtyEvent, PtyMsg, PtyProgram};
use crate::runtime;

/// Windows handles are `isize` underneath and safe to use from another thread;
/// the binding just doesn't say so. The reader thread needs the read end and
/// the pump task needs the write end, so each is carried across in one of these
/// rather than by making the whole struct `unsafe impl Send`.
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// Same for the console handle, which the resize path uses.
struct SendPseudoConsole(HPCON);
unsafe impl Send for SendPseudoConsole {}
unsafe impl Sync for SendPseudoConsole {}

/// Everything the pty owns, freed in one place. Dropping this closes the
/// console -- which is how the child shell is asked to exit -- and then the
/// handles, so closing a tab really does end the shell behind it.
struct Console {
    console: SendPseudoConsole,
    input_write: HANDLE,
    output_read: HANDLE,
    process: HANDLE,
    thread: HANDLE,
}

// Windows handles are process-wide and safe to use from any thread; the
// binding just models them as raw pointers, which are neither `Send` nor
// `Sync` by default. The pump task holds this across an await and the reader
// thread holds a handle of its own, so both facts have to be stated.
unsafe impl Send for Console {}
unsafe impl Sync for Console {}

impl Drop for Console {
    fn drop(&mut self) {
        unsafe {
            // Closing the console sends the child the equivalent of a hangup and
            // lets it flush; the pipes go afterwards, or the flush has nowhere
            // to land and the last line of output is lost.
            ClosePseudoConsole(self.console.0);
            let _ = CloseHandle(self.input_write);
            let _ = CloseHandle(self.output_read);
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

/// Starts `shell` under a new pseudo-console and wires it to a `Pty`.
///
/// Synchronous, unlike the SSH opener: there is no round trip to wait for, and
/// everything that can fail does so before the first byte moves.
pub fn open(cols: u32, rows: u32, program: PtyProgram, shell: String) -> Result<Pty, String> {
    let command = match program {
        PtyProgram::LoginShell => shell,
        // A command replaces the shell rather than being fed to it: quoting a
        // command line through an unknown shell is a guess, and the caller
        // already knows exactly what it wants run.
        PtyProgram::Command(command) => command,
    };

    let console = spawn(cols.max(1) as i16, rows.max(1) as i16, &command)?;
    let console = Arc::new(console);

    let (tx, rx) = mpsc::unbounded_channel::<PtyMsg>();
    // Bounded like the SSH side, so a tab that stops draining exerts
    // backpressure instead of growing without limit.
    let (event_tx, events) = flume::bounded::<PtyEvent>(512);

    // A dedicated OS thread, not a runtime task: `ReadFile` on a pipe blocks
    // until the child writes, and there is no async form of it that works on a
    // ConPTY pipe. One thread per terminal tab is the honest cost.
    let reader = SendHandle(console.output_read);
    let output = event_tx.clone();
    std::thread::Builder::new()
        .name("pulseterm-conpty".into())
        .spawn(move || read_loop(reader, output))
        .map_err(|e| format!("Failed to start the reader thread: {e}"))?;

    // And a second one, waiting on the child itself. See `wait_loop`: the
    // read never ends on its own, so without this a dead shell looks live.
    let child = SendHandle(console.process);
    let exit = event_tx.clone();
    std::thread::Builder::new()
        .name("pulseterm-conpty-wait".into())
        .spawn(move || wait_loop(child, exit))
        .map_err(|e| format!("Failed to start the exit watcher: {e}"))?;

    runtime::detach(pump(console, rx, event_tx));
    Ok(Pty { tx, events })
}

/// Blocking reads until the pipe is closed at this end.
///
/// **It does not end when the child does.** That is the trap in ConPTY and it
/// is the opposite of a Unix pty: the write end of the output pipe belongs to
/// the console host, not to the child, and the host holds it until
/// `ClosePseudoConsole`. So a shell can exit, its process be gone, and this
/// read still block -- which is exactly how a dead local shell went on looking
/// like a live one, with no "[session ended]" and a header that lied.
/// `wait_loop` is what notices; this loop ends a moment later, when dropping
/// the `Pty` drops the `Console` and closes the console for real.
fn read_loop(handle: SendHandle, events: flume::Sender<PtyEvent>) {
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let mut read = 0u32;
        let ok = unsafe { ReadFile(handle.0, Some(&mut buffer), Some(&mut read), None) };
        if ok.is_err() || read == 0 {
            break;
        }
        // The blocking send is deliberate: it is this thread that waits when
        // the view is behind, never the child's output that is dropped.
        if events
            .send(PtyEvent::Output(buffer[..read as usize].to_vec()))
            .is_err()
        {
            break;
        }
    }
    let _ = events.send(PtyEvent::Closed { error: None });
}

/// Waits on the child process and reports its exit, because nothing else will
/// -- see `read_loop`.
///
/// The pause afterwards is for the output the child wrote on its way out: the
/// process handle signals as soon as it is gone, and the reader thread may
/// still be a buffer or two behind. A tenth of a second costs nothing on a
/// path that runs once per window and keeps the last line of a build, or of a
/// `git log`, from being cut off.
fn wait_loop(process: SendHandle, events: flume::Sender<PtyEvent>) {
    unsafe {
        WaitForSingleObject(process.0, INFINITE);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = events.send(PtyEvent::Closed { error: None });
}

/// Writes and resizes. Held on the transport runtime to mirror the SSH pty, and
/// because holding the `Console` here is what keeps the child alive exactly as
/// long as the `Pty` that owns this queue.
async fn pump(
    console: Arc<Console>,
    mut rx: mpsc::UnboundedReceiver<PtyMsg>,
    events: flume::Sender<PtyEvent>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            // Writes go straight out rather than through `spawn_blocking`: a
            // keystroke is a handful of bytes into a pipe the console drains
            // continuously, so this does not block in practice.
            PtyMsg::Data(bytes) => {
                let mut written = 0u32;
                let ok = unsafe {
                    WriteFile(console.input_write, Some(&bytes), Some(&mut written), None)
                };
                if ok.is_err() {
                    break;
                }
            }
            PtyMsg::Resize(cols, rows) => unsafe {
                let _ = ResizePseudoConsole(
                    console.console.0,
                    COORD {
                        X: cols.max(1) as i16,
                        Y: rows.max(1) as i16,
                    },
                );
            },
            PtyMsg::Close => break,
        }
    }
    // Dropping the console here ends the child; the reader thread then sees EOF
    // and emits `Closed`, so this path stays silent rather than racing it.
    drop(console);
    drop(events);
}

fn spawn(cols: i16, rows: i16, command: &str) -> Result<Console, String> {
    unsafe {
        let mut input_read = HANDLE::default();
        let mut input_write = HANDLE::default();
        let mut output_read = HANDLE::default();
        let mut output_write = HANDLE::default();
        let attributes = SECURITY_ATTRIBUTES::default();

        CreatePipe(&mut input_read, &mut input_write, Some(&attributes), 0)
            .map_err(|e| format!("Failed to create the input pipe: {e}"))?;
        CreatePipe(&mut output_read, &mut output_write, Some(&attributes), 0)
            .map_err(|e| format!("Failed to create the output pipe: {e}"))?;

        let console = CreatePseudoConsole(COORD { X: cols, Y: rows }, input_read, output_write, 0)
            .map_err(|e| format!("Failed to create the console: {e}"))?;

        // The console owns the far ends now. Leaving them open here would keep
        // the pipes alive after the child exits, and the reader would never see
        // EOF -- the tab would look like a shell that never ends.
        let _ = CloseHandle(input_read);
        let _ = CloseHandle(output_write);

        let mut startup = STARTUPINFOEXW {
            StartupInfo: STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        // Sized by asking: the call is *expected* to fail the first time, with
        // the required size written back.
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut attribute_buffer = vec![0u8; size];
        let attribute_list =
            LPPROC_THREAD_ATTRIBUTE_LIST(attribute_buffer.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(attribute_list), 1, None, &mut size)
            .map_err(|e| format!("Failed to set up the process attributes: {e}"))?;
        startup.lpAttributeList = attribute_list;

        UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(console.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(|e| format!("Failed to attach the console: {e}"))?;

        // `CreateProcessW` may write into the command line, so it gets a
        // writable copy of its own.
        let mut command_line: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let working_dir = dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let working_dir_w: Vec<u16> = working_dir
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut process = PROCESS_INFORMATION::default();
        let created = CreateProcessW(
            None,
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR(working_dir_w.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        );
        DeleteProcThreadAttributeList(attribute_list);

        if let Err(error) = created {
            ClosePseudoConsole(console);
            let _ = CloseHandle(input_write);
            let _ = CloseHandle(output_read);
            return Err(format!("Failed to start {command}: {error}"));
        }

        Ok(Console {
            console: SendPseudoConsole(console),
            input_write,
            output_read,
            process: process.hProcess,
            thread: process.hThread,
        })
    }
}
