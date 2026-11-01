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

pub fn base_port(facts: &Facts) -> u16 {
    if facts.running || facts.port_busy {
        SHIFTED_PORT
    } else {
        DEFAULT_PORT
    }
}

const DIR: &str = r#"D="$HOME/.config/pulseterm/sunshine""#;

/// Writes the config and clears any instance a crash left behind. With
/// `kms`, capture is pinned to KMS -- no portal dialog, so a host with nobody
/// at it streams; otherwise Sunshine picks (wlroots, X11, then the portal).
fn prepare_script(port: u16, kms: bool) -> String {
    let capture = if kms { "capture = kms\n" } else { "" };
    format!(
        r#"set -e
{DIR}
mkdir -p "$D"; chmod 700 "$D"
if [ -f "$D/pid" ]; then kill "$(cat "$D/pid")" 2>/dev/null || true; rm -f "$D/pid"; fi
cat > "$D/sunshine.conf" <<EOF
sunshine_name = PulseTerm
port = {port}
file_state = $D/state.json
credentials_file = $D/credentials.json
log_path = $D/sunshine.log
origin_web_ui_allowed = pc
{capture}EOF
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
export XDG_CONFIG_HOME="$HOME/.config/pulseterm"
f="$D/stdin"; rm -f "$f"; mkfifo -m 600 "$f"
sunshine -0 -1 "$D/sunshine.conf" <"$f" >"$D/run.log" 2>&1 &
pid=$!
echo "$pid" > "$D/pid"
cat >"$f"
kill "$pid" 2>/dev/null
for i in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.3; done
kill -9 "$pid" 2>/dev/null
rm -f "$f" "$D/pid""#
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

pub async fn start(bridge: &Bridge, port: u16, kms: bool) -> Result<Instance, String> {
    let prepared = bridge.exec(&prepare_script(port, kms)).await?;
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

pub async fn stop(bridge: &Bridge, instance: Instance) {
    let _ = bridge.kill(instance.handle).await;
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
        let s = prepare_script(48089, false);
        assert!(s.contains("port = 48089"));
        assert!(s.contains(".config/pulseterm/sunshine"));
        assert!(s.contains("origin_web_ui_allowed = pc"), "web UI is never on the network");
        assert!(!s.contains("capture"));
        assert!(prepare_script(47989, true).contains("capture = kms\n"));
    }

    #[test]
    fn failure_reason_is_readable() {
        let out = "exited\n[2026-11-02 04:06:37.053]: Error: Couldn't bind to port\n";
        assert_eq!(clean_log(out), "exited: Error: Couldn't bind to port");
    }
}
