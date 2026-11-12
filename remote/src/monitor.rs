//! The fake monitor: the host's driver is told a monitor is plugged in, so
//! the desktop that is logged in there -- with nothing attached -- has a
//! screen to draw on, at the resolution and refresh rate chosen here.
//!
//! A monitor is described by its EDID, and every way of faking one is a
//! driver setting, so it needs root. `pulseterm-monitor` (`TOOL`) is a small
//! fixed script installed once, with a sudoers rule letting this account run
//! it -- and only it -- without a password (`setup_command`); from then on
//! the settings drive it over SSH like everything else. Two ways in:
//!
//! * **NVIDIA on X11** (`Method::Nvidia`): `ConnectedMonitor` and
//!   `CustomEDID` in xorg.conf's Device section, which X reads when it
//!   starts -- so turning it on or off restarts the login screen. The EDID
//!   lists every mode on offer, so a resolution or refresh change after that
//!   is an `xrandr` away, no root and no restart. Tested on a Jetson (orion).
//! * **Everything else -- AMD, Intel** (`Method::Kms`): the kernel's own
//!   EDID override (debugfs `edid_override`) on a free DisplayPort or HDMI
//!   connector, then forcing it connected through sysfs. Live, Wayland or
//!   X11, and gone at the next boot -- this window turns it back on.

/// Resolutions on offer: setting value, label, width, height.
pub const RESOLUTIONS: [(&str, &str, u32, u32); 4] = [
    ("1280x720", "720p", 1280, 720),
    ("1920x1080", "1080p", 1920, 1080),
    ("2560x1440", "1440p", 2560, 1440),
    ("3840x2160", "4k", 3840, 2160),
];

/// Refresh rates on offer, Hz.
pub const REFRESH: [u32; 4] = [30, 60, 120, 144];

/// 4K stops at 60 Hz: past that a mode no longer fits an EDID's detailed
/// timing (655.35 MHz), and a fake monitor gains nothing from it.
pub fn max_refresh(height: u32) -> u32 {
    if height > 1440 { 60 } else { 144 }
}

/// Every mode the fake monitor offers.
pub fn modes() -> Vec<(u32, u32, u32)> {
    let mut out = Vec::new();
    for &(_, _, w, h) in &RESOLUTIONS {
        for &r in &REFRESH {
            if r <= max_refresh(h) {
                out.push((w, h, r));
            }
        }
    }
    out
}

/// One detailed timing, CVT reduced-blanking shaped (160-pixel horizontal
/// blank, at least 460 us of vertical blank), with the clock set so the
/// refresh comes out at the nominal rate rather than CVT's 59.94-style one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub width: u32,
    pub height: u32,
    /// Pixel clock in units of 10 kHz, as an EDID stores it.
    pub clock: u32,
    pub h_blank: u32,
    pub v_blank: u32,
}

pub fn timing(width: u32, height: u32, refresh: u32) -> Timing {
    const H_BLANK: u32 = 160;
    const MIN_VBLANK_US: f64 = 460.0;
    let line_us = (1_000_000.0 / refresh as f64 - MIN_VBLANK_US) / height as f64;
    let v_blank = ((MIN_VBLANK_US / line_us).floor() as u32 + 1).max(3 + 5 + 6);
    let total = (width + H_BLANK) as u64 * (height + v_blank) as u64 * refresh as u64;
    Timing { width, height, clock: ((total + 5_000) / 10_000) as u32, h_blank: H_BLANK, v_blank }
}

/// The 18 bytes of a detailed timing descriptor.
fn dtd(t: Timing) -> [u8; 18] {
    let (h_fp, h_sync, v_fp, v_sync) = (48u32, 32u32, 3u32, 5u32);
    let (h_mm, v_mm) = (531u32, 299u32);
    let (ha, hb, va, vb) = (t.width, t.h_blank, t.height, t.v_blank);
    [
        (t.clock & 0xff) as u8,
        (t.clock >> 8) as u8,
        (ha & 0xff) as u8,
        (hb & 0xff) as u8,
        (((ha >> 8) << 4) | (hb >> 8)) as u8,
        (va & 0xff) as u8,
        (vb & 0xff) as u8,
        (((va >> 8) << 4) | (vb >> 8)) as u8,
        (h_fp & 0xff) as u8,
        (h_sync & 0xff) as u8,
        (((v_fp & 0xf) << 4) | (v_sync & 0xf)) as u8,
        (((h_fp >> 8) << 6) | ((h_sync >> 8) << 4) | ((v_fp >> 4) << 2) | (v_sync >> 4)) as u8,
        (h_mm & 0xff) as u8,
        (v_mm & 0xff) as u8,
        (((h_mm >> 8) << 4) | (v_mm >> 8)) as u8,
        0,
        0,
        // Digital separate sync, horizontal +, vertical - (reduced blanking).
        0x1a,
    ]
}

/// CTA-861 video codes for the modes that have one: one byte each instead
/// of an 18-byte timing, which is what lets every mode fit in 256 bytes.
fn vic(w: u32, h: u32, r: u32) -> Option<u8> {
    Some(match (w, h, r) {
        (1280, 720, 30) => 62,
        (1280, 720, 60) => 4,
        (1280, 720, 120) => 47,
        (1920, 1080, 30) => 34,
        (1920, 1080, 60) => 16,
        (1920, 1080, 120) => 63,
        (3840, 2160, 30) => 95,
        (3840, 2160, 60) => 97,
        _ => return None,
    })
}

/// A 256-byte EDID (base block and one CTA-861 extension) for a 24-inch
/// "PulseTerm" monitor whose preferred mode is the one chosen, and which
/// offers every other mode too. The serial number changes with the chosen
/// mode, so a desktop that remembers settings per monitor (GNOME does) takes
/// the new preferred mode instead of the one it saw last time.
pub fn edid(width: u32, height: u32, refresh: u32) -> Vec<u8> {
    let mut b = vec![0u8; 256];
    b[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
    b[8..10].copy_from_slice(&[0x41, 0x94]); // manufacturer "PLT"
    b[10..12].copy_from_slice(&[0x01, 0x00]); // product
    let serial = (width / 16) << 20 | (height / 8) << 8 | refresh;
    b[12..16].copy_from_slice(&serial.to_le_bytes());
    b[16] = 1; // week
    b[17] = 36; // 2026
    b[18..20].copy_from_slice(&[1, 3]); // EDID 1.3
    b[20..25].copy_from_slice(&[0x80, 53, 30, 0x78, 0x0a]); // digital, 53x30 cm, gamma 2.2, preferred timing
    b[25..35].copy_from_slice(&[0xee, 0x91, 0xa3, 0x54, 0x4c, 0x99, 0x26, 0x0f, 0x50, 0x54]); // sRGB
    for s in b[38..54].chunks_mut(2) {
        s.copy_from_slice(&[1, 1]); // no standard timings
    }
    b[54..72].copy_from_slice(&dtd(timing(width, height, refresh)));
    // Range limits: 24-150 Hz, 15-255 kHz, 660 MHz.
    b[72..90].copy_from_slice(&[0, 0, 0, 0xfd, 0, 24, 150, 15, 255, 66, 0, 0x0a, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20]);
    let mut name = [0u8; 18];
    name[3] = 0xfc;
    name[5..18].copy_from_slice(b"PulseTerm\n   ");
    b[90..108].copy_from_slice(&name);
    b[111] = 0x10; // dummy descriptor
    b[126] = 1; // one extension
    b[127] = checksum(&b[..127]);

    // CTA-861 extension: the video codes, then the rest as detailed timings.
    let e = &mut b[128..];
    e[0] = 0x02;
    e[1] = 0x03;
    let (vics, rest): (Vec<_>, Vec<_>) = modes().into_iter().partition(|&(w, h, r)| vic(w, h, r).is_some());
    let codes: Vec<u8> = vics.iter().map(|&(w, h, r)| vic(w, h, r).unwrap()).collect();
    e[4] = (2 << 5) | codes.len() as u8;
    e[5..5 + codes.len()].copy_from_slice(&codes);
    let start = 5 + codes.len();
    e[2] = start as u8;
    for (i, &(w, h, r)) in rest.iter().enumerate() {
        let at = start + i * 18;
        e[at..at + 18].copy_from_slice(&dtd(timing(w, h, r)));
    }
    e[127] = checksum(&e[..127]);
    b
}

fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x)))
}

/// How the host fakes a monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Nvidia,
    Kms,
}

impl Method {
    pub fn arg(self) -> &'static str {
        match self {
            Method::Nvidia => "nvidia",
            Method::Kms => "kms",
        }
    }
}

/// The installed tool's version this build expects; the probe reads the
/// installed one's from its header line.
pub const TOOL_VERSION: u32 = 1;
pub const TOOL_PATH: &str = "/usr/local/sbin/pulseterm-monitor";

/// The root script. Validates everything it is handed: it runs as root on
/// the word of whoever can run it without a password.
pub const TOOL: &str = r#"#!/bin/sh
# pulseterm-monitor v1
# PulseTerm's fake monitor: tells the display driver a monitor is attached.
# Installed by PulseTerm's remote desktop setup and run by it through sudo.
#   pulseterm-monitor on nvidia|kms EDID-BASE64
#   pulseterm-monitor off
#   pulseterm-monitor status
# Remove: pulseterm-monitor off, then delete this file and
# /etc/sudoers.d/pulseterm-monitor.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
state=/run/pulseterm-monitor
xconf=/etc/X11/xorg.conf
xedid=/etc/X11/pulseterm-edid.bin
dropin=/etc/X11/xorg.conf.d/90-pulseterm-monitor.conf

die() { echo "$*" >&2; exit 1; }

# The EDID, decoded into a file root owns: 256 bytes of base64, nothing else.
edid_file() {
  case "$1" in *[!A-Za-z0-9+/=]*|'') die "bad EDID" ;; esac
  f=$(mktemp)
  printf %s "$1" | base64 -d > "$f" 2>/dev/null || { rm -f "$f"; die "bad EDID"; }
  [ "$(wc -c < "$f")" -eq 256 ] || { rm -f "$f"; die "bad EDID size"; }
  echo "$f"
}

# X reads xorg.conf only when it starts; so does the login on top of it.
restart_x() {
  if pgrep -x Xorg >/dev/null || pgrep -x X >/dev/null; then
    systemctl restart display-manager
    echo restarted
  fi
}

nvidia_on() {
  new=$(edid_file "$1")
  changed=no
  if ! cmp -s "$new" "$xedid"; then install -m 644 "$new" "$xedid"; changed=yes; fi
  rm -f "$new"
  if ! grep -qs 'CustomEDID.*pulseterm-edid' "$xconf" "$dropin"; then
    if grep -qs 'Driver *"nvidia"' "$xconf"; then
      cp -n "$xconf" "$xconf.pulseterm-backup"
      sed -i '/Driver *"nvidia"/a\    Option      "ConnectedMonitor" "DFP-0"\n    Option      "CustomEDID" "DFP-0:/etc/X11/pulseterm-edid.bin"' "$xconf"
    else
      mkdir -p /etc/X11/xorg.conf.d
      printf 'Section "Device"\n    Identifier "PulseTerm"\n    Driver "nvidia"\n    Option "ConnectedMonitor" "DFP-0"\n    Option "CustomEDID" "DFP-0:/etc/X11/pulseterm-edid.bin"\nEndSection\n' > "$dropin"
    fi
    changed=yes
  fi
  if [ $changed = yes ]; then restart_x; fi
}

nvidia_off() {
  changed=no
  if grep -qs 'pulseterm-edid' "$xconf"; then
    sed -i '/"ConnectedMonitor" "DFP-0"/d; /CustomEDID.*pulseterm-edid/d' "$xconf"
    changed=yes
  fi
  if [ -f "$dropin" ]; then rm -f "$dropin"; changed=yes; fi
  rm -f "$xedid"
  if [ $changed = yes ]; then restart_x; fi
}

# The connector's debugfs directory, where its EDID override lives: named by
# the card's minor number, or by its PCI address on newer kernels.
debug_dir() {
  [ -d /sys/kernel/debug/dri ] || mount -t debugfs none /sys/kernel/debug 2>/dev/null || true
  card=${1%%-*}; conn=${1#*-}
  if [ -e "/sys/kernel/debug/dri/${card#card}/$conn/edid_override" ]; then
    echo "/sys/kernel/debug/dri/${card#card}/$conn"; return 0
  fi
  pci=$(basename "$(readlink -f "/sys/class/drm/$card/device")")
  for d in /sys/kernel/debug/dri/*; do
    if grep -qs "$pci" "$d/name" && [ -e "$d/$conn/edid_override" ]; then echo "$d/$conn"; return 0; fi
  done
  return 1
}

# A free DisplayPort or HDMI connector -- the one already used, if on.
free_connector() {
  if [ -f "$state" ]; then cat "$state"; return 0; fi
  for s in /sys/class/drm/card[0-9]*-*/status; do
    n=${s%/status}; n=${n##*/}
    case ${n#*-} in DP-*|HDMI-*|DVI-*) ;; *) continue ;; esac
    [ "$(cat "$s")" = disconnected ] || continue
    debug_dir "$n" >/dev/null || continue
    echo "$n"; return 0
  done
  return 1
}

kms_on() {
  new=$(edid_file "$1")
  n=$(free_connector) || { rm -f "$new"; die "no free DisplayPort or HDMI port whose EDID can be overridden (debugfs edid_override) on this machine"; }
  d=$(debug_dir "$n") || { rm -f "$new"; die "no EDID override for $n"; }
  cat "$new" > "$d/edid_override"
  rm -f "$new"
  echo "$n" > "$state"
  # Off and on again, so the desktop sees a new monitor with the new EDID.
  echo detect > "/sys/class/drm/$n/status"
  echo on > "/sys/class/drm/$n/status"
}

kms_off() {
  [ -f "$state" ] || return 0
  n=$(cat "$state")
  if d=$(debug_dir "$n"); then echo reset > "$d/edid_override" || true; fi
  echo detect > "/sys/class/drm/$n/status" || true
  rm -f "$state"
}

status() {
  if grep -qs 'CustomEDID.*pulseterm-edid' "$xconf" "$dropin"; then echo "on nvidia"
  elif [ -f "$state" ]; then echo "on kms $(cat "$state")"
  else echo off
  fi
}

case "${1:-}" in
  on)
    [ $# -eq 3 ] || die "usage: pulseterm-monitor on nvidia|kms EDID-BASE64"
    [ "$(id -u)" = 0 ] || die "needs root"
    case "$2" in
      nvidia) nvidia_on "$3" ;;
      kms) kms_on "$3" ;;
      *) die "unknown method: $2" ;;
    esac ;;
  off)
    [ "$(id -u)" = 0 ] || die "needs root"
    kms_off
    nvidia_off ;;
  status) status ;;
  *) die "usage: pulseterm-monitor on nvidia|kms EDID-BASE64 | off | status" ;;
esac
"#;

/// Shell that puts the tool beside PulseTerm's other host files, for the
/// one-time setup to install from. Run with every probe, so the copy there
/// is always this build's.
pub fn stage() -> String {
    format!("mkdir -p \"$HOME/.config/pulseterm\" && cat > \"$HOME/.config/pulseterm/pulseterm-monitor\" <<'PTMONITOREOF'\n{TOOL}PTMONITOREOF\n")
}

/// The one-time setup, one line: install the tool as root's, and let this
/// account run it -- that file, any arguments -- without a password.
pub fn setup_command() -> String {
    format!(
        "sudo install -D -m 755 -o root -g root \"$HOME/.config/pulseterm/pulseterm-monitor\" {TOOL_PATH} && \
echo \"$(id -un) ALL=(root) NOPASSWD: {TOOL_PATH}\" | sudo tee /etc/sudoers.d/pulseterm-monitor >/dev/null && \
sudo chmod 440 /etc/sudoers.d/pulseterm-monitor"
    )
}

/// Turns the fake monitor on at this mode. With it already on under
/// NVIDIA, the mode is changed with `xrandr` on the logged-in desktop
/// (no restart) and only if that fails -- the mode is not in the EDID X
/// started with -- does it go back to root, which restarts the login.
pub fn on_command(method: Method, width: u32, height: u32, refresh: u32, already_on: bool) -> String {
    let edid = openssl::base64::encode_block(&edid(width, height, refresh));
    let root = format!("sudo -n {TOOL_PATH} on {} {edid}", method.arg());
    if method == Method::Nvidia && already_on {
        format!(
            "xenv=$(systemctl --user show-environment 2>/dev/null | grep -E '^(DISPLAY|XAUTHORITY)='); \
if [ -n \"$xenv\" ] && env $xenv sh -c 'o=$(xrandr --query | awk \"/ connected/ {{print \\$1; exit}}\"); [ -n \"$o\" ] && xrandr --output \"$o\" --mode {width}x{height} --rate {refresh}' 2>/dev/null; \
then echo live; else {root}; fi"
        )
    } else {
        root
    }
}

pub fn off_command() -> String {
    format!("sudo -n {TOOL_PATH} off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_fits_one_edid_with_valid_checksums() {
        assert_eq!(modes().len(), 14);
        for (w, h, r) in modes() {
            let e = edid(w, h, r);
            assert_eq!(e.len(), 256);
            assert_eq!(e[..128].iter().fold(0u8, |a, &x| a.wrapping_add(x)), 0);
            assert_eq!(e[128..].iter().fold(0u8, |a, &x| a.wrapping_add(x)), 0);
            // The preferred mode is the chosen one.
            let d = &e[54..72];
            assert_eq!(d[2] as u32 | ((d[4] as u32 >> 4) << 8), w);
            assert_eq!(d[5] as u32 | ((d[7] as u32 >> 4) << 8), h);
            // Every timing fits a detailed timing's clock, and the 8 video
            // codes plus 6 timings fill the extension without overrunning it.
            assert!(timing(w, h, r).clock <= 0xffff, "{w}x{h}@{r}");
            assert!(e[128 + 2] as usize + 6 * 18 <= 127);
        }
        // A new mode is a new monitor, as far as a desktop remembers.
        assert_ne!(edid(1920, 1080, 60)[12..16], edid(1920, 1080, 144)[12..16]);
    }

    #[test]
    fn timings_land_on_the_nominal_refresh() {
        // CVT-RB's 1080p60 is 2080 x 1111; ours keeps that and sets the clock.
        let t = timing(1920, 1080, 60);
        assert_eq!((t.width + t.h_blank, t.height + t.v_blank), (2080, 1111));
        assert_eq!(t.clock, 13865);
        for (w, h, r) in modes() {
            let t = timing(w, h, r);
            let hz = t.clock as f64 * 10_000.0 / ((w + t.h_blank) as f64 * (h + t.v_blank) as f64);
            assert!((hz - r as f64).abs() < 0.05, "{w}x{h}@{r} came out {hz}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_tool_and_commands_are_sound_shell() {
        let sh = |s: &str| std::process::Command::new("sh").arg("-n").arg("-c").arg(s).status().unwrap().success();
        assert!(TOOL.starts_with("#!/bin/sh\n# pulseterm-monitor v1\n"));
        assert!(TOOL.contains(&format!("pulseterm-monitor v{TOOL_VERSION}")));
        assert!(sh(TOOL));
        assert!(sh(&stage()));
        assert!(sh(&setup_command()) && !setup_command().contains('\n'));
        for already in [false, true] {
            let c = on_command(Method::Nvidia, 2560, 1440, 144, already);
            assert!(sh(&c));
            assert_eq!(c.contains("xrandr"), already);
        }
        assert!(sh(&on_command(Method::Kms, 1920, 1080, 60, true)));
        assert!(sh(&off_command()));
    }

    /// The tool refuses what it should, as a user; `status` needs no root.
    #[cfg(unix)]
    #[test]
    fn the_tool_validates_before_touching_anything() {
        let dir = std::env::temp_dir().join(format!("pulseterm-monitor-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("pulseterm-monitor");
        std::fs::write(&tool, TOOL).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("sh").arg(&tool).args(args).output().unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
        };
        let (ok, out) = run(&["status"]);
        assert!(ok && (out.starts_with("off") || out.starts_with("on")), "{out}");
        assert!(!run(&["on", "nvidia"]).0);
        assert!(!run(&["on", "bogus", "AAAA"]).0);
        assert!(!run(&["frobnicate"]).0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
