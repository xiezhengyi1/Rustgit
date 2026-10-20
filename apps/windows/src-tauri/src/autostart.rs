//! Start at login through the per-user registry Run key (HKCU\Software\Microsoft\Windows\
//! CurrentVersion\Run). This needs no administrator rights and installs no service; the user
//! can also remove it in Windows Settings > Apps > Startup.

#[cfg(windows)]
mod imp {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "RustGit";

    pub fn supported() -> bool {
        true
    }

    pub fn get() -> Result<bool, String> {
        get_named(VALUE)
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        set_named(VALUE, enabled)
    }

    pub(super) fn get_named(name: &str) -> Result<bool, String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .map_err(|e| e.to_string())?;
        Ok(key.get_value::<String, _>(name).is_ok())
    }

    #[cfg(test)]
    pub(super) fn value_named(name: &str) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .ok()?
            .get_value::<String, _>(name)
            .ok()
    }

    pub(super) fn set_named(name: &str, enabled: bool) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE | KEY_READ)
            .map_err(|e| e.to_string())?;
        if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            // Quoted path (it may contain spaces) and --background: start hidden in the tray.
            key.set_value(name, &format!("\"{}\" --background", exe.display()))
                .map_err(|e| e.to_string())
        } else {
            match key.delete_value(name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn supported() -> bool {
        false
    }
    pub fn get() -> Result<bool, String> {
        Ok(false)
    }
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("start at login is only implemented for Windows".into())
    }
}

#[tauri::command]
pub async fn autostart_supported() -> bool {
    imp::supported()
}

#[tauri::command]
pub async fn autostart_get() -> Result<bool, String> {
    imp::get()
}

#[tauri::command]
pub async fn autostart_set(enabled: bool) -> Result<(), String> {
    imp::set(enabled)
}

#[cfg(all(test, windows))]
mod tests {
    use super::imp::{get_named, set_named, value_named};

    /// Runs against the real per-user Run key, with a throwaway value name.
    #[test]
    fn run_key_roundtrip_is_user_level_and_quoted() {
        let name = "RustGitAutostartTest";
        set_named(name, false).unwrap();
        assert!(!get_named(name).unwrap());
        set_named(name, true).unwrap();
        assert!(get_named(name).unwrap());
        let v = value_named(name).unwrap();
        assert!(v.starts_with('"') && v.ends_with("\" --background"), "got {v}");
        set_named(name, false).unwrap();
        assert!(!get_named(name).unwrap());
        // deleting a missing value is not an error
        set_named(name, false).unwrap();
    }
}
