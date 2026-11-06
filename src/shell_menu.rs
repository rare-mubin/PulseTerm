//! "Open with PulseTerm" in File Explorer's right-click menu (Windows).
//!
//! Three verbs under `HKEY_CURRENT_USER\Software\Classes`, so registering needs
//! no administrator and touches no other account:
//!
//! * `*\shell\PulseTerm` -- on any file: `PulseTerm.exe "<file>"`, which
//!   opens it in the editor (`launch`, a bare path).
//! * `Directory\shell\PulseTerm` -- on a folder: a shell started in it.
//! * `Directory\Background\shell\PulseTerm` -- on the empty space inside a
//!   folder: the same, for the folder you are in.
//!
//! The command names this very executable (`current_exe`), so moving the exe
//! means switching the setting off and on again to point it at the new place.
//! On Windows 11 the verbs appear under "Show more options": the new menu
//! takes only packaged apps' handlers, which a plain exe cannot register.
//!
//! The Settings switch mirrors the registry rather than trusting itself --
//! `is_registered` is what the page shows -- so a menu removed by hand, or
//! added by an older copy, still reads truthfully.

/// Whether the menu is offered at all: Windows only. On Linux the desktop
/// entry (`packaging/linux`) is what puts PulseTerm in file managers.
pub const SUPPORTED: bool = cfg!(windows);

#[cfg(windows)]
mod imp {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    const LABEL: &str = "Open with PulseTerm";

    /// Each verb's key, and the argument Explorer substitutes: `%1` is the
    /// file clicked, `%V` the folder (or the folder you are inside).
    const VERBS: [(&str, &str); 3] = [
        (r"Software\Classes\*\shell\PulseTerm", "%1"),
        (r"Software\Classes\Directory\shell\PulseTerm", "%V"),
        (r"Software\Classes\Directory\Background\shell\PulseTerm", "%V"),
    ];

    pub fn is_registered() -> bool {
        VERBS.iter().all(|(key, _)| {
            let mut hkey = HKEY::default();
            let path = HSTRING::from(format!(r"{key}\command"));
            let found = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_READ, &mut hkey) }
                == ERROR_SUCCESS;
            if found {
                unsafe {
                    let _ = RegCloseKey(hkey);
                }
            }
            found
        })
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        if !enabled {
            for (key, _) in VERBS {
                // Absent already is fine: the point is that it is gone.
                unsafe {
                    let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
                }
            }
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| format!("Could not find PulseTerm.exe: {e}"))?;
        let exe = exe.to_string_lossy().to_string();
        for (key, arg) in VERBS {
            write(key, None, LABEL)?;
            write(key, Some("Icon"), &format!("\"{exe}\",0"))?;
            write(&format!(r"{key}\command"), None, &format!("\"{exe}\" \"{arg}\""))?;
        }
        Ok(())
    }

    /// Creates `key` if needed and sets one string value on it (`None` is the
    /// key's default value, which is what Explorer shows and runs).
    fn write(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let mut hkey = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("Could not create {key}: error {}", status.0));
        }
        // REG_SZ is UTF-16 with its terminating nul.
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
        let name = name.map(HSTRING::from);
        let status = unsafe {
            RegSetValueExW(
                hkey,
                name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
                None,
                REG_SZ,
                Some(bytes),
            )
        };
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if status != ERROR_SUCCESS {
            return Err(format!("Could not write {key}: error {}", status.0));
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{is_registered, set};

#[cfg(not(windows))]
pub fn is_registered() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set(_: bool) -> Result<(), String> {
    Err("File Explorer integration is Windows only".into())
}
