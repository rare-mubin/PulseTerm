//! PulseTerm's own Sunshine instance on the host (REMOTE_DESKTOP.md, 2.4).
//!
//! Never the user's Sunshine: its own config directory
//! (`~/.config/pulseterm/sunshine/`), its own ports when the default ones are
//! taken, and flags that keep nothing between runs:
//!
//! * `-1` -- no saved state is loaded and none kept, so every launch starts
//!   with no paired clients. Re-pairing every launch then needs no clean-up.
//! * `-0` -- the pairing PIN is read from stdin, which is the SSH channel
//!   (phase 2), so no web admin password is needed at all.
//!
//! It runs on a spawned channel behind a small wrapper that waits on its own
//! stdin: when the channel closes -- the window closed, PulseTerm
//! disconnected, the network went -- the wrapper kills Sunshine. It cannot
//! outlive the session that started it.

use crate::bridge::Bridge;
use crate::probe::Facts;

/// Moonlight's default base port, and the one used when something else
/// already holds it. Sunshine derives every port from this one setting
/// (`base - 5` .. `base + 21`), and the two ranges do not overlap.
pub const DEFAULT_PORT: u16 = 47989;
pub const SHIFTED_PORT: u16 = 48089;

/// The host-side settings written into Sunshine's config when it starts.
/// Changing any of them needs a restart (`App::connect` does it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tuning {
    pub preset: String,
    pub threads: u32,
    pub fec: u32,
    /// Capture the host's sound at all: off saves host CPU.
    pub audio: bool,
    /// Every encoder's low-latency settings (only the one in use matters).
    pub low_latency: bool,
}

impl Tuning {
    pub fn from(config: &crate::config::StreamConfig) -> Self {
        Self {
            preset: config.sw_preset.clone(),
            threads: config.threads,
            fec: config.fec,
            audio: config.audio,
            low_latency: config.latency == "low",
        }
    }

    /// Whether the running instance must restart for `wanted`. Audio does
    /// not count: it mutes here at once, and the host's capture follows the
    /// next time Sunshine starts anyway.
    pub fn restart_needed(&self, wanted: &Tuning) -> bool {
        (&self.preset, self.threads, self.fec, self.low_latency) != (&wanted.preset, wanted.threads, wanted.fec, wanted.low_latency)
    }

    fn lines(&self) -> String {
        // Low latency, per encoder -- Sunshine uses whichever the host has:
        // NVENC's fastest preset without a second pass, AMD's ultra-low-latency
        // usage, QSV's fastest, VA-API's strict per-frame size (no oversized
        // frame stalls the network), and the software encoder's fastest preset.
        let low = if self.low_latency {
            "nvenc_preset = 1\nnvenc_twopass = disabled\nnvenc_latency_over_power = enabled\nnvenc_spatial_aq = disabled\n\
             amd_usage = ultralowlatency\namd_quality = speed\namd_preanalysis = disabled\namd_vbaq = disabled\n\
             qsv_preset = veryfast\nvaapi_strict_rc_buffer = enabled\n"
        } else {
            ""
        };
        let preset = if self.low_latency { "ultrafast" } else { self.preset.as_str() };
        low.to_string() + &format!(
            "sw_preset = {}\nsw_tune = zerolatency\nmin_threads = {}\nfec_percentage = {}\nstream_audio = {}\n",
            preset,
            self.threads,
            self.fec,
            if self.audio { "enabled" } else { "disabled" }
        )
    }
}

pub fn base_port(facts: &Facts) -> u16 {
    if facts.running || facts.port_busy {
        SHIFTED_PORT
    } else {
        DEFAULT_PORT
    }
}

const DIR: &str = r#"D="$HOME/.config/pulseterm/sunshine""#;

/// Writes the config and clears any instance a crash left behind. `capture`
/// pins the method (`probe::capture` decides: X11 on X11, KMS on Wayland when
/// permitted); `None` lets Sunshine pick (wlroots, then the portal).
fn prepare_script(port: u16, capture: Option<&str>, tuning: &Tuning) -> String {
    let capture = capture.map(|c| format!("capture = {c}\n")).unwrap_or_default();
    let tuning = tuning.lines();
    format!(
        r#"set -e
{DIR}
mkdir -p "$D"; chmod 700 "$D"
# An earlier instance -- a crash, or a Stop a moment ago whose wrapper is
# still winding down -- must be gone and its port free before this one starts:
# otherwise the ready check below sees the *old* one listening and pairing
# talks to a Sunshine that will never read the PIN.
old=""
if [ -f "$D/pid" ]; then old=$(cat "$D/pid"); kill "$old" 2>/dev/null || true; fi
i=0
while [ $i -lt 50 ]; do
  busy=no
  [ -n "$old" ] && kill -0 "$old" 2>/dev/null && busy=yes
  ss -ltn 2>/dev/null | grep -q ":{port} " && busy=yes
  [ $busy = no ] && break
  i=$((i+1)); sleep 0.2
done
rm -f "$D/pid"
cat > "$D/sunshine.conf" <<EOF
sunshine_name = PulseTerm
port = {port}
file_state = $D/state.json
credentials_file = $D/credentials.json
log_path = $D/sunshine.log
origin_web_ui_allowed = pc
{capture}{tuning}EOF
echo ok"#
    )
}

/// The wrapper. Sunshine gets the graphical session's environment from the
/// user's systemd manager (an SSH session has no `WAYLAND_DISPLAY`), reads its
/// stdin from a FIFO that `cat` fills from the channel, and is killed the
/// moment `cat` sees the channel close.
///
/// `XDG_CONFIG_HOME` is pointed at `~/.config/pulseterm`: Sunshine keeps
/// files outside its config file's settings -- the screen-sharing portal's
/// restore token, `apps.json` -- in `$XDG_CONFIG_HOME/sunshine`, which would
/// otherwise be the user's own Sunshine folder.
fn run_script() -> String {
    format!(
        r#"{DIR}
for v in WAYLAND_DISPLAY DISPLAY XDG_SESSION_TYPE XDG_CURRENT_DESKTOP DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR XAUTHORITY; do
  val=$(systemctl --user show-environment 2>/dev/null | sed -n "s/^$v=//p")
  [ -n "$val" ] && export "$v=$val"
done
# X11 capture needs the display's cookie. The user manager does not always
# carry XAUTHORITY; GDM keeps it here, older setups in the home directory.
if [ -n "$DISPLAY" ] && [ -z "$XAUTHORITY" ]; then
  for a in "/run/user/$(id -u)/gdm/Xauthority" "$HOME/.Xauthority"; do
    [ -r "$a" ] && {{ export XAUTHORITY="$a"; break; }}
  done
fi
[ -z "$DISPLAY" ] && [ -z "$WAYLAND_DISPLAY" ] && [ -S /tmp/.X11-unix/X0 ] && export DISPLAY=:0
export XDG_CONFIG_HOME="$HOME/.config/pulseterm"
f="$D/stdin"; rm -f "$f"; mkfifo -m 600 "$f"
sunshine -0 -1 "$D/sunshine.conf" <"$f" >"$D/run.log" 2>&1 &
pid=$!
echo "$pid" > "$D/pid"
cat >"$f"
kill "$pid" 2>/dev/null
for i in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.3; done
kill -9 "$pid" 2>/dev/null
# Only our own files: a newer instance may already have written its own.
[ "$(cat "$D/pid" 2>/dev/null)" = "$pid" ] && rm -f "$f" "$D/pid""#
    )
}

/// Waits (up to 90 s) for Sunshine to listen on its base port, or reports
/// why it did not with the tail of its log. Normally a second or two; the
/// first start on a Wayland host waits for the user to allow screen sharing
/// in a dialog on the host's screen, which is why the limit is generous.
fn ready_script(port: u16) -> String {
    format!(
        r#"{DIR}
i=0
while [ $i -lt 360 ]; do
  if ss -ltn 2>/dev/null | grep -q ":{port} "; then echo ready; exit 0; fi
  if [ -f "$D/pid" ] && ! kill -0 "$(cat "$D/pid")" 2>/dev/null; then echo exited; tail -n 4 "$D/run.log"; exit 1; fi
  i=$((i+1)); sleep 0.25
done
echo "not listening after 90 s"; tail -n 4 "$D/run.log"; exit 1"#
    )
}

/// A running instance: the spawned channel that keeps it alive, and its port.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    pub handle: u64,
    pub port: u16,
}

pub async fn start(bridge: &Bridge, port: u16, capture: Option<&str>, tuning: &Tuning) -> Result<Instance, String> {
    let prepared = bridge.exec(&prepare_script(port, capture, tuning)).await?;
    if prepared.status != Some(0) {
        return Err(format!("could not prepare Sunshine's config: {}", prepared.stderr.trim()));
    }
    let handle = bridge.spawn(&run_script()).await?;
    let ready = bridge.exec(&ready_script(port)).await?;
    if ready.status == Some(0) {
        Ok(Instance { handle, port })
    } else {
        let _ = bridge.kill(handle).await;
        Err(clean_log(&ready.stdout))
    }
}

/// Stops an instance and waits (up to 5 s) until it is really gone, so a
/// Connect straight after starts against a free port. Closing the channel
/// only *asks*: the wrapper takes a moment to kill Sunshine.
pub async fn stop(bridge: &Bridge, instance: Instance) {
    let _ = bridge.kill(instance.handle).await;
    let _ = bridge.exec(&gone_script(instance.port)).await;
}

fn gone_script(port: u16) -> String {
    format!(
        r#"{DIR}
i=0
while [ $i -lt 50 ]; do
  alive=no
  [ -f "$D/pid" ] && kill -0 "$(cat "$D/pid")" 2>/dev/null && alive=yes
  ss -ltn 2>/dev/null | grep -q ":{port} " && alive=yes
  [ $alive = no ] && exit 0
  i=$((i+1)); sleep 0.1
done"#
    )
}

/// The reason from `ready_script`'s output: its first line, plus the last
/// log line with Sunshine's timestamp prefix trimmed off.
fn clean_log(out: &str) -> String {
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("Sunshine did not start").to_string();
    match lines.last() {
        Some(last) => {
            let last = last.rsplit_once("]: ").map(|(_, rest)| rest).unwrap_or(last);
            format!("{first}: {last}")
        }
        None => first,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_ports_only_when_the_default_is_taken() {
        let mut f = Facts::default();
        assert_eq!(base_port(&f), DEFAULT_PORT);
        f.running = true;
        assert_eq!(base_port(&f), SHIFTED_PORT);
        // Sunshine uses base-5 .. base+21: the shifted range must not overlap.
        assert!(SHIFTED_PORT - 5 > DEFAULT_PORT + 21);
    }

    #[test]
    fn the_config_is_ours_alone() {
        let tuning = Tuning { preset: "ultrafast".into(), threads: 4, fec: 10, audio: false, low_latency: false };
        let low = Tuning { low_latency: true, preset: "veryfast".into(), ..tuning.clone() };
        let l = prepare_script(48089, None, &low);
        assert!(l.contains("nvenc_preset = 1\n") && l.contains("amd_usage = ultralowlatency\n"));
        assert!(l.contains("sw_preset = ultrafast\n"), "low latency overrides the software preset");
        let s = prepare_script(48089, None, &tuning);
        assert!(s.contains("sw_preset = ultrafast\n") && s.contains("min_threads = 4\n"));
        assert!(s.contains("fec_percentage = 10\n") && s.contains("stream_audio = disabled\n"));
        assert!(s.contains("port = 48089"));
        assert!(s.contains(".config/pulseterm/sunshine"));
        assert!(s.contains("origin_web_ui_allowed = pc"), "web UI is never on the network");
        assert!(!s.contains("capture"));
        assert!(prepare_script(47989, Some("kms"), &tuning).contains("capture = kms\n"));
        assert!(prepare_script(47989, Some("x11"), &tuning).contains("capture = x11\n"));
    }

    #[test]
    fn failure_reason_is_readable() {
        let out = "exited\n[2026-11-02 04:06:37.053]: Error: Couldn't bind to port\n";
        assert_eq!(clean_log(out), "exited: Error: Couldn't bind to port");
    }
}
