// Hide the console window in release builds on Windows.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod autostart;
mod net;
mod rgit;
mod serve;
mod settings;
mod state;
mod tray;

use std::time::Duration;

use tauri::{Manager, RunEvent, WindowEvent};

use state::AppState;

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub(crate) use show_main_window as show_window;

fn main() {
    let background = std::env::args().any(|a| a == "--background");

    let app = tauri::Builder::default()
        // A second launch must not start a second host process fighting over the same lock.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            rgit::rgit_spawn,
            rgit::rgit_cancel,
            rgit::reveal_path,
            net::lan_addresses_cmd,
            serve::serve_start,
            serve::serve_stop,
            serve::serve_status,
            settings::settings_get,
            settings::settings_save,
            settings::ui_language_set,
            autostart::autostart_supported,
            autostart::autostart_get,
            autostart::autostart_set,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let loaded = settings::load(&handle);
            let start_hosting = loaded.start_hosting_on_launch.then(|| loaded.hosting.clone()).flatten();
            *app.state::<AppState>().settings.lock().unwrap() = loaded;
            tray::create(&handle)?;
            if !background {
                show_main_window(&handle);
            }
            if let Some(cfg) = start_hosting {
                // Give the tray a moment to exist so its label follows the host's state.
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    let _ = serve::start(&h, cfg);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                let keep = state.settings.lock().unwrap().keep_hosting_in_tray;
                // Closing the window keeps the host alive in the tray only while it is hosting.
                if keep && serve::is_running(&state) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the application");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            let state = app.state::<AppState>();
            serve::kill(&state);
            rgit::kill_all(&state);
        }
    });
}
