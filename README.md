<p align="center"><img src="assets/logo.svg" width="112" alt="PulseTerm logo"></p>

# PulseTerm

A GPU-rendered SSH terminal for Windows and Linux, built on [gpui](https://gpui.rs)
— the UI framework behind the Zed editor. TUI aesthetic, niri-style scrollable
tiling, several hosts open at once, and no remote-side install — plus an
optional remote desktop for Linux hosts.

## Why

Nothing needs to run on the remote host. Terminal sessions are plain SSH PTY
channels, which every stock `sshd` already provides. If `ssh user@host` works,
PulseTerm works — there is no server, agent, or extension host to deploy, and
nothing to keep updated.

Every window on a host shares one SSH connection. Each terminal is a separate
channel multiplexed over the same TCP session, so opening a tenth costs a
channel, not a tenth handshake.

## What it looks like

Everything is monospace on a fixed cell grid — the chrome is drawn in the same
character cells as the terminal content. Hairline borders instead of shadows and
rounded corners, no radius anywhere, and exactly one navigation bar — put it on
the top, bottom, left or right edge in Settings. It carries the actions, the
open sessions, a read-out of the strip with a mark per column, the clock and the
window buttons; the three pieces of chrome it replaced (title bar, icon rail,
status line) spent three edges saying the same things. Two palettes come with
it, Ink and Paper: the same drawing with the ink ramp inverted, and Ink is what
a fresh install opens on.

## Features

### Hosts and sessions

- **Several hosts at once** — this computer and any number of SSH hosts, each a
  session with its own columns and file tree. The navigation bar lists them;
  click one, or `ctrl-shift-pagedown` / `pageup`, to switch. Shells in the
  sessions you are not looking at keep running. Disconnecting closes just that
  one.
- **Works without SSH too** — "this computer" opens the same terminal, file
  tree and editor against this machine (ConPTY on Windows, a Unix pty on
  Linux). Pick the shell in Settings — only ones actually installed are
  offered — and a local workspace comes straight back the next time you open
  the app.
- **Connect from the home screen** — SSH opens into your saved hosts; pick one
  and a password (or key passphrase) field slides open under it. "New
  connection…" asks `user@host:port`, then the password. An × forgets a host.
- **One command bar for everything** — `ctrl-shift-k` opens a single line.
  Type to filter your saved hosts, type `user@host:port` for one you have never
  used, or a path to open it; going local, disconnecting, settings, the
  overview and a new terminal are in the same list; `del` forgets a saved host.
- **No secret is ever kept** — a password or passphrase is asked for when you
  connect and cleared the moment it is handed to the SSH layer: never in a
  file, and not in memory after that. Saved hosts remember host, port, username
  and key path only, and only after a connection succeeded.
- **TOFU host-key pinning** — the first connection to a host pins its
  fingerprint; a later mismatch is refused rather than warned about.
- **Password or public-key auth**, with passphrase-protected keys supported.

### The terminal

- **Full VT/ANSI emulation** via `alacritty_terminal` — the same core Zed's
  terminal uses. Full-screen TUI programs (vim, htop, tmux, btop) render
  correctly, including 256-colour and true-colour output.
- **Fast** — two million lines of `seq` in about 0.7 s on Linux, close to
  Alacritty, with ordinary output still drawn at full frame rate.
- **Mouse support** — text selection with copy and paste, and full mouse
  reporting to remote TUIs, so clicking in htop or tmux works. Hold shift to
  select text out of a program that has grabbed the mouse.
- Scrollback (10,000 lines by default), and a cursor that goes hollow when its
  terminal doesn't have the focus.

### Tiling

- **Scrollable tiling, niri-style — no tabs.** The workspace is one unbounded
  horizontal strip of columns, and a column stacks its windows vertically with
  all of them on screen. Opening a window never resizes what you are looking at:
  it lands in a new column right of the focus and the strip simply gets longer,
  scrolling to keep the focused column in view. Each column is a third, a half,
  two thirds or the full viewport wide (`ctrl-shift-r` cycles), or any width you
  drag its seam to — and widening one column pushes its neighbour along instead
  of squeezing it.
- **Windows move without being recreated** — drag one by its header onto
  another window to stack it there or open a new column beside it; or from the
  keyboard, expel it into a column of its own, consume the next column's into
  yours, carry a column along the strip. The terminal, and the SSH channel
  behind it, survives every move.
- **The strip slides instead of cutting** — moving the focus slides the
  columns across, so you can see which way you went and how far. Speed, curve
  and off are in Settings → Tiling; a seam you are dragging never
  animates, because it has to stay under the pointer.
- **See the whole strip** — `ctrl-shift-space` lays every column out as a card,
  with a few lines of each terminal's screen in it so two shells on one host
  can be told apart. Escape puts you back exactly where you were.
- **Resumes where you left off** — the columns, the windows in them, their
  widths and where the file tree was pointed come back on the next launch:
  files reopen at the same paths, terminals come back as fresh shells (an SSH
  shell cannot outlive its connection). A layout is only restored onto the
  same host, and never over work already open.

### Files

- **File tree** — docked beside the strip (`ctrl-shift-b`), over SFTP or this
  machine's disk, with an editable path bar and a context menu for create,
  rename, copy/paste, delete (recursive for folders) and properties.
- **File transfer** — upload and download through native file pickers, or drag
  files in from your file manager, with live progress, and "3 of 7" when
  several go at once.
- **Code editor** — double-click a file to edit it in a window, with syntax
  highlighting (Rust, Python, Go, C/C++, JavaScript/TypeScript, Bash, Markdown,
  YAML, TOML, JSON, HTML/CSS, Java, Zig and more), line numbers, search, undo
  and IME; `ctrl-s` saves it back. Built on
  [gpui-component](https://github.com/longbridge/gpui-component)'s editor.
- **Markdown preview and image viewing** — preview a `.md` file rendered, with
  highlighted code blocks, or open an image (PNG, JPEG, WebP, GIF, SVG, BMP,
  TIFF); both can sit in the column beside their source.

### Remote desktop (optional, Linux hosts)

- **Stream a host's desktop** over the SSH connection you already have — an
  icon in the navigation bar (`ctrl-shift-m`) opens a separate window that
  starts PulseTerm's own [Sunshine](https://github.com/LizardByte/Sunshine)
  instance on the host, pairs with it (nothing to type, nothing stored) and
  streams it with hardware decoding, audio and full keyboard and mouse.
  Settings change live in a sidebar on the side you choose (right by
  default): in a window it opens beside the picture from a strip on that
  edge; in full screen it slides over the picture from that edge.
- **A host with no monitor attached** gets a fake one: the host's driver is
  told a monitor is plugged in, at the resolution and refresh rate you pick
  (720p to 4K, 30 to 144 Hz), so the desktop logged in there has a screen to
  stream. On and off from the settings. NVIDIA on X11 does it in xorg.conf
  (on and off restart the login screen once; mode changes are live), AMD
  and Intel through the kernel's EDID override (live, re-applied after a
  reboot). A one-time setup on the host -- one command, which the setup
  screen gives -- installs the small root script it uses.
- It is a **separate program** (`pulseterm-remote`, GPL-3.0) that PulseTerm
  finds at run time; built without it, PulseTerm simply has no remote icon. See
  [REMOTE_DESKTOP.md](./REMOTE_DESKTOP.md) for the design, the host
  requirements and how to build it.

### The rest

- **Opens on a home screen** — three graduated dials make the clock, with the
  ways in beside them (SSH, this computer, settings) and the last session ready
  to resume with space. The logo or the clock brings it back; escape returns.
- **Asks before closing** — the X, Alt-F4 and a shutdown all stop for a
  confirmation while any SSH host is connected or a file has unsaved changes.
  Confirming hangs every session up properly first. Closing a single editor
  window with unsaved changes asks too, and offers to save it.
- **Themes and settings** — a Settings page with fourteen built-in palettes
  (Ink, Paper, Tokyo Night, Catppuccin, Dracula, Nord, Gruvbox, One Dark,
  Solarized, Rosé Pine, GitHub Dark and more, or follow the system's light or
  dark), a colour picker for every interface and terminal colour, any installed
  monospace font with size, line height, weight and ligatures, window
  transparency, rebindable keys, and terminal, file tree and editor options.
  Everything applies live, including to open terminals.
- **As much chrome as you want** — the navigation bar and every window header
  can each be shown, auto-hidden (they appear when the pointer reaches them,
  drawn over the content so nothing resizes) or hidden; headers go top or
  bottom; the minimise / maximise / close buttons are optional.
- **"Open with PulseTerm" in File Explorer** (Windows, optional) — on a file it
  opens in the editor, on a folder a shell opens there. Per user, no admin;
  `PulseTerm.exe PATH` does the same from the command line.
- **Native window chrome** — the navigation bar is the window's caption: on
  Windows with the real OS move loop (Snap, Snap Assist, Aero Shake); on Linux
  with client-side decorations and the compositor's own move, resize and menu.

## Requirements

- Windows 10/11, or Linux (Wayland or X11)
- A remote host running a standard OpenSSH server — nothing else installed
  there (the optional remote desktop needs Sunshine on the host)

## Getting started

```bash
cargo run
```

PulseTerm opens on its home screen: pick **SSH** for a saved host or a new one,
or **this computer** for a local shell. `ctrl-shift-k` opens the command bar
from anywhere.

## Building

The repository holds two programs: **PulseTerm** itself, and the optional
remote desktop helper **`pulseterm-remote`** (`remote/`, GPL-3.0). A plain
`cargo` command builds PulseTerm only; the helper is built when you name it
(`-p pulseterm-remote`) or ask for both (`--workspace`). PulseTerm finds the
helper beside its own exe at run time, so build both in the **same** profile
(dev with dev, release with release) to get the remote desktop icon.

| What | Dev build (fast to build, debug info) | Release build (optimised, small) |
|---|---|---|
| PulseTerm | `cargo build` | `cargo build --release` |
| Remote desktop helper | `cargo build -p pulseterm-remote` | `cargo build --release -p pulseterm-remote` |
| Both together | `cargo build --workspace` | `cargo build --release --workspace` |

Run straight from the source tree:

```bash
cargo run                                                   # PulseTerm, dev
cargo run --release                                         # PulseTerm, release
cargo run -p pulseterm-remote -- --local --label test       # the helper alone, against this machine
```

The output lands in `target/debug/` (dev) or `target/release/` (release):
`PulseTerm` (`PulseTerm.exe` on Windows) and `pulseterm-remote`
(`pulseterm-remote.exe`). Tests: `cargo test` for PulseTerm,
`cargo test -p pulseterm-remote` for the helper, `cargo test --workspace` for both.

Building both together merges the two programs' dependency features, so the
first `--workspace` build -- and the first single-program build after one --
recompiles the shared dependencies once (minutes, not seconds).

> The first build compiles roughly 1400 crates and takes 15–25 minutes.
> Incremental rebuilds after that are a few seconds. A release build is slower
> (fat LTO, one codegen unit): about 10 minutes for PulseTerm from scratch.

### What the remote desktop helper needs first

PulseTerm needs only Rust and a C compiler. The helper also needs the
moonlight-common-c submodule and a few native libraries:

```bash
git submodule update --init --recursive
```

- **Linux:** CMake, clang, and the FFmpeg, SDL3, libva, OpenSSL and Opus
  development packages. The exact package list (and a Fedora trap that removes
  the graphics driver) is in [REMOTE_DESKTOP.md](./REMOTE_DESKTOP.md).
- **Windows:** Visual Studio 2022 Build Tools, LLVM (`winget install
  LLVM.LLVM`), OpenSSL (`winget install ShiningLight.OpenSSL.Dev`), and the SDL3,
  FFmpeg and Opus development files. Windows has no system location for them,
  so tell the build where they are -- in the same PowerShell window you build
  from (paths are examples; full details in REMOTE_DESKTOP.md, *Building on
  Windows*):

  ```powershell
  $deps = "D:\Apps\pulseterm-remote-deps"
  $env:LIBCLANG_PATH       = "C:\Program Files\LLVM\bin"
  $env:OPENSSL_DIR         = "C:\Program Files\OpenSSL-Win64"
  $env:OPENSSL_LIB_DIR     = "C:\Program Files\OpenSSL-Win64\lib\VC\x64\MD"
  $env:OPENSSL_INCLUDE_DIR = "C:\Program Files\OpenSSL-Win64\include"
  $env:FFMPEG_DIR          = "$deps\ffmpeg\ffmpeg-n9.0-latest-win64-gpl-shared-9.0"
  $env:SDL3_LIB_DIR        = "$deps\sdl3\SDL3-3.4.18\lib\x64"
  $env:OPUS_LIB_DIR        = "$deps\opus\install\lib"
  ```

  On Windows the helper also needs its DLLs **beside** `pulseterm-remote.exe`.
  After a build, copy them into the folder you built (`target\release` here;
  `target\debug` for a dev build):

  ```powershell
  $out = "target\release"
  Copy-Item "$deps\sdl3\SDL3-3.4.18\lib\x64\SDL3.dll",
            "$env:FFMPEG_DIR\bin\avcodec-*.dll",
            "$env:FFMPEG_DIR\bin\avutil-*.dll",
            "$env:FFMPEG_DIR\bin\swresample-*.dll",
            "$deps\opus\install\bin\opus.dll",
            "C:\Program Files\OpenSSL-Win64\bin\libcrypto-4-x64.dll",
            "C:\Program Files\OpenSSL-Win64\bin\libssl-4-x64.dll" $out
  ```

  To copy the release build elsewhere, take `PulseTerm.exe`,
  `pulseterm-remote.exe` and those seven DLLs together. `PulseTerm.exe` alone
  is the whole terminal; without the helper beside it, the remote desktop icon
  simply doesn't appear.

### Windows

Release builds compile gpui's shaders ahead of time and need `fxc.exe` from the
Windows SDK. gpui finds it by itself in the `10.0.26100.0` kit; with a different
SDK version, point it at that kit's compiler:

```powershell
$env:GPUI_FXC_PATH = "C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\fxc.exe"
```

The result is `target/release/PulseTerm.exe` — copy it anywhere and run it.

### Linux

Install it for your user (no root) so it shows up in your applications menu as
a terminal:

```bash
cargo build --release
packaging/linux/install.sh                    # ~/.local/bin/pulseterm, icon, menu entry
packaging/linux/install.sh --default-terminal # also make it what "open in terminal" uses
packaging/linux/install.sh --uninstall
```

It takes the arguments desktops and file managers hand a terminal:

```bash
pulseterm -e htop                      # run a command in a local tab
pulseterm --working-directory ~/src    # a shell in that folder
```

Every launch is its own window. Package names for the build dependencies are in
[LINUX.md](./LINUX.md).

## Keys

App shortcuts are all on **ctrl+shift**, deliberately: that is the one region of
the keyboard a terminal has no encoding for, so nothing is stolen from the remote
shell where ctrl-c, ctrl-d and ctrl-z all mean something. Every one of them can
be rebound in Settings → Keys.

| Key | Action |
| --- | --- |
| `ctrl-shift-t` | New terminal in a new column |
| `ctrl-shift-d` | Stack another terminal in this column |
| `ctrl-shift-w` | Close the focused window |
| `ctrl-shift-←` / `→`, `ctrl-tab` / `ctrl-shift-tab` | Focus a column |
| `ctrl-shift-↑` / `↓` | Focus a window inside the column |
| `ctrl-shift-alt-←` / `→` | Move the column along the strip |
| `ctrl-shift-alt-↑` / `↓` | Move the window inside its column |
| `ctrl-shift-o` | Expel the window into a column of its own |
| `ctrl-shift-u` | Consume the next column's top window |
| `ctrl-shift-r` | Cycle the column's width |
| `ctrl-shift-b` | Open or close the file tree |
| `ctrl-shift-space` | Overview: the whole strip at once |
| `ctrl-shift-k` | Command bar — connect, open, settings, overview |
| `ctrl-shift-pagedown` / `pageup` | Next / previous session |
| `ctrl-shift-m` | Remote desktop (SSH, when the helper is installed) |
| `ctrl-shift-,` | Settings |
| `ctrl-shift-q` | Disconnect |
| `ctrl-shift-c` / `ctrl-shift-v` | Copy / paste |
| `ctrl-s` | Save file (in an editor window only — a terminal still gets it) |
| `ctrl-=` / `ctrl--` | Bigger / smaller font, everywhere |
| `ctrl-0` | Reset font size |
| `space` | Continue the previous session (home screen only) |
| `esc` | Back to your session (home screen only) |

Everything else goes to the shell.

## Tech stack

- [gpui](https://gpui.rs) 0.2 — GPU-accelerated UI (DirectX on Windows, Vulkan
  on Linux)
- [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) — VT parsing
  and grid model, driven by SSH bytes rather than a local pty
- [`russh`](https://github.com/Eugeny/russh) — pure-Rust SSH, `ring` backend, no
  OpenSSL or libssh dependency
- tokio for the SSH transport, bridged to gpui's executor (`src/runtime.rs`)
- The remote desktop helper: SDL3, FFmpeg (VA-API) and
  [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c)

## Status and limitations

Not yet built:

- Uploading or downloading whole folders — files only
- Dragging a window out into its own OS window
- The TTY1–4 `conspy` console shortcuts (the backend path is wired; the UI isn't)
- Restoring every session at launch — only the one on screen when you quit
  comes back
- No installer or Start-menu entry on Windows yet: the build is a folder of
  files you copy (Linux has `packaging/linux/install.sh`)
- The remote desktop helper streams on both; on Windows it needs its DLLs
  beside it ([REMOTE_DESKTOP.md](./REMOTE_DESKTOP.md), *Building on Windows*)
- No end-to-end or UI tests (there are unit tests for the parts that can be
  checked without a window)

Everything since the Linux port began was built and tested on Linux (Fedora,
niri on Wayland), then checked on Windows 11; [LINUX.md](./LINUX.md) has the
Windows results and what is still open there. macOS has not been tried.

## Project layout

See [CLAUDE.md](./CLAUDE.md) for the architecture — the tokio/gpui runtime
seam, the terminal render path, the scrollable strip, sessions, and the
decisions that look odd until you know why. [LINUX.md](./LINUX.md) is the Linux
port's handover, and [REMOTE_DESKTOP.md](./REMOTE_DESKTOP.md) the remote
desktop's design and log.
