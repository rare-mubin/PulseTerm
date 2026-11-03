# Remote desktop — design

Status (2026-11-03): **streaming works.** One SDL3 window, Sunshine started,
paired and launched over the existing SSH connection, video, audio, keyboard
and mouse -- tested against `orion` (NVIDIA Jetson, Ubuntu, X11) and
`cachyos` (CachyOS, niri on Wayland, NVIDIA). *Where it stands* below is the
current picture; the dated sections under *Progress* at the end are the
history, kept for the reasoning and the traps.

Decided with the user (2026-11-02):

* **Hosts** are mostly Linux, Wayland or X11.
* **Goal** is smooth video and gaming, not just admin access.
* **Agent:** Sunshine, the open-source Moonlight/GameStream host. A
  lightweight agent on the host is acceptable for this feature only.
* **Entry point:** an icon in PulseTerm's navigation bar, shown on SSH
  sessions only.
* **The client is its own window**, detached from the main PulseTerm window,
  drawn in PulseTerm's design language, with its own config. The main app stays
  as it is.

Decided next (2026-11-02), following the recommendations below:

* **D1 → (a):** a separate GPL-3.0 helper program, `pulseterm-remote`, linked
  with `moonlight-common-c`; the main app keeps its own licence.
* **D2 → (a):** FFmpeg (libavcodec) with VAAPI/NVDEC hardware decode.
* **D3 → re-pair every launch:** the pairing key lives in memory only, for
  as long as the Remote window is open; nothing is stored, so the "no secret
  on disk" rule holds unchanged. PulseTerm runs **its own Sunshine instance**
  with its own config and credentials, so the user's Sunshine setup is never
  touched (see 2.4).
* **Toolkit:** the streaming window is **SDL3**, not gpui -- see 2.7.
  *Superseded 2026-11-03 at the user's request:* the gpui launcher and the
  separate stream process are gone; the whole helper is **one SDL3 window**
  in one process, its setup screen drawn in the design language by a small UI
  of its own (`remote/src/ui`). gpui is not a dependency of the helper.

---

## Where it stands (2026-11-03)

**Built and verified on real hosts**

* **PulseTerm side:** an SSH-only nav icon / `Ctrl+Shift+M` starts the helper
  (found beside the exe or on `PATH`; hidden when absent) and serves it over a
  JSON pipe -- exec, port forward, long-lived spawned commands -- on the SSH
  connection it already has (`src/remote.rs`, `src/ssh/exec.rs`).
* **Host checks** in one round trip: Sunshine and how to install it (exact
  command per distro: COPR on Fedora, the right `.deb` on Ubuntu/Debian,
  LizardByte's own pacman repo on Arch and derivatives), session type,
  displays, capture route, hardware encoder (NVENC only with
  `libnvidia-encode`; Jetson and the NVIDIA driver/library mismatch called
  out), uinput, an active firewall (the ports to open, for the client's
  network), port clashes.
* **PulseTerm's own Sunshine**: own config dir and `XDG_CONFIG_HOME`, `-1`
  (no saved pairings), `-0` (PIN on stdin), X11 capture on X11 and KMS on
  Wayland when permitted (no portal dialog), a wrapper on an SSH channel so it
  dies with the window or the connection, a clean stop-then-start.
* **Pairing** in Rust over SSH-forwarded ports: nothing to type, nothing
  stored, a wrong PIN refused (an opt-in test against real Sunshine).
* **Streaming**: moonlight-common-c (git submodule), FFmpeg VA-API decode,
  SDL3 window, Opus audio with an adaptive, rate-steered buffer.
* **One window**: setup screen; while streaming the picture fills it, an arrow
  revealed at the right edge (or `Ctrl+Alt+Shift+S`) opens a sidebar with
  readings (fps, received Mbps, network/host/decode ms, audio queue), the
  controls and the settings.
* **Settings, mostly live**: resolution, fps, codec, bitrate (rejoin in a
  second or so); audio here/host/both; full keyboard; mouse desktop/game;
  pointer host's/both; scroll as here/reversed; latency normal/low; host CPU
  (encoder speed, threads, error correction -- Apply restarts Sunshine).
* **Input**: full keyboard while the window is focused and the pointer is on
  it (an evdev grab, after a one-time keyboards-only udev grant; otherwise
  the compositor's shortcut inhibit); mouse with drags clamped to the picture,
  nothing left held on the host, coalesced motion, smooth scrolling.

**Next, in the order agreed**

1. ~~Zero-copy decode~~ -- **done**: decode 10.4 → 5.3 ms a 1080p HEVC
   frame against cachyos, colours checked (*Zero-copy display* under
   Progress).
2. ~~Resolution from the window~~ -- **done**, checked against cachyos
   (*Resolution "window"* under Progress).
3. ~~Choosing the host's display~~ -- **done**, checked against cachyos
   (*Choosing the display* under Progress).
4. **Windows port of the helper** (keyboard grab, font lookup, build) -- it
   has only ever been built on Linux.

Smaller: live theme updates (the theme is handed over once, at launch); the
shifted-port case (written, untested); the pointer lock on GNOME/KDE/X11
hosts; the shifted ports' firewall rule.

**Not planned** (the user does not need them): gamepads, and recognising a
VPN (Tailscale, WireGuard) in the firewall check. The *Gamepads* switch was
removed from the settings; the input check still notes that Sunshine needs
uinput for them, as a fact about the host.

Read `CLAUDE.md` first; the design language (`ui/kit.rs`, Ink/Paper, hairlines,
no radius, no shadow, monospace grid) applies to the new window unchanged.

---

## 1. Shape of the thing

```
 PulseTerm (main window)                 PulseTerm Remote (own window)
 ───────────────────────                 ─────────────────────────────
 nav bar ─ [remote desktop] ──open──▶    host status · config · connect
   │                                       │
   │ SSH connection (already open)         │ Moonlight protocol (UDP, LAN/VPN)
   ▼                                       ▼
 ┌──────────────── remote Linux host ─────────────────┐
 │  sshd ── exec: probe / install hint / start / pair │
 │  Sunshine ── capture (KMS/wlroots/portal/X11)      │
 │           ── encode (VAAPI / NVENC)                │
 │           ── input (uinput: kbd, mouse, gamepads)  │
 └────────────────────────────────────────────────────┘
```

Two different paths to the host, on purpose:

* **Control goes over SSH**, the connection PulseTerm already has: detecting
  Sunshine, starting it, setting its credentials and pairing all happen through
  SSH exec channels and an SSH-forwarded connection to Sunshine's web API on
  `127.0.0.1:47990`. Nothing about Sunshine's web UI is exposed to the network,
  and the user never opens it.
* **The stream goes direct**, over Moonlight's own UDP ports. SSH carries TCP
  only; tunnelling video through it adds head-of-line stutter that defeats the
  point. So the host must be reachable on those ports: same LAN, or a VPN such
  as Tailscale/WireGuard for remote use. The window says so plainly when the
  ports cannot be reached.

## 1.1 Two programs, built separately

PulseTerm and the helper are two programs in one Cargo workspace:

| | package | binary | licence | built by |
|---|---|---|---|---|
| main app | `pulseterm` (repo root) | `PulseTerm` | the app's own | `cargo build` |
| remote desktop | `pulseterm-remote` (`remote/`) | `pulseterm-remote` | GPL-3.0 | `cargo build -p pulseterm-remote` |

* **`default-members = ["."]`** in the root `Cargo.toml`: plain `cargo build`,
  `run`, `check` and `test` touch PulseTerm only. SDL3, FFmpeg and
  moonlight-common-c are dependencies of `remote/` alone, so someone who only
  wants the terminal never needs them installed. `--workspace` builds both.
* **Nothing in PulseTerm depends on the helper at compile time.** No cargo
  feature, no shared code linked in: PulseTerm launches it as a process and
  passes what it needs (host, SSH-forwarded ports, config path) on the
  command line or a pipe. That is also what keeps the GPL on the helper's side.
* **Found at run time:** beside the PulseTerm executable first, then on
  `PATH`. Not found, the nav bar icon is simply not shown -- a PulseTerm built
  alone looks exactly like PulseTerm before this feature. Built later and
  dropped beside it, the icon appears on the next SSH session; no rebuild of
  PulseTerm.
* **Shared design code: none -- the theme is handed over.** PulseTerm passes
  its *resolved* tokens (palette with the user's overrides, font, size) as
  JSON in `--theme` (`remote::theme_handoff`); the helper's `theme.rs` reads
  them and re-derives the `kit` measurements from the type the same way. So
  the helper matches whatever palette is in use and links none of PulseTerm's
  code. Without a handoff it falls back to Ink, and a font that is not
  installed falls back down PulseTerm's preference list -- gpui silently
  turns a missing family into a proportional one.

## 2. Components

### 2.1 Nav bar icon (main app)

* Lucide `MonitorPlay` (or `Cast`), in the actions group, **SSH sessions only**
  -- hidden in local mode and while disconnected, like disconnect, and hidden
  when the helper is not installed (1.1).
* Tooltip: `Remote desktop  <shortcut>`; a rebindable command
  `window.remote` in `keymap::COMMANDS`.
* Click: open (or raise) the Remote window for **this** host. One Remote window
  per host.

### 2.2 SSH exec (main app, `ssh/`)

New: `Connection::exec(cmd) -> Result<ExecOutput { status, stdout, stderr }>`,
an exec channel that collects output and closes -- the one-shot counterpart of
`ssh::pty`. And `Connection::forward(remote_port) -> local stream`, a
`direct-tcpip` channel for reaching `127.0.0.1:47990` on the host.

### 2.3 Host probe and control (main app or Remote, over SSH)

All read-only until the user acts:

| check | how | why |
|---|---|---|
| Sunshine installed, version | `command -v sunshine; sunshine --version` | offer install steps per distro if missing |
| running | `pgrep -x sunshine` / `systemctl --user is-active sunshine` | start/stop button |
| session type | `loginctl show-session $XDG_SESSION_ID -p Type`, `$WAYLAND_DISPLAY`, `$DISPLAY` | decides capture method and its caveats |
| GPU encoders | `vainfo`, `nvidia-smi -L` | warn when only software encoding is available |
| input permission | `test -w /dev/uinput`, group membership | gamepads/keyboard need it; show the one-time udev fix |
| another Sunshine running | `pgrep -a sunshine`, its ports in `ss -lnu` | stop it for the session, or shift our `port` |
| a display to capture | outputs present (headless host?) | headless needs a virtual display or dummy plug |

Control actions: start/stop PulseTerm's own Sunshine instance (a detached
process with its own config, see 2.4), set its web credentials, and pair. The
user's own Sunshine install and its config are never modified.

### 2.4 Pairing, and PulseTerm's own Sunshine instance

Moonlight pairing is a client certificate plus a 4-digit PIN entered on the
host. Normally the user types the PIN into Sunshine's web UI; here PulseTerm
does it: it generates the PIN itself and posts it to Sunshine's `/api/pin`
over the SSH-forwarded web API. The user sees nothing to type.

**Re-paired on every launch; nothing is stored.**

* Each time the Remote window connects, it makes a fresh client certificate
  and key **in memory**, pairs, streams, and on close **unpairs** that client
  through the web API. Costs about 1-2 s per connect.
* A crash skips the unpair, so pairing first removes any leftover client named
  `pulseterm-*` from the paired list. The list never grows.
* Pairing needs the SSH connection, which every entry point already has (the
  icon is SSH-only), so nothing is lost by not remembering.
* A copied data directory or a stolen laptop holds nothing that can connect.

**As built (2026-11-02) -- supersedes the credential plan below where they
differ.** `remote/src/sunshine.rs`:

* Config in `~/.config/pulseterm/sunshine/`, and `XDG_CONFIG_HOME` pointed at
  `~/.config/pulseterm` when it runs: Sunshine keeps files outside its config
  (the portal's restore token, `apps.json`) in `$XDG_CONFIG_HOME/sunshine`,
  which would otherwise be the user's own folder. **Even `sunshine --version`
  creates `~/.config/sunshine`**, so the probe runs it against a temp dir.
* Started with `-1` (no state loaded or kept -- every launch has no paired
  clients, so there is nothing to unpair) and `-0` (PIN from stdin).
* **`-0` pairing verified (2026-11-02)** with the real Moonlight client
  (`moonlight pair 127.0.0.1 --pin 4321`) against this exact instance and
  wrapper: the PIN written into the wrapper's FIFO -- in the product, the SSH
  channel (`Bridge::write`) -- completed pairing, *"Pairing completed
  successfully"*, with no web credentials anywhere. Sunshine prints
  `Please insert pin:` once at start-up, not per request; a PIN written
  before the client's request arrived was not used, so **our client writes
  the PIN after it has sent the first pairing request** (it waits on the
  server during that phase anyway). After the test no state file existed:
  `-1` kept nothing. So the web API, its credentials and the forwarded
  47990 port are not needed for pairing at all.
* It runs under a wrapper on a **spawned channel** (`Connection::spawn`):
  stdin is a FIFO fed by `cat` from the channel, and when `cat` sees EOF the
  wrapper kills Sunshine. Verified: closing the Remote window stops it, and
  so does `kill -9` on PulseTerm (sshd closes the dropped session's stdin).
* The graphical session's environment comes from `systemctl --user
  show-environment` (an SSH session has no `WAYLAND_DISPLAY`).
* **Capture needs nobody at the host when Sunshine may use KMS.** The probe
  checks `getcap` for `cap_sys_admin` on the binary (Fedora's package and the
  .deb set it; a Flatpak cannot), and then the config pins `capture = kms`:
  the screen is read from the driver, with no portal, no dialog and no
  `WAYLAND_DISPLAY` -- verified over SSH from an empty config, ready in under
  a second. Without it Sunshine picks for itself: wlroots screencopy (niri,
  sway, Hyprland) and X11 never ask either; GNOME and KDE fall back to the
  **screen-sharing portal, whose dialog appears once on the host's screen**
  and Sunshine waits for it (the token is then remembered). The launcher's
  *Capture* check says which route a host will take, and offers the
  `setcap` command that switches it to KMS. The ready check waits up to 90 s
  for the portal case.
* **For a host with no keyboard or mouse at all**, KMS covers capture and
  `uinput` covers input, but three things are still the host's business:
  someone has to be logged in graphically (enable auto-login), a display has
  to be active (a monitor or an HDMI dummy plug; KMS captures what the GPU
  scans out), and NVIDIA needs `nvidia-drm.modeset=1` for KMS.
* Base port 47989, or 48089 when the user's own Sunshine (or anything) holds
  it. There is no "keep running": an instance started with `-1` forgets its
  pairings anyway.

**PulseTerm's own Sunshine instance.** Pairing goes through Sunshine's web
admin API, which needs its admin username and password. Setting those with
`sunshine --creds` on the user's own install would overwrite a password they
chose. So PulseTerm never touches the user's Sunshine config:

* It starts Sunshine with a config of its own, under
  `~/.config/pulseterm/sunshine/` on the host (`sunshine.conf`, state, logs),
  passed as Sunshine's config-file argument.
* Its web credentials are random, generated over SSH when the instance is
  started, and held in memory only; a new instance gets new ones.
* **Ports:** two Sunshine instances cannot share Moonlight's ports. If the
  user's own Sunshine is already running, the probe says so and the window
  offers either to stop it for the session or to run ours on a shifted base
  `port` (Sunshine derives every port from that one setting).
* The instance runs while the Remote window is connected and is stopped when
  it closes, unless "keep running" is set in the config.

### 2.5 The Remote window

A second gpui window, its own process-level state, same theme engine:

* **Before connecting:** host header (name, distro, session type, GPU,
  encoder), a checklist of the probe results with fixes inline, the config,
  and one action: *connect*.
* **While streaming:** the video edge to edge; a thin bar along an edge (the
  design's one bar) with stats (fps, latency, bitrate, dropped frames),
  capture toggle, disconnect. Hidden in fullscreen, shown on the release key.
* Config (per host, `remote.json`, same rules as `settings.json`):
  resolution (match window / native / fixed), fps (30/60/90/120), bitrate,
  codec (H.264 / HEVC / AV1), HDR, audio on/off, gamepad passthrough, which
  display, auto-start Sunshine, capture-release chord.

### 2.6 Streaming client

The Moonlight protocol: HTTPS pairing (47984/47989), RTSP setup (48010),
ENet control stream (47999), RTP video with Reed-Solomon FEC (47998), RTP Opus
audio (48000), input over the encrypted control stream.

* **Protocol:** see decision D1.
* **Video decode:** see decision D2. Hardware decode is required for 4K/120.
* **Rendering:** decoded frames drawn into the window each frame -- see risk R1.
* **Audio:** Opus decode, output through SDL3's audio.
* **Input:** SDL3 key, mouse and gamepad events translated to Moonlight
  packets (relative mouse while captured). While captured, *every* key goes to the host, PulseTerm's
  ctrl+shift shortcuts included, except the release chord (default
  `ctrl+alt+shift+q`, configurable).

### 2.7 Toolkits: gpui for the launcher, SDL3 for the stream

The helper is two processes from one binary:

```
PulseTerm ──▶ pulseterm-remote            (gpui: status, config, connect)
                    │ connect
                    ▼
               pulseterm-remote stream    (SDL3: video, input, audio)
```

* **Why not gpui for the stream:** it has no pointer lock, and its image path
  is unproven for 60–120 fps video.
* **Why SDL3:** it is what Moonlight's own client is built on, so the hard
  parts are a known path: relative mouse with pointer lock on Wayland
  (`zwp_pointer_constraints_v1`) and X11, gamepads with rumble, audio output,
  vsync'd presentation, and an EGL/GL context into which FFmpeg's VAAPI frames
  are imported zero-copy as dmabufs. zlib-licensed.
* **Why two processes:** gpui and SDL each want to own the main thread's event
  loop. Separate processes keep both simple; the launcher hands the stream its
  host, pairing and config on the command line or a pipe.
* **Design language in the stream window:** its one UI element, the bar
  (stats, capture, disconnect), is drawn in the same tokens -- Ink/Paper
  colours, hairlines, the user's monospace font via SDL_ttf -- just not by
  gpui. Fullscreen hides it; the release chord shows it.

Considered and not chosen: winit + wgpu (pointer lock is there, but zero-copy
import of decoded frames into wgpu is hard -- a copy through system memory is
fine at 1080p60 and risky at 4K/120); GStreamer sinks (good video, no input or
pointer-lock story).

## 3. Decisions (resolved -- kept for the reasoning)

**D1 — the protocol library, and the licence.** Sunshine and Moonlight are
GPL-3.0, including the client protocol library `moonlight-common-c`.

* **(a) A separate helper program**, `pulseterm-remote`, GPL-3.0, linked with
  `moonlight-common-c`, launched by PulseTerm as its own process. The Remote
  window *is* that process -- which matches "a different window detached from
  the app" naturally. The main app keeps its own licence as long as the two
  only talk as separate programs (arguments/IPC). The shared design code
  (theme, kit) would need a licence compatible with both. **Recommended.**
* **(b) Link `moonlight-common-c` into PulseTerm** -- simplest code, but
  PulseTerm as a whole becomes GPL-3.0.
* **(c) Reimplement the protocol in Rust** -- no GPL, months of work, and
  every protocol change in Sunshine is ours to chase.

(Not legal advice; if the licence matters commercially, check with someone
who does this for a living.)

**D2 — the video decoder.**

* **(a) FFmpeg (libavcodec)** with VAAPI/NVDEC: covers every GPU and codec.
  Large dependency, and Fedora's default `ffmpeg-free` omits H.264/HEVC --
  users there need RPM Fusion's build.
* **(b) `cros-codecs`** (ChromeOS's Rust VAAPI decoders: H.264, HEVC, VP9,
  AV1): no FFmpeg, pure Rust over libva; Intel/AMD natively, NVIDIA only through
  `nvidia-vaapi-driver`.
* **(c) GStreamer**: flexible, also a large dependency, plugin availability
  varies by distro.

Recommended: **(a)** for coverage, with (b) worth a look if the dependency
weight hurts.

**D3 — persisting the pairing key** (see 2.4): keyring, file, or re-pair
every launch. Chosen: **re-pair every launch** -- with pairing automated over
SSH it costs a second or two and no typing, and it keeps every secret out of
storage. Its one complication, needing Sunshine's admin password, is solved
by running PulseTerm's own instance rather than the user's.

## 4. Risks to settle first

* **R1 — drawing video.** Resolved by 2.7: SDL3 + EGL dmabuf import, the
  Moonlight path. Still worth a one-day spike: FFmpeg VAAPI decode of a test
  clip into an SDL3 window at 120 fps, measured, on Intel/AMD and on NVIDIA.
* **R2 — pointer lock.** Resolved by 2.7: SDL3 relative mouse mode. Spike it
  on niri, GNOME and KDE (Wayland) and on X11.
* **R3 — host permissions.** KMS capture needs `cap_sys_admin` on the Sunshine
  binary; portal capture shows a consent dialog on the host; `/dev/uinput`
  needs a udev rule. All one-time, all explainable from the probe -- but they
  are on the host, outside our control.
* **R4 — headless hosts.** No monitor, nothing to capture: needs a virtual
  output (compositor headless output) or a dummy plug.
* **R5 — network reach.** UDP ports must be reachable; over the internet that
  means a VPN or port forwarding, which PulseTerm can detect but not set up.

## 5. Phases

0. **Spikes** (days): an SDL3 window showing FFmpeg-VAAPI-decoded frames at
   120 fps (R1), and SDL3 relative mouse on Wayland and X11 (R2).
1. **Control** (main app): `Connection::exec` and `forward`; the nav icon
   (SSH only) and `window.remote` command; the probe; start/stop Sunshine;
   the Remote window shell in the design language with status and config.
   Useful on its own: it tells you whether a host can stream at all.
2. **Pairing**: PulseTerm's own Sunshine instance (config dir, random web
   credentials, port check); in-memory certificate, PIN via the forwarded web
   API, unpair on close, leftover cleanup.
3. **Streaming**: protocol (per D1), decode (per D2), render, audio.
   First milestone: a desktop at 60 fps, absolute mouse, keyboard.
4. **Input for games**: capture mode and release chord, relative mouse
   (per R2). *Done; gamepads dropped -- not needed.*
5. **Polish**: stats bar, reconnect, multiple displays, resolution matching
   the window, HDR if hosts support it. *Readings, reconnect and live
   settings done; the rest is "Next" above.*

(Phases 0-3 are done; see *Where it stands*.)

## 6. Testing

There is no UI test (see CLAUDE.md); every phase is checked by running it.
Phase 1 needs an SSH-reachable Linux host with Sunshine; the development
machine can be its own host (it runs `sshd`) if key-based SSH to `localhost`
is set up. Phase 3 additionally needs the host's UDP ports reachable and a
GPU encoder on it.

## Progress

**Built (phase 1, in part):**

* `src/ssh/exec.rs` -- `Connection::exec` (one-shot command, output capped
  at 1 MB) and `Connection::forward` (local listener → `direct-tcpip`; the
  returned guard stops it within a second of being dropped).
* `src/remote.rs` -- finding the helper (beside the exe, then `PATH`, cached
  5 s), launching it, and the pipe: the helper writes JSON requests to its
  stdout, PulseTerm answers on its stdin; `{"event":"raise"}` when the icon is
  clicked again; EOF means PulseTerm let go. Protocol in that file's header.
  The helper is dropped (so told to quit) on every connection change.
* Nav bar `MonitorPlay` icon and `session.remote` command (`ctrl-shift-m`),
  SSH sessions only and only with the helper installed.
* `Connection::spawn` and the `spawn` / `write` / `kill` pipe ops: a
  long-lived command whose stdin is the channel. Every one is closed when the
  helper exits, and sshd closes them if the connection drops.
* `remote/src/sunshine.rs` -- PulseTerm's own instance, see 2.4 "As built".
  Connect starts it; the launcher's Sunshine section shows its state with a
  start / stop button.
* `remote/` -- the gpui launcher: host facts, the checklist with copyable
  fix commands (commands only; advice stays in the text), stream settings
  in `remote.json` per host, recheck, and a connect button that says the
  stream is not built yet. `--local` probes this machine without PulseTerm.
* Tests: probe parsing and checks, config clamping, theme handoff, request
  parsing. Checked by running: `--local`, a fake PulseTerm driving the real
  pipe (request answered, Paper theme applied, raise, clean exit on EOF), and
  **end to end** (2026-11-02): PulseTerm connected over SSH to `localhost`,
  the icon appeared, `ctrl-shift-m` opened the launcher with PulseTerm's
  theme, the probe ran over the SSH connection and came back all green
  (Sunshine 2026.914, wayland · niri, H.264 · HEVC, uinput), and disconnecting
  closed the helper.

**Audio gaps while the mouse moved (2026-11-03).** A moving pointer keeps
the host's picture changing, video frames grow and arrive in bursts, and the
audio packets beside them arrive unevenly. The audio code made that audible
two ways: past its limit it *cleared the whole queue* (up to 80 ms at once),
and when packets were late it ran dry between each one. Now an over-full
queue drops only the incoming packet, and a dry queue pauses the device until
a cushion (`audio::PRIME_MS`: 20 ms, 10 in low latency) is rebuilt -- one
short wait instead of many gaps; playback also starts on that cushion. And
mouse motion is gathered and sent once per pass of the loop
(`Stream::flush_motion`, relative movement summed, positions replaced by the
newest) instead of one input packet per event -- a high-rate mouse or touchpad
sent hundreds a second. Pending motion is flushed before any button or wheel
event, so clicks land where the pointer was.

Measured with the sidebar's audio line on cachyos (the cushion then
63 ms): still -- 128 ms queued, 2 gaps, 3 drops; after ten seconds of mouse
movement -- 114 ms queued, **2 gaps (unchanged), 36 drops**. So the adaptive
cushion stopped the underruns, but the queue sat far above it (clumps topped
it up and nothing drained it) and each further clump overflowed: the drops
were the clicks. Now the queue is steered back to the cushion with
`SDL_SetAudioStreamFrequencyRatio` -- 1.02-1.04 above it, 0.98 well below --
and dropping is kept for the extreme (three cushions plus 60 ms). This also
brings the sound's delay back down from ~120 ms to about the cushion.

**Bitrate is a cap (2026-11-03).** The user saw ~10-11 Mbps on the network
with 20 Mbps set. Both expected: moonlight-common-c sends 80% of the setting
as the video's limit and reserves the rest for error correction
(`SdpGenerator.c`, `adjustedBitrate = bitrate * 0.80`), and the encoder --
NVENC without filler, like Sunshine's others -- spends only what the picture
needs under that cap; a desktop or a still page compresses far below it.
The sidebar now shows the cap for the picture beside the setting and the
**received** video bitrate (`decoder::VIDEO_BYTES`, whole frames before
decoding) beside the frame rate.

**Zero-copy display (2026-11-03).** The copy path downloads each VA-API
surface to memory, copies it again and uploads it to an SDL texture: ~10 ms a
1080p frame measured on this client. `stream/zerocopy.rs` keeps the frame on
the GPU instead: the decoder maps the surface to DRM PRIME
(`av_hwframe_map`, `extra_hw_frames = 4` so the frame on screen is never
recycled under it), the window imports the Y and CbCr planes as EGL images
(`EGL_EXT_image_dma_buf_import`, with the format modifier) bound to two GL
textures, and SDL draws them as one NV12 texture it was handed
(`SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_NUMBER` / `_UV_NUMBER`). The window
asks SDL for OpenGL over EGL (`SDL_VIDEO_FORCE_EGL`, then the `opengl`
renderer, falling back to SDL's own choice). Any missing piece -- another
renderer, no dma-buf import, a 10-bit layout, a failed import -- logs why and
returns to the copy path for the rest of the stream (measured against
cachyos, 1080p60 HEVC: decode 10.4 → 5.3 ms a frame); stderr says
`zero-copy display on` or `off (why)`. A frozen picture during a rejoin keeps
the importer alive with it. SDL's NV12 shader reads chroma as luminance-alpha (Cr from alpha), so the CbCr texture swizzles alpha from green -- without it every picture came out red (seen on the first run). Both paths now draw in BT.709 limited, which is
what `connect` asks the host for; the copy path used SDL's YUV default
(BT.601) before, shifting reds and greens slightly.

**Resolution "window" (2026-11-03).** The default resolution now streams at
the window's own size in real pixels (HiDPI included), so the picture is
drawn 1:1 instead of a fixed 1080p scaled to fit: sharper, and the host
encodes no detail the scaling would throw away. `window_resolution` caps it
at 4K keeping the window's shape, floors it at 640x360, and rounds each side
down to a multiple of 8 (encoders work in blocks; NVENC refuses odd sizes).
Sunshine scales the host's desktop into that size and pads the difference
in shape, so a tall window gets a tall stream with bars, not a stretched
desktop. A resize or fullscreen mid-stream rejoins at the new size through
the same debounce as a settings change, so a drag rejoins once, at its end;
the auto bitrate scales with the pixel count. "native" (never offered in the
UI) is treated as "window".

**The "network" reading is not the network (2026-11-03).** The sidebar's
network figure is ENet's round trip on the control channel
(`LiGetEstimatedRttInfo` → `peer->roundTripTime`): the time until the host's
Sunshine acknowledges a control message. It read ~40 ms steady against both
cachyos and orion, while plain pings were 1.9 ms laptop → host *during* the
stream and 2.5 ms host → laptop (so Wi-Fi power save, ruled out separately by
switching it off, is not it either). Moving the mouse lifts it 10-20 ms, as
Sunshine handles input on that same thread (`control_server_t::iterate`, one
event per call, 150 ms wait). The video and audio travel on their own UDP
streams with no acknowledgements, so the 40 ms is not in the picture's path.
Where in the ack path it comes from was not pinned down: ENet acknowledges
before dispatching on both ends in the code read. `PULSETERM_REMOTE_STATS=1`
in PulseTerm's environment logs every reading to stderr every two seconds,
which is how this was measured. The sidebar now labels it "control", not "network".

**Frames skipped while the mouse moved (2026-11-03).** Stats showed 60 frames
decoded but only 42-55 shown while the mouse moved. The window redrew on every
event, and with vsync each draw waits for a screen refresh, so motion spent
refreshes redrawing an unchanged picture while the next video frame replaced
the one not yet shown. While streaming with the sidebar closed, motion alone
no longer redraws: a draw happens when a frame is waiting
(`stream::frame_waiting`), on any other event, and at least every 100 ms (the
edge arrow).

**Choosing the display (2026-11-03).** Sunshine lists the host's displays in
its log while starting, before it listens -- KMS as `Monitor 0 is eDP-1: AU
Optronics - 0xB98C - eDP-1` (read off cachyos), X11 and wlroots as `Detected
display: HDMI-0 (id: 1) connected: true`. `sunshine::start` greps that list
once the instance is ready (`parse_displays`: both formats, connected only,
each name once) and the sidebar's host section offers a **Display** row --
auto plus each connector -- when there is more than one, with the make and
model under it. The choice is saved per host as the connector name, which
Sunshine's docs recommend for `output_name` because the index can change
between reboots, and it is part of `Tuning`, so it applies through "Apply ·
restarts Sunshine" like the CPU settings. The name reaches a shell script on
the host, so only letters, digits and `-_.` pass (`safe_display`); anything
else is dropped, both on load and when writing the config.

**Where sound plays (2026-11-03).** *Audio: this PC / host / both*
replaces the on/off switch (`StreamConfig::sound`). It drives two things:
playing here (`stream::set_muted`, instant) and the launch request's
`localAudioPlayMode` -- 1 keeps the host's own sound playing, 0 lets Sunshine
take it -- so a change mid-stream rejoins for that half. "Host" also turns
Sunshine's capture off (`stream_audio = disabled`) the next time it starts,
saving host CPU. Older files map audio on → this PC, off → host; `sound` is
empty by default so that the old switch decides (a test caught the default
overriding it).

**Low latency (2026-11-03).** *Latency: normal / low*. Here, at once
(`App::sync_latency`): vsync off, so a frame shows the moment it is decoded
instead of waiting up to a refresh (it may tear), and the audio queue capped
at 30 ms instead of 80. On the host, through `sunshine::Tuning` (so a change
mid-stream gets the *Apply · restarts Sunshine* button): every encoder's
low-latency settings at once -- `nvenc_preset = 1`, `nvenc_twopass =
disabled`, `nvenc_latency_over_power`, `amd_usage = ultralowlatency`,
`amd_quality = speed`, `qsv_preset = veryfast`, `vaapi_strict_rc_buffer`, and
`sw_preset = ultrafast` -- Sunshine uses whichever encoder the host has; all
were checked to load. The sidebar now reads where the delay is: the network
round trip (`LiGetEstimatedRttInfo`), the host's processing per frame
(`frameHostProcessingLatency`, when Sunshine sends it) and decode time here.

**This computer reacts to no key (2026-11-03).** The shortcut inhibit was
not enough: tested with a bare SDL window on niri 26.04, **Mod+drag moved the
window with the inhibit on, and with the pointer locked too** -- niri starts
its interactive move on the modifier state, before any client sees the
click, and has no rule to switch it off per window. Here Mod is Alt
(`mod-key "Alt"`), so Alt+drag in the stream moved the stream window.

So the keyboards are now claimed below the compositor (`stream/grab.rs`): an
exclusive `EVIOCGRAB` on every `/dev/input/eventN` udev marks
`ID_INPUT_KEYBOARD`, read on a thread and sent to the host as virtual keys
(`grab::evdev_vk`). niri then sees no key, so no shortcut and no Mod. Rule
(*Full keyboard: over window*, now the default): held while the window has
focus **and** the pointer is on it (read from `SDL_GetWindowFlags` every
pass); released the moment either stops -- with niri's `focus-follows-mouse`
that is simply moving the pointer off. Mouse and touchpad are not grabbed.

Safety: the grab starts only with no key down (`EVIOCGKEY`), or niri would
see a press without its release; the Ctrl+Alt+Shift hotkeys are read on the
grab thread and posted to the window, never sent; a heartbeat from the
window's loop -- two seconds without it and the grab lets go; and the kernel
ends a grab when its file closes, so a crash cannot keep the keyboard.

It needs read access to the keyboards, which a desktop user lacks
(`root:input 0660`). `grab::GRANT` is the one-time fix the settings offer to
copy: a udev rule tagging keyboard devices only `uaccess`, so logind gives
the active session an ACL on them -- the trade-off being that any program in
that session could read the keyboards. Without it the app falls back to the
compositor's inhibit, and says what the grant would add.

**Full mouse (2026-11-03).**

* **Drags past the picture.** In desktop mode a position outside the
  picture (the letterbox) is held at its edge (`stream::clamped`) instead of
  dropped, so an overshooting drag keeps going and the host's screen edges and
  corners are easy to hit.
* **Nothing stays held on the host.** The stream remembers which buttons and
  keys the host was told are down, drops an up it never had a down for, and
  `Stream::release_all` lets go of the rest on focus loss, when the sidebar
  opens (`App::set_sidebar`), and when a stream is replaced or ends (`Drop`).
* **The arrow no longer covers the host.** It shows only while the sidebar is
  open or after the pointer has rested 300 ms on the window's right edge
  (`App::tab_visible`); otherwise the host gets the whole picture.
  Ctrl+Alt+Shift+S still opens the sidebar any time.
* **One pointer.** This computer's cursor hides over the picture, which shows
  the host's (`Pointer: host's`). The GameStream protocol carries **no cursor
  shape** -- the host's hand, text beam and busy cursor are drawn into the
  video -- so a local cursor cannot follow them; `both` therefore shows a
  small dot (`app::dot_cursor`) that marks the spot without the round trip
  instead of an arrow claiming a shape the host is not showing. Matching
  shapes would need the host's cursor sent out of band (XFixes on X11; no
  general Wayland way) -- not built.
* **Scrolling** carries fractions, so touchpads scroll smoothly instead of
  in rounded steps. Values go out as SDL gives them: they are already in the
  direction this computer's natural-scroll setting makes them, so the host
  scrolls the way the touchpad feels here. (For a while the flip was undone,
  which reversed scrolling on the host for a natural-scroll touchpad -- the
  user noticed.) *Scroll: as here / reversed* turns it round on purpose. Back and forward
  (buttons 4, 5) were already passed through.
* **Mouse: desktop / game.** Game makes a click in the picture capture the
  pointer (relative motion; with the keyboard setting at "with mouse", the
  keyboard too) instead of reaching the host; Ctrl+Alt+Shift+Z lets go.

Not exercised here: with this machine as its own host, positions move this
machine's own pointer and clicks do not arrive (see the testing trap above);
to be tried against a second machine.

**Full keyboard (2026-11-03).** This computer's own shortcuts (niri's
Super bindings, Alt+Tab) were acting here instead of on the host. Now
`SDL_SetWindowKeyboardGrab` hands the host the whole keyboard -- on Wayland
SDL uses the compositor's keyboard-shortcuts-inhibit protocol (niri offers
it), on X11 a keyboard grab. A setting, *Full keyboard*: **with mouse**
(default; while the mouse is captured with Ctrl+Alt+Shift+Z), **always**
(while streaming), **off**. `App::sync_keyboard` settles it every pass, and it
is never on while the sidebar is open. The Ctrl+Alt+Shift hotkeys are always
handled here -- Z gives mouse and keyboard back, S opens the sidebar, Q
quits -- and niri's `Mod+Escape` (`allow-inhibiting=false`) toggles the
inhibit off as a last resort.

**A firewall on the host (2026-11-03).** On CachyOS (ufw on by default)
pairing worked -- it is tunnelled over SSH -- but Connect failed with *RTSP
handshake failed (110)*: a timeout, the firewall dropping the direct
connection (no listener would be 111, refused). The probe now reports an
active ufw or firewalld as a *Firewall* warning with the fix: TCP base+21
(48010) and UDP base+9..base+11 (47998-48000) -- nothing else, the web admin
page stays shut -- opened to the client's /24, read from `$SSH_CLIENT` on the
host. A warning, not a blocker: the rules need root to read. A VPN (Tailscale
`100.64.0.0/10`) is not special-cased yet.

**Installing Sunshine on Arch (2026-11-03).** Sunshine is not in Arch's
official repositories. LizardByte publishes its own pacman repository
(`LizardByte/pacman-repo`) and calls it *"the only official source of
LizardByte packages"*; it does not support the AUR. So the probe's hint for
Arch and anything with `ID_LIKE=arch` (EndeavourOS, CachyOS, Manjaro) adds
`[lizardbyte]` to `/etc/pacman.conf` once and installs with `pacman -Syu
lizardbyte/sunshine` -- a full upgrade, because installing after a bare
`-Sy` is a partial upgrade, which Arch does not support. The repository is
`SigLevel = Optional` (its packages are not signed), as LizardByte's own
instructions say.

**Settings change mid-stream, without disconnecting (2026-11-03).** Three
kinds, by where the setting lives:

* **Here, at once:** audio (muting: packets are still decoded, so unmuting
  is seamless -- `stream::set_muted`), mouse capture, fullscreen.
* **The session, by rejoining (~1-8 s):** resolution, frame rate, codec,
  bitrate. The protocol fixes these per session, so no Moonlight client
  changes them in place; the app rejoins the same desktop (`/resume`, same
  Sunshine, still paired) 700 ms after the last change, so clicking through
  options rejoins once. Verified on orion: 30 → 60 fps, same Sunshine pid,
  sidebar reading 60 decoded / 60 shown.
* **Sunshine's config, by restarting it (~30 s on orion):** encoder speed,
  threads, error correction. Mid-stream an *Apply · restarts Sunshine*
  button appears under Host CPU; Sunshine restarts, pairs again and the
  stream resumes on its own (verified: new pid, stream back). Audio capture
  is in the config too but does not force a restart (`Tuning::restart_needed`):
  it mutes here at once and the host stops capturing next time Sunshine
  starts.

While a stream is being replaced, its last picture stays up
(`Stream::freeze` → `stream::Frozen`) with a small status line over it, so a
change reads as a hiccup rather than a disconnect. The Reconnect button is
gone.

**Audio and host-CPU settings (2026-11-03).**

* **Audio:** `stream/audio.rs` decodes moonlight-common-c's Opus multistream
  packets with the system libopus (a four-function `extern` block, linked as
  `opus`) and queues the PCM on an SDL3 audio stream to the default output.
  The queue is cleared past 80 ms, so sound never trails the picture. With
  Audio off no callbacks are given, and Sunshine is started with
  `stream_audio = disabled`, so the host does not capture sound at all.
  Verified: during a stream the helper is an active PulseAudio/PipeWire
  playback stream.
* **Host CPU** (settings, below Gamepads): *Encoder speed* (Sunshine's
  `sw_preset`: fastest = ultrafast, fast = superfast -- Sunshine's default --
  better = veryfast), *Encoder threads* (`min_threads`: 1/2/4/8), *Error
  correction* (`fec_percentage`: 5/10/20 %), a **720p** resolution, and a
  *Lowest CPU* button (720p, 30 fps, H.264, fastest, no audio, 10 % FEC).
  They matter on a host without a hardware encoder -- a Jetson, a VM -- where
  the encoder is most of the CPU. They are written into PulseTerm's Sunshine
  config at start (`sunshine::Tuning`); the app remembers what the running
  instance was started with, and Connect restarts Sunshine (and pairs again)
  when they differ. Measured on orion (single btop readings, 1080p30):
  Sunshine at **22.0 % → 15.7 %** CPU going from fast to fastest.

**One window (2026-11-03), at the user's request.** The gpui launcher and
the separate SDL stream process are gone: the helper is **one SDL3 window**
in one process (`remote/src/app.rs`), and gpui is no longer a dependency of
the helper at all.

* Before a stream it is the setup screen (host, checks, stream settings,
  Connect), drawn by `remote/src/ui` -- a small immediate-mode UI on SDL's
  renderer in PulseTerm's design language: flat grounds, one-pixel
  hairlines, tracked capital labels, segmented choices, laid out in points
  and drawn in pixels for HiDPI. Text is `fontdue` (pure Rust) rasterising
  the theme's font, found with `fc-match` (`theme::font_files`).
* While streaming the picture fills the window and a translucent arrow
  (`‹`) sits on the right edge; it -- or **Ctrl+Alt+Shift+S** -- slides in a
  sidebar with the stream's readings (asked-for mode, decoded/shown fps), the
  controls (capture mouse, fullscreen, disconnect, stop Sunshine, each a
  full-width row with its shortcut) and the same settings with Reconnect.
  With the sidebar open nothing reaches the host; a click outside it or
  Escape shuts it. Disconnect returns to the setup screen still paired.
* Waiting work (probe, Sunshine, pairing, launch) runs on threads and comes
  back as a message, waking the SDL loop with a user event.
* **Testing trap:** with this machine as its own host, button presses stop
  reaching any window while Sunshine runs here (its virtual input devices);
  kill that Sunshine and clicks arrive again. Test clicks against a second
  machine.

**Stop, then Connect, failed -- fixed.** Stopping only *asks* the wrapper,
which takes up to ~3 s to kill Sunshine; a Connect straight after started a
new instance while the old one still listened, the ready check saw the *old*
one, and pairing went to a Sunshine that would never read the PIN. The old
wrapper also deleted the new instance's `pid` and FIFO by name on its way
out. Now `stop` waits until the instance is gone, `prepare` waits for an old
one to die and its port to free, and the wrapper removes only its own files.
The opt-in pairing test runs its two attempts back to back with no pause;
on orion, Stop Sunshine → Connect started a fresh instance and streamed.

**Over the network, to a second machine -- working (2026-11-03).** Host
`orion`: an NVIDIA Jetson Orin, Ubuntu 24.04 arm64, X11, over Wi-Fi/LAN from
this machine. The user installed Sunshine with the launcher's copied command
(the .deb for `ubuntu24.04_arm64`, found through the GitHub API since the
file names carry the version). Connect streamed orion's desktop, and the user
confirmed **keyboard and mouse work**. What it took:

* **KMS capture fails on a Jetson** ("GPU driver doesn't support universal
  planes"), even though the .deb grants `cap_sys_admin`. So an X11 session
  now always pins `capture = x11` (`probe::capture`), and KMS is only used on
  Wayland; the wrapper also finds `XAUTHORITY` (GDM's
  `/run/user/<uid>/gdm/Xauthority`, then `~/.Xauthority`) and falls back to
  `DISPLAY=:0`.
* **Sunshine cannot use a Jetson's video encoder** (it is not NVENC; the
  probe now asks for `libnvidia-encode`, not just `nvidia-smi`), so it
  encodes in software on the ARM cores; the check says so.
* **The tegra kernel has no `uinput` module at all**, and Ubuntu has no
  extra-modules package for it -- yet keyboard and mouse worked: on X11
  Sunshine falls back to XTest. Only gamepads need uinput there. The Input
  check now tells *missing from the kernel*, *not loaded* (`modprobe` fix) and
  *not writable* (udev fix) apart, and on X11 says keyboard and mouse still
  work.

**Phase 3, first stream -- working (2026-11-03).** Connect now starts
Sunshine, pairs, launches the host's "Desktop" app and opens the stream
window; on this machine (AMD Renoir, niri) it streamed 1080p HEVC at a steady
**60 fps decoded and shown**, KMS capture and VA-API encode on the host,
VA-API decode on the client.

* `remote/third_party/moonlight-common-c` is a git submodule (GPL-3.0, with
  its own enet and nanors submodules); `remote/build.rs` builds it with CMake
  as a static library (`-Wno-error` over upstream's `-Werror`) and binds
  `Limelight.h` with bindgen **0.72** (0.73 fails against this workspace's
  `syn`). Clone with `git submodule update --init --recursive`.
* After pairing, the launcher (it holds the identity) asks `/serverinfo`
  and `/applist` over the pinned TLS, then `/launch` -- or `/resume` when
  `currentgame` is set, because closing the stream window leaves the app
  running on the host the way every Moonlight client does. Only the *port* of
  the returned `sessionUrl0` is used; moonlight-common-c connects to the
  address we give it, so a tunnelled HTTPS step and a direct stream mix.
* `pulseterm-remote stream` is the same binary in a second process. It gets
  the `Session` as one JSON line on stdin (never the command line: it carries
  the input AES key) and ends when stdin closes. moonlight-common-c runs on its
  own threads, started from a thread so the window paints while connecting;
  video is decoded in its callback (`CAPABILITY_DIRECT_SUBMIT`) by FFmpeg with
  VA-API, copied to NV12 and handed to the SDL3 thread as the newest picture
  only; SDL draws it letterboxed with vsync. App id
  `pulseterm-remote-stream`.
* Input: SDL scancodes → Windows virtual keys (`stream/keys.rs`), absolute
  mouse positions relative to the letterboxed picture, relative motion while
  captured, buttons, high-resolution wheel. Hotkeys on Ctrl+Alt+Shift: **Q**
  quits, **Z** captures the mouse, **X** fullscreen.
* Every 5 s the stream logs decoded and shown fps on stderr.

**Phase 2, pairing -- built and verified (2026-11-02).** `remote/src/pairing.rs`
is the Moonlight pairing handshake in Rust over OpenSSL (pairing is not in
moonlight-common-c; every client implements it): `getservercert` → AES-128-ECB
challenges under `SHA-256(salt ‖ PIN)[..16]` → server secret with its RSA
signature checked against the certificate from step 1 and its hash checked
against the secret (this is what refuses a wrong PIN or a man in the middle) →
our signed secret → `pairchallenge` over TLS with our certificate, Sunshine's
pinned. Then `/serverinfo` over TLS must say `PairStatus` 1.

* **Nothing to type, nothing stored.** A fresh certificate, key and unique id
  per launch, in memory only (kept in the launcher for the stream window).
  The PIN is random and goes to Sunshine's stdin through the spawned channel
  (`Bridge::write`), written 300 ms after the first request is sent.
* **Over SSH.** Both pairing ports (base and base − 5) are reached through
  `Connection::forward`, so a host firewall does not matter for pairing.
* Connect now starts Sunshine *and* pairs; a failed pairing stops Sunshine so
  the next try starts clean (`-0` reads the PIN once).
* Verified: through PulseTerm over SSH to `localhost` ("Paired."), and by an
  opt-in test against real Sunshine instances (`cargo test -p
  pulseterm-remote -- --ignored`): the right PIN pairs and is confirmed, a
  wrong one is refused. Sunshine logs nothing about pairing at info level, so
  the TLS `serverinfo` check is the proof.

**Phase 0 results** (2026-11-02, AMD Renoir iGPU, niri, throwaway spike):

* **Video:** FFmpeg VAAPI decode of 1080p120 H.264 into an SDL3 window, vsync
  off, steady **~291 fps** -- through the *copy* path (GPU frame downloaded to
  NV12, ~2.9 ms, then uploaded to an SDL texture, ~0.3 ms). 2.4x the 120 fps
  target before zero-copy. (An early run that dropped to 107 fps was the
  spike's own clip looping, not the pipeline.) SDL3 must be told
  `SDL_VIDEO_DRIVER=wayland` (or the hint): left alone it picked X11 through
  XWayland.
* **Pointer lock:** SDL3 relative mouse mode on native Wayland reported
  active and delivered exact relative deltas (20 moves of (10,5) summed to
  (200,100)). The compositor only locks once the pointer is inside the window.
* Still untested: GNOME, KDE and X11 hosts for the pointer lock; 4K; the
  zero-copy dmabuf → EGL path.

**Not yet:** see *Where it stands* at the top -- that list is kept current.
(The gpui launcher's GNOME-decorations gap went with the gpui launcher.)

**Fedora packages, and a trap.** The helper needs RPM Fusion's `ffmpeg` (the
default `ffmpeg-free` has no H.264/HEVC decoder), `mesa-va-drivers-freeworld`
(installs beside Fedora's in `/usr/lib64/dri-freeworld/`, replaces nothing),
and `ffmpeg-devel libva-devel SDL3-devel mesa-libEGL-devel clang cmake
openssl-devel opus-devel`. **Never `dnf swap mesa-va-drivers ...`** on Fedora
44: `mesa-dri-drivers` provides `mesa-va-drivers`, so the swap removes the
graphics driver and ~360 packages with it (niri, GDM, GTK4...). Plain
`dnf install mesa-va-drivers-freeworld` is the right command.
