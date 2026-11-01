//! What the host can do, asked over SSH in one round trip.
//!
//! One shell script prints `key=value` lines; `Facts::parse` reads them and
//! `checks` turns them into the launcher's checklist. Both are pure so they are
//! tested here -- the script is the only part that needs a real host.
//!
//! Everything is read-only. Nothing here starts, installs or changes anything.

/// Run with `sh -c` on the host. Each probe is guarded so a missing tool
/// prints nothing rather than failing the whole script, and `timeout` keeps a
/// wedged tool from holding the launcher up.
pub const SCRIPT: &str = r#"
t() { command -v timeout >/dev/null && timeout 5 "$@" || "$@"; }
. /etc/os-release 2>/dev/null && echo "distro=$PRETTY_NAME"
echo "hostname=$(uname -n)"
echo "sunshine=$(command -v sunshine)"
# Even `--version` creates and writes ~/.config/sunshine -- the user's own
# Sunshine config -- so it runs against a throwaway one.
if command -v sunshine >/dev/null; then
  tmp=$(mktemp -d)
  echo "sunshine_version=$(XDG_CONFIG_HOME="$tmp" t sunshine --version 2>&1 | grep -o 'Sunshine version: [^ ]*' | head -n1 | sed 's/Sunshine version: //')"
  rm -rf "$tmp"
fi
# The user's own Sunshine -- never PulseTerm's, whose config path gives it away.
# KMS capture reads the screen from the driver: no portal dialog, nobody at
# the host needed. It needs CAP_SYS_ADMIN on the binary (Fedora's and the .deb
# set it; a Flatpak cannot have it).
command -v getcap >/dev/null && command -v sunshine >/dev/null && \
  getcap "$(readlink -f "$(command -v sunshine)")" 2>/dev/null | grep -q cap_sys_admin && echo "kms=yes"
echo "running=$(pgrep -ax sunshine | grep -v 'pulseterm/sunshine' | head -n1 | cut -d' ' -f1)"
ours=no
P="$HOME/.config/pulseterm/sunshine/pid"
[ -f "$P" ] && kill -0 "$(cat "$P")" 2>/dev/null && ours=yes
echo "ours=$ours"
for s in $(loginctl list-sessions --no-legend 2>/dev/null | awk '{print $1}'); do
  ty=$(loginctl show-session "$s" -p Type --value 2>/dev/null)
  us=$(loginctl show-session "$s" -p Name --value 2>/dev/null)
  if [ "$us" = "$(id -un)" ] && { [ "$ty" = wayland ] || [ "$ty" = x11 ]; }; then
    echo "session=$ty"
    echo "desktop=$(loginctl show-session "$s" -p Desktop --value 2>/dev/null)"
    break
  fi
done
for v in /sys/class/drm/card*/device/vendor; do [ -r "$v" ] && echo "gpu_vendor=$(cat "$v")"; done
command -v vainfo >/dev/null && t vainfo --display drm 2>/dev/null | grep VAEntrypointEncSlice | awk '{print "encode=" $1}'
command -v nvidia-smi >/dev/null && t nvidia-smi -L 2>/dev/null | head -n1 | sed 's/^/nvidia=/'
[ -w /dev/uinput ] && echo "uinput=yes" || echo "uinput=no"
echo "outputs=$(cat /sys/class/drm/card*-*/status 2>/dev/null | grep -c '^connected')"
[ "$ours" = no ] && command -v ss >/dev/null && echo "port_busy=$(ss -ltn 2>/dev/null | grep -c ':47989 ')"
true
"#;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub distro: String,
    pub hostname: String,
    pub sunshine: Option<String>,
    pub sunshine_version: String,
    pub running: bool,
    pub session: Option<String>,
    pub desktop: String,
    pub gpus: Vec<String>,
    /// VA-API profiles with an encode entrypoint, e.g. `VAProfileH264Main`.
    pub encode: Vec<String>,
    pub nvidia: Option<String>,
    pub uinput: bool,
    pub outputs: u32,
    /// Something other than PulseTerm's instance holds Moonlight's base port.
    pub port_busy: bool,
    /// Sunshine may capture through KMS: no screen-sharing dialog on the host.
    pub kms: bool,
    /// PulseTerm's own instance is running (from an earlier window).
    #[allow(dead_code)]
    pub ours: bool,
}

impl Facts {
    pub fn parse(text: &str) -> Self {
        let mut f = Facts::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key {
                "distro" => f.distro = value.into(),
                "hostname" => f.hostname = value.into(),
                "sunshine" if !value.is_empty() => f.sunshine = Some(value.into()),
                "sunshine_version" => f.sunshine_version = value.into(),
                "running" => f.running = !value.is_empty(),
                "session" if !value.is_empty() => f.session = Some(value.into()),
                "desktop" => f.desktop = value.into(),
                "gpu_vendor" => {
                    let name = match value {
                        "0x1002" => "AMD",
                        "0x10de" => "NVIDIA",
                        "0x8086" => "Intel",
                        _ => continue,
                    };
                    if !f.gpus.iter().any(|g| g == name) {
                        f.gpus.push(name.into());
                    }
                }
                "encode" if !f.encode.iter().any(|e| e == value) => f.encode.push(value.into()),
                "nvidia" if !value.is_empty() => f.nvidia = Some(value.into()),
                "uinput" => f.uinput = value == "yes",
                "outputs" => f.outputs = value.parse().unwrap_or(0),
                "port_busy" => f.port_busy = value.parse::<u32>().unwrap_or(0) > 0,
                "ours" => f.ours = value == "yes",
                "kms" => f.kms = value == "yes",
                _ => {}
            }
        }
        f
    }

    /// Codecs the host can encode in hardware, in the order they are offered.
    pub fn codecs(&self) -> Vec<&'static str> {
        let has = |needle: &str| self.encode.iter().any(|e| e.contains(needle));
        let nvenc = self.nvidia.is_some();
        let mut out = Vec::new();
        if has("H264") || nvenc {
            out.push("H.264");
        }
        if has("HEVC") || nvenc {
            out.push("HEVC");
        }
        if has("AV1") {
            out.push("AV1");
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub label: &'static str,
    pub state: State,
    pub detail: String,
    /// A command to run on the host that fixes it -- only ever a command, so
    /// the launcher can offer it to copy. Advice goes in `detail`.
    pub fix: Option<String>,
}

/// The checklist, in the order a fix has to happen: no point fixing input
/// before Sunshine is there to use it.
pub fn checks(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(match &f.sunshine {
        Some(path) => Check {
            label: "Sunshine",
            state: State::Ok,
            // `--version` prints its log preamble first; the script keeps only
            // the number.
            detail: if f.sunshine_version.is_empty() { path.clone() } else { format!("version {}", f.sunshine_version) },
            fix: None,
        },
        None => Check {
            label: "Sunshine",
            state: State::Fail,
            detail: "not installed".into(),
            fix: Some(install_hint(&f.distro)),
        },
    });
    out.push(match f.session.as_deref() {
        Some(kind) => Check {
            label: "Desktop session",
            state: State::Ok,
            detail: if f.desktop.is_empty() { kind.into() } else { format!("{kind} · {}", f.desktop) },
            fix: None,
        },
        None => Check {
            label: "Desktop session",
            state: State::Fail,
            detail: "nobody is logged in graphically, so there is nothing to capture -- log in on the host's screen, or enable auto-login".into(),
            fix: None,
        },
    });
    out.push(if f.outputs > 0 {
        Check {
            label: "Display",
            state: State::Ok,
            detail: format!("{} connected", f.outputs),
            fix: None,
        }
    } else {
        Check {
            label: "Display",
            state: State::Warn,
            detail: "no monitor connected -- attach one, or an HDMI dummy plug; a headless host needs a virtual output".into(),
            fix: None,
        }
    });
    out.push(capture_check(f));
    let codecs = f.codecs();
    out.push(if codecs.is_empty() {
        Check {
            label: "Hardware encoding",
            state: State::Warn,
            detail: "none found -- streaming falls back to the CPU. Needs the GPU's VA-API driver (or NVIDIA's), and libva-utils to detect it".into(),
            fix: encoder_hint(&f.distro, &f.gpus),
        }
    } else {
        Check {
            label: "Hardware encoding",
            state: State::Ok,
            detail: codecs.join(" · "),
            fix: None,
        }
    });
    out.push(if f.uinput {
        Check { label: "Input", state: State::Ok, detail: "/dev/uinput writable".into(), fix: None }
    } else {
        Check {
            label: "Input",
            state: State::Warn,
            detail: "/dev/uinput not writable -- no keyboard, mouse or gamepads".into(),
            fix: Some(
                "echo 'KERNEL==\"uinput\", SUBSYSTEM==\"misc\", OPTIONS+=\"static_node=uinput\", TAG+=\"uaccess\"' | sudo tee /etc/udev/rules.d/60-sunshine.rules && sudo udevadm trigger"
                    .into(),
            ),
        }
    });
    if f.running || f.port_busy {
        out.push(Check {
            label: "Ports",
            state: State::Warn,
            detail: format!(
                "Moonlight's ports are taken (another Sunshine?) -- PulseTerm's starts on base port {}",
                crate::sunshine::SHIFTED_PORT
            ),
            fix: None,
        });
    }
    out
}

/// The one command that gets a hardware encoder going, where there is one.
/// Fedora ships Mesa with H.264/HEVC encoding switched off; RPM Fusion's
/// "freeworld" build has it.
fn encoder_hint(distro: &str, gpus: &[String]) -> Option<String> {
    let fedora = distro.to_lowercase().contains("fedora");
    let amd_or_intel = gpus.iter().any(|g| g == "AMD" || g == "Intel");
    match (fedora, amd_or_intel) {
        (true, true) if gpus.iter().any(|g| g == "Intel") => {
            Some("sudo dnf install intel-media-driver libva-utils".into())
        }
        (true, true) => Some("sudo dnf install mesa-va-drivers-freeworld libva-utils   (RPM Fusion)".into()),
        _ => None,
    }
}

/// How the screen will be captured, and whether that needs someone at the
/// host. KMS never does; the screen-sharing portal asks once (then remembers);
/// wlroots compositors and X11 never ask.
fn capture_check(f: &Facts) -> Check {
    if f.kms {
        return Check {
            label: "Capture",
            state: State::Ok,
            detail: "KMS -- no dialog on the host, works with nobody there".into(),
            fix: None,
        };
    }
    let wlroots = ["niri", "sway", "hyprland", "wayfire", "river"]
        .iter()
        .any(|c| f.desktop.to_lowercase().contains(c));
    match f.session.as_deref() {
        Some("x11") => Check {
            label: "Capture",
            state: State::Ok,
            detail: "X11 -- no dialog on the host".into(),
            fix: None,
        },
        Some("wayland") if wlroots => Check {
            label: "Capture",
            state: State::Ok,
            detail: "wlroots screencopy -- no dialog on the host".into(),
            fix: None,
        },
        _ => Check {
            label: "Capture",
            state: State::Warn,
            detail: "screen-sharing portal -- the host asks once to allow it, so someone has to be there the first time. KMS capture avoids that".into(),
            fix: Some("sudo setcap cap_sys_admin+p \"$(readlink -f \"$(command -v sunshine)\")\"".into()),
        },
    }
}

fn install_hint(distro: &str) -> String {
    let d = distro.to_lowercase();
    if d.contains("fedora") {
        "sudo dnf copr enable lizardbyte/stable && sudo dnf install Sunshine".into()
    } else if d.contains("arch") || d.contains("cachyos") || d.contains("endeavour") {
        "yay -S sunshine   (AUR)".into()
    } else if d.contains("ubuntu") || d.contains("debian") || d.contains("mint") || d.contains("pop") {
        "download the .deb from github.com/LizardByte/Sunshine/releases".into()
    } else {
        "flatpak install flathub dev.lizardbyte.app.Sunshine".into()
    }
}

/// True when nothing blocks streaming. Warnings do not; failures do.
pub fn ready(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.state != State::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "distro=Fedora Linux 44 (Workstation Edition)\nhostname=box\n\
        sunshine=/usr/bin/sunshine\nsunshine_version=2026.914.233613\n\
        running=\nsession=wayland\ndesktop=niri\ngpu_vendor=0x1002\ngpu_vendor=0x1002\n\
        encode=VAProfileH264Main\nencode=VAProfileHEVCMain\nencode=VAProfileH264Main\n\
        uinput=no\noutputs=1\nport_busy=0\n";

    #[test]
    fn parses_the_script_output() {
        let f = Facts::parse(SAMPLE);
        assert_eq!(f.hostname, "box");
        assert_eq!(f.sunshine.as_deref(), Some("/usr/bin/sunshine"));
        assert!(!f.running);
        assert_eq!(f.session.as_deref(), Some("wayland"));
        assert_eq!(f.gpus, vec!["AMD"]);
        assert_eq!(f.encode.len(), 2, "duplicates folded");
        assert_eq!(f.codecs(), vec!["H.264", "HEVC"]);
        assert!(!f.uinput);
        assert_eq!(f.outputs, 1);
    }

    #[test]
    fn empty_values_mean_absent() {
        let f = Facts::parse("sunshine=\nsession=\nrunning=\n");
        assert_eq!(f.sunshine, None);
        assert_eq!(f.session, None);
        assert!(!f.running);
    }

    #[test]
    fn missing_sunshine_blocks_and_suggests_the_distros_way() {
        let f = Facts::parse("distro=Fedora Linux 44\nsession=x11\noutputs=1\nuinput=yes\n");
        let c = checks(&f);
        assert_eq!(c[0].state, State::Fail);
        assert!(c[0].fix.as_deref().unwrap().contains("dnf"));
        assert!(!ready(&c));
    }

    #[test]
    fn capture_route_says_whether_anyone_must_be_there() {
        let kms = capture_check(&Facts::parse("kms=yes\nsession=wayland\ndesktop=GNOME\n"));
        assert_eq!(kms.state, State::Ok);
        let gnome = capture_check(&Facts::parse("session=wayland\ndesktop=GNOME\n"));
        assert_eq!(gnome.state, State::Warn);
        assert!(gnome.fix.unwrap().contains("setcap"));
        let niri = capture_check(&Facts::parse("session=wayland\ndesktop=niri\n"));
        assert_eq!(niri.state, State::Ok);
    }

    #[test]
    fn warnings_do_not_block() {
        let c = checks(&Facts::parse(SAMPLE));
        assert!(c.iter().any(|c| c.state == State::Warn), "uinput warns");
        assert!(ready(&c));
    }
}
