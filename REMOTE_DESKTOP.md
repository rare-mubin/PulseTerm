# Remote desktop — design

Status: **design, nothing built.** Toolkit and decisions settled (see below). Decided with the user (2026-11-02):

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
* **Toolkit:** the streaming window is **SDL3**, not gpui -- see 2.7. The
  launcher (status, config, connect) stays **gpui**, for the design language
  and the shared theme code. Two processes, one event loop each.

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
* **Shared design code:** the launcher wants PulseTerm's theme and `kit`
  measurements. Phase 1 decides between copying the small parts it needs or
  moving them into a third crate both depend on; that crate would need a
  licence the GPL helper can use (MIT/Apache both are).

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
   (per R2), gamepads with rumble.
5. **Polish**: stats bar, reconnect, multiple displays, resolution matching
   the window, HDR if hosts support it.

## 6. Testing

There is no UI test (see CLAUDE.md); every phase is checked by running it.
Phase 1 needs an SSH-reachable Linux host with Sunshine; the development
machine can be its own host (it runs `sshd`) if key-based SSH to `localhost`
is set up. Phase 3 additionally needs the host's UDP ports reachable and a
GPU encoder on it.
