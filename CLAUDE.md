# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## What this is

PulseTerm: a GPU-rendered SSH terminal for Windows, built on [gpui](https://gpui.rs)
(Zed's UI framework). It talks to a stock `sshd` — terminal sessions are SSH PTY
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
- `cargo check` — type-check. Do this after any `src/**.rs` change.
- `cargo build --release` — optimised standalone `target/release/PulseTerm.exe`.
  Release is tuned for size (`[profile.release]`: strip, fat LTO, one codegen
  unit), so it is much slower to build than debug and rebuilds every crate when
  those settings change. It also needs **`fxc.exe`**: release builds compile
  gpui's HLSL shaders ahead of time (debug compiles them at runtime), and gpui
  only looks in `GPUI_FXC_PATH`, then `PATH`, then the `10.0.26100.0` kit. Here
  the compiler is `C:\Program Files (x86)\Windows Kits\10\bin\10.0.28000.0\x64\fxc.exe`,
  so if the build stops with *Failed to find fxc.exe*, set `GPUI_FXC_PATH` to it.
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

Rust here is the **GNU** target (`stable-x86_64-pc-windows-gnu`). There is no
Visual Studio and no MSVC linker on this machine — the MinGW toolchain from
`BrechtSanders.WinLibs.POSIX.UCRT` is the whole story for compiling and linking.
The Windows Kits *bin* tools are present, though, and release builds use one of
them (`fxc.exe`, see Commands).

Zed's own docs require MSVC for gpui on Windows, but gpui 0.2.2 **does** build,
link, and run on windows-gnu — this was verified before the project was started,
with a minimal window under the DirectX renderer. Don't switch the default
toolchain to MSVC on the assumption that it's required; it isn't, and MSVC isn't
installed. `~/.cargo/bin` is not on `PATH` in a fresh shell here.

`russh` is pinned to `default-features = false, features = ["ring", "flate2",
"rsa"]` — the `ring` crypto backend is pure Rust; the default `aws-lc-rs` one
needs a C toolchain that doesn't exist here.

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
- `known_hosts.rs` — TOFU pinning in `%APPDATA%/PulseTerm/known_hosts.json`, not
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
  Each frame walks the grid once and emits one shaped line per row plus one quad
  per run of same-coloured background. **The grid is resized from `prepaint`**,
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
  into the emulator.

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
- `mod.rs` — the `Workspace` root view. Panes never reach into each other: they
  emit `PaneEvent`s, and the workspace is the only thing that restructures the
  strip. Moving a window **re-parents the existing item** (`take_item` /
  `insert_item`); the terminal entity, and so the SSH channel behind it, is
  never recreated.

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
| `^⇧B` | open (or close) the file tree as a column of its own |
| `^⇧Space` | the overview: the whole strip at once |
| `^⇧K` | the command bar |

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
`animation_ms` and `animation_curve` (Settings -> Window -> Motion) are the
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
the bar is mark, actions, separator, session state, strip read-out, a gap that
drags like a caption, clock, window buttons -- identity, then what you can do,
then where you are, then the window itself.

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

What the app opens on, and what the bar's house button brings back: three
graduated dials standing on end, over on the left of the window, with the
reading where they cross the middle.

```text
    --  10   --  45   --  40
    -        -        -
    05    :  45    :  05
    -        -        -
    --  05   --  40   --  35
```

They are scales, not rows of dashes: a graduation per unit, long and numbered
every fifth, short between. **Each slides at its own unit's rate** -- one
graduation per second on the right, per minute in the middle, per hour on the
left -- so the seconds dial visibly runs, the minutes dial creeps, and the hours
dial is all but still. That is the clock, not decoration.

`scale` is handed the live value, fraction and all: its whole part names each
graduation and its fraction is how far the scale has slid, so one function does
all three dials. Handing it a value already divided by its period
(`seconds / 60.`) makes a dial that looks broken -- it was the first thing
tried, and it moved a graduation a minute.

Three details of the drawing:

* **The geometry comes from `window.viewport_size()`**, not from constants, so
  the dials and the type keep their proportions rather than their pixel sizes.
  The date is centred under the reading in a box exactly as wide as it -- but
  only while it fits; a longer line is left-aligned instead, because centring
  it would hang it off both ends and the left end is the one that falls off
  the screen.
  Column centres are computed rather than laid out, because the graduations are
  absolutely positioned and have to line up with digits that are not.
* **The ways in are a column at every size**, right-aligned in the corner.
  Abreast they were a line across the window that grew with the font and had
  to be measured against the clock every time; stacked they are a corner, and
  the corner is where the eye already is. Below ~900px wide the tracking comes
  off and the type steps down, which is all `compact` decides now.
* **Graduations are filtered, not clipped**: off-screen ones are dropped, and
  so is any that would fall in the band the reading and the date occupy
  (`Clear`). Text has no background to hide a mark behind, and there are two
  such patches: the reading with its date, and the block in the corner. Both
  are measured from the type -- monospace makes that a multiplication -- since
  a constant that was right at one font size put a dial through the text at
  another. They also fade with distance from the middle, but only so far,
  because a hairline at a fifth opacity is a smudge, not a graduation.
* **gpui has no letter spacing**, so the wide tracking is spelled into the
  string (`spaced`). Display text only; nothing is ever parsed back out of one.

(An earlier take on the same reference drew two huge dials off the left edge
with rotated radial dashes. If that is ever wanted back: only `svg()` and
`img()` can be rotated in gpui -- `Transformation` does not exist for `div` --
and Lucide's `Minus` rotated to the radius is what a tick was made of.)

**It takes the window rather than floating over it.** `Workspace::render`
returns early while `home_open`, so the navigation bar and every pane simply
stop being rendered -- that is what makes it
somewhere to *be* rather than a panel to look past. A session behind it keeps
running; nothing about a pty depends on being on screen.

Two things survive the hiding, deliberately: an unpainted strip along the top
that drags like a caption, and the three window buttons in the corner
(`nav_bar::drag_region` and `nav_bar::window_controls`, made `pub(crate)`
for this). A window that cannot be moved, minimised or closed is not minimal,
it is stuck.

* It is a free function against the workspace, like `nav_bar`: everything it
  draws already lives on the workspace or in a global, so a view of its own
  would only add a subscription to keep in sync.
* **It animates, so it asks for a frame at a time.** `home::render` ends with
  `request_animation_frame`, which is the whole loop: the seconds ring sweeps,
  the minute tape slides, and the moment home stops being rendered nothing asks
  again and the window goes back to painting only on change. (A one-second
  timer came first and was right for a clock that only counted; it is wrong for
  one that moves.)
* Space is bound in a `Home` key context, never `Workspace`, for the same
  reason Escape is scoped to the dialogs: bound wider it would be taken from
  the shell. Home takes focus on render, checked first -- focusing
  unconditionally would steal it back on every tick.
* `continue_session` is honest about what it can do. A local session goes
  straight back; a remote one cannot, because no password is stored, so it
  comes up on that host with the form filled in. Escape returns to a running
  session and does nothing when there is none, rather than dropping you on an
  empty workspace.
* Dismissing the command bar with nothing open returns to home, which is
  where it was opened from.

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

**The file tree is a window on the strip, not a side panel.** It used to be a
fixed slab pinned between the rail and the columns, with a width of its own, a
resize handle of its own and an open/closed flag of its own -- three mechanisms
to say what a column already says. Now `^⇧B` opens (or closes) a column
holding `ItemKind::Files`: it is resized at its seam like every other column,
carried along the strip like every other column, stacked under a terminal if
you want it there, and saved in the layout as `SavedItem::Files` rather than in
three fields beside it. `Workspace::explorer_open` answers the question by
scanning the strip, because the strip is now the only place the answer lives.
It lands leftmost (`open_leftmost`, at `ColumnWidth::Third`), which is where a
side panel used to be -- the habit was worth keeping even when the mechanism
was not.

So it paints no ground and draws no border: the column body already painted
one, and a second layer doubles a translucent window's opacity. Its own footer
is the one thing it gained -- how many rows are here on the left, `LOCAL` or
`SFTP` on the right. Both are things you want while looking at a tree and
nowhere else, which is exactly why they are on the window rather than in the
navigation bar.

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
something to say. Double-clicking a file emits `OpenFile`, which nothing consumes
yet.

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

Disconnect closes terminal tabs only (`Pane::close_items_of_kind`); a Settings
tab is not tied to a session and survives.

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
**No secret is ever written to disk** — not passwords, not passphrases. A profile
click prefills the dialog and focuses the password field; it deliberately stops
short of connecting. Profiles are only recorded after a connection actually
succeeds, so the list never fills with typos.

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

A saved `SavedItem::Files` window has its directory queued
(`ExplorerView::restore`) *before* the connection is published, because
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

The page is gpui-component's `setting::Settings` element — resizable section
sidebar, search, grouped rows. Its fields are `(get, set)` closure pairs over
`&App`; `settings_view.rs` wraps them as `switch` / `number` / `dropdown`
helpers bound to one `Settings` field each. Controls that need their own entity
(a `ColorPickerState` per colour, the font `SelectState`, the opacity
`SliderState`) live on `SettingsView`. Pickers keep their own copy of the value,
so a theme switch or reset calls `sync_pickers`; `ColorPickerState::set_value`
does **not** emit, so that can't bounce back as a pile of overrides.

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
* Each region paints its own, once: the navigation bar, the explorer,
  and every window header in `surface`; each window body and the strip's own
  ground in `bg`. The 7px seam bands paint nothing — the columns either side of
  them already did, and a band with a ground would stack over one of them.
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

Every single-line field (connect form, explorer path bar, rename/create row) is
gpui-component's `Input`, built through `ui::text_field::text_input`, which only
sets the size. It replaced a hand-rolled field that drew its text as plain
children — a long value ran out of the box over the buttons beside it, and there
was no selection or clipboard. Three things differ from what that field did:

* **Escape raises no event.** `InputState` just lets the key propagate. gpui then
  tries the next matching binding (so the popup's `Connections` Escape still
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
strip has one drag left — a column seam — and it is the resize case below.)

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
jitters under a fast drag. The one handle left therefore measures against a
fixed reference: a column seam sets its column's width to `cursor.x` less that
column's own left edge, which `Strip::resize_column` works out from the strip's
recorded `bounds`, the scroll offset and the widths to the left of it. `canvas`
is gpui's escape hatch for reading laid-out bounds without writing a whole
custom element, and it is the only way the strip knows its own width.

(There were two. The explorer had a handle of its own, measured against
`nav_bar::inset_left`; the file tree is a column now, so it is resized at its
seam like everything else and both the handle and that helper are gone -- one
gesture for the whole strip instead of a special one for a panel.)

**Only the dragged column changes size.** Its neighbour is pushed along, not
squeezed — that is the whole difference between a strip and a split, and it is
why `resize_column` touches exactly one entry. (The old split tree instead
redistributed weight between the two members meeting at a divider; an even
earlier version guessed a `PX_PER_FLEX` constant and drifted.)

**One hairline per seam, and the whole seam grabs.** A column draws the line on
its own right edge and nothing draws one on its left, so a seam is one hairline
and never two; the focused column tints its own. The window's outer edges come
from the navigation bar. Over that line sits a 7px
transparent band that is the entire hitbox — the neighbours are mouse-hungry (a
terminal reports clicks to the remote program, an editor starts a selection), so
a press a pixel off the seam lands in a column and reads as "it doesn't move".
The bands are painted after every column, or the column drawn next would cover
them.

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

**Which shell is a setting** (`Settings::local_shell`), and the dropdown lists
only shells actually found on the machine -- offering one that isn't installed
produces a tab that opens and immediately dies. Empty means "best installed",
which is also the fallback when a configured path has since been uninstalled.
Read when a terminal is created, so a change reaches the next tab and leaves
running ones alone.

**No disconnect, deliberately.** There is no session to hang up: local shells
are this machine's own processes and die with their tabs. So the bar's button
and the popup's button are both hidden (`Workspace::is_local`),
and closing the window asks nothing unless an editor is dirty. The two transfer
actions keep working and rename themselves -- copying a file into a local
directory is a copy, not an upload.

A local session **resumes by itself at launch** (`session::SavedTarget::Local`):
there is nothing to authenticate and nothing to choose, so a popup in front of
it would only be a key to press before seeing last run's workspace. Connecting
somewhere else afterwards tears the local one down first -- its tabs cannot
follow it to another machine.

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
means the default beside the other stores). Settings -> General has the picker.

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
  the `Terminal` context. Bindings made with `KeyBinding::new` carry no `meta`,
  which gpui's matcher treats as user-defined, so the deeper `NoAction` cancels
  `Root`'s binding and the key falls through to the terminal's encoder.
* **gpui-component fetches its own icons through our asset source**, in
  kebab-case (`icons/chevron-down.svg`). `ui::icon::resolve` accepts that as well
  as our PascalCase, plus `close` → `X`, the one name it spells differently from
  Lucide. Without it the editor's search bar renders blank.

Its `Input` context binds `ctrl-shift-e` (select to end of line), which shadows
our Split Down while an editor has focus. Deliberately left alone: that's the
editor doing editor things, and Split Down still works everywhere else.

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
* Extensions are read from the **last path segment**: a directory called
  `site.png` must not make every file inside it look like an image. There's a
  test for exactly that.

## Not yet ported from RemoteTTY

Drag-a-window-out-to-a-new-OS-window, and the TTY1-4 `conspy` console shortcuts
(`PtyProgram::Command` is already wired for the last of these — only the UI to
trigger it is missing).

New with the strip, and not built either: **moving a window between columns
with the mouse.** The keyboard has it — expel, consume, and carrying a column
along the strip — and the plumbing a drag would need (`take_item` /
`insert_item`, which never recreate the entity) is already what those use.
