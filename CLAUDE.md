# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## What this is

PulseTerm: a GPU-rendered SSH terminal for Windows and Linux, built on [gpui](https://gpui.rs)
(Zed's UI framework). A Linux port is under way -- `LINUX.md` is its handover
and status, and every Linux-specific arm is `cfg`'d beside the Windows one.
**Everything since the port began was written on Linux and has been
built and run on Windows once (2026-11-03); `LINUX.md` -> "Windows: check
this on the next Windows build" has that run's results and what is still
open.** It talks to a stock `sshd` — terminal sessions are SSH PTY
channels, so there is no remote-side agent, server, or install step. The UI is
deliberately TUI-shaped: everything is monospace on a fixed cell grid, hairline
borders instead of shadows and radii, and one navigation bar along an edge of
the user's choosing.

It is a rewrite of `D:\Projects\RemoteTTY` (Tauri + WebView2 + xterm.js +
CodeMirror). The SSH layer is a direct port of that project's working `russh`
code; everything above it is new. RemoteTTY is still the reference for the two
behaviours not yet ported: detach-a-window-to-a-new-OS-window and the TTY1-4 console
shortcuts.

## Commands

- `cargo run` — build and launch. This is how the app is normally tested.
- The repo is a Cargo workspace of two programs, but **plain `cargo` commands
  build PulseTerm only** (`default-members`). The remote desktop helper in
  `remote/` (GPL-3.0, see `REMOTE_DESKTOP.md`) builds only with
  `cargo build -p pulseterm-remote` or `--workspace`; PulseTerm finds it at run
  time and hides the remote icon without it. Never make PulseTerm depend on it.
- `cargo check` — type-check. Do this after any `src/**.rs` change.
- `packaging/linux/install.sh` — installs for this user, no root: the binary
  as `~/.local/bin/pulseterm` (the remote helper beside it if built), the
  icon, and `pulseterm.desktop` (categories `TerminalEmulator`, a New Window
  action, `SingleMainWindow=false`). Release build if there is one, else
  debug; `--default-terminal` also puts it first in `xdg-terminals.list`;
  `--uninstall` removes it all. Run it again after rebuilding -- it copies.
- `pulseterm -e CMD...` / `--working-directory DIR` — what desktops and file
  managers pass a terminal (`src/launch.rs`): the first window opens a local
  tab running that instead of what the startup setting says. Every launch is
  its own process and window; nothing looks for a running copy. The command
  is quoted for the platform -- `/bin/sh` on Unix, CreateProcess's double
  quotes on Windows -- and a directory travels apart from it
  (`PtyProgram::CommandIn`), so both work together on Windows too.
- `pulseterm PATH` — a bare path: a file opens in the editor (no terminal
  beside it), a folder opens a shell there (`launch::Start`). It is what File
  Explorer's "Open with PulseTerm" runs; see that section.
- `cargo build --release` — optimised standalone `target/release/PulseTerm.exe`.
  Release is tuned for size (`[profile.release]`: strip, fat LTO, one codegen
  unit), so it is much slower to build than debug and rebuilds every crate when
  those settings change. It also needs **`fxc.exe`**: release builds compile
  gpui's HLSL shaders ahead of time (debug compiles them at runtime), and gpui
  only looks in `GPUI_FXC_PATH`, then `PATH`, then the `10.0.26100.0` kit. That
  kit is the one installed here now, so it is found with nothing set; on a
  machine with a different SDK version, if the build stops with *Failed to
  find fxc.exe*, point `GPUI_FXC_PATH` at that kit's `x64\fxc.exe`.
- `cargo test` -- unit tests only, for the parts that need no window: the
  session file, the syntax queries, preview detection, tab titles, the plugin
  catalogue. There is no end-to-end or UI test; anything involving a window is
  checked by running it.
- `graphify extract . --code-only --cargo` then `graphify cluster-only .` --
  rebuilds the code knowledge graph in `graphify-out/` (`graph.html` is
  clickable). Local tree-sitter, no API key, ~12 s. Community *naming* is the
  one part that wants an LLM and degrades to `Community N` without one.

First build compiles ~1400 crates and takes 15-25 minutes; incremental builds of
this crate alone are seconds. `[profile.dev.package."*"] opt-level = 2` in
`Cargo.toml` keeps dependencies optimised so debug runs are usable — leave it.

## Toolchain (Windows-specific, non-obvious)

Rust here is now the **MSVC** target (`stable-x86_64-pc-windows-msvc`, 1.98),
with Visual Studio 2022 **Build Tools** (MSVC 14.44) for the linker and for the
C compiler the tree-sitter grammars and `ring` need, and Windows SDK
`10.0.26100.0` for `fxc.exe` and `rc.exe`. Checked 2026-11-03: `cargo check`,
`cargo test` and a debug build all succeed with it.

It used to be the **GNU** target with MinGW (`BrechtSanders.WinLibs`) and no
Visual Studio at all, and gpui 0.2.2 builds, links and runs on windows-gnu as
well -- so either works, whatever Zed's docs say about requiring MSVC. MinGW's
gcc is still on this machine (`D:\Apps\mingw64`), but no GNU Rust toolchain is
installed. `~/.cargo/bin` is not on `PATH` in a fresh shell here.

`russh` is pinned to `default-features = false, features = ["ring", "flate2",
"rsa"]` — the `ring` crypto backend needs only a C compiler; the default
`aws-lc-rs` one also wants CMake and NASM, and there is no reason to take that
on.

`blade-graphics` (gpui's Vulkan renderer, Linux only) is a **patched copy** in
`patches/blade-graphics`, through `[patch.crates-io]`: it rebuilds an
out-of-date swapchain itself, without which a Wayland window froze after its
third frame until resized (see `LINUX.md`). Keep the patch when touching gpui;
the change is marked `PulseTerm patch` in `src/vulkan/surface.rs` and `mod.rs`.

## Architecture

### `src/runtime.rs`, `src/pty.rs` — shared by both backends

- `runtime.rs` — the seam between two async worlds. russh is written against
  tokio; gpui's executor is smol-based. Rather than a compat shim, the transport
  layer runs on its own process-wide tokio runtime, and `spawn()` bridges each
  call back to the caller's executor through a oneshot. `detach()` is for the
  long-lived per-channel pump tasks. Local file IO goes through it too, which is
  why it is not under `ssh/`.
- `pty.rs` — `Pty`, `PtyEvent`, `PtyProgram`: bytes in, bytes out, a size and an
  end. An SSH channel and a pseudo-console have nothing in common below this and
  everything in common above it.

### `src/ssh/` — SSH transport, knows nothing about gpui
- `client.rs` — connect, authenticate, TOFU host-key check. `Connection` is
  cheaply cloneable and shares one `russh::client::Handle`, which is what makes
  every tab a multiplexed channel rather than a new TCP connection.
- `pty.rs` — one SSH channel per terminal, with a background task pumping bytes
  both ways. **Each pty owns its own `flume` event channel.** The Tauri build
  broadcast every byte to every window and made the frontend filter by id; here a
  terminal view only ever receives its own output. `Pty::drop` closes the channel,
  so closing a tab really does free the remote side — there is no separate
  teardown call to forget.
- `sftp.rs` — the file-browsing half, opened as a subsystem channel on the same
  connection at connect time, so the explorer costs no extra handshake or
  authentication. Remote paths are POSIX no matter what this client runs on, so
  `join`/`parent` are hand-written; `std::path` would emit backslashes here.
- `known_hosts.rs` — TOFU pinning in `known_hosts.json` in the platform data
  directory (`%APPDATA%/PulseTerm`, `~/.local/share/PulseTerm` on Linux), not
  the real `~/.ssh/known_hosts`.

### `src/terminal/` — VT emulation and grid rendering

- `emulator.rs` — wraps `alacritty_terminal`'s `Term` (the same crate Zed's
  terminal uses) and drives it with bytes from an SSH channel instead of a local
  pty, so none of alacritty's own tty/spawn code is involved. alacritty pushes
  side effects out through an `EventListener`; we buffer them and `feed()` returns
  them as `TermAction`s. **`TermAction::Write` must be sent back up the pty** —
  those are cursor-position reports and device attributes, and dropping them is
  what makes some TUI programs hang waiting for a reply.
- `element.rs` — a hand-written gpui `Element`, not a tree of divs. An 80x24 grid
  is ~2000 cells and a div per cell would rebuild that many taffy nodes per frame.
  Each frame walks the grid once and emits one quad per run of same-coloured
  background and **one glyph per non-blank cell from a glyph cache** (`GLYPHS`). **The grid is resized from `prepaint`**,
  before content is read out — doing it after layout makes a full-screen TUI paint
  one garbled frame at the old size on every resize.
- `keymap.rs` — keystroke to pty bytes. Cursor keys switch between CSI (`ESC [`)
  and SS3 (`ESC O`) introducers depending on `TermMode::APP_CURSOR`; getting that
  wrong is why arrows "work in bash but not in vim" in a hand-rolled terminal.
  Backspace sends DEL (0x7f), not BS. Ctrl+Shift+letter deliberately encodes to
  nothing so that whole keyboard region is free for app shortcuts.
- `palette.rs` — alacritty colour to gpui `Hsla`. OSC overrides from the remote
  program win over the theme, which is what lets vim colourschemes and
  `dircolors` look right.
- `mouse.rs` — who owns the mouse. A program that enabled mouse tracking
  (`TermMode::MOUSE_MODE`) gets clicks, drags and wheel ticks forwarded to it;
  otherwise they drive local selection and scrollback. **Holding shift always
  forces local handling** — the long-standing convention for selecting text out
  of htop, tmux or vim, and the only way to do it while a program has the mouse.
  Adding selection without this is what broke clicking in TUIs.
  SGR (1006) is used when the program asked for it and the legacy 1000 protocol
  otherwise; the legacy one can't express coordinates past 223, so those events
  are dropped rather than reported at a wrong cell.
- `view.rs` — focus, key handling, scrollback, and the task pumping pty events
  into the emulator. Its padding is `Settings::terminal_padding` (Settings ->
  Terminal, 0-48 px, default 4 -- what the fixed `p_1` gave), in plain pixels
  so it does not scale with the font; the grid fits what is left in prepaint,
  and mouse positions are taken against the grid, so a change reflows open
  terminals and nothing else. **The pump takes everything already queued per wake**
  (up to 1 MB) and asks for a frame when the queue runs dry or 16 ms have
  passed -- `seq` on a terminal writes a line at a time, ~100,000 events a
  second, and one trip into the view per event was the cost.
  `PULSETERM_TRACE=1` prints frames drawn, frames asked for, MB parsed and
  the element's prepaint/paint cost per frame, once a second.
- **Glyphs are shaped once, not every frame** (`GLYPHS`, `GlyphCache`). Each
  distinct character -- with its combining marks, in bold or italic -- goes
  through gpui's `layout_line` once (so font fallback for braille, emoji and
  CJK resolves as before), and every frame after that only paints cached
  glyph ids at `column x cell width` on the base font's baseline with
  `paint_glyph` / `paint_emoji`, plus underline and strikethrough where gpui's
  own line painter puts them. Whole rows used to be shaped every frame, and
  a TUI that redraws the screen in colour was bound by it: `lite-eq` (56 rows,
  ~1,300 colour runs, ~6,400 characters a frame) cost ~15 ms a frame in the
  shaper and 65% of a core, against Alacritty's 5%. After: 0.7 ms a frame and
  under 9% of a core. The cache is shared by all terminals and empties itself
  when the font, size or weight changes, or past 10,000 entries. It also
  fixed two things row shaping got wrong: combining accents and italic.
- **With ligatures on, rows are still shaped whole** -- a ligature depends on
  its neighbours -- and there **trailing blanks are never shaped**
  (`RowBuilder::trim_trailing_blanks`): `1234567` used to reach the shaper as
  7 characters and ~190 spaces, ~30 ms a frame under a flood, which made the
  release build 3-5x *slower* than debug at `seq 1 2000000`. Throughput now:
  `seq` 2M lines in ~0.68 s (Alacritty 0.63 s), coloured `ls` level with it.

### `src/workspace/` — the scrollable strip

Niri's model, not Zed's. **There are no tabs and no split tree.** A workspace is
one unbounded horizontal strip of columns; a column stacks its windows
vertically and every one of them is on screen. The strip is wider than the
window on purpose, and scrolls.

The deciding property, and the reason it is not a split tree: **opening a window
never resizes what you are looking at.** A new column lands right of the focus
and the strip simply gets longer; in a split tree every new pane steals space
from its neighbours.

- `strip.rs` — `Strip` is a `Vec<Column>`, a focus index and a scroll offset.
  No recursion, no tree, nothing to splice. `ColumnWidth` is a share of the
  viewport (⅓, ½, ⅔, full) rather than a pixel count, so a layout survives a
  window resize; dragging a seam pins that one column to `Fixed` pixels.
  **`scroll_to_focus` runs during layout**, because only then is the viewport's
  width known — `bounds` is recorded each frame by a `canvas`, the same escape
  hatch the old axis used. The one invariant is that the focused column is
  wholly inside the viewport; everything else about the scroll is left alone, so
  moving focus one column never jumps the view.
- `pane.rs` — **a `Pane` is a column.** It was already one in all but name: an
  ordered list of items and a focused index. What changed is that it draws them
  all, stacked, each under its own header, instead of one in front behind tabs —
  so `unsaved_files`, path dedupe, `close_items_of_kind` and the session format
  all kept working untouched. `ordinal` is the column's 1-based place on the
  strip, which the workspace sets (`renumber_columns`): a pane cannot know where
  it sits and has no reason to. Headers read `02` for a column of one and
  `02.2` for the second of a stack.
- `item.rs` — `PaneItem` is an enum, not a trait. Zed uses a trait so third-party
  crates can add tab kinds; with only two kinds ever planned here (terminals now,
  remote-file editors later) an enum stays fully typed for a fraction of the
  plumbing. **Dispatch is confined to that one file** so trading up to a trait
  later is a local change.
**Everything opens in a column of its own** -- Settings, editors, previews,
`^⇧T` (the file tree is the exception: docked beside the strip) -- through `Workspace::column_for_new_window`, which
reuses the focused column only when it is empty (a fresh workspace has one,
and opening beside it left a column of nothing on screen). Stacking is the
deliberate gesture: `^⇧D` and a header's "+". Empty columns are collapsed
*before* a new one is inserted -- the new one is empty too until it is filled,
and collapsing afterwards removed it, leaving its window off the strip. A
restored layout drops columns that come back empty, and opens a terminal if
nothing at all came back.

**With more than one column, none is wider than `Settings::max_column_width`**
(default 90%, `Strip::max_share`, set by the workspace each frame): a column
as wide as the window has its seam on the window's edge, where it cannot be
grabbed, and hides that anything is beside it. A lone column still fills.

**The strip never leaves the window part-empty** (`Strip::pixel_widths`):
when the columns add up to less than the viewport, the last one stretches to
the edge, and a lone column is that rule at its limit. The *last* column, not
all of them proportionally, so dragging any other seam still puts that edge
under the pointer. Derived, not stored: a stretched column keeps its own
width and glides back to it when another column arrives.

**Stacked windows resize at the boundary between them** (`Pane::resize_row`,
an 11px `RowSeam` band like the column seams). Each window's share is a flex
weight keyed by `ItemId`, so it follows the window through moves and closes;
the drag is absolute against the two windows' combined span. Weights are not
saved in the session file -- a restored stack comes back even.

**Dragging a window** by its header (`pane::WindowDrag`): dropped on another
window, the top or bottom half stacks it above or below in that column, and
the left or right quarter opens a new column on that side (`DropZone`; the
sides are the smaller target because a new column is the bigger change). The
pane only reports the drop (`PaneEvent::DropWindow`); `Workspace::drop_window`
re-parents the item with `take_item` / `insert_item`, so a shell survives the
move. The hover indicator follows the drag-and-drop rules below:
`on_drag_move` checks its own bounds, and `has_active_drag` going false clears
it.

**Window headers follow the design** (frame 02): `02`, or `02.2` for a
stacked window, the title, an unsaved dot, then the column's width share
(`1/3`, `1/2`, `FULL`, a percentage when dragged) on the first window or
`STACKED` on the rest, a boxed `FOCUS` tag, and a small `×` -- the one button
kept, so a window can be closed without knowing a shortcut; its click target
is a square the header's full height. It stops its own mouse-down from
reaching the header: the header is the window's drag handle, gpui starts a
drag after 2px of movement, and a slightly wobbly press on the `×` used to
pick the window up instead of closing it. A window whose content has a vertical
divider (Settings' sidebar, `PaneItem::header_divider`) has it continued up
through the header, so the line runs to the top of the window. The header has
no ground of its own and no hairline under it: the window paints one ground
behind header and content, so the header is simply the top of the window. The width label is pushed into each pane each
frame by `Workspace::sync_width_labels`, because the drawn share changes with
the viewport and no structural event announces it.

**This machine is the default host** (`Workspace::ensure_session`): a new
terminal, the file tree or Settings asked for with nothing connected starts a
local session first (restoring last run's local layout if there is one),
rather than opening onto a workspace with no host behind it.

**Headers go top or bottom, and show, auto-hide or hide**
(`Settings::header_position`, `header_mode`). `Pane::render` builds a
window's header once and puts it in one of three slots (`HeaderSlot`): before
the content, after it, or laid over it. Auto-hide is the overlay -- opaque
(`palette.bg`), invisible until hovered (`opacity` 0, `hover` 1) -- so a
header appearing never reflows the terminal under it. The content divider
carried through the header follows it to the bottom, and is dropped when the
header is not in the flow. Hidden, a window is closed and moved from the
keyboard.

**Keyboard focus is kept inside the workspace.** Every app shortcut lives in
the `Workspace` context, so focus that escapes it -- onto nothing, or onto a
closed view -- kills them all. `render` puts it back on the active window
whenever the window is active and the workspace does not contain it.

**Closing a column's last window moves the keyboard on** (`close_active_tab`,
the `Empty` handler): otherwise focus stays on a view that is no longer drawn
and every key after it goes nowhere.

- `mod.rs` — the `Workspace` root view. Panes never reach into each other: they
  emit `PaneEvent`s, and the workspace is the only thing that restructures the
  strip. Moving a window **re-parents the existing item** (`take_item` /
  `insert_item`); the terminal entity, and so the SSH channel behind it, is
  never recreated.

**Columns are positioned as fractions of the strip**, not pixels
(`Strip::render`'s `frac`). The pixel arithmetic can only use last frame's
width, so during a window resize every frame drew one resize behind and the
strip jittered against its edge; a fraction resolves against this frame's
layout. Preset columns land exactly; only a dragged (pixel) column can lag.

**The strip's numbers are final; only its drawing lags.** `scroll` and each
`ColumnWidth` change the instant something asks them to, and beside each one
is a `Glide` (`ui::motion`) that catches up over a frame or two. `render`
positions everything from the glides and nothing from the raw values, which
is what keeps the focused-column invariant from ever being a frame out of
date -- and makes switching animations off remove the lag and nothing else.

**`active_pane` and the strip's focus index must never disagree** — the strip
scrolls to the index while every action acts on the entity. `focus_pane` sets
both and is the only thing allowed to assign `active_pane`.

The gestures, all on ctrl+shift as usual:

| keys | what |
|---|---|
| `^⇧T` | new column, right of the focus |
| `^⇧D` | stack another window in the focused column |
| `^⇧W` | close the focused window |
| `^⇧←` `^⇧→`, `^⇥` `^⇧⇥` | focus a column; the strip does not wrap |
| `^⇧↑` `^⇧↓` | focus a window inside the column |
| `^⇧⌥←` `^⇧⌥→` | carry the focused column past its neighbour |
| `^⇧⌥↑` `^⇧⌥↓` | reorder a window inside its column |
| `^⇧O` | expel: push the focused window out into a column of its own |
| `^⇧U` | consume: pull the next column's top window into this one |
| `^⇧R` | cycle the focused column's width |
| `^⇧B` | open (or close) the file tree, docked beside the strip |
| `^⇧Space` | the overview: the whole strip at once |
| `^⇧K` | the command bar |
| `^⇧PgDn` `^⇧PgUp` | next / previous session |

`^⇧I` would have been the obvious partner for `^⇧O`, and is not usable: gpui
binds it to its own inspector in a debug build, and the inspector wins.

**The session file did not change shape.** A strip *is* one horizontal axis of
panes, so `snapshot_strip` writes exactly that and `flatten_member` reads a nest
of any depth back as a flat list of columns — a file from a build that still had
the split tree still loads. Widths ride in a new `widths` field rather than the
old `flexes`, because a flex weight of 1.0 meant "an equal share of this axis"
and a width share of 1.0 means "the whole viewport": reusing the field would
have restored every column full-width.

### The design's measurements (`ui/kit.rs`)

Every fixed number the redesign specifies lives here, and **nothing else may
spell one out**. A navigation bar is `bar()`, a window header `header()`, a
list row `row()`; the type scale is `micro()` (the tracked-out labels),
`head()`, and the body size the theme already carries.

**They are derived from the type, not constants.** `bar()` is `line_height +
26`, `header()` is `line_height + 8`, `micro()` is `font_size * 0.72` with a
floor -- so the chrome keeps its *proportion* when the font size changes
instead of its pixel size, and a 20px font does not leave a header with two
pixels of air in it. A constant that was right at one font size is wrong at
every other, which the home screen learned first.

`head()` (16/12 of the body) and `display()` (4x) are the design's heading
and numeral sizes, used by home and Settings only.

`spaced()` and `label()` are the other half: **gpui has no letter spacing**, so
the design's 0.18em tracking is spelled into the string, and `label()` is
uppercase plus tracking in one call. Display text only -- nothing is ever
parsed back out of a spaced string.

### Motion (`ui/motion.rs`)

**The strip is why this exists.** Columns are absolutely positioned and the
scroll offset used to be applied the instant it changed, so focusing the
column next door teleported the strip sideways: the picture before and the
picture after were both correct and nothing joined them, which left you
working out where you had just gone. A scroll you can *watch* is the whole
point of a strip longer than the window -- the motion is what says which way
it went and how far.

Three rules keep the module small, and breaking any of them is how an
animated UI turns into a buggy one:

* **Nothing animates state, only the drawing of it.** `Glide` holds a target
  that ordinary code sets and reads as a plain number; only `sample`, called
  while painting, lags behind it. So `scroll_to_focus`, the read-out, the
  session file and every invariant go on working in final values, and the
  focused column is inside the viewport from the frame you asked for it --
  not two frames later when the slide finishes.
* **Retargeting starts from where the eye is.** `Glide::to` begins the new
  leg at the *shown* value, not at the last leg's origin, so four quick
  presses of the focus key read as one accelerating slide rather than four
  fights over the same pixels.
* **Frames are asked for one at a time**, the loop the home screen already
  ran: whatever is still moving calls `request_animation_frame`, and the
  window goes back to painting only on change the moment nothing is. There is
  no timer and no global ticker, and a background window animates nothing.

**Anything under the pointer must `jump`, never glide.** A seam drag sets a
`Fixed` width every mouse-move; gliding to it would leave the edge trailing
the cursor by a frame, which reads as the app being slow rather than as the
app being smooth. `Column::snap` is that exception, and `resize_column` is
the only thing that sets it. A **window resize** is the same case one level
up -- it re-derives every column's pixel width at once -- so `Strip` keeps
`last_viewport` and snaps everything when it changes.

What moves: the strip's scroll offset and each column's width (`strip.rs`),
and the entrance of the two overlays (`Entrance`, in `command_bar.rs` and
`overview.rs`). A new column glides out from zero width, which is why
`Column::new` starts its glide there. Nothing else does, deliberately: the
terminal grid is the content, and content that slides around is content you
cannot read.

**Off is a setting, not a degraded mode.** `Settings::animations`,
`animation_ms` and `animation_curve` (Settings -> Tiling) are the
only inputs; with the switch off every `sample` returns its target and
`Entrance::progress` returns 1, which is exactly what the app did before this
module. So a page multiplies by the progress unconditionally and never asks
whether animations are on.

### Chrome: one navigation bar (`ui/nav_bar.rs`)

**There is exactly one bar, and nothing may add a second.** The app had three
pieces of chrome -- a title bar across the top, an icon rail down the left, a
status line across the bottom -- which is three edges spent on framing before
a single cell of terminal is drawn, and three places to look for the state of
one session. They are now one bar whose *edge* is a setting
(`Settings::nav_edge`, `Edge::{Left, Right, Top, Bottom}`): the same contents,
laid out along whichever side the user puts it on. `ui/title_bar.rs`,
`ui/icon_rail.rs` and `ui/status_bar.rs` are gone, and a new view that wants a
strip of its own along an edge is the thing this section exists to refuse.

`nav_bar::render` is a free function against the workspace, like the old status
bar: everything it draws already lives on the workspace or in a global, so a
view of its own would only add a subscription to keep in sync. The order along
the bar is mark, actions, separator, session state (or, from two sessions on,
one entry per session), strip read-out, a gap that drags like a caption,
disconnect, clock, window buttons -- identity, then what you can do, then
where you are, then the window itself.

**Shown, auto-hide or hidden** (`Settings::nav_mode`, laid out by
`Workspace::with_bar`, the one place both home and the workspace go through).
Hidden, the body takes the whole window, but a 4px `drag_region` strip stays
along the bar's edge: a window with no caption at all could not be moved.
Auto-hide uses the same strip to reveal the bar (`nav_revealed`), drawn *over*
the body -- in the flow, every reveal would resize every terminal -- and lets
it go on the first pointer move outside it. That is a window-wide
`on_mouse_event` registered from the bar's paint, **not** the bar's own
`on_hover(false)`: gpui only reports leaving after it has seen the pointer
inside, so a pointer that crossed the 44px bar in one move (a flick) left it
out for good. **The window buttons are optional**
(`Settings::window_buttons`): off, the separator before them goes too, and the
OS still closes the window through the same close guard.

* **`thickness` is derived from the type, not a constant** (`line_height +
  26px`), so the bar keeps its proportion rather than its pixel size when the
  font size changes -- the same rule the home screen's dials follow.
* **Vertical is not horizontal rotated.** gpui cannot rotate a `div` at all
  (only `svg()` and `img()` take a `Transformation`), so the two edges are two
  layouts: on a side, the state word is stacked a letter per line (`stacked`)
  and the window buttons run down instead of across. Anything that cannot be
  stacked legibly is simply not in the vertical bar.
* **The read-out is the strip, drawn small.** One mark per column, its length
  its share of the viewport, tinted by `Segment` -- accent for the focused
  column, `fg_muted` for the rest on screen, `fg_subtle` for those scrolled
  off. Each one is clickable and focuses that column. It is wrapped in
  `overflow_hidden` with a cap of 30% of the bar (40% vertical), because the
  strip is unbounded and the bar is not.
* **The actions are Lucide icons in the app's own hand** -- one weight, one
  size, muted ink, the accent only for the file tree while it is open:
  `Search` (command bar), `FolderTree`, `SquareTerminal`, `Settings2`.
  `Unplug` (disconnect) is not among them: it sits at the far end beside the
  clock (`disconnect_cell`), apart from the controls that open things. The design's plain shapes were tried and
  could not be read at a glance. Every one has a tooltip with its name and its
  *current* shortcut (`keymap::describe`, so a rebinding shows). On a side the
  clock is the `Clock` icon with the time as its tooltip.
* **Read-out marks click like cells, not 3px rules**: each mark sits in a cell
  the bar's full thickness across.
* **The clock is a way home**, like the logo: clicking it shows home.
* **The clock ticks on the minute, not on a timer.** `Workspace::tick_clock`
  sleeps exactly as long as is left of the current minute, so the digit changes
  when it should rather than up to a second late, and the window paints once a
  minute rather than once a second.

`title_bar::drag_region` and `title_bar::window_controls` moved here under the
same names; the home screen still borrows both, which is what keeps a window
that hides the chrome movable and closable.

The bar draws the caption itself (`appears_transparent: true` hides the system
one), and everything below is what that costs on Windows.

**`window_control_area` is not used here, deliberately.** It routes the region to
Windows through `WM_NCHITTEST`, which resolves against gpui's *cached* mouse hit
test — and that cache is only as fresh as the last client-area mouse move, so in
practice the OS asked before it was populated and drag, minimise and maximise all
did nothing. (Close appeared to work only because it had an explicit handler.)
The marker is also dangerous on a container with interactive children:
`mouse_hit_test.ids` holds the whole ancestor chain and the first match in paint
order wins, so a Drag marker on the bar's root claims every button in it too and
Windows eats their clicks as caption drags.

Minimise and maximise are plain mouse handlers (`minimize_window`,
`zoom_window`), which act from inside the client area where our own events are
authoritative. Close is not: it goes through `Workspace::close_window`, because
closing with a session live asks first.

**The close guard has two doors and one lock.** `Workspace::request_close`
decides, and both ways in call it: the navigation bar's X (hence `window_controls`
taking a `Context<Workspace>`) and every OS close — Alt-F4, the taskbar, a
shutdown — through `Window::on_window_should_close`, registered in
`Workspace::new`, where returning `false` vetoes it. It asks when a session is
live *or* any editor is dirty (`unsaved_files`); with neither, closing stays
instant. Confirming sets `closing` so the second pass sails through — without
that flag the veto fires again and the window can never close. Escape is bound
in the dialog's own `ConfirmClose` context, never `Workspace` — same rule as the
command bar, or vim loses the key.

**Confirming disconnects first, and waits for it.** `Connection::disconnect` is
fire-and-forget, which is right while the app keeps running but races process
teardown when it doesn't: the server would see a dropped socket rather than a
goodbye. `close_anyway` awaits `disconnect_and_wait` instead, capped at two
seconds by a `select` against a timer so an unresponsive server can't trap the
app open, and the dialog says "disconnecting…" while that happens.

**On Linux none of what follows applies**, and the trap is a different one.
`main.rs` asks for client-side decorations (gpui's default there is a system
titlebar *above* ours), `start_window_move` and a toggling `zoom_window` both
work -- but starting the move on mouse-down hands the pointer to the
compositor and the double-click is never seen. So the Linux arm of
`window_controls` arms on the press, starts the move on the first drag, zooms
on a double-click and opens the compositor's window menu on a right-click.
The resize border, which client-side decorations take away, is
`ui/decorations.rs`: a transparent `set_client_inset` margin, none on tiled
edges. Because that margin must stay unpainted, **on Linux the window ground
is painted by `decorations`, not `Root`** -- `apply_component_theme` clears
`Root`'s there.

**Two caption jobs gpui cannot do on Windows at all**, both failing silently —
see `ui/window_controls.rs`:

* `Window::start_window_move` is implemented for X11 and Wayland only; the
  Windows backend never overrides it, so it resolves to the empty default in
  `platform.rs`. That is why dragging the bar appeared dead while the buttons
  worked.
* `Window::zoom_window` maps to gpui's `zoom()`, which unconditionally calls
  `ShowWindowAsync(SW_MAXIMIZE)`. There is **no restore path**, so the button
  goes dead once maximised. The icon follows `window.is_maximized()`, which is
  why `nav_bar::render` takes a `&mut Window`.

**Both go through `WM_SYSCOMMAND`.** gpui's `handle_system_command` intercepts
only `SC_KEYMENU` and returns `None` for everything else, so `SC_MOVE`,
`SC_MAXIMIZE` and `SC_RESTORE` reach `DefWindowProcW` untouched.
`SC_MOVE | HTCAPTION` starts the OS's real caption move loop, and that is what
brings **genuine** Aero Snap: system snap zones, the system preview overlay, Snap
Assist, Snap Layouts, Aero Shake. An app-side imitation reproduces the geometry
but none of the shell integration, so don't rebuild it — this project had a
hand-rolled `ui/snap.rs` with its own zones and a layered preview window, and it
was deleted the moment the native route was found.

Everything is **posted**, never sent. The move loop is modal and would freeze
rendering until the drag ended; and dispatching `SC_MAXIMIZE` synchronously from
inside a gpui input callback re-enters `WM_SIZE`, which gpui misses — the frame
maximises while the renderer keeps drawing at the old size, leaving the desktop
showing through.

**Do not post `WM_NCLBUTTONDOWN` with `HTCAPTION`** — the usual custom-titlebar
trick, and a trap here. gpui intercepts it in `handle_nc_mouse_down_msg`,
re-injects it as a synthetic *client* `MouseDownEvent` at the same position
(which its own `click_state` scores as click #2) and returns `Some(0)` so
`DefWindowProc` never runs. One physical press then arrives twice, the second
indistinguishable from a double-click, and no move loop starts. Symptom: the
window maximises the moment you touch the bar and never drags.

**Do not test for a caption double-click with gpui's `click_count`.** The move
loop consumes the mouse-up, so gpui never sees the button released and its
counter never resets; the next single press claims `click_count: 2`.
`window_controls` applies Windows' own rule instead — `GetDoubleClickTime` and
the `SM_CXDOUBLECLK`/`SM_CYDOUBLECLK` rectangle — and consumes the stored press
on a match. (`GetDoubleClickTime` is under `Win32::UI::Input::KeyboardAndMouse`,
not `WindowsAndMessaging` where the rest of these live.)

### Home screen (`ui/home.rs`)

The design's launcher (Figma frame 06), and what the logo brings back. **The
navigation bar stays** -- it is the window's caption and buttons, and its
state word reads `IDLE` while nothing is connected -- and home is the body
beside it: `START` on the left (three numbered ways in: SSH, this computer,
settings, each with what it needs and what it would reach; SSH opens in
place into the saved hosts, newest first, and "new connection…", sliding
the other two down), `RESUME` on the
right (target, columns and windows, "closed HH:MM", the strip drawn small,
`SPACE CONTINUE`), then `TIME`: three graduated dials and a footer.

* **Each dial is one centre line** -- graduations, reading and unit label --
  and the three are a block centred in the panel, sized so it fits.
* **The dials slide at their own unit's rate** -- one graduation a second on
  the right, a minute in the middle, an hour on the left. `scale` is handed
  the live value, fraction and all; handing it `seconds / 60.` makes a dial
  that moves a graduation a minute.
* **They are laid out by arithmetic against their panel**, measured by a
  `canvas` into `Workspace::home_dials`. Home repaints every frame
  (`request_animation_frame` is the whole loop), so the measurement is never
  visibly a frame old. Graduations are filtered out of the bands where type
  is drawn (`Clear`), because text has no background to hide a mark behind.
* `Workspace::resume` builds the RESUME block: the live strip when connected
  (home is then a screen you step off with Escape), otherwise last run's
  layout read out of the pending session without restoring it. "Closed" is
  `session.json`'s modified time, captured by the read startup already does.
* **Startup budget:** the SSH entry's host is `Profiles::latest` (already in
  memory); the local entry's shell is the setting or `$SHELL`, and `user@host`
  is environment variables plus `gethostname` -- no file is read and no shell
  probed on the first frame. On Windows, with no shell set, the name comes
  from `local::default_shell` (pwsh before powershell before cmd), probed on
  a thread the first time home draws and picked up a frame or two later --
  `%ComSpec%` alone always said `cmd`.
* Space and Escape are bound in the `Home` key context, never `Workspace`, so
  they are not taken from a shell.
* **Connecting from home happens in place** (`Workspace::home_pick`,
  `HomeForm`): a saved host slides a field open under its row for the
  password or key passphrase; "new connection…" asks `user@host[:port]`
  first (`command_bar::parse_target`), then the password. One `InputState`
  serves every question and is cleared the moment `connect` is handed the
  secret, as in the command bar. The form stays up to show "connecting…" or
  the failure to retry from, and a failed session it opened is dropped when
  it folds. It has a `HomeForm` key context of its own, where Space is
  `NoAction` (a password character, not "continue") and Escape folds the
  form instead of leaving home; and home's per-frame focus grab stands down
  while it is open, or the new field loses the keyboard on its first frame.
  The x on a saved host forgets it (`forget_profile`). Home takes focus on render, checked first.

### Icons

`ui/icon.rs` — the Lucide set via the **`icons` crate** (rust-ui.com), with
`default-features = false`. That crate is built for Leptos and Dioxus and both
features are deliberately off: they generate framework components gpui cannot
use. What we take is its always-compiled `common` module, which exposes raw path
data as `&'static str`.

gpui's `svg()` resolves a *path* through the `AssetSource`, so the path is the
lookup key — `icons/<IconType name>.svg` — and `Assets::load` synthesises the SVG
markup from the registry on demand. Nothing is stored on disk and the whole
~1500-icon set is available; `Icon` is just an alias for `IconType`.
`Application::new().with_assets(Assets)` must be called or every `svg()` silently
renders nothing. The generated markup uses `currentColor`, so an icon takes its
tint from `text_color` like any glyph.

The **app logo** is `assets/logo.svg`, and it is the one source for both places
it appears. In the navigation bar it's drawn with `img(icon::logo())`, never
`svg()` —
gpui's `svg()` renders a one-colour alpha mask, so the logo would come out as a
flat silhouette. It is **a PNG, not the SVG**: `build.rs` rasterises a 128px
`logo.png` into `OUT_DIR`, compiled in with `include_bytes!`, because handed an
SVG `img()` renders it at the file's intrinsic size ×2 on every
launch — a 2000×2000 texture, filters and all, for an 18px spot.

**Never pass `img()` a bare asset name.** gpui treats a string as a URL if it
parses as an `http::Uri`, and `logo.png` does — as an authority, the way a host
name would — so it goes to the HTTP client, fails silently, and draws nothing.
That is exactly how the logo first came out blank (as SVG *and* as PNG).
`icon::logo()` sidesteps names altogether: it hands `img()` the PNG bytes as an
in-memory `gpui::Image`, built once in a `OnceLock`. For the
window, taskbar and Explorer icon, `build.rs` rasterises the same SVG (with the
resvg gpui already uses) into a multi-size PNG-entry `.ico` and links it through
MinGW's `windres` as icon resource **1**, the id gpui's Windows backend loads for
its window class. Replace the SVG and both update on the next build.

### File explorer (`ui/explorer.rs`)

**The file tree is docked beside the strip, not a window on it.** It was a
column for a while -- resized at its seam, scrolled past, carried along,
stackable -- and the user asked for it back as a panel: a tree is somewhere
you start from, and one that scrolls off the side or ends up under a shell
is one you have to go looking for. `^⇧B` toggles `Workspace::explorer_docked`;
`render_explorer_dock` draws it left of the strip at `explorer_width`, with a
hairline and a 7px grab band on its strip side (`ExplorerSeam`, measured
absolutely from the panel's own left edge). It is never scrolled, moved or
dropped into, and there is no `PaneItem::Files` any more. The session keeps it
in `Session::explorer` (open, path, width); a file from the column era carries
it as `SavedItem::Files` in the layout, which a restore reads as "docked, at
this path" and skips when rebuilding columns.

The dock paints the ground (`theme.bg`) and the explorer itself paints none,
so a translucent window still has one layer there. Its own footer
is the one thing it gained -- how many rows are here on the left, `LOCAL` or
`SFTP` on the right. Both are things you want while looking at a tree and
nowhere else, which is exactly why they are on the window rather than in the
navigation bar.

The context menu is drawn like every other list: a tracked header naming the
target, word rows at `kit::row` height, the raised ground and 2px accent rule
on hover, hairlines between groups, Delete in the danger colour; a click
anywhere outside closes it (`on_mouse_down_out`).

Drawn to frame 09: a `PATH` label and the path as a boxless input, `+`/`-`
expanders as characters, no file or folder icons, and the selected row
raised with a 2px accent rule. Its top row is exactly `kit::header` tall, so
its line runs level with the window headers beside it. The terminal icon
beside the path emits `ExplorerEvent::OpenTerminal`, and the workspace opens a
terminal column running `PtyProgram::ShellIn(path)` -- the shell started in
that directory: `current_dir` locally, ConPTY's working directory on Windows,
and over SSH `cd '<path>' && exec "$SHELL" -l` with the path single-quoted.

VS Code-shaped: an editable path bar over one flat scrolling list of rows
indented by depth, not a nested widget tree. Directory listings are cached in a
`HashMap` keyed by path, so expanding a folder inserts one entry and collapsing
keeps what was fetched — re-expanding is instant. A nested `Vec` structure would
have to be rebuilt on every expand. `rows()` flattens the expanded set
depth-first at render time.

**Three distinct gestures, deliberately kept apart:** the chevron (and only the
chevron) expands a folder in place — it calls `cx.stop_propagation()` or the row
handler underneath would also fire; single-clicking a row selects; double-clicking
a folder *navigates*, making it the new root, which is what gives the path bar
something to say. Double-clicking a file emits `OpenFile`, which the workspace routes
to an editor or a preview (see *Preview tabs*).

Rename and create share one input (`Editing`): renaming swaps a row's label
for it, creating inserts a blank row in the target directory. Creating inside a
collapsed folder expands it first, or the new row would appear somewhere off
screen.

Mutations go through `after_change`, which reloads just the affected directory
rather than the whole tree. Paste finds a free name by probing `exists` — pasting
into the folder you copied from would otherwise collide.

The context menu uses **`deferred(anchored(...))`**. `deferred` paints its child
after every ancestor, which is the only way it escapes the panel — laid out inside
a 280px column and a scrolling tree, an ordinary absolute div is clipped on both
axes. `anchored` positions in *window* coordinates (its default
`AnchoredPositionMode`), so the raw mouse position goes in untouched, and
`snap_to_window_with_margin` flips the menu back inside the window when the click
lands near an edge. This is the primitive to reach for for any popup that has to
overflow its container.

**`SftpSession::write` cannot create a file.** Despite the name, it opens with
`OpenFlags::WRITE` alone — no `CREATE` — so it fails with "No such file" on any
path that doesn't already exist. `create_file` uses `SftpSession::create`, which
sends `CREATE | TRUNCATE | WRITE`, and shuts the handle down explicitly rather
than relying on drop.

Transfers use `cx.prompt_for_paths` / `cx.prompt_for_new_path` for the native
pickers, and `ExternalPaths` for files dragged in from the OS file manager --
`drag_over::<ExternalPaths>` + `on_drop` is all that takes. Only files are
accepted: recursing a *local* directory tree is a different job from the
remote-side recursion in `sftp::copy`, and isn't built.

Transfers copy in 64 KB chunks rather than through `tokio::io::copy`, purely to
report progress — the helper returns nothing until it finishes, so a 200 MB
upload would sit at 0% and then jump to done. Each chunk bumps an
`sftp::Progress`, a pair of atomics shared with the caller as an `Arc`. Nothing
is pushed: the copy runs on the SSH runtime and the panel paints on gpui's, so a
channel would need draining on the far side. The panel *reads* the counters
while it paints, and since nothing notifies it when they move, `ExplorerView`'s
render asks for another frame while a transfer is live. A multi-file upload is
driven one file at a time from the view rather than in one background task, so
the line can name the file it's on ("3 OF 7"). `total` is 0 when the server
won't stat the file; the bar then stays empty rather than inventing a position.

**The progress strip sits under the header and outside the scroll.** It was the
tree's first child, which meant it scrolled out of sight the moment you looked
anywhere in a long directory -- a transfer is the panel's state, not a row in
it. Its shape is the design's: the name on the left, the reading on the right,
a rule joining them the way the two ends of a contents page are joined, and a
2px accent bar under all three. **The bar is the only thing that gives the
percentage**, which is why the words beside it do not; and the *name* is what
truncates when the panel is narrow, never the reading, because the reading is
the half that is moving.

`sftp::remove` and `sftp::copy` are recursive. SFTP has no recursive delete and no
copy verb at all: `remove_dir` only unlinks an empty directory, and a file copy is
a read/write stream between two open handles on the *server*, so the bytes cross
the wire twice but never touch this machine's disk. Async recursion needs the
boxed-future `Task` alias — `async fn` cannot call itself.

### Command bar (`ui/command_bar.rs`)

**One line and a list, and it is the only modal in the app.** `ui/connections.rs`
is gone: a connect dialog with labelled fields for host, port, username, auth
kind, key path and password was a form to fill in before you could reach a
shell, and four of those six fields were already known from the profile you
were about to click. The bar replaces it with what every other part of this app
is -- type, see what matches, press enter.

* **The query is parsed, not just filtered.** `parse_target` reads
  `user@host:port` (and `host`, `user@host`, `host:port`), so a machine you have
  never connected to is reachable by typing its name; `looks_like_path` spots a
  path and offers to open it instead. Both are pure functions with unit tests,
  because they are the two places where a typo turns into the wrong action.
* **Delete forgets a saved host**, and only on a `Profile` row. The old dialog
  had a button for it and a one-line bar has nowhere to put one; nothing else
  in the list is a thing you own, so nothing else answers to the key. The
  selection stays where it is rather than jumping to the top -- forgetting the
  third of five should leave you looking at the list.
* **A secret is asked for on the same line, and never stored.** One `InputState`
  serves as the query box and, once a target is chosen, as the prompt for a
  password, a key path or a passphrase (`Ask`, `Pending`). The field is masked
  for the two that need it and **cleared before the connect event is emitted**,
  so the secret exists for exactly as long as it takes to hand it to `russh` --
  the same rule as `store::SavedAuth` and `session.json`, applied to memory
  rather than to disk.
* **Reopening the bar focuses its query line** (`CommandBar::focus_query`),
  not the bar: a bar focused on its container looks open and ignores typing
  and Enter. It opens by itself after a disconnect, which is where this bit.
* The three things that made the *original* modal unusable are fixed and must
  stay fixed: it takes focus on its **first render** (a focus request in the
  constructor is dropped, because the window has no live focus tree yet, and
  Escape then goes nowhere), the backdrop uses `block_mouse_except_scroll` so
  clicks don't fall through to the terminal, and Escape / up / down are bound
  in the `CommandBar` key context, never `Workspace`, or they would be taken
  from vim.
* `ConnectionStatus` and the `Workspace::set_connection` contract did not
  change, which is why swapping the view in touched almost nothing above it.
  `CommandBar` **mirrors** connection state rather than reading it back out of
  the workspace: every transition goes through `set_connection`, which pushes
  the status into the bar *and* the connection into the explorer. Opening the
  bar only refreshes its status (`sync_connections_view`) -- it must not re-send
  the connection, because `ExplorerView::set_connection` resets the tree to the
  home directory. That keeps the view a leaf that emits intent upward
  (`CommandEvent`) and renders what it was last handed.

**Settings is a tab, the command bar is a popup, and both have been the other
thing** -- so don't "unify" them. Settings is a pane item: a singleton tab
raised by `Workspace::activate_existing`, which scans panes for a matching
`ItemKind` rather than stacking a second copy (`ItemId::next()` is one
allocator across all kinds, because a drag payload carries only an id and a
pane). Connecting is a short modal act and shouldn't cost a permanent tab.
`SettingsView` needs no events at all: every control writes through
`Settings::update` (see "Settings and theming" below).

Disconnect with other sessions open drops the whole session and shows its
neighbour (see *Sessions*). Disconnecting the last one closes its terminals,
editors and previews (`Pane::close_items_of_kind`) -- an editor left open
could never save again -- and a Settings tab, which is not tied to a host,
survives.

### Overview (`ui/overview.rs`)

`^⇧Space` shows the whole strip at once, one card per column. **The strip is
longer than the window on purpose**, and this is the only place it is ever seen
whole; pick a column, press escape, and nothing has been rearranged.

**It is not a switcher.** There is no z-order, no stacking and no
most-recently-used list, because position on the strip is the one thing there
is to remember and nothing here changes it. A switcher reorders itself as you
use it, so the thing you want is somewhere new every time; the overview is the
same picture every time you open it.

* Every column is drawn the same width, whatever its width on the strip. A
  column squeezed to a third would be the hardest card to read, which is
  backwards -- a narrow column is the one you are most likely to have lost.
* **Only a terminal gets a thumbnail** (`Emulator::preview_lines`: the last few
  non-blank screen rows as plain text, colour and attributes discarded). A
  terminal's content is the only thing you cannot name -- two shells on one host
  are identical from the outside. An editor, a preview and the Settings page are
  each fully described by the caption under the card, and drawing the file name
  inside it as well would be the same word twice.
* Lines are clipped at the card's edge (`overflow_hidden().whitespace_nowrap()`),
  never ellipsised. A thumbnail is a picture of a screen and a screen is cut off
  by its own edge too; a column of "..." would say only that the lines are long.
* The card stack is `flex_1`, never `h_full`: the card and its caption share the
  column's height, and a stack that took all of it pushes the caption off the
  bottom of the window.
* Like `home` and `nav_bar` it is a free function over the workspace, and like
  them it reads every column out (`collect`) **before** anything is drawn --
  reading a leased entity aborts the process.
* Escape, enter, left and right are bound in the `Overview` key context. They
  are the unqualified keys because nothing else has the keyboard while it is up.

### `src/store.rs` — profiles and settings

Connection profiles record host, port, username, auth *kind*, and key-file path.
**No secret is ever written to disk** — not passwords, not passphrases. Picking
a saved host (home's SSH list, or the command bar) asks for its password or
key passphrase on the spot and connects with it; nothing about a host lets it
skip that. Profiles are only recorded after a connection actually succeeds, so
the list never fills with typos, and home's x (or Delete in the command bar)
forgets one.

### `src/session.rs` — resuming last run

`session.json` beside the other stores: the strip's columns, the windows in
each and which had focus, the column widths, the explorer's directory, width and
open state, and the target it all belonged to (host, port, username -- **no
secret**, same rule as `store::SavedAuth`).

A **terminal saves nothing**. An SSH PTY has no state this side of the wire: the
remote shell died with the connection, taking its working directory, its history
and whatever it was running, so restoring one honestly means a fresh shell in
the same slot. Editors and previews save their path; Settings saves that it was
open.

Three rules keep a restore from doing harm:

* **Only onto the same target.** The paths in a layout are one machine's, so
  `Session::matches` compares host, port and username against the connection
  being made. Connecting somewhere else leaves the tabs closed.
* **Only into an untouched workspace.** `take_matching_session` refuses if
  anything is already open -- tabs opened while disconnected are real work, and
  a restore that threw them away would be worse than no restore.
* **Only once, and never over an unused file.** `pending_session` is taken at
  the first successful connect, and `save_session` refuses to write while it is
  still `Some`: otherwise the first frame after launch would replace last run's
  layout with the empty workspace that is about to load it.

The docked tree's saved directory (`Session::explorer`, or an older file's
`SavedItem::Files` window) is queued (`ExplorerView::restore`) *before* the
connection is published, because
`set_connection` leaves for the home directory the instant it has one.

**The save is driven from `render`, and that is not laziness.** `cx.observe` on
a pane never fires: gpui's `App::notify` takes the invalidation path for an
entity that a window renders and pushes no `Effect::Notify` at all, so nothing
observing a view is told -- which means closing a *tab* reaches the workspace
through no event whatsoever. `track_layout_changes` therefore runs at the top of
`Workspace::render` (before any pane is rendered -- reading a leased entity
aborts the process), hashes pane and item ids plus the divider weights, and
schedules a debounced write only when that changes. An unchanged layout costs a
hash and no allocation. Divider drags reach the file through the same hash;
`request_close` also writes synchronously, so the final state is never the one
the debounce was still holding.

### Settings and theming

`Settings` (`store.rs`) is everything on the Settings page, one JSON file.
`#[serde(default)]` on the *struct* is what lets an older or hand-trimmed file
still load — a missing field takes its `Default` value instead of failing the
whole parse. `clamp()` runs on load and on every change, so a hand-edited value
can't reach the renderer out of range.

**Every change goes through `Settings::update(cx, |s| ...)`**, which clamps,
saves, and calls `theme::apply`. `apply` rebuilds the `Theme` global from
scratch, re-maps gpui-component's theme, and refreshes every window. Nothing
edits `Theme` in place: views read it at render time, so replacing the global
*is* the live update. Settings that aren't visual are **reconciled where they're
used rather than pushed**: the terminal element calls `TerminalView::configure`
every prepaint (scrollback and default cursor shape via `Term::set_options`,
a no-op unless changed), the editor compares line numbers / soft wrap in its
render, the explorer filters dotfiles at render time. That is how a change
reaches every open terminal and editor without the Settings page knowing any of
them exist — the same trick as the font-size reflow, where terminals re-measure
their cell from the theme in prepaint and resize their remote pty to match.

**The page is drawn by `settings_view.rs`, to the design (frame 08)** -- not
gpui-component's stock `setting::Settings` element, which it used to be. A
section list (Interface, Appearance, Terminal, Tiling, Files, Editor, Keys)
with the chosen one raised and accent-ruled, a heading with its count, and
hairline rows of label, description and control. Nearly every control is a
**segmented** choice (on/off is a two-word one) or a **stepper**; only the
opacity slider and the colour pickers are gpui-component's. **The font list is
ours** (`SettingsView::font_picker`): gpui-component's Select paints its
dropdown with the theme's *window background*, which is transparent on Linux
and on any translucent window, so the list came out see-through. Ours is an
opaque `deferred(anchored(..))` list with a find line, each family drawn in
itself, closed by a click outside or Escape. Neighbouring boxes never overlap -- each gives up the edge
it shares rather than drawing over the next, or the chosen box's edge is
painted over -- and the stepper's minus and plus sit in an invisible 8px
margin that takes the click. Appearance lists the palettes as rows (chip
strip, name, light/dark, accent rule on the chosen one), the colours as a
compact grid with outlined swatches and hex, and wide content in `wide` rows.
Rows stack when the *page* (tab less sidebar and padding) is under 640px. Each control writes through `Settings::update`. It opens as
a FULL-width column, and below 640px the section list runs across the top.
The theme is chosen in one place, Appearance's palette list, whose first entry
is **System**: `theme = "system"` resolves to Paper or Ink in `theme::apply`
from the desktop's appearance, and the workspace re-applies on a change; `default_width` is what
a new column takes.

**Keys are rebindable** (`src/keymap.rs`, frame 12): every ctrl+shift command
is listed once with an id, group and defaults, and `keys.json` holds only the
user's changes. **A change appends, never clears** -- `clear_key_bindings`
would also wipe gpui-component's own -- by binding `NoAction` on the keys
given up and then the new ones; gpui ranks a later binding above an earlier
one in the same context. Recording uses `cx.intercept_keystrokes`, which runs
before any binding resolves, so the chord being recorded does not also fire.
A chord already held by another command swaps with it. `main.rs` keeps only
the bindings that are not the user's to change (dialog Escape, overview keys,
Tab through to the shell).

The font list is measured, not looked up: Windows has no "monospace" flag to
query, so `theme::monospace_fonts` keeps families whose `i` and `W` advance
alike. It resolves every installed font, so it runs once per Settings tab, not
per frame — and `theme::apply` caches the resolved family so it doesn't walk
the font list on every opacity-slider tick.

### `src/ui/`, `src/theme.rs`

`theme.rs` is closer to a terminal colour scheme than a widget stylesheet: three
background steps, a foreground ramp, and the 16 ANSI colours the emulator indexes
into. A `Palette` is those colours, opaque; `presets()` are the built-in
palettes, and **Ink and Paper come first in that list on purpose** -- they are
the redesign's own two palettes, taken token for token from the Figma file
(ground / recessed / raised, a three-step ink ramp, one hairline, one accent),
and Ink is what an unconfigured install opens on (`Settings::default`). They
are a matched pair rather than a dark theme and a light one bolted together:
Paper is the same drawing with the ramp inverted, which is why nothing in the
UI asks which of the two is in use. Every other preset is a guest in that
scheme; `Settings::colors` overrides individual tokens on top (keyed `bg`,
`accent`, `ansi3`, ...), so switching preset or resetting is just clearing that
map. `Theme` is the palette with window opacity applied to the three grounds
(`bg`, `surface`, `raised`) — text, borders and popups stay opaque — plus
`Theme::palette`, the opaque originals, for text drawn *in* a background colour
(the glyph under a block cursor) and for the colour pickers.

**Window transparency: every pixel is painted by exactly one translucent
layer.** gpui's DirectX renderer composites with premultiplied alpha, so
translucent paint shows the desktop; `window.set_background_appearance` picks
plain (`Transparent`) or acrylic (`Blurred`) behind it. But alpha *stacks* — a
70% panel over a 70% ground reads as 91%, i.e. solid — and there is no blend
mode that avoids it, so the fix is structural (`Theme::translucent`):

* `Root`'s `background` is transparent on a translucent window, so there is no
  window-wide ground at all.
* Each region paints its own, once: the navigation bar in `surface`; the
  explorer dock and each window (header and body together, `Pane`) in `bg`.
  **The strip paints no ground of its own on a translucent window** -- every
  window on it already did, and the strip is never left part-empty, so its
  ground was a second layer under every window (74% over 74% reads as 93%:
  windows came out near-solid beside a see-through bar). It keeps one on an
  opaque window, as the safety net. The 11px seam bands paint nothing — the
  columns either side of them already did, and a band with a ground would
  stack over one of them.
* Anything nested inside those paints no ground: terminal, editor, the Settings
  page, and gpui-component's `sidebar` / `group_box` (transparent when
  translucent). A new view that adds `.bg(theme.bg)` breaks this.
* Highlights (`raised`: selected row) stack on purpose.
* The one forced exception is the editor gutter: gpui-component paints it with
  the syntax theme's `editor.background`, *over* the text, because it doesn't
  clip text to the code area — that fill is what hides code scrolled sideways
  under the line numbers. It's pointed at our `bg` rather than made transparent.

On an opaque window stacking is harmless, and `Root` keeps painting the ground
as a safety net under any region that forgets to. The appearance change is
`cx.defer`red, because `apply` runs inside a window update and a window can't be
re-entered from its own update.

Every single-line field (command bar, home's connect field, explorer path bar,
rename/create row) is
gpui-component's `Input`, built through `ui::text_field::text_input`, which only
sets the size. It replaced a hand-rolled field that drew its text as plain
children — a long value ran out of the box over the buttons beside it, and there
was no selection or clipboard. Three things differ from what that field did:

* **Escape raises no event.** `InputState` just lets the key propagate. gpui then
  tries the next matching binding (so the `CommandBar` Escape still
  dismisses), and after that runs key-down listeners — which is how the explorer
  catches it, with an `on_key_down` on the div wrapping each field.
* **`set_value` needs a `&mut Window`.** `ExplorerView::navigate` is reached from
  `set_connection`, which has none, so the path bar's text is queued in
  `pending_path` and applied at the top of the next render.
* Focus goes through `InputState::focus`, not a bare `window.focus`, so the caret
  starts blinking.

## gpui drag-and-drop, the part that bites

`on_drag_move` is **not** scoped to the element it is declared on. gpui registers
it as a window-level mouse handler with no hover gating, so every listener is
told about every drag move anywhere in the window, and `DragMoveEvent::bounds`
is merely the declaring element's bounds — it is up to the handler to work out
whether the cursor is actually inside them. Clamping the relative position into
0..1 (the obvious-looking thing) makes every one of them believe the drag is
over its own left edge.

There is also no "the drag left me" event to pair with it, so hover-driven state
has no natural reset; `cx.has_active_drag()` being false is the cheap invariant
to clear it on.

`on_drop`, by contrast, **is** hover-gated (`hitbox.is_hovered`), so the two fire
under different conditions. Don't assume state set by one is valid to the other.

(This bit the old split tree hardest, where every pane previewed a drop. The
strip has it again: dragging a window by its header previews a `DropZone` on
whichever window it is over, by exactly these rules. The seams are the resize
case below.)

## Resize handles: use `on_drag_move`, never `on_mouse_move`

**`on_mouse_move` is gated on the element being hovered.** Only `on_drag_move`
is window-wide. This is the opposite of the drag-and-drop section's warning and
it is easy to get backwards — the two live side by side in `div.rs` and only one
has the hover check.

A resize handle is a few pixels wide, so any reasonably fast drag puts the cursor
past it and `on_mouse_move` stops firing: the resize appears to let go
mid-gesture. gpui's own doc for `on_drag_move` says it is "useful for
implementing draggable UIs that don't conform to a drag and drop style
interaction, like resizing" — that is the intended API. `ui/resize.rs` holds the
empty `NoPreview` view that `on_drag` demands, since these drags show nothing
under the cursor.

Two consequences of that route:

* `on_drag_move::<T>` fires on **every** element listening for payload type `T`,
  not just the one the drag started on, so the payload carries an identity
  (`SeamDrag { ix }`) and each handler ignores drags that aren't its own.
* There is no drag state to keep, and nothing to reset on release — gpui drops
  the payload on mouse-up.

**Compute the new size absolutely, never as a delta from the handle's own
position.** `DragMoveEvent::bounds` is the handle's bounds as of the *last
paint*, and several mouse moves can arrive before the next one. Correcting
against the same stale position each time overshoots, and the handle visibly
jitters under a fast drag. Every handle therefore measures against a
fixed reference: a column seam sets its column's width to `cursor.x` less that
column's own left edge, which `Strip::resize_column` works out from the strip's
recorded `bounds`, the scroll offset and the widths to the left of it. `canvas`
is gpui's escape hatch for reading laid-out bounds without writing a whole
custom element, and it is the only way the strip knows its own width.

(The docked file tree has the one other handle, `ExplorerSeam` on its strip
side, and follows the same rule: its width is measured absolutely from the
panel's own left edge, never as a delta. While the tree was a column for a
while it was resized at its seam like everything else; it is a panel again,
see *File explorer*.)

**Only the dragged column changes size.** Its neighbour is pushed along, not
squeezed — that is the whole difference between a strip and a split, and it is
why `resize_column` touches exactly one entry. (The old split tree instead
redistributed weight between the two members meeting at a divider; an even
earlier version guessed a `PX_PER_FLEX` constant and drifted.)

**One hairline per seam, and the whole seam grabs.** The line is its own 1px
element, drawn after every column -- not a column's right border, which it
was until columns were placed by fraction: a right edge could then round onto
the next column's first pixel, which painted its ground over the border, and
the line between two windows came and went with the widths. Seams are always
the plain hairline; focus is marked in the window header alone (its accent bar
and FOCUS tag), not by a full-height red rule beside the column. There is no edge tick any more for "the
strip continues past here": it was the same red as focus and read as a bug;
the nav bar's read-out says what is off screen.

**The grab band is 11px and `occlude`d** (column seams and the row seams in a
stack alike): without `occlude` the window underneath was hovered too, took
the mouse-down, and grabbed the focus while the seam started resizing.
**The seam on the focused column's left resizes the focused column**
(`Strip::resize_column`): its right edge stays put, its left edge follows the
pointer, and the left neighbour gives or takes the difference -- the one case
where a seam changes two widths, because a left edge cannot move otherwise. The window's outer edges come
from the navigation bar. Over that line sits the 11px
transparent band that is the entire hitbox — the neighbours are mouse-hungry (a
terminal reports clicks to the remote program, an editor starts a selection), so
a press a pixel off the seam lands in a column and reads as "it doesn't move".
The bands are painted after every column, or the column drawn next would cover
them.

## File Explorer's "Open with PulseTerm" (`src/shell_menu.rs`, Windows)

Settings -> Files -> *File Explorer menu*. Three verbs under
`HKEY_CURRENT_USER\Software\Classes` -- `*\shell` (a file), `Directory\shell`
and `Directory\Background\shell` (a folder, and the inside of one) -- so it
needs no administrator and touches no other account. Each runs this very exe
(`current_exe`) with the path as a bare argument, which `launch` reads as
"a file opens in the editor, a folder in a shell there" (`launch::Start`);
opening a file starts the local session *without* a terminal, since a shell
beside it is not what was asked. The switch shows `shell_menu::is_registered`
-- the registry, not the saved setting -- and `set` runs for off as well as on
(an `&&` the other way round once skipped the removal). Moving the exe means
switching it off and on to re-point it. On Windows 11 the verbs sit under
"Show more options": the new menu only takes packaged apps' handlers.

## Local mode: the same app pointed at this machine

`src/host.rs` is the seam. A `Host` is either an SSH connection or this
computer, and **everything above the transport takes one and never learns which
it got** -- terminals, explorer, editor, preview, the session file. Local mode is
not a second UI; it is the same one pointed somewhere else.

* `Files` (in `host.rs`) is an enum over `ssh::sftp::Sftp` and `local::fs::LocalFs`
  whose methods mirror each other exactly, down to `String` paths and
  `Result<_, String>`. The shared vocabulary -- `FileEntry`, `Properties`,
  `Progress` -- lives in `host.rs` rather than under `ssh`, because the local
  backend is just as entitled to it.
* **Path arithmetic is the one thing that genuinely differs.** Remote paths are
  POSIX whatever this client runs on; local ones are Windows'. So `Files::join`
  and `Files::parent` dispatch, and `ExplorerView` has its own thin wrappers --
  the free `sftp::join` it used to call would emit forward slashes into
  `C:\Users`.
* **Above a drive root is "This PC"** (`local::fs::THIS_PC`), a virtual folder
  whose entries are the drives (`GetLogicalDrives`, which touches no drive).
  Without it, Up stopped at `C:\` and the tree could only leave the drive by
  typing a whole path. A typed bare `D:` is normalised to `D:\`
  (`Files::normalize`, called from `navigate`): Windows reads `D:` as "the
  current folder on D", so it listed the wrong place and joined names on as
  `D:name`. Because the drives are listed as folders, the context menu offers
  Delete and Rename on them, so **every mutating `LocalFs` call refuses a
  drive root or anything under This PC** (`refuse_roots`) -- a recursive
  delete of `C:\` must not be one confirmation away.
* `src/pty.rs` holds `Pty`, `PtyEvent` and `PtyProgram`, which were under `ssh`
  and belonged to neither side; `src/runtime.rs` (the tokio bridge) moved up for
  the same reason -- local file IO uses it too.
* Local mode still passes through `ConnectionState::Connecting` for a frame
  rather than having a state of its own, so the navigation bar, the explorer and
  the session file need no second case.

**`local::conpty` is Windows' own pseudo-console**, which is the platform
primitive here: it gives the child a real console, translates its console API
calls into VT, and hands us bytes `alacritty_terminal` already knows how to
parse. Nothing in the emulator, the grid or the key encoder changes for a local
tab. The order Windows requires is not optional: two pipes, `CreatePseudoConsole`
over their far ends, an attribute list carrying the `HPCON`, then
`CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT`. Miss the attribute and the
child quietly gets a console of its own and the tab stays blank. The pipe ends
handed to the console **must** also be closed here, or nothing will ever close
them; and reads are a dedicated OS thread, because `ReadFile` on that pipe
blocks and has no async form.

**A ConPTY read does not end when the child does**, and that is the opposite of
a Unix pty. The write end of the output pipe belongs to the console host, not
to the shell, and the host holds it until `ClosePseudoConsole` -- so `exit` can
kill the shell, the process be gone, and `ReadFile` still block with no EOF and
no `PtyEvent::Closed`. A dead shell then looks exactly like a live one: no
`-- exited` in the column header, and keystrokes going nowhere with nothing to
say why. **The exit has to be waited for separately**, so `conpty::wait_loop`
is a second OS thread sitting in `WaitForSingleObject` on the child handle; the
reader ends a moment later, when the view drops the `Pty`, the pump drops the
`Console`, and `ClosePseudoConsole` finally closes the pipe for real. The pause
`wait_loop` takes before reporting is for the output the child wrote on its way
out -- the handle signals the instant the process is gone, and the reader may
still be a buffer behind.

**On Linux the same seat is `local::unix_pty`**, behind the one entry point
`local::open_pty`. Its read *does* end with the child -- the opposite of
ConPTY -- but only once our own copy of the slave fd is dropped, which `spawn`
does straight after the fork. The hangup is `killpg(pid, SIGHUP)` at the end of
`pump` (the reader thread holds a master fd of its own, so closing ours is not
the last close), and it shares a lock with the reap so it can never reach a
recycled pid. **A hangup can be lost**: bash's line editor swaps signal
handlers around every prompt, and a SIGHUP landing in that swap is delivered
and dropped -- the shell stays alive, idle and orphaned (caught by a test that
closes the tab right after output, about one run in seven). So a
`pulseterm-pty-hangup` thread repeats the SIGHUP after 0.5 s and sends SIGKILL
after 3 s, each step checking the reap under the same lock first. The `wait_loop` thread is still there: a background job can keep
the slave open after the shell has gone. See `LINUX.md` for the rest of the
port.

**Which shell is a setting** (`Settings::local_shell`), and the dropdown lists
only shells actually found on the machine -- offering one that isn't installed
produces a tab that opens and immediately dies. Empty means "best installed",
which is also the fallback when a configured path has since been uninstalled.
Read when a terminal is created, so a change reaches the next tab and leaves
running ones alone.

**A Microsoft Store PowerShell 7 lives nowhere in Program Files.** All it
leaves on disk is an app execution alias, `%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe`,
which CreateProcess runs like any exe -- so it is a candidate after the MSI
path, and the probe (`local::installed`) also accepts a successful
`symlink_metadata`, because `Path::exists` follows the alias and fails on it.
Without both, PowerShell 7 installed from the Store was simply not offered.

**No disconnect while it is the only session.** There is nothing to hang up:
local shells are this machine's own processes and die with their tabs. So the
bar's button and the popup's button are hidden (`Workspace::can_disconnect`)
-- until another session is open, when disconnecting closes the local one and
shows its neighbour (see *Sessions*). Closing the window asks nothing about a
local session, only about SSH hosts and dirty editors. The two transfer
actions keep working and rename themselves -- copying a file into a local
directory is a copy, not an upload.

A local session **resumes by itself at launch** (`session::SavedTarget::Local`):
there is nothing to authenticate and nothing to choose, so a popup in front of
it would only be a key to press before seeing last run's workspace. Connecting
somewhere else afterwards opens a session **beside** it (see *Sessions* below);
it no longer tears the local one down.

## Sessions: several hosts at once

This computer and any number of SSH hosts can be open together, one
**session** each, and the navigation bar switches between them. A session is
everything that belongs to one host: its strip, its file tree, its connection,
its remote desktop helper and its saved target.

**The session on screen lives in the workspace's own fields; the others are
`Parked`.** A switch (`swap_session`) trades the two sets of fields with
`mem::swap`, so the 2000-odd lines that say `self.strip` and
`self.connection` never learned there is more than one -- that was the
deciding argument against a `Vec<Session>` every method indexes into. A
parked session's shells keep running: nothing about a pty depends on being
drawn.

* **Late events find their own session** (`in_session`): a connect that
  finishes after the user switched away is applied to the session that asked,
  by swapping it in for the length of the closure. Pane and explorer events
  need no routing -- only what is drawn can be clicked -- and a pane event from
  a parked strip is ignored as stale.
* **Connecting while connected adds a session** (`new_session`, placed just
  after the current one in the bar). *This computer* goes to the local session
  if one is open: there is only ever one of it.
* **Disconnect ends just that session** and shows its neighbour
  (`drop_current_session`); with only one, it behaves as it always did.
  Disconnect is offered for local too once there is another session to go back
  to (`can_disconnect`). A connection that failed in a session of its own is
  removed when the command bar is dismissed.
* **Closing the window counts every session**: it asks when any SSH host is
  connected or any editor anywhere is dirty, and hangs them all up under one
  two-second cap (`live_hosts`).
* **One session looks exactly as before.** The bar shows its state block as it
  always did; from two on it shows one numbered entry per session (number,
  dot, kind and host along the top or bottom; number and dot on a side, the
  host in the tooltip), the one on screen in full ink with the accent rule.
  `Ctrl+Shift+PageDown`/`PageUp` cycle them.
* `session.json` still holds one layout: the session on screen, as the hash in
  `track_layout_changes` sees it.

## Startup: the budget, and where the time actually goes

Measured, not guessed (instrument `main` with `Instant` and read stderr; the
numbers below are from a 2026-11 run on this machine):

| phase | debug | note |
|---|---|---|
| gpui platform init, before our callback | 834 ms | DirectX device; **debug compiles the HLSL shaders at runtime**, release precompiles them with `fxc` |
| `gpui_component::init` | 18 ms | |
| `languages::register` -- 23 grammars | **0.5 ms** | the registry stores configs; queries compile on first use |
| `Settings::load` | 8.5 ms | |
| `Profiles::load` + `theme::apply` | 1.2 ms | |
| window + first frame | ~430 ms | |
| **total to activate** | **1.29 s** | |

Window-visible wall clock: **release 720 ms warm / 1258 ms cold, debug ~1670
ms.** Our own code is under 10 ms of the release figure. Two things follow, and
both are easy to get backwards:

* **Registering every grammar at startup is not a cost** -- half a millisecond.
  The expensive part of a language is compiling its queries, and that happens
  the first time one is used. Do not "optimise" the registration away.
* **The levers that remain are gpui's init and the size of the binary.** The
  500 ms gap between a cold and a warm start is paging in 34 MB of exe, most of
  it tree-sitter parse tables (CLAUDE.md's own note: 28 grammars were ~30 MB).
  Moving grammars out of the binary is therefore a *startup* change as much as
  a plugin one -- see `GrammarSource::Library`.

**The budget, for anything added later: nothing on the path to the first frame
may read more than one small file, and nothing may wait on a socket, a shell or
a scan.** Work that needs the disk starts on `cx.background_executor()` and
swaps its result in when it lands. The plugin catalogue is built to this rule
and is the worked example.

## Plugins (`src/plugins/`)

A plugin is **a folder with a `plugin.json` and its data beside it** -- queries,
an icon name, the extensions it claims. It is not code. Nothing in a plugin is
executed, so the worst a bad one can do is leave a file uncoloured; it cannot
fault a process that is holding SSH channels and a GPU device. That was the
deciding argument against the two alternatives: a cdylib ABI would put
third-party code in-process, and WASM would add a runtime of its own to a
binary already tuned for size with fat LTO.

The grammar still comes from the binary (`GrammarSource::Builtin`, resolved by
`languages::builtin_grammar`). A plugin *describes* a file kind and borrows a
parser that is already linked in. `GrammarSource::Library` is the declared
shape for a grammar shipped as a dynamic library beside the manifest -- not
wired up, and `load_language` says so in as many words rather than failing
somewhere later.

**Laziness is the design, not an optimisation.**

* Startup reads `.index.json` and nothing else: one small file listing what
  each plugin claims. A folder with a hundred plugins costs what an empty one
  costs, and that was measured -- twelve installed plugins moved a debug launch
  by about twenty milliseconds, flat in plugin count.
* The manifest, the `.scm` queries and the grammar are read the first time a
  file that plugin claims is actually opened (`languages::from_plugins`), and
  the result is remembered -- including a failure, so a broken plugin complains
  once rather than on every keystroke.
* Staleness is noticed by listing folder *names* and comparing, never by
  opening manifests. The rescan runs on `cx.background_executor()` and swaps
  the new catalogue in, so installing a plugin takes effect without a restart.

Two rules worth keeping: a query path containing `..` is refused, because a
manifest is data from outside and should not reach the rest of the disk; and
plugins resolve **before** the built-in table in `for_file_name`, so an
installed plugin can replace a built-in as well as extend the set -- a plugin
that fails to load falls through to the built-in instead of leaving the file
plain.

The folder is `Settings::plugins_dir` (empty, or a path that no longer exists,
means the default beside the other stores). Settings -> Interface -> Plugins folder is the picker.

## Entity leases: the rule that will bite you

An entity is **leased** for the duration of its own `Render::render`, and
`entity.read(cx)` on a leased entity panics with *"cannot read X while it is
already being updated"* — which aborts the process, so it is not a soft failure.

It is why `Workspace::track_layout_changes` is the *first* thing in
`Workspace::render`: it reads every pane for the layout fingerprint, and one of
them rendering first would already hold its own lease. It is also why the tab
strip, back when there was one, took an owned snapshot rather than the pane it
drew — it was called both from the navigation bar (pane free) and from `Pane::render`
(pane leased), and reading in both cases crashed the moment anything split.

`cx.emit`, by contrast, is **safe** from inside an update: it pushes onto
`pending_effects` and subscribers run after the lease is released. So event
handlers may freely `read` the emitting pane — `collapse_empty_columns` does,
and `Pane::take_item` relies on it when it emits `Empty`.

The general rule: inside a `Render` impl, reach for `&self`, never
`cx.entity().read(cx)`. If a helper needs data from the rendering entity, pass it
in.

## Things that look like bugs but are deliberate

- The grid is resized inside `prepaint`, which mutates view state during render.
  This is intentional and is what Zed's terminal does too — see `element.rs`.
- `TermSize::new` clamps to a minimum of 1x1. Zero in either axis is reachable
  transiently while a pane is laid out or dragged, and it panics inside
  alacritty's indexing.
- App shortcuts are all on ctrl+**shift**. Anything on plain ctrl would be stolen
  from the remote shell, where ctrl-c/ctrl-d/ctrl-z all mean something. The zoom
  keys are the deliberate exception — `ctrl-=`, `ctrl--`, `ctrl-0`, which every
  app has and no shell binds. Bound in the `Workspace` context they are
  dispatched before the terminal's key handler, so the remote side never sees
  them. (gpui parses a trailing `-` as the minus key, so `"ctrl--"` is valid.)
  They step `Settings::font_size`, which is global: an editor tab draws at
  gpui-component's `mono_font_size`, so a per-pane size would silently do
  nothing there.
- Background quads are only emitted where a cell differs from the window ground.
  On a mostly-empty screen that skips nearly every cell, keeping the quad count
  proportional to real content rather than to grid area.
- The trailing half of a double-width character (`WIDE_CHAR_SPACER`) is skipped
  when building a row. Emitting it would shift the rest of the row right.
- `display_iter` yields **buffer** lines, which go negative once you scroll back;
  the viewport row is `point.line.0 + display_offset`. Selections and the cursor
  are in buffer coordinates, cell painting is in viewport coordinates, and mixing
  them up renders scrollback above the top of the pane. `point_to_viewport` /
  `viewport_to_point` are alacritty's own converters for this.
- Mouse drag and release are gated on `reported_button`, not on hover. Once a
  program has been told about a press it must receive the matching release even
  if the cursor has left the pane, or it is left thinking the button is still
  down.
- Wheel ticks on the alternate screen with `ALTERNATE_SCROLL` become arrow keys
  (`mouse::alternate_scroll`). There is no scrollback to move there, and this is
  what makes the wheel work in `less` and `man`.
- `CommandBar` takes focus on its first *render*, not in its constructor. It
  is built from `Workspace::new`, before the window has a live focus tree, so a
  focus request there is silently dropped — and Escape then goes nowhere, which
  is what made the popup impossible to dismiss.
- Escape is bound in the `CommandBar` key context, never `Workspace`. Bound any
  wider it would be swallowed before reaching vim. Up and down are scoped the
  same way, for the same reason.
- Pane subscriptions are keyed by `PaneId` in a map and dropped when the pane
  collapses; the popup's is a single replaceable slot. Both were `Vec::push` at
  first, which leaked one entry per split/close and per connect.
- A dragged seam does not animate while every other width change does. The
  thing under the pointer has to be where the pointer is; see `Column::snap`
  in `ui::motion`'s section.
- The navigation bar's read-out jumps to the new column while the strip is
  still sliding to it. It is measured against the final scroll on purpose:
  the bar says where you are going and then the strip shows you going, which
  is the right order for the two to speak in.

## Code editor (`ui/editor.rs`, via gpui-component)

The editor is **gpui-component 0.5.1**'s `InputState` in code-editor mode —
rope-backed, with undo, selection, search, line numbers, IME and tree-sitter
syntax highlighting. `EditorView` owns only the *file*: its path, the text last
saved, and moving it over SFTP. Hand-writing an editor on raw gpui was the
alternative, and would have been the largest single piece of work in the project.

**Pinned to exactly `=0.5.1`, and it has to stay pinned.** It is the last release
built on crates.io `gpui` 0.2.2; from 0.6.0 gpui-component moved to `gpui-pre`, a
different crate, and two gpui crates in one binary are two incompatible
App/Window/Entity type families. Upgrading it means migrating the whole app to
`gpui-pre` first. (`cargo info` doesn't list dependencies; the crates.io sparse
index at `index.crates.io/gp/ui/gpui-component` gives every version's `gpui`
requirement in one fetch.)

**Languages are ours, not gpui-component's.** Its `tree-sitter-languages`
feature is all-or-nothing in 0.5.1 — 28 grammars, compiled in C by the MinGW gcc,
whose parse tables were ~30 MB of the release binary — so it is **off**, and
`ui/languages.rs` registers the grammars we keep (listed in `Cargo.toml`) through
`LanguageRegistry::singleton().register`. C#, Scala, Swift, SQL, Ruby and Elixir
were dropped at the user's request and open as plain text. Each kept entry
mirrors what the feature did: same grammar, same injected languages, and where
gpui-component used query files of its own they're copied verbatim under
`assets/queries/` (Apache-2.0, `NOTICE.md` there). A unit test compiles every
query against its grammar, because a bad one doesn't fail at runtime — the file
just opens uncoloured.

Two things the feature used to do that must now be done by hand:

* **Aliases.** gpui-component resolved `rs`, `py`, `markdown-inline` … through
  `Language::from_str`, which is gated by the same feature. Each alias is
  registered as a name of its own.
* **Unknown names fall back to JSON.** `LanguageRegistry::language` retries
  through that same (now JSON-only) table, so an unregistered name gets the JSON
  grammar and query. `languages::for_file_name` therefore only ever returns a
  registered name, or `text` — a highlight-free entry registered for exactly
  this. Don't hand the editor a raw extension.
* **Some upstream queries are fragments.** tree-sitter's C++ highlights file
  opens `; inherits: c`, and TypeScript's `; inherits: javascript` — a line the
  *editor* is expected to act on, not something the query carries. Passed alone,
  C++ coloured `virtual` and `concept` and left `if`, `while`, `return` and every
  type plain (this is how it looked before, feature or no feature: gpui-component
  passed the same fragment). So `cpp` is the C++ query with C's appended, and
  `tsx` uses the self-contained TypeScript query rather than the crate's
  fragment. Order matters: the highlighter keeps the *earlier* pattern when two
  match the same token, so the specific language goes first and the inherited
  base after. Check any newly added language for that header.

(`Cargo.lock` still carries `cc 1.2.67`, pinned when the SQL grammar needed
`cc ~1.2.1`. That grammar is gone, so the pin no longer matters; gpui's `cc ^1.2`
is happy either way.)

Four integration rules, each read out of gpui-component's source rather than its
docs:

* **The window root must be `gpui_component::Root`.** The editor calls
  `Root::read` while painting, and that `expect`s it. `main.rs` wraps the
  `Workspace` in one.
* **`Root` paints from gpui-component's own theme** — background, text colour
  and font — across the whole window, so `theme::apply_component_theme` maps our
  palette onto it on every `theme::apply` (first straight after
  `gpui_component::init`). Its `font_size` is set to our font size ÷ 0.875:
  `Root` uses it as the window's rem size, and gpui-component sizes its text in
  rems (`text_sm` = 0.875rem), so that ratio makes its labels, inputs and menus
  exactly our font size. Left at the default 16px, the Settings page ignored the
  font-size setting entirely. The cost is that rem-based spacing (`p_1`, `gap_2`,
  ours included) scales with the font size too — the UI grows and shrinks as a
  whole. `mono_font_size` and `mono_font_family` are set for the editor tabs.
  The line-height setting reaches non-grid text the same way: the workspace root
  sets it as a relative (inherited) line height.
* **`Root` binds `tab` / `shift-tab` to focus cycling**, and gpui dispatches a
  matched binding *before* an element's own key handler, stopping once handled —
  which would take Tab away from the shell. `main.rs` binds both to `NoAction` in
  the `Terminal` context, and `tab` in `CommandBar` too, where Tab switches a new
  host from password to private key (without it the caret jumped to the file
  tree's path box and the key path was typed there). Bindings made with `KeyBinding::new` carry no `meta`,
  which gpui's matcher treats as user-defined, so the deeper `NoAction` cancels
  `Root`'s binding and the key falls through to the terminal's encoder.
* **gpui-component fetches its own icons through our asset source**, in
  kebab-case (`icons/chevron-down.svg`). `ui::icon::resolve` accepts that as well
  as our PascalCase, plus `close` → `X`, the one name it spells differently from
  Lucide. Without it the editor's search bar renders blank.

Its `Input` context binds `ctrl-shift-e` (select to end of line). That once
shadowed a Split Down command; stacking is `^⇧D` now and nothing of ours is on
`ctrl-shift-e`, so the binding takes nothing away. Check any new app shortcut
against gpui-component's `Input` bindings, which win while an editor has
focus.

Files load whole and are refused over 5 MB or when not UTF-8 — a binary opened as
text is destroyed by the first save. Save truncates and rewrites in place: SFTP v3
`rename` won't replace an existing file, so write-then-rename would need a delete
in between and stop being atomic anyway. Dirtiness is a comparison against the
last-saved text rather than a keystroke flag, so undoing back to the saved state
leaves the tab clean, and `is_dirty` is what both close guards count.

**A tab with unsaved changes asks too, and the pane is not the one asking.**
Every tab close funnels through `Pane::close_item_at` -- the X, middle-click,
ctrl-shift-w -- so the check lives there alone; but dialogs belong to the
workspace, so the pane emits `PaneEvent::ConfirmClose` and removes nothing until
it is told, through `force_close_item`. That takes an `ItemId`, never the index:
the dialog outlives the frame that raised it and the tab can be dragged to
another slot, or another pane, while it is up. `close_items_of_kind` (the
disconnect path) bypasses the guard deliberately -- those tabs are terminals,
and the connection they belong to is already going.

"save and close" writes first and closes on `EditorEvent::Saved`, with the
dialog saying "saving…" in between; `SaveFailed` dismisses the dialog and leaves
the tab open, its own error banner explaining and the text still there to retry
from. Closing first and letting the write finish behind it would be a lie in the
one case that matters. Both dialogs share `close_focus` and the `ConfirmClose`
key context -- only one is ever rendered, and the window's question outranks a
tab's.

## Preview tabs (`ui/preview.rs`)

A read-only tab showing one remote file: Markdown rendered through
gpui-component's `TextView::markdown` (which highlights fenced code with the
same tree-sitter set the editor registers), or an image drawn with `img()`.
Images are fetched with `sftp::read_bytes` and handed to `Image::from_bytes`
with the format picked from the extension; gpui contain-fits them by default, so
the pane scales the picture without distorting it. Capped at 20 MB, because the
bytes become one texture.

* **A tab of its own, not a mode of the editor.** `ItemKind::Preview` is
  deduplicated by path like an editor, but a preview *and* an editor of the same
  file coexist — different kinds — which is what lets you split source against
  rendered output. It is also why the tab uses `Icon::Eye` rather than the
  file's own icon: two tabs of one file need telling apart.
* **Routing lives in the workspace, not the explorer.** Double-clicking a file
  emits `OpenFile` as before; the workspace sends images to the preview (the
  editor refuses non-UTF-8 anyway) and everything else, Markdown included, to
  the editor. `PreviewFile` is the explicit ask from the context menu, whose
  "Preview" entry appears only when `preview::can_preview` says so.
* **A double-click replaces the file window you have; a middle-click opens
  another** (`workspace::Place`). `OpenFile` is `Place::Reuse`: the new file
  takes the place of the focused window if that is an editor or a preview,
  else of the file window opened last (`last_file`), through
  `Pane::replace_item` -- in place, because taking the only window out of a
  column would collapse it. Browsing a tree file by file no longer leaves a
  window behind per file. `OpenFileNew` (middle-click on a row, or "Open in
  New Window" at the top of a file's menu) is `Place::New`, as are the
  command bar, "Preview" and the editor's Preview button -- each a deliberate
  "beside this". **An editor with unsaved changes is never the one
  replaced**; the file opens in a new window instead. A file already open is
  still just focused.
* Extensions are read from the **last path segment**: a directory called
  `site.png` must not make every file inside it look like an image. There's a
  test for exactly that.

## Remote desktop (`remote/`, `src/remote.rs`)

`REMOTE_DESKTOP.md` is the design and the log; its *Where it stands* section
is the current picture. In short: an SSH-only nav icon (`Ctrl+Shift+M`)
starts **`pulseterm-remote`**, a separate GPL-3.0 program -- one SDL3 window
(a setup screen, then the stream with a sidebar on the side its setting picks, right by default -- docked
behind a strip in a window, laid over the picture in full screen) built
on moonlight-common-c and FFmpeg, streaming from **PulseTerm's own Sunshine**
on the host. Things to know before touching it:

* **PulseTerm never links it.** It is found at run time (beside the exe, then
  `PATH`) and the icon is hidden without it. It has no SSH of its own:
  PulseTerm serves it a JSON pipe (`src/remote.rs`) of exec, port-forward and
  long-lived spawned commands on the existing connection (`src/ssh/exec.rs`).
  It gets the theme as JSON at launch and links none of PulseTerm's code.
* **Building it** needs `git submodule update --init --recursive`
  (`remote/third_party/moonlight-common-c`), CMake, clang, and the FFmpeg,
  SDL3, libva, OpenSSL and Opus development packages -- on Fedora RPM
  Fusion's FFmpeg; `REMOTE_DESKTOP.md` has the list and a `dnf swap` trap
  that removes the graphics driver.
* **A host with no monitor gets a fake one** (`remote/src/monitor.rs`): an
  EDID offering every mode, the chosen one preferred, handed to NVIDIA's X
  driver (`ConnectedMonitor` + `CustomEDID`; on/off restart the login) or to
  the kernel's debugfs `edid_override` on a free port (AMD, Intel; live).
  Root goes through `pulseterm-monitor`, installed once with a sudoers rule
  for that file only -- the helper has no text field to ask for a password.
  It is the user's own desktop that streams, not a second one: a separate
  virtual screen (sway, Xvfb) was built and dropped. `REMOTE_DESKTOP.md`,
  *Fake monitor*.
* **Nothing secret is stored.** Pairing is redone every launch with a PIN
  written to Sunshine's stdin over SSH (`-0`), against an instance started
  with `-1` and its own config/`XDG_CONFIG_HOME`, so the user's Sunshine is
  never touched. The one exception to "nothing on this machine changes" is
  opt-in: a keyboards-only udev grant for the full keyboard takeover.
* **Testing:** with this machine as its own host, clicks stop arriving while
  Sunshine runs (its virtual devices) and pointer positions move this
  machine's own cursor -- test input against a second machine.
  `cargo test -p pulseterm-remote -- --ignored` pairs with a real Sunshine.

## Not yet ported from RemoteTTY

Drag-a-window-out-to-a-new-OS-window, and the TTY1-4 `conspy` console shortcuts
(`PtyProgram::Command` is already wired for the last of these — only the UI to
trigger it is missing).

Moving a window between columns with the mouse **is** built now: see
"Dragging a window" under the workspace section.
