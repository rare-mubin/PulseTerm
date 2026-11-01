//! This machine as a host: its files, and a terminal on its own console.

pub mod conpty;
pub mod fs;

use std::path::PathBuf;

/// A shell that is actually installed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// What gets run. Also the value stored in Settings, so a shell that later
    /// disappears leaves a setting that says exactly what is missing.
    pub command: String,
    pub label: String,
}

fn system_root() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
}

fn candidates() -> Vec<(PathBuf, &'static str)> {
    let root = system_root();
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    vec![
        (
            PathBuf::from(&program_files).join("PowerShell\\7\\pwsh.exe"),
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
pub fn shells() -> Vec<Shell> {
    candidates()
        .into_iter()
        .filter(|(path, _)| path.exists())
        .map(|(path, label)| Shell {
            command: path.to_string_lossy().to_string(),
            label: label.to_string(),
        })
        .collect()
}

/// What to run when Settings names nothing, or names something no longer here.
/// `cmd.exe` is the last resort because it is the one shell Windows always has.
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

/// Resolves the setting to a command line: what was chosen, if it is still
/// installed, and otherwise the default. A stale path would open a tab that
/// dies on arrival.
pub fn resolve_shell(configured: &str) -> String {
    if !configured.is_empty() && PathBuf::from(configured).exists() {
        return configured.to_string();
    }
    default_shell()
}
