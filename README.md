<p align="center"><img src="assets/logo.svg" width="112" alt="PulseTerm logo"></p>

# PulseTerm

A GPU-rendered SSH terminal for Windows, built on [gpui](https://gpui.rs) — the UI
framework behind the Zed editor. TUI aesthetic, niri-style scrollable tiling, no
remote-side install.

## Why

Nothing needs to run on the remote host. Terminal sessions are plain SSH PTY
channels, which every stock `sshd` already provides. If `ssh user@host` works,
PulseTerm works — there is no server, agent, or extension host to deploy, and
nothing to keep updated.

Every window shares one SSH connection. Each terminal is a separate channel
multiplexed over the same TCP session, so opening a tenth costs a channel, not a
tenth handshake.

## What it looks like

Everything is monospace on a fixed cell grid — the chrome is drawn in the same
character cells as the terminal content. Hairline borders instead of shadows and
rounded corners, no radius anywhere, and exactly one navigation bar — put it on
the top, bottom, left or right edge in Settings. Two palettes come with it, Ink
and Paper: the same drawing with the ink ramp inverted, and Ink is what a fresh
install opens on. It carries the connection state, a read-out of
the strip with a mark per column, the clock, and the window buttons; the three
pieces of chrome it replaced (title bar, icon rail, status line) spent three
edges saying the same things.

## Features

- **Works without SSH too** — "use this computer" opens the same terminal,
  explorer and editor against this machine, on Windows' own ConPTY. Pick the
  shell in Settings (only ones actually installed are offered). There is no
  disconnect, because there is no session to hang up, and a local workspace
  comes straight back the next time you open the app.
- **SSH terminal** with full VT/ANSI emulation via `alacritty_terminal` — the same
  emulation core Zed's terminal uses. Full-screen TUI programs (vim, htop, tmux,
  btop) render correctly, including 256-colour and true-colour output.
- **Scrollable tiling, niri-style — no tabs.** The workspace is one unbounded
  horizontal strip of columns, and a column stacks its windows vertically with
  all of them on screen. Opening a window never resizes what you are looking at:
  it lands in a new column right of the focus and the strip simply gets longer,
  scrolling to keep the focused column in view. Each column is a third, a half,
  two thirds or the full viewport wide (`ctrl-shift-r` cycles), or any width you
  drag its seam to — and widening one column pushes its neighbour along instead
  of squeezing it.
- **Windows move without being recreated** — expel one into a column of its own,
  consume the next column's into yours, carry a column along the strip. The
  terminal entity, and the SSH channel behind it, survives every one.
- **Native window chrome** — the navigation bar doubles as the caption, with
  real Windows dragging, snapping, Snap Assist and Aero Shake (the OS move
  loop, not an imitation).
- **Mouse support**: text selection with copy/paste, and full mouse reporting to
  remote TUIs, so clicking in htop or tmux works. Hold shift to select text out
  of a program that has grabbed the mouse.
- **One command bar for everything** — `ctrl-shift-k` opens a single line.
  Type to filter your saved hosts, or type `user@host:port` for one you have
  never used, or a path to open it; connect, go local, disconnect, settings,
  overview and a new terminal are all in the same list; `del` forgets a saved
  host. A password or passphrase is asked for on that same line and is never
  kept — not in a file,
  and not in memory past the moment it is handed to the SSH layer.
- **The strip scrolls instead of cutting** — moving the focus slides the
  columns across, so you can see which way you went and how far, which is the
  thing a long strip is otherwise bad at telling you. Column widths and the
  two overlays move with it. Speed, curve and off are in
  Settings → Window → Motion; a seam you are dragging never animates, because
  it has to be under the pointer.
- **See the whole strip** — `ctrl-shift-space` lays every column out as a card,
  with a few lines of each terminal's screen in it so two shells on one host
  can be told apart. Click one, or walk them with the arrow keys; escape puts
  you back exactly where you were, because nothing has been rearranged.
- **The file tree is a window, not a panel** — `ctrl-shift-b` opens it as a
  column on the strip, so it resizes at its seam, moves along the strip and
  stacks under a terminal like anything else.
- **Saved connections** — host, port, username and key path are remembered after
  a successful connect. Passwords and passphrases are never written to disk.
- **Opens on a home screen** — three graduated dials make the clock, and three
  ways in sit under it (SSH, this computer, settings), with every piece of
  chrome hidden. Space picks up last run's session; the house button in the
  sidebar brings the screen back, escape returns to what you were doing.
- **Resumes where you left off** — the columns, the windows stacked in them,
  their widths and where the file tree was pointed all come back on the next
  launch. The command bar opens on the host they belong to, and reconnecting
  rebuilds them:
  files reopen at the same paths, terminals come back as fresh shells (an SSH
  session cannot outlive the connection). A layout is only ever restored onto
  the same host, and never over work already open.
- **Asks before closing** — the X, Alt-F4 and a shutdown all stop for a
  confirmation while a connection is open or a file has unsaved changes, naming
  what's at stake. Confirming hangs the SSH session up properly first, then
  closes. Closing a single editor window with unsaved changes asks too, and
  offers to save it on the way out.
- **Remote file explorer** — a VS Code-style tree over SFTP with an editable
  path bar, per-extension file icons, and a context menu for create, rename,
  copy/paste, delete (both recursive for folders) and properties.
- **File transfer** — upload and download through native file pickers, or drag
  files straight in from Windows Explorer, with live progress: bytes, percent,
  speed and a bar, and "3 of 7" when several files go at once.
- **Code editor** — double-click a file in the explorer to edit it in a window,
  with syntax highlighting (Rust, Python, Go, C/C++, JavaScript/TypeScript,
  Bash, Markdown, YAML, TOML, JSON, HTML/CSS, Java, Zig and more), line
  numbers, search, undo and IME;
  `ctrl-s` saves it back over SFTP. Built on
  [gpui-component](https://github.com/longbridge/gpui-component)'s editor.
- **Themes and settings** — a searchable Settings page with twelve built-in
  themes (Tokyo Night, Catppuccin, Dracula, Nord, Gruvbox, One Dark, Solarized,
  Rosé Pine, GitHub Dark and more), a colour picker for every interface and
  terminal colour, any installed monospace font with size, line height, weight
  and ligatures, window transparency with acrylic blur, and terminal, explorer
  and editor options. Everything applies live, including to open terminals.
- **Markdown preview and image viewing** — right-click a `.md` file and choose
  Preview to read it rendered, with highlighted code blocks; double-click an
  image (PNG, JPEG, WebP, GIF, SVG, BMP, TIFF) to look at it. Both are
  read-only windows, so a preview can sit in the column beside its source.
- **TOFU host-key pinning** — first connection to a host pins its fingerprint;
  a later mismatch is refused rather than warned about.
- **Password or public-key auth**, with passphrase-protected keys supported.
- Scrollback (10k lines), per-pane focus, and a cursor that goes hollow when its
  terminal doesn't have focus.

## Requirements

- Windows 10/11
- A remote host running a standard OpenSSH server — nothing else installed there

## Getting started

```bash
cargo run
```

Then `ctrl-shift-k` for the command bar — type a host, or pick a saved one.

Set windows sdk env:
```bash
$env:GPUI_FXC_PATH = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.28000.0\x64\fxc.exe"
```
Build a standalone executable:
```bash
cargo build --release
```

The result is `target/release/PulseTerm.exe` — copy it anywhere and run it.

> The first build compiles roughly 1400 crates and takes 15–25 minutes.
> Incremental rebuilds after that are a few seconds.

## Keys

App shortcuts are all on **ctrl+shift**, deliberately: that is the one region of
the keyboard a terminal has no encoding for, so nothing is stolen from the remote
shell where ctrl-c, ctrl-d and ctrl-z all mean something.

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
| `ctrl-shift-k` | Command bar — connect, open, settings, overview |
| `ctrl-shift-space` | Overview: the whole strip at once |
| `ctrl-shift-,` | Settings |
| `ctrl-shift-q` | Disconnect |
| `ctrl-shift-c` / `ctrl-shift-v` | Copy / paste |
| `ctrl-shift-b` | Open or close the file tree as a column |
| `ctrl-s` | Save file (in an editor window only — a terminal still gets it) |
| `ctrl-=` / `ctrl--` | Bigger / smaller font, everywhere |
| `ctrl-0` | Reset font size |
| `space` | Continue the previous session (home screen only) |
| `esc` | Back to your session (home screen only) |

Everything else goes to the remote shell.

## Tech stack

- [gpui](https://gpui.rs) 0.2 — GPU-accelerated UI, DirectX backend on Windows
- [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) — VT parsing
  and grid model, driven by SSH bytes rather than a local pty
- [`russh`](https://github.com/Eugeny/russh) — pure-Rust SSH, `ring` backend, no
  OpenSSL or libssh dependency
- tokio for the SSH transport, bridged to gpui's executor (see `src/ssh/runtime.rs`)

## Status and limitations

Not yet built:

- Uploading or downloading whole folders — files only
- Moving a window between columns with the mouse (the keyboard does it)
- Dragging a window out into its own OS window
- The TTY1–4 `conspy` console shortcuts (the backend path is wired; the UI isn't)
- More than one SSH connection at a time
- No end-to-end or UI tests (there are unit tests for the parts that can be
  checked without a window: session files, syntax queries, preview and title
  handling)

Developed and tested on Windows against the GNU toolchain. gpui is
cross-platform, but this hasn't been tried on macOS or Linux.

## Project layout

See [CLAUDE.md](./CLAUDE.md) for the architecture breakdown — the tokio/gpui
runtime seam, the terminal render path, the split-tree model, and the decisions
that look odd until you know why.
