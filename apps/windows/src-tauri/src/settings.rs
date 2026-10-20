use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::serve::HostingConfig;
use crate::state::AppState;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub language: String,
    pub keep_hosting_in_tray: bool,
    pub start_hosting_on_launch: bool,
    pub hosting: Option<HostingConfig>,
    pub last_project: Option<String>,
    pub recent_projects: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: "auto".into(),
            keep_hosting_in_tray: true,
            start_hosting_on_launch: false,
            hosting: None,
            last_project: None,
            recent_projects: Vec::new(),
        }
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    path(app)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write via a temporary file so a crash never leaves a half-written settings file.
fn save(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = path(app).ok_or("no config directory")?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn settings_get(app: AppHandle) -> Settings {
    app.state::<AppState>().settings.lock().unwrap().clone()
}

#[tauri::command]
pub async fn settings_save(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    save(&app, &settings)?;
    *app.state::<AppState>().settings.lock().unwrap() = settings.clone();
    Ok(settings)
}

/// The UI reports its resolved language ("en" / "zh") so the tray menu matches it.
#[tauri::command]
pub async fn ui_language_set(app: AppHandle, language: String) {
    *app.state::<AppState>().ui_language.lock().unwrap() = language;
    crate::tray::refresh(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_hosting_in_tray_and_do_not_autostart_hosting() {
        let s = Settings::default();
        assert!(s.keep_hosting_in_tray);
        assert!(!s.start_hosting_on_launch);
        assert_eq!(s.language, "auto");
    }

    #[test]
    fn matches_the_json_the_ui_sends_and_tolerates_missing_fields() {
        // exactly what src/platform/types.ts AppSettings serialises to
        let ui = r#"{"language":"zh","keepHostingInTray":false,"startHostingOnLaunch":true,
            "hosting":{"dir":"C:\\srv\\p.rgit","init":true,"listen":"127.0.0.1:7878","insecureListenAny":false},
            "lastProject":"C:\\work\\demo","recentProjects":["C:\\work\\demo"]}"#;
        let s: Settings = serde_json::from_str(ui).unwrap();
        assert_eq!(s.language, "zh");
        assert!(!s.keep_hosting_in_tray);
        assert_eq!(s.hosting.as_ref().unwrap().listen, "127.0.0.1:7878");
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["hosting"]["insecureListenAny"], false);
        assert_eq!(back["recentProjects"][0], "C:\\work\\demo");
        // an older/partial file still loads
        let partial: Settings = serde_json::from_str(r#"{"language":"en"}"#).unwrap();
        assert!(partial.keep_hosting_in_tray);
    }
}
