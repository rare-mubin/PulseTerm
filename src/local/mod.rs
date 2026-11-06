//! This machine as a host: its files, and a terminal on its own console
//! (ConPTY on Windows, a Unix pty on Linux).

#[cfg(target_os = "windows")]
mod conpty;
pub mod fs;
#[cfg(target_os = "linux")]
mod unix_pty;

use std::path::PathBuf;

use crate::pty::{Pty, PtyProgram};

/// Starts a terminal on this machine: ConPTY on Windows, a Unix pty on Linux.
/// Everything above this sees the same `Pty` either way.
pub fn open_pty(cols: u32, rows: u32, program: PtyProgram, shell: String) -> Result<Pty, String> {
    #[cfg(target_os = "windows")]
    return conpty::open(cols, rows, program, shell);
    #[cfg(target_os = "linux")]
    return unix_pty::open(cols, rows, program, shell);
    // Other platforms still build and still do SSH, with one honest sentence
    // where a shell would be.
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = (cols, rows, program, shell);
        Err("Local terminals are not supported on this platform yet.".into())
    }
}

/// A shell that is actually installed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// What gets run. Also the value stored in Settings, so a shell that later
    /// disappears leaves a setting that says exactly what is missing.
    pub command: String,
    pub label: String,
}

#[cfg(target_os = "windows")]
fn system_root() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
}

#[cfg(target_os = "windows")]
fn candidates() -> Vec<(PathBuf, &'static str)> {
    let root = system_root();
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    vec![
        (
            PathBuf::from(&program_files).join("PowerShell\\7\\pwsh.exe"),
            "PowerShell 7",
        ),
        // The Microsoft Store install puts nothing in Program Files: all it
        // leaves on disk is this app execution alias, which CreateProcess runs
        // like any exe. Checked second so an MSI install, if both exist, wins.
        (
            PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
                .join("Microsoft\\WindowsApps\\pwsh.exe"),
            "PowerShell 7",
        ),
        (
            root.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            "Windows PowerShell",
        ),
        (root.join("System32\\cmd.exe"), "Command Prompt"),
        (
            PathBuf::from(&program_files).join("Git\\bin\\bash.exe"),
            "Git Bash",
        ),
        (root.join("System32\\wsl.exe"), "WSL"),
    ]
}

/// The shells present on this machine, best first.
///
/// Probed rather than listed: offering a shell that isn't installed produces a
/// terminal tab that opens and immediately dies with a message about a missing
/// file, which is a worse answer than not offering it. The list is short and
/// this runs when the Settings page opens, not per frame.
#[cfg(target_os = "windows")]
pub fn shells() -> Vec<Shell> {
    let mut found: Vec<Shell> = Vec::new();
    for (path, label) in candidates() {
        // One entry per shell: an MSI and a Store PowerShell 7 are the same
        // choice, and the first (the MSI) is the one kept.
        if installed(&path) && !found.iter().any(|shell| shell.label == label) {
            found.push(Shell { command: path.to_string_lossy().to_string(), label: label.to_string() });
        }
    }
    found
}

/// Whether a shell is on disk. `Path::exists` follows links, and on a
/// Microsoft Store app execution alias (`WindowsApps\pwsh.exe`) following it
/// fails -- the alias is a reparse point only CreateProcess resolves -- so the
/// link itself being there counts too.
fn installed(path: &std::path::Path) -> bool {
    path.exists() || std::fs::symlink_metadata(path).is_ok()
}

/// What to run when Settings names nothing, or names something no longer here.
/// `cmd.exe` is the last resort because it is the one shell Windows always has.
#[cfg(target_os = "windows")]
pub fn default_shell() -> String {
    shells()
        .into_iter()
        .next()
        .map(|shell| shell.command)
        .unwrap_or_else(|| {
            system_root()
                .join("System32\\cmd.exe")
                .to_string_lossy()
                .to_string()
        })
}

/// The shells present on this machine, best first.
///
/// Same rule as on Windows -- probe, never list -- with the candidates this
/// platform has: `$SHELL` first, because it is the user's own answer; then the
/// distro's `/etc/shells`; then a short fallback for a system without one.
/// Deduplicated by resolved path, or `$SHELL` and `/etc/shells` would offer
/// bash twice (and Fedora lists `/bin/bash` *and* `/usr/bin/bash`).
#[cfg(not(target_os = "windows"))]
pub fn shells() -> Vec<Shell> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(shell) = std::env::var("SHELL") {
        candidates.push(shell);
    }
    if let Ok(list) = std::fs::read_to_string("/etc/shells") {
        candidates.extend(
            list.lines()
                .map(str::trim)
                .filter(|line| line.starts_with('/'))
                .map(String::from),
        );
    }
    candidates.extend(
        ["/bin/bash", "/usr/bin/zsh", "/usr/bin/fish", "/bin/sh"]
            .iter()
            .map(|s| s.to_string()),
    );

    let mut seen = Vec::new();
    let mut shells = Vec::new();
    for command in candidates {
        let path = PathBuf::from(&command);
        if !path.is_file() {
            continue;
        }
        let resolved = std::fs::canonicalize(&path).unwrap_or(path.clone());
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved);
        shells.push(Shell {
            label: unix_label(&path),
            command,
        });
    }
    shells
}

#[cfg(not(target_os = "windows"))]
fn unix_label(path: &std::path::Path) -> String {
    let stem = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    match stem.as_str() {
        "bash" => "Bash".into(),
        "zsh" => "Zsh".into(),
        "fish" => "Fish".into(),
        "nu" => "Nushell".into(),
        "pwsh" => "PowerShell".into(),
        "tmux" => "tmux".into(),
        _ => stem,
    }
}

/// What to run when Settings names nothing, or names something no longer here.
/// `/bin/sh` is the last resort because it is the one shell POSIX guarantees.
#[cfg(not(target_os = "windows"))]
pub fn default_shell() -> String {
    shells()
        .into_iter()
        .next()
        .map(|shell| shell.command)
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Resolves the setting to a command line: what was chosen, if it is still
/// installed, and otherwise the default. A stale path would open a tab that
/// dies on arrival.
pub fn resolve_shell(configured: &str) -> String {
    if !configured.is_empty() && installed(std::path::Path::new(configured)) {
        return configured.to_string();
    }
    default_shell()
}
