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
. /etc/os-release 2>/dev/null && { echo "distro=$PRETTY_NAME"; echo "distro_id=$ID"; echo "distro_like=$ID_LIKE"; echo "distro_version=$VERSION_ID"; }
echo "arch=$(uname -m)"
# NVENC needs NVIDIA's desktop driver; a Jetson has nvidia-smi but a
# different encoder that Sunshine does not drive.
ldconfig -p 2>/dev/null | grep -q libnvidia-encode && echo "nvenc=yes"
[ -f /etc/nv_tegra_release ] && echo "jetson=yes"
echo "hostname=$(uname -n)"
# Where the SSH connection comes from: the stream comes from there too.
echo "client=${SSH_CLIENT%% *}"
# A firewall drops the stream's direct connection while pairing (over SSH)
# still works -- "RTSP handshake failed (110)", a timeout. Its rules need root
# to read, so only whether one is active is known here.
for s in ufw firewalld; do systemctl is-active --quiet "$s" 2>/dev/null && echo "firewall=$s"; done
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
command -v nvidia-smi >/dev/null && t nvidia-smi -L 2>&1 | head -n1 | sed 's/^/nvidia=/'
# Keyboard, mouse and gamepads reach the host through uinput: writable,
# present but not ours, a module not loaded, or no module in this kernel at all
# (an NVIDIA Jetson's tegra kernel, for one).
if [ -e /dev/uinput ]; then
  [ -w /dev/uinput ] && echo "uinput=yes" || echo "uinput=no"
else
  modinfo uinput >/dev/null 2>&1 && echo "uinput=unloaded" || echo "uinput=missing"
fi
echo "outputs=$(cat /sys/class/drm/card*-*/status 2>/dev/null | grep -c '^connected')"
[ "$ours" = no ] && command -v ss >/dev/null && echo "port_busy=$(ss -ltn 2>/dev/null | grep -c ':47989 ')"
true
"#;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub distro: String,
    pub hostname: String,
    /// The client's address as the host sees it (`$SSH_CLIENT`), if any.
    pub client: String,
    /// An active host firewall: `ufw` or `firewalld`.
    pub firewall: Option<String>,
    pub sunshine: Option<String>,
    pub sunshine_version: String,
    pub running: bool,
    pub session: Option<String>,
    pub desktop: String,
    pub gpus: Vec<String>,
    /// VA-API profiles with an encode entrypoint, e.g. `VAProfileH264Main`.
    pub encode: Vec<String>,
    pub nvidia: Option<String>,
    pub distro_id: String,
    /// `ID_LIKE`: what a derivative (EndeavourOS, CachyOS, Manjaro) is built on.
    pub distro_like: String,
    pub distro_version: String,
    /// `uname -m`: x86_64, aarch64, ...
    pub arch: String,
    /// NVIDIA's encode library is installed (desktop driver).
    pub nvenc: bool,
    /// An NVIDIA Jetson: its video encoder is not one Sunshine uses.
    pub jetson: bool,
    /// NVIDIA's driver was upgraded but the old kernel module is still
    /// loaded ("Driver/library version mismatch"): nothing can use the GPU
    /// until the host reboots. An Arch `-Syu` does this.
    pub nvidia_mismatch: bool,
    pub uinput: bool,
    /// `yes`, `no` (not writable), `unloaded`, or `missing` from the kernel.
    pub uinput_state: String,
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
                "client" => f.client = value.into(),
                "firewall" if !value.is_empty() => f.firewall = Some(value.into()),
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
                "nvidia" if value.contains("version mismatch") => f.nvidia_mismatch = true,
                "nvidia" if value.contains("Failed to initialize NVML") => {}
                "nvidia" if !value.is_empty() => f.nvidia = Some(value.into()),
                "distro_id" => f.distro_id = value.into(),
                "distro_like" => f.distro_like = value.trim_matches('"').into(),
                "distro_version" => f.distro_version = value.trim_matches('"').into(),
                "arch" => f.arch = value.into(),
                "nvenc" => f.nvenc = value == "yes",
                "jetson" => f.jetson = value == "yes",
                "uinput" => {
                    f.uinput = value == "yes";
                    f.uinput_state = value.into();
                }
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
        let nvenc = self.nvenc;
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
            fix: Some(install_hint(f)),
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
    out.push(if f.nvidia_mismatch {
        Check {
            label: "Hardware encoding",
            state: State::Fail,
            detail: "NVIDIA's driver was updated but the old one is still loaded (driver/library version mismatch), so the GPU cannot encode or capture -- reboot the host".into(),
            fix: Some("sudo reboot".into()),
        }
    } else if f.jetson {
        Check {
            label: "Hardware encoding",
            state: State::Warn,
            detail: "NVIDIA Jetson: Sunshine cannot use its video encoder, so it encodes on the CPU -- expect 1080p at 30-60 fps in H.264".into(),
            fix: None,
        }
    } else if codecs.is_empty() {
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
    // Without uinput, Sunshine still types and clicks on an X11 desktop
    // through XTest (verified on a Jetson whose kernel has no uinput); what is
    // lost there is gamepads. On Wayland, uinput is the only way in.
    let x11 = f.session.as_deref() == Some("x11");
    let lost = if x11 { "keyboard and mouse still work through X11, but gamepads need uinput" } else { "no keyboard, mouse or gamepads" };
    out.push(if f.uinput {
        Check { label: "Input", state: State::Ok, detail: "/dev/uinput writable".into(), fix: None }
    } else if f.uinput_state == "missing" && x11 {
        Check {
            label: "Input",
            state: State::Warn,
            detail: format!("this kernel has no uinput module -- {lost}"),
            fix: None,
        }
    } else if f.uinput_state == "missing" {
        Check {
            label: "Input",
            state: State::Warn,
            detail: "this kernel has no uinput module -- the picture streams, but the host cannot receive keyboard, mouse or gamepads until uinput is built for it".into(),
            fix: None,
        }
    } else if f.uinput_state == "unloaded" {
        Check {
            label: "Input",
            state: State::Warn,
            detail: format!("the uinput module is not loaded -- {lost}"),
            fix: Some("sudo modprobe uinput && echo uinput | sudo tee /etc/modules-load.d/uinput.conf".into()),
        }
    } else {
        Check {
            label: "Input",
            state: State::Warn,
            detail: format!("/dev/uinput not writable -- {lost}"),
            fix: Some(
                "echo 'KERNEL==\"uinput\", SUBSYSTEM==\"misc\", OPTIONS+=\"static_node=uinput\", TAG+=\"uaccess\"' | sudo tee /etc/udev/rules.d/60-sunshine.rules && sudo udevadm trigger"
                    .into(),
            ),
        }
    });
    if let Some(fw) = &f.firewall {
        out.push(firewall_check(fw, &f.client, crate::sunshine::base_port(f)));
    }
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

/// The stream connects to the host directly, not over SSH: TCP for the
/// handshake (base + 21) and UDP for video, control and audio (base + 9 to
/// base + 11). Pairing and launching go over SSH, so nothing else -- the web
/// admin page included -- needs opening. Opened to the client's network only.
fn firewall_check(firewall: &str, client: &str, base: u16) -> Check {
    let (tcp, udp) = (base + 21, (base + 9, base + 11));
    let from = local_network(client);
    let fix = match firewall {
        "ufw" => {
            let source = from.as_deref().map(|n| format!("from {n} to any ")).unwrap_or_default();
            format!(
                "sudo ufw allow {source}port {tcp} proto tcp && sudo ufw allow {source}port {}:{} proto udp",
                udp.0, udp.1
            )
        }
        _ => match from.as_deref() {
            Some(net) => format!(
                "sudo firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address={net} port port={tcp} protocol=tcp accept' --add-rich-rule='rule family=ipv4 source address={net} port port={}-{} protocol=udp accept' && sudo firewall-cmd --reload",
                udp.0, udp.1
            ),
            None => format!(
                "sudo firewall-cmd --permanent --add-port={tcp}/tcp --add-port={}-{}/udp && sudo firewall-cmd --reload",
                udp.0, udp.1
            ),
        },
    };
    Check {
        label: "Firewall",
        state: State::Warn,
        detail: format!(
            "{firewall} is active -- the stream needs TCP {tcp} and UDP {}-{} open to this computer, or connecting stops at the RTSP handshake",
            udp.0, udp.1
        ),
        fix: Some(fix),
    }
}

/// The client's IPv4 network as a /24, for a firewall rule that admits the
/// local network and not the internet. `None` for IPv6 or no address.
fn local_network(client: &str) -> Option<String> {
    let octets: Vec<u8> = client.split('.').map(|o| o.parse().ok()).collect::<Option<_>>()?;
    (octets.len() == 4).then(|| format!("{}.{}.{}.0/24", octets[0], octets[1], octets[2]))
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

/// The capture method to pin in PulseTerm's Sunshine config, or `None` to let
/// Sunshine choose. An X11 session always captures through X11: reliable, no
/// dialog, and KMS is not always there to fall back on (a Jetson's display
/// driver has no universal planes, and Sunshine's KMS capture then fails to
/// start at all). On Wayland, KMS when permitted -- it is what spares the
/// portal dialog.
pub fn capture(f: &Facts) -> Option<&'static str> {
    match f.session.as_deref() {
        Some("x11") => Some("x11"),
        _ if f.kms && !f.jetson => Some("kms"),
        _ => None,
    }
}

/// How the screen will be captured, and whether that needs someone at the
/// host. KMS never does; the screen-sharing portal asks once (then remembers);
/// wlroots compositors and X11 never ask.
fn capture_check(f: &Facts) -> Check {
    if capture(f) == Some("x11") {
        return Check {
            label: "Capture",
            state: State::Ok,
            detail: "X11 -- no dialog on the host".into(),
            fix: None,
        };
    }
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

/// The one command that installs Sunshine on this host. For Ubuntu and Debian
/// it fetches the current release's .deb for this exact version and CPU
/// architecture -- the file names carry the version number, so a fixed link
/// would break with the next release.
fn install_hint(f: &Facts) -> String {
    let d = f.distro.to_lowercase();
    let arch = match f.arch.as_str() {
        "aarch64" | "arm64" => "arm64",
        _ => "amd64",
    };
    let deb = match f.distro_id.as_str() {
        "ubuntu" if !f.distro_version.is_empty() => Some(format!("ubuntu{}_{arch}", f.distro_version)),
        "debian" => Some(format!("debiantrixie_{arch}")),
        _ => None,
    };
    if let Some(deb) = deb {
        return format!(
            "url=$(curl -s https://api.github.com/repos/LizardByte/Sunshine/releases/latest | grep -o 'https://[^\"]*{deb}.deb' | head -n1) && wget -O /tmp/sunshine.deb \"$url\" && sudo apt install -y /tmp/sunshine.deb"
        );
    }
    // Arch and everything built on it: LizardByte's own pacman repository,
    // which LizardByte calls the only official source (it does not support
    // the AUR). Added once, then a full upgrade with the install -- Arch's
    // rule: installing after a bare `-Sy` risks a partly upgraded system.
    let arch_based = f.distro_id == "arch" || f.distro_like.split_whitespace().any(|l| l == "arch");
    if arch_based {
        return "grep -q '^\\[lizardbyte\\]' /etc/pacman.conf || printf '\\n[lizardbyte]\\nSigLevel = Optional\\nServer = https://github.com/LizardByte/pacman-repo/releases/latest/download\\n' | sudo tee -a /etc/pacman.conf >/dev/null; sudo pacman -Syu lizardbyte/sunshine".into();
    }
    if d.contains("fedora") {
        "sudo dnf copr enable lizardbyte/stable && sudo dnf install Sunshine".into()
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
        let jetson = Facts::parse(
            "distro=Ubuntu 24.04.5 LTS\ndistro_id=ubuntu\ndistro_version=\"24.04\"\narch=aarch64\njetson=yes\nnvidia=GPU 0: Orin (nvgpu)\n",
        );
        let arch = Facts::parse("distro=Arch Linux\ndistro_id=arch\narch=x86_64\n");
        let endeavour = Facts::parse("distro=EndeavourOS\ndistro_id=endeavouros\ndistro_like=arch\n");
        for host in [&arch, &endeavour] {
            let h = install_hint(host);
            assert!(h.contains("pacman -Syu lizardbyte/sunshine") && !h.contains("AUR"), "{h}");
        }
        let hint = install_hint(&jetson);
        assert!(hint.contains("ubuntu24.04_arm64.deb"), "{hint}");
        assert!(jetson.codecs().is_empty(), "nvidia-smi alone is not NVENC");
        assert!(checks(&jetson).iter().any(|c| c.label == "Hardware encoding" && c.detail.contains("Jetson")));
    }

    #[test]
    fn x11_sessions_capture_through_x11_even_with_kms() {
        // orion: a Jetson on X11 whose Sunshine has the KMS capability.
        let f = Facts::parse("session=x11\nkms=yes\njetson=yes\n");
        assert_eq!(capture(&f), Some("x11"));
        assert_eq!(capture(&Facts::parse("session=wayland\nkms=yes\n")), Some("kms"));
        assert_eq!(capture(&Facts::parse("session=wayland\n")), None);
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
    fn input_advice_fits_the_cause() {
        let fix = |text: &str| checks(&Facts::parse(text)).into_iter().find(|c| c.label == "Input").unwrap();
        assert!(fix("uinput=missing\n").fix.is_none(), "no command can add a missing module");
        assert!(fix("uinput=unloaded\n").fix.unwrap().contains("modprobe"));
        assert!(fix("uinput=no\n").fix.unwrap().contains("udev"));
        assert_eq!(fix("uinput=yes\n").state, State::Ok);
        // orion: no uinput, but an X11 desktop -- keyboard and mouse work.
        let jetson = fix("uinput=missing\nsession=x11\n");
        assert!(jetson.detail.contains("keyboard and mouse still work"), "{}", jetson.detail);
        assert!(fix("uinput=missing\nsession=wayland\n").detail.contains("cannot receive keyboard"));
    }

    #[test]
    fn an_updated_but_unloaded_nvidia_driver_blocks_with_reboot() {
        let f = Facts::parse("nvidia=Failed to initialize NVML: Driver/library version mismatch\nnvenc=yes\n");
        assert!(f.nvidia_mismatch && f.nvidia.is_none(), "the error is not a GPU name");
        let c = checks(&f);
        let enc = c.iter().find(|c| c.label == "Hardware encoding").unwrap();
        assert_eq!(enc.state, State::Fail);
        assert_eq!(enc.fix.as_deref(), Some("sudo reboot"));
        assert!(!ready(&c));
    }

    #[test]
    fn an_active_firewall_gets_the_streams_ports_for_the_local_network() {
        let f = Facts::parse("client=192.168.68.113\nfirewall=ufw\n");
        let c = checks(&f);
        let fw = c.iter().find(|c| c.label == "Firewall").expect("a firewall check");
        let fix = fw.fix.as_deref().unwrap();
        assert_eq!(
            fix,
            "sudo ufw allow from 192.168.68.0/24 to any port 48010 proto tcp && sudo ufw allow from 192.168.68.0/24 to any port 47998:48000 proto udp"
        );
        assert!(!fix.contains("47990"), "the web admin page stays closed");
        assert_eq!(fw.state, State::Warn, "a warning, not a blocker: the rules may already be there");
        let fwd = firewall_check("firewalld", "", 47989);
        assert!(fwd.fix.unwrap().contains("--add-port=48010/tcp --add-port=47998-48000/udp"));
        assert!(checks(&Facts::parse("client=10.0.0.5\n")).iter().all(|c| c.label != "Firewall"));
    }

    #[test]
    fn warnings_do_not_block() {
        let c = checks(&Facts::parse(SAMPLE));
        assert!(c.iter().any(|c| c.state == State::Warn), "uinput warns");
        assert!(ready(&c));
    }
}
