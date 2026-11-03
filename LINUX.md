# PulseTerm on Linux

The Linux port's handover and its status. It began as a plan written on the
Windows machine, where nothing could be compiled for Linux; the port has since
been built and tested on Linux. **Status** and **Windows: check this** below
are the current picture. The numbered sections after them (§0–§8) are the
original plan, kept for the reasoning and the traps -- where they disagree
with Status, Status wins.

Read `CLAUDE.md` first; it is the architecture document and it still governs.
This file only covers what differs.

---

## Status (2026-11-01, on a Fedora 44 box)

The port has started. `cargo check` is clean on `x86_64-unknown-linux-gnu`;
what follows is what is done, and what the machine said that this document had
guessed differently.

**Done**

* §3.1 — `src/local/unix_pty.rs`, behind `local::open_pty`, which is now the
  one entry point `host.rs` calls (ConPTY on Windows, this on Linux, an honest
  error elsewhere). All four traps are handled as described, plus one more:
  the hangup and the reap share a lock, and the child is waited for with
  `WNOWAIT` first, so `killpg` can never reach a recycled pid.
* §3.2 — shell discovery: `$SHELL`, then `/etc/shells`, then a short fallback,
  deduplicated by canonical path (Fedora lists `/bin/bash` *and*
  `/usr/bin/bash`, which are one file).
* §3.3 — client-side decorations and `app_id` in `main.rs`. The caption's
  Linux arm (`ui/window_controls.rs`) is **not** the one-liner this file
  expected: starting the move on mouse-down hands the pointer to the
  compositor and swallows the release, so a double-click is never seen. It
  now follows Zed -- the press arms, the first drag starts the move, a
  double-click zooms, right-click opens the compositor's window menu.
  Resize edges are `src/ui/decorations.rs`: a 7px transparent margin via
  `set_client_inset`, none on tiled edges, a hairline frame, no shadow and no
  radius.
* §3.4 — a Linux `PREFERRED` font list.
* §3.5 — `libc` for Linux; `windows_subsystem` gated on Windows.
  `embed-resource` was **left** unconditional: `build.rs` decides by
  `CARGO_CFG_TARGET_OS` at run time, and gating the crate by host would break
  cross-compiling to Windows from Linux.
* §3.6 — the doc comments.

**What the source said that this document had wrong**

* **§3.4's startup risk is probably not one.** On Linux gpui's
  `all_font_names()` iterates cosmic-text's in-memory font database, which
  gpui filled during its own platform init -- before our callback runs. No
  fontconfig scan happens in `theme::apply`. Debug builds print
  `[startup] theme::apply …` on stderr so the number can be confirmed rather
  than argued about.
* **§3.8: blur is not a no-op everywhere.** gpui's Wayland backend binds KDE's
  `org_kde_kwin_blur` when the compositor offers it, so "Blur behind" works on
  Plasma. The switch stays.
* **The ground moves on Linux.** A client-decorated Wayland surface is
  transparent (gpui forces it, for the margin), so `Root` cannot paint the
  window ground there -- it would fill the resize margin. On Linux
  `apply_component_theme` clears `Root`'s background and `ui::decorations`
  paints the ground inside the margin instead, once, and only when opaque.

**Tested by driving the real app** (niri on Wayland, input injected with
`ydotool`, checked with niri screenshots and `ps`/`/proc`):

* It builds and links **without** the `-devel` packages in §4 -- gpui loads
  those libraries at run time. A C compiler is all the build needed here.
* Local terminal: a prompt; `tty` is a `/dev/pts`; the size matches the column
  and follows a width change (56 → 75 columns); `TERM`/`COLORTERM` right; bash
  is a session leader holding the foreground group; ctrl-c interrupts `sleep`
  (exit 130); `vi` draws, and arrows work in it; `exit` marks the header
  `-- exited` within a second; no zombies after closing; a local session
  resumes at launch.
* Caption: dragging the bar moves the window exactly as far as the pointer;
  double-click maximises; the side bar's restore button restores; the close
  button closes. Exactly one titlebar.
* Resize margin: dragging it resizes; a press just inside the content does not.

**Found by that testing, and fixed**

* **Ignored signals leaked into the shell.** Started from `nohup` or a
  script's `&`, PulseTerm itself ignores SIGHUP/SIGINT/SIGQUIT, and ignored
  dispositions survive exec -- so ctrl-c did nothing to `sleep`, and closing a
  tab left it running. `unix_pty::spawn` now resets every signal to its
  default before exec (std only clears the mask). There is a test.
* **A hangup could be lost.** bash's line editor swaps signal handlers
  around every prompt, and a SIGHUP landing in that swap is dropped -- the
  shell stayed alive and orphaned, about one close in seven. A
  `pulseterm-pty-hangup` thread now repeats SIGHUP after 0.5 s and sends
  SIGKILL after 3 s, each step checking the reap under the same lock first.
* **The margin was measured against the wrong size.** On Wayland
  `window_bounds()` is the window *less* the inset, so every zone was shifted
  inward: the cursor hitbox missed the right and bottom margins, and a press in
  the last 14px of the content started a resize. `decorations` now measures
  with `viewport_size()`, the whole surface.

**Open, needs a person or another compositor**

* **niri marks a floating window's edges as tiled** if it was tiled first, so
  gpui adds no margin and there is nothing to grab; niri's own Mod+drag still
  resizes. After some maximise/restore sequences it reports free edges and the
  margin works. This is niri's state, faithfully followed; check GNOME or KDE,
  which do not tile normal windows, before changing anything here.
* **The resize cursor in the margin** was not seen after the size fix, because
  niri would not report free edges again in the same run. The seam's
  `col-resize` cursor does show, so gpui's cursor path works on niri.
* **The home screen's restore button did nothing** on a window that niri had
  maximised from floating; the side bar's identical button had worked minutes
  before. Both call `window_controls::toggle_maximize`. Unexplained.
* The window has no title as far as niri can tell (`title: null`).
* §3.7 (portal checks), §8 (packaging beyond `packaging/linux/`: the
  `.desktop` file and `install.sh`, a per-user install that makes desktops
  list PulseTerm as a terminal -- no distro package yet).
* The Windows arm of every file touched here was edited without a Windows
  compiler. The changes there are small (new no-op functions in
  `window_controls`' Windows `platform`, cfg gates), but the first Windows
  build after this should be watched.

---

## The remote desktop helper

`remote/` (`pulseterm-remote`) is a second program, built only with
`cargo build -p pulseterm-remote`. It streams from Linux only so far: its
full keyboard takeover is an evdev grab. It does **build and open on
Windows** too (2026-11-03; `REMOTE_DESKTOP.md`, *Building on Windows*), with
a Windows font lookup in place of `fc-match`. Building it needs the moonlight-common-c
submodule and system packages -- see `REMOTE_DESKTOP.md` (status, packages,
and the `dnf swap` trap). PulseTerm itself does not depend on it.

## Windows: check this on the next Windows build

Everything since the Linux port began was written on Linux (Fedora,
niri/Wayland), where no Windows compiler exists. **It has now been built and
run on Windows once (2026-11-03)**; the checklist at the end of this section
records what that run found. Most of it is shared code that behaves the same
on both; the list says which parts are not.

**Edited Windows-only code, unverified -- check these compile first**

* `src/local/conpty.rs` -- `PtyProgram::ShellIn(path)` (the file tree's
  "open in terminal"): `open` passes the path to `spawn(.., start_in)`, which
  uses it as `CreateProcessW`'s working directory instead of home.
* `src/ui/window_controls.rs`, Windows `platform` module -- four new no-op
  functions (`caption_drag`, `caption_release`, `caption_click`,
  `caption_menu`) so the shared wrappers resolve. The `WM_SYSCOMMAND` drag,
  maximise and double-click logic is untouched.
* `src/local/mod.rs` -- `conpty` is now a private module behind
  `local::open_pty`, which `host.rs` calls; the Windows shell discovery is
  unchanged, the Unix one is `cfg`'d out.
* `src/main.rs` -- `windows_subsystem` is now gated on Windows as well as
  release; `window_decorations` is set on Linux only; `app_id` is set
  everywhere (a no-op off Linux).

**Linux-only, fenced off -- should not affect Windows**

`local::unix_pty`, `ui::decorations` (a pass-through off Linux), the
`libc` dependency, `home::host_name` (Windows reads `COMPUTERNAME`), and
`theme::apply_component_theme` clearing `Root`'s background (Linux only;
Windows keeps painting the ground there as before).

**Shared changes made on Linux -- check they look and behave the same**

* Window headers: `02` / `02.2`, width share or `STACKED`, `FOCUS` tag, `x`
  with a header-height hit area that does not start a drag; no hairline
  under the header; no red line beside the focused column.
* Columns: positioned by fraction; last column stretches to fill; none wider
  than *Widest column* (default 90%) when there are several; lone column
  full; seam bands 11px and occluding; the seam left of the focused column
  resizes the focused column; stacked windows resize at their boundary;
  windows drag by their header to stack or split.
* File tree docked left (`^⇧B`), header at window-header height with
  "open in terminal", `+`/`-` rows, new context menu.
* Nav bar Lucide icons with shortcut tooltips; clock goes home; `IDLE` when
  disconnected.
* Home screen redesign (START / RESUME / TIME), dials centred.
* Settings redesign (sections, segmented controls, steppers, own font list,
  Appearance palette list with *System*), Keys page with rebinding saved to
  `keys.json` in the data directory (`%APPDATA%/PulseTerm`).
* Anything opened with no session starts a local one (ConPTY on Windows).
* Keyboard focus is pulled back into the workspace whenever it escapes.

**Checklist for the first Windows run** -- run 2026-11-03 on Windows 11
(MSVC toolchain, see CLAUDE.md), by driving the real app

- [x] `cargo check` clean; `cargo build --release` builds (36 MB, ~10 min).
      Startup to a visible window, release: **~700 ms warm, 1231 ms cold** --
      the same as before the port (720 / 1258), so nothing added on Linux
      touched the first frame.
- [x] `cargo test` -- 37 passed, as predicted (38 after the Windows-path test
      added in the same run).
- [x] Drag the nav bar: the window moves by exactly the drag (the OS move
      loop); minimise / maximise / restore / close buttons act.
      **Double-click did nothing -- fixed**: the press posted `SC_MAXIMIZE`
      while gpui still held the mouse capture and Windows dropped it; it now
      toggles on the release (`window_controls`, `ZOOM_ON_RELEASE`).
- [x] Exactly one titlebar (ours).
- [x] Local terminal opens (ConPTY, PowerShell); typing, colour, pty size,
      Ctrl+C in `ping -t`; `exit` marks the header `-- exited`.
- [x] File tree -> terminal icon opens a shell in that folder.
- [x] Settings -> Appearance -> *System* resolves to Ink in Windows dark mode.
      Live switching was not tried (it means changing the OS setting).
- [x] Keys page: recorded `^⇧Y` for New column, it worked and `^⇧T` stopped;
      Restore defaults put it back.
- [~] Translucent window: the font list stays opaque; the ground looked even,
      but against a busy desktop one-layer-vs-two could not be judged closely.
- [ ] The remote desktop icon on an SSH session -- not tried (no host
      password here). Hidden on local sessions, as designed.

**Found on Windows in that run, and fixed**

* **Paths split on `/` only.** A local Windows path's file name was the whole
  path: editor windows were titled `D:\Projects\...\build.rs`, and the file
  tree's **paste pasted beside the original** (`build.rs (1)`) instead of into
  the folder, because the whole absolute path reached `Path::join`. `editor`,
  `preview` and `explorer` now split on both separators; a test covers it.
* **Clicking the caption or the bar took the keyboard from the shell.** The
  root tracks the workspace focus handle, so a press there focused the root,
  and the focus keeper accepted that. Keys typed after dragging the window
  were lost. The keeper now also corrects focus on the bare root. Not
  Windows-specific.
* **The overview ignored escape, enter and the arrows.** It focused the root,
  but its `Overview` key context is on a child, and gpui looks for bindings
  upward from the focused element. It has its own focus handle now, and
  `focus_active_item` returns to it while it is open (dismissing the command
  bar over it used to strand focus on an undrawn terminal). Not
  Windows-specific.
* **The close dialog called the local session "still connected".** It now
  names only the SSH hosts the guard counts (`live_hosts`).

**Found, not fixed**

* `--working-directory` with `-e` fails (`cd '...' && exec ...` is POSIX).
  `-e` alone and `--working-directory` alone both work. Needs a
  command-in-a-directory `PtyProgram`.
* Home's *This computer* says `cmd` while the shell is PowerShell: with no
  shell set it reads `%ComSpec%`, while `local::default_shell` takes the first
  of pwsh / powershell / cmd that exists. Matching it costs a few file checks
  on the first frame.
* Pressing a partly hidden window's header to drag it focuses it, the strip
  slides to show it, and the drop lands under a pointer that has moved.
* Cosmetic: the Plugins folder path runs into `0 INSTALLED`; at 16px the Keys
  page wraps chord chips; a symbol font (Marlett) is listed and draws its own
  name as icons; the command bar's password field shows "type to filter".

---

## 0. Read this first

Four rules that outrank anything convenient. Breaking one is worse than
shipping without Linux support.

1. **The app must open instantly.** Nothing on the path to the first frame may
   read more than one small file, and nothing may wait on a socket, a shell or
   a scan. Work that needs the disk starts on `cx.background_executor()` and
   swaps its result in when it lands. Windows measures **720 ms warm / 1258 ms
   cold** to a visible window in release, of which our own code is under 10 ms.
   Linux must not be worse. Section 5 named font enumeration as the one risk
   on Linux; Status found it is probably not one (gpui fills the font database
   before our callback), and debug builds print `[startup] theme::apply` to
   prove it either way.
2. **No secret is ever written to disk** — not passwords, not passphrases, not
   in `profiles.json`, `session.json` or anywhere else. The command bar holds a
   secret in memory only until it is handed to `russh`, then clears the field.
3. **There is exactly one navigation bar**, and nothing may add a second. On
   Linux this is not just taste — see §3.3, where the default is to get a
   system titlebar *on top of* ours.
4. **App shortcuts are all on ctrl+shift.** The only exceptions are the zoom
   keys (`ctrl-=`, `ctrl--`, `ctrl-0`). Anything on plain ctrl is stolen from
   the remote shell.

---

## 1. What PulseTerm is

A GPU-rendered SSH terminal built on [gpui](https://gpui.rs) (Zed's UI
framework). It talks to a stock `sshd` — terminal sessions are SSH PTY
channels, so there is no remote-side agent, server or install step. It also
runs against the local machine ("use this computer"), which is the half this
document is about.

The UI is deliberately TUI-shaped: everything monospace on a fixed cell grid,
hairline borders instead of shadows and radii, no corner radius anywhere, and
a workspace that is one unbounded horizontal **strip of columns** (niri's
model, not Zed's split tree).

### Links

| what | where |
|---|---|
| UI/UX design | <https://www.figma.com/design/hjKLOjcA4kD4PhbifqRold/PulseTerm> |
| UI framework | <https://gpui.rs> — crates.io `gpui` 0.2.2 |
| Widgets / code editor | `gpui-component` **pinned `=0.5.1`** (last release on crates.io gpui 0.2.2) |
| Terminal emulation | [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) 0.26 |
| SSH | [`russh`](https://github.com/Eugeny/russh) 0.62, `ring` backend |
| Icons | [`icons`](https://rust-ui.com) 0.18 (Lucide), `default-features = false` |
| Predecessor | `D:\Projects\RemoteTTY` — Tauri + WebView2 + xterm.js. Still the reference for the two unported behaviours (detach-window-to-OS-window, TTY1-4 `conspy` shortcuts) |
| Code knowledge graph | `graphify extract . --code-only --cargo` then `graphify cluster-only .` → `graphify-out/graph.html` |

### The design language, from the Figma file

Frame 01 is the token sheet. These are the exact values in `src/theme.rs`
(`presets()`), and Ink is what a fresh install opens on.

**Ink (dark)** — ground `#121212`, recessed `#191919`, raised `#202020`,
fg `#EDEAE3`, fg2 `#9A958B`, fg3 `#5E5A52`, hairline `#2B2B2B`,
accent `#FF2E57`

**Paper (light)** — ground `#EDEAE3`, recessed `#E3DFD5`, raised `#F5F3EE`,
ink `#121212`, ink2 `#565249`, ink3 `#948F83`, hairline `#C9C4B8`,
accent `#E3103C`

Paper is the same drawing with the ramp inverted, which is why nothing in the
UI asks which of the two is in use.

**Fixed metrics** (all of them in `src/ui/kit.rs`, derived from the type rather
than hard-coded, so the chrome keeps its proportion when the font size
changes): navigation bar `line_height + 26`, window header `line_height + 8`,
list row `line_height + 10`, hairline 1px, **corner radius 0 — always**,
**shadow none — ever**, 8px grid rhythm.

**Type scale**: `micro` (`font_size * 0.72`, tracked out at 0.18em, used for
every uppercase label) and the theme's own `font_size` for everything else,
terminal grid included -- plus `head()` and `display()`, the heading and
numeral sizes that only home and Settings use. gpui has no letter spacing, so the
tracking is spelled into the string (`kit::spaced` / `kit::label`).

Frames worth opening before changing any screen: **05 Overview**,
**07 Command bar**, **09 Files & editor as windows**, **10 Ink theme**.

---

## 2. What already works on Linux, unchanged

Checked by reading, not by compiling. This is the good news and it is most of
the app.

* **gpui itself.** Its `default` features already include `wayland` and `x11`
  (plus `font-kit`), so `Cargo.toml` needs no feature change for the renderer.
  The Linux backend is blade-graphics over Vulkan.
* **Everything above the transport.** `src/host.rs` is the seam: a `Host` is
  either an SSH connection or this computer, and terminals, explorer, editor,
  preview and the session file take one and never learn which. None of that is
  platform-aware.
* **`src/local/fs.rs`** is already portable — `std::path`, `tokio::fs`,
  `dirs::home_dir()`. Its `join`/`parent` use `Path::join`, which wants the
  platform separator, which on Linux is what we want. Only its header comment
  says "Windows paths"; the code does not.
* **`src/store.rs`** uses `dirs::data_dir()`, so the stores land in
  `~/.local/share/PulseTerm/` with no change. (Several doc comments say
  `%APPDATA%/PulseTerm` — see §3.6.)
* **`src/ui/window_controls.rs`** already has a
  `#[cfg(not(target_os = "windows"))] mod platform` that calls
  `window.start_window_move()` and `window.zoom_window()` — both of which gpui
  *does* implement on X11 and Wayland. The elaborate `WM_SYSCOMMAND` work is
  the Windows arm only. **This file should need no edit**; see §3.3 for what
  does.
* **`build.rs`** already returns early for non-Windows targets after writing
  `logo.png`, which `ui::icon` includes on every platform.
* **The `windows` and `raw-window-handle` crates** are already behind
  `[target.'cfg(target_os = "windows")'.dependencies]`.
* **Dependencies with no platform story**: `alacritty_terminal`, `russh`
  (pure-Rust `ring` backend, deliberately not `aws-lc-rs`), `russh-sftp`,
  `tokio`, `flume`, `serde`, `chrono`, `icons`, all 23 tree-sitter grammars.
  The grammars compile C, so a C compiler is required (§4).
* **The emulator, the grid element, the key encoder, the mouse protocol.**
  Nothing in `src/terminal/` is platform-aware. A local tab on Linux feeds the
  same bytes into the same `alacritty_terminal` as an SSH tab.
* **`src/ui/motion.rs`** and the strip's glides — pure time arithmetic.

---

## 3. The work

### 3.1 Local terminal: a Unix pty — the big one

`src/local/conpty.rs` (305 lines) is Windows' pseudo-console and has no Linux
meaning. It is called from exactly one place:

```
src/host.rs:279   Kind::Local => crate::local::conpty::open(cols, rows, program, shell),
```

**Proposed layout**

```
src/local/mod.rs
    #[cfg(target_os = "windows")] mod conpty;
    #[cfg(target_os = "linux")]   mod unix_pty;
    pub fn open_pty(cols: u32, rows: u32, program: PtyProgram, shell: String)
        -> Result<Pty, String>
```

with `host.rs` calling `crate::local::open_pty(...)` and the fallback arm
returning `Err("Local terminals are not supported on this platform yet.")` so
macOS and the BSDs still *build* and still do SSH, with one honest sentence
where a shell would be. Also update the `src/pty.rs` header comment, which
names `local::conpty` as the other implementation.

**The primitive.** Use `posix_openpt` + `grantpt` + `unlockpt` + `ptsname_r`,
not `openpty`. `openpty` lives in **libutil** on glibc older than 2.34 and the
`libc` crate declares it in a plain `extern "C"` block with no
`#[link(name = "util")]` — so it links on a current distro and fails on
Ubuntu 20.04. The `posix_openpt` family is in libc proper on every glibc and
on musl. `ptsname_r` is the one Linux/glibc-ism in that set, which is why the
module is cfg'd `target_os = "linux"` rather than `unix`.

**Spawn with `std::process::Command`, not a bare `fork`.** The process already
has tokio threads and gpui threads running by the time a tab opens, and only
async-signal-safe calls are legal between `fork` and `exec`. `Command` does
the dangerous part correctly and reports exec failure back through its own
pipe; `CommandExt::pre_exec` is where the two calls that must happen in the
child go.

```rust
// sketch — not compiled
let master: OwnedFd = posix_openpt(O_RDWR | O_NOCTTY)?;
grantpt(&master)?;
unlockpt(&master)?;
let slave_path = ptsname_r(&master)?;            // "/dev/pts/N"
set_winsize(&master, cols, rows)?;               // TIOCSWINSZ

let slave = OpenOptions::new().read(true).write(true).open(&slave_path)?;

let mut command = match program {
    // The settings store a path, so this is the program, not a command line.
    PtyProgram::LoginShell => Command::new(&shell),
    // Unlike Windows, where there is no shell you can count on, Linux always
    // has /bin/sh -- so a command can be handed to it to parse instead of
    // being split here with a guess at the quoting rules.
    PtyProgram::Command(line) => { let mut c = Command::new("/bin/sh");
                                   c.arg("-c").arg(line); c }
};
command
    .stdin(slave.try_clone()?)
    .stdout(slave.try_clone()?)
    .stderr(slave.try_clone()?)
    // Matches what the SSH side asks the server for (`ssh/pty.rs:32`), so a
    // local tab and a remote one are the same terminal to every program.
    .env("TERM", "xterm-256color")
    .env("COLORTERM", "truecolor")
    .env_remove("COLUMNS")
    .env_remove("LINES");
unsafe {
    command.pre_exec(|| {
        // A new session, then claim the pty as its controlling terminal.
        // Without both, job control is dead: ctrl-c reaches nothing, and
        // anything that opens /dev/tty fails.
        if libc::setsid() < 0 { return Err(io::Error::last_os_error()); }
        if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 { return Err(io::Error::last_os_error()); }
        Ok(())
    });
}
let child = command.spawn().map_err(...)?;
drop(slave);   // ← see below
```

**Four traps, in the order they will bite:**

1. **Drop our copy of the slave fd after spawning.** The child has its own
   three. If this end stays open, the kernel never signals the master, the
   read never ends, and a dead shell looks exactly like a live one — which is
   the same symptom ConPTY has for the opposite reason (see the "A ConPTY read
   does not end when the child does" note in `CLAUDE.md`; on Linux it *does*
   end, and this is why).
2. **Hang up by signalling the process group, not by closing the master.**
   Closing the last master fd does send SIGHUP, but the reader thread needs a
   descriptor of its own, so "last" is not under one function's control.
   `setsid()` makes the child a session and process-group leader with
   `pgid == pid`, so the teardown path is `killpg(pid, SIGHUP)` and then drop
   the master. This is the direct counterpart of `ClosePseudoConsole` in
   `conpty.rs`, and it belongs in the same place — the end of `pump`, when the
   `Pty` has been dropped.
3. **Reads are a dedicated OS thread.** `read()` on the master blocks and has
   no async form worth having here. Mirror `conpty::read_loop` exactly,
   including the *blocking* send into the `flume` channel: it is that thread
   that waits when the view is behind, never the child's output that is
   dropped. End the loop on `Ok(0)` and on `EIO` (which is what a Linux master
   returns once the last slave closes — it is normal, not an error to report).
   Retry `EINTR`.
4. **Reap the child.** A second thread owning the `Child` and calling
   `child.wait()` mirrors `conpty::wait_loop`, including the 100 ms pause
   before sending `PtyEvent::Closed` — the process is gone the instant it
   exits but the reader may still be a buffer behind, and that tenth of a
   second is what keeps the last line of a build from being cut off. Without
   this thread every closed tab leaves a zombie.

**Resize** is `ioctl(master, TIOCSWINSZ, &winsize)` with
`ws_col`/`ws_row` clamped to at least 1 (same clamp reason as
`TermSize::new`). **Writes** go straight out with `write(2)` in a loop over
short writes, from the `pump` task, exactly as `conpty::pump` does.

The `Pty`/`PtyEvent`/`PtyMsg` contract in `src/pty.rs` does not change, which
is the whole point of that file.

### 3.2 Shell discovery

`src/local/mod.rs` probes a hard-coded list of Windows paths. Keep the shape —
**probe, never list**, because offering a shell that is not installed produces
a tab that opens and immediately dies, which is a worse answer than not
offering it — and swap the candidates:

* `$SHELL` first, if it is set and the file exists. It is the user's own
  answer and no probe beats it.
* Then `/etc/shells`, which is the distro's own list, filtered by `exists()`.
* Then a short fallback list for the labels: `/bin/bash` "Bash",
  `/usr/bin/zsh` "Zsh", `/usr/bin/fish` "Fish", `/bin/sh` "sh".
* `default_shell()` falls back to `/bin/sh`, the one thing POSIX guarantees —
  the role `cmd.exe` plays on Windows.

Label them by file stem where the list came from `/etc/shells`. Deduplicate by
resolved path, or `$SHELL` and `/etc/shells` will offer bash twice.

`resolve_shell(configured)` already does the right thing — it falls back when
a configured path has since been uninstalled — and needs no change.

### 3.3 Window decorations, app id, and the caption

**This is the one that will look broken first.** `WindowOptions` defaults
`window_decorations` to `None`, and gpui's Linux backend reads that as
`WindowDecorations::Server` (`gpui-0.2.2/src/window.rs:997`). So the window
gets the desktop's own titlebar **above** our navigation bar — two captions,
which rule 3 of §0 exists to forbid.

In `src/main.rs`, on the `WindowOptions`:

```rust
// Only Linux reads this; the other backends resolve `request_decorations`
// to nothing, so it costs a cfg rather than earning one.
#[cfg(target_os = "linux")]
window_decorations: Some(gpui::WindowDecorations::Client),
// Unconditional: `PlatformWindow::set_app_id` has an empty default body
// (`platform.rs:541`), so this is a no-op everywhere but Linux.
app_id: Some("pulseterm".to_string()),
```

* `appears_transparent` on `TitlebarOptions` is a **no-op on Linux** — the
  Linux backend never reads it. Decorations are the knob.
* `app_id` sets the Wayland app id and the X11 `WM_CLASS`. Without it the
  window has no icon in the dock and does not group — and it must match the
  `.desktop` file's basename (§8).
* With client-side decorations the window has no resize border unless one is
  declared: `window.set_client_inset(px(N))` plus `window.window_decorations()`
  → `Decorations::Client { tiling }` to know which edges are tiled (a snapped
  or maximised window must not draw a resize edge where there is none). This
  wants a small Linux arm somewhere near the nav bar; the bar is already the
  caption on every platform.
* `window.start_window_move()` and `window.zoom_window()` are implemented on
  X11 and Wayland, so `ui/window_controls.rs`'s non-Windows arm already works
  and the Win32 double-click rule does not apply — the compositor handles the
  double-click when the drag is its own.
* `window.is_maximized()` drives the maximise icon and should be fine, but it
  is worth an eye: under Wayland a tiled window is not necessarily "maximized".

### 3.4 Fonts — and this is a startup item

`src/theme.rs:585`:

```rust
const PREFERRED: &[&str] = &[
    "Cascadia Mono", "Cascadia Code", "JetBrains Mono", "Fira Code",
    "Consolas", "Lucida Console", "Courier New",
];
```

Five of those seven are Windows fonts. On a stock Linux box the list falls
through to the literal family `"monospace"`, which is a fontconfig *alias*, not
a family — gpui may or may not resolve it, and `resolve_font` exists precisely
because **gpui answers an unknown family by silently falling back to a
proportional face**, which in a cell grid looks like a broken renderer rather
than a missing font.

Make `PREFERRED` platform-specific and put the Linux-likely families first:
`JetBrains Mono`, `Fira Code`, `Cascadia Mono`, `DejaVu Sans Mono`,
`Liberation Mono`, `Noto Sans Mono`, `Ubuntu Mono`, `monospace`. Keep
`monospace` last as the honest last resort.

**The startup risk is in the same function.** `resolve_font` calls
`cx.text_system().all_font_names()`, and `theme::apply` is on the path to the
first frame (`main.rs:55`). On Windows that whole step measures **1.2 ms**
alongside `Profiles::load`. On Linux the text system is cosmic-text over
font-kit over **fontconfig**, and enumerating every family with a cold
fontconfig cache is not a 1.2 ms operation — it can be hundreds of
milliseconds, and on a first run after a font install, seconds.

Measure it before deciding anything (§5). If it is over ~20 ms, the fix that
keeps rule 1 is the one the plugin catalogue already demonstrates:

* resolve synchronously **only** when `Settings::font_family` is non-empty —
  the common case, one string compare away from free if gpui can answer
  "is this family installed" without a full enumeration;
* otherwise paint the first frame with the saved family name (or
  `"monospace"`), kick the enumeration off on `cx.background_executor()`, and
  swap the resolved family in with a `theme::apply` when it lands. A font that
  corrects itself one frame later is invisible; a window that opens half a
  second late is not.

`theme::monospace_fonts` (the measured is-it-monospace pass for the picker)
runs when the **Settings page opens**, not per frame and not at startup —
leave it alone.

### 3.5 `Cargo.toml` and `build.rs`

```toml
[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"
```

`libc` is already in the tree via tokio and gpui, so this adds nothing to the
build.

Move the icon build-dependency behind the same gate it is already useless
outside of — `build.rs` returns early for non-Windows, but `embed-resource`
is still compiled:

```toml
[target.'cfg(target_os = "windows")'.build-dependencies]
embed-resource = "3.0.11"
```

`resvg` stays unconditional: `build.rs` writes `logo.png` on every platform
and `ui::icon` includes it.

In `src/main.rs:5`, tighten the subsystem attribute so it cannot matter:

```rust
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]
```

The release profile (`strip`, fat LTO, one codegen unit) is portable and
should stay as it is — it is tuned for binary size, and §5's cold-start figure
is paging that binary in.

**`fxc.exe` is Windows-only.** Release builds there precompile gpui's HLSL
shaders; on Linux gpui compiles SPIR-V for blade at build time with no external
tool. If a build ever asks for `fxc`, something is cross-compiling for Windows.

### 3.6 Paths, stores and the documentation that names them

No code change — `dirs::data_dir()` already does the right thing — but these
comments say `%APPDATA%` and should say "the platform data directory
(`%APPDATA%/PulseTerm` on Windows, `~/.local/share/PulseTerm` on Linux)":

* `src/session.rs:1`
* `src/ssh/known_hosts.rs` (the TOFU store; note it is **not** `~/.ssh/known_hosts`, deliberately, and that stays true on Linux)
* `CLAUDE.md`, in the `ssh/` and `session.rs` sections
* `src/local/fs.rs:7` — "Paths here are Windows paths" → "this platform's paths"

### 3.7 File pickers, drag-and-drop, clipboard

* `cx.prompt_for_paths` / `cx.prompt_for_new_path` (the explorer's upload and
  download) go through **`ashpd`**, the XDG desktop portal, on Linux. That
  needs `xdg-desktop-portal` plus a backend (`xdg-desktop-portal-gtk`, `-kde`,
  `-gnome`, or `-wlr`) installed and running, or the picker never appears and
  the transfer silently does nothing. Worth a check and, if it is cheap, a
  visible error rather than silence.
* `ExternalPaths` drops from the OS file manager: X11 and Wayland both carry
  `text/uri-list`, and gpui handles it — but it is worth testing from both
  Nautilus/Dolphin and a terminal drag, because URI-decoding (`%20`) is a
  classic place for a path to arrive wrong.
* Clipboard: X11 via `x11-clipboard`, Wayland via the data device. Both are
  gpui's problem, but `copy_on_select` and `ctrl-shift-c/v` are worth a pass.

### 3.8 Transparency and blur

`Settings::opacity` and `Settings::blur` map to
`WindowBackgroundAppearance::Transparent` / `Blurred`. On Linux:

* Transparency needs a running compositor. Most do; a bare X11 session without
  one will show black instead of the desktop.
* **Blur is almost certainly a no-op** — there is no DWM equivalent, and
  gpui's Linux backend has nothing to implement it with. Either hide the
  "Blur behind" switch on Linux or leave it with its existing description
  ("Only visible below 100% opacity") and accept that it does nothing. Hiding
  it is more honest; the Settings page already hides the disconnect action in
  local mode (`Workspace::is_local`), so there is precedent for hiding a
  control that cannot act.

The structural rule in `CLAUDE.md` — **every pixel is painted by exactly one
translucent layer** (`Theme::translucent`) — is renderer-independent and holds
unchanged.

---

## 4. Building and running on Linux

### Toolchain

```bash
rustup default stable
```

Nothing exotic: the Windows machine is on the **GNU** target because it has no
MSVC, which is a Windows-only story. Linux is plain `x86_64-unknown-linux-gnu`.

### System packages

gpui needs a C/C++ toolchain, Vulkan, fontconfig, and the X11 and Wayland
client libraries. The 23 tree-sitter grammars need a C compiler too. On Debian
or Ubuntu:

```bash
sudo apt install build-essential cmake pkg-config clang libfontconfig-dev libfreetype-dev libxcb-dev libxkbcommon-x11-dev libx11-xcb-dev libwayland-dev libxkbcommon-dev libssl-dev libvulkan-dev vulkan-tools mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk
```

On Fedora:

```bash
sudo dnf install @development-tools cmake clang fontconfig-devel freetype-devel libxcb-devel libxkbcommon-x11-devel libX11-devel wayland-devel openssl-devel vulkan-loader-devel vulkan-tools mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk
```

on Arch:

```bash
sudo pacman -S base-devel cmake clang fontconfig freetype2 libxcb libxkbcommon-x11 libx11 wayland openssl vulkan-icd-loader vulkan-tools xdg-desktop-portal xdg-desktop-portal-gtk
```

Check Vulkan actually resolves before blaming the app — `vulkaninfo --summary`
should name a real device. gpui on Linux is blade-graphics over Vulkan and
will not start without one; in a VM, install the software rasteriser
(`mesa-vulkan-drivers` ships lavapipe).

### Commands

Same as on Windows, from `CLAUDE.md`:

```bash
cargo run                  # build and launch; this is how it is normally tested
cargo check                # after any src/**.rs change
cargo test                 # 43 unit tests on Linux, none of them need a window
cargo build --release      # target/release/PulseTerm
```

The first build compiles ~1400 crates and takes 15–25 minutes; incremental
builds of this crate alone are seconds. Keep
`[profile.dev.package."*"] opt-level = 2` — it is what makes debug runs usable.

Force a backend while testing, because they fail differently:

```bash
WAYLAND_DISPLAY= cargo run      # X11
cargo run                       # Wayland, when WAYLAND_DISPLAY is set
```

---

## 5. The startup budget on Linux

Measure before optimising, and measure the way the Windows numbers were taken:
instrument `main` with `Instant` and read stderr (debug keeps the console
attached). The Windows breakdown, for comparison:

| phase | debug | note |
|---|---|---|
| gpui platform init, before our callback | 834 ms | DirectX device; debug compiles HLSL at runtime |
| `gpui_component::init` | 18 ms | |
| `languages::register` — 23 grammars | **0.5 ms** | the registry stores configs; queries compile on first use |
| `Settings::load` | 8.5 ms | |
| `Profiles::load` + `theme::apply` | 1.2 ms | **the one to watch on Linux — see §3.4** |
| window + first frame | ~430 ms | |
| **total to activate** | **1.29 s** | release: 720 ms warm / 1258 ms cold |

Two conclusions from Windows that are easy to get backwards and that still
hold:

* **Registering every grammar at startup is not a cost** — half a millisecond.
  The expensive part of a language is compiling its queries, and that happens
  the first time one is used. Do not "optimise" the registration away.
* **The cold/warm gap is paging in the binary**, ~34 MB of it, most of that
  tree-sitter parse tables. That is why the release profile is tuned for size.

What to watch for on Linux specifically:

1. **`theme::apply` → `resolve_font` → `all_font_names()`** (§3.4). The likely
   regression, and the fix is already described.
2. **The plugin catalogue** reads `.index.json` and nothing else at startup —
   twelve installed plugins moved a debug launch by about twenty milliseconds,
   flat in plugin count. That design is portable; keep it.
3. **`xdg-desktop-portal` must not be touched at startup.** It is a D-Bus round
   trip to another process and belongs only in the file-picker path.
4. **Fontconfig's first-run cache build** can dominate a genuinely cold start
   on a fresh machine. It is not ours to fix, but know it before chasing a
   number that will not reproduce.

---

## 6. Test checklist

Unit tests cover the session file, the syntax queries, preview detection, tab
titles, the plugin catalogue, the motion curves, the pty, the launch
arguments and more — 43 of them on Linux, none needing a window. Everything below is by hand, because there is no UI test.

**Local terminal (the new code)**

- [ ] A local tab opens and shows a prompt.
- [ ] `ctrl-c` interrupts a running `sleep 100` — this is the job-control test, and it fails if `TIOCSCTTY` was missed.
- [ ] `tput cols` / `tput lines` match the pane; resizing the column reflows the shell immediately.
- [ ] `vim`, `htop`, `less` and `tmux` draw correctly, in colour.
- [ ] Arrows work **in vim as well as bash** (the `TermMode::APP_CURSOR` CSI/SS3 switch).
- [ ] Mouse reporting: clicking in `htop` works; **holding shift always selects locally instead**.
- [ ] Wheel in `less`/`man` scrolls (alternate-screen arrow translation).
- [ ] `exit` marks the header `-- exited` **within about a second**, and the scrollback stays readable.
- [ ] Closing a tab kills the shell: `ps` shows no orphan, and `ps -eo stat` shows no `Z` zombie after a dozen open/close cycles.
- [ ] A `256`-colour and a true-colour test script look right (`TERM`/`COLORTERM`).
- [ ] Two tabs at once, both live, neither receiving the other's output.

**Window and chrome**

- [ ] Exactly one titlebar — ours. (§3.3)
- [ ] Drag the nav bar moves the window; double-click maximises and restores.
- [ ] Minimise, maximise and close buttons all act.
- [ ] Resize from every edge and corner.
- [ ] The nav bar on all four edges (Settings → Interface → Navigation bar edge).
- [ ] Under both X11 and Wayland.

**The rest**

- [ ] Home screen: the three dials run at their own rates, space continues the last session.
- [ ] Command bar: filters saved hosts, parses `user@host:port`, asks for a password inline, `del` forgets a host, and **no secret appears in `~/.local/share/PulseTerm/*.json`**.
- [ ] SSH connect, an SFTP tree, open/edit/save a remote file, upload and download with progress.
- [ ] The file tree docked beside the strip (`ctrl-shift-b`), its footer reading `N ITEMS` / `LOCAL`.
- [ ] The overview (`ctrl-shift-space`) with terminal thumbnails.
- [ ] Motion: the strip **slides** when focus moves; a dragged seam stays under the pointer; Settings → Tiling switches it off, changes the duration and the curve.
- [ ] Session restore: a local workspace comes straight back at launch.
- [ ] Font size `ctrl-=` / `ctrl--` / `ctrl-0` reflows terminals and the whole UI.
- [ ] Ink and Paper both look like frames 01 and 10.
- [ ] Startup time, measured, against §5.

**Added after the first checklist (2026-11-03)**

- [ ] Sessions: this computer plus two SSH hosts open at once; the bar's numbered
      entries switch them, `ctrl-shift-pagedown`/`pageup` cycle them, a parked
      session's shells keep running, disconnect closes only the one on screen,
      and closing the window asks while any SSH host is connected.
- [ ] Home's SSH list: it slides open, a host asks for its password inline (space
      types, escape folds), "new connection…" asks the address then the
      password, the x forgets a host.
- [ ] The disconnect button sits at the far end of the bar, beside the clock.
- [ ] Translucent window: windows on the strip are exactly as see-through as the
      bar (the strip paints no ground of its own when translucent).
- [ ] Throughput: `seq 1 2000000` in well under a second; normal output still
      drawn at full frame rate (`PULSETERM_TRACE=1` prints the numbers).
- [ ] **`-e` is POSIX-only as written.** `launch.rs` quotes the command for
      `/bin/sh` and turns `--working-directory` plus `-e` into
      `cd '<dir>' && exec <cmd>`, which ConPTY's command line will not
      understand. On Windows, join the words with Windows quoting and pass the
      directory as ConPTY's working directory instead. `--working-directory`
      alone (a shell in that folder) should already work.
- [ ] No `.desktop` or install script equivalent exists for Windows; the
      Start-menu entry and "open terminal here" registration are still to do.

---

## 7. Decide on the machine

Things that cannot be settled from Windows:

1. **How long `all_font_names()` actually takes.** Everything in §3.4's second
   half depends on the number.
2. **Whether `is_maximized()` is honest under Wayland tiling**, and what
   `set_client_inset` needs to be for the resize edges to feel right.
3. **Whether blur is silently a no-op** (§3.8) — decide to hide the switch or
   keep it.
4. **Which shells to label and in what order** (§3.2) once `/etc/shells` on a
   real box is in front of you.
5. **Whether `ptsname_r` is enough**, or the module should be `unix` with a
   `ptsname` fallback for macOS/BSD. Only worth doing if those targets matter.
6. **The `.desktop` and icon story** (§8), which depends on how it will be
   distributed.

---

## 8. Packaging (optional, and last)

Linux has no icon resource to link, so `build.rs`'s `.ico` path stays Windows-
only and the window icon comes from the desktop entry instead. Minimum:

```ini
[Desktop Entry]
Type=Application
Name=PulseTerm
Comment=A GPU-rendered SSH terminal
Exec=/usr/bin/pulseterm
Icon=pulseterm
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=pulseterm
```

`StartupWMClass` and `Icon` must both match the `app_id` from §3.3, or the
window will not pick up its own icon. Install `assets/logo.svg` to
`/usr/share/icons/hicolor/scalable/apps/pulseterm.svg` — the same single source
the Windows icon and the in-app logo come from, so replacing the SVG still
updates everything.

Beyond that: a `.tar.gz` of the release binary is the honest minimum, an
AppImage or a Flatpak the obvious next step. Note that a Flatpak would need
the SSH key paths and `/etc/shells` to be reachable through the sandbox, which
is a real design question, not a packaging detail.
