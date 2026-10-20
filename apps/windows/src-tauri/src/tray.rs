//! System tray item: opens the window, shows whether this computer is hosting, stops hosting,
//! and quits (which also stops the host process).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::serve;
use crate::state::AppState;

/// What the tray should currently say; applied by the closure `create` installs.
pub struct TrayView {
    pub status: String,
    pub open: &'static str,
    pub stop: &'static str,
    pub quit: &'static str,
    pub stop_enabled: bool,
    pub tooltip: String,
}

pub type TrayUpdater = Box<dyn Fn(&TrayView) + Send + Sync>;

struct Labels {
    open: &'static str,
    stop: &'static str,
    quit: &'static str,
    idle: &'static str,
    starting: &'static str,
    running: &'static str, // contains {addr}
    tooltip: &'static str,
}

fn labels(lang: &str) -> Labels {
    if lang.starts_with("zh") {
        Labels {
            open: "打开 RustGit",
            stop: "停止托管",
            quit: "退出 RustGit",
            idle: "未托管",
            starting: "托管正在启动…",
            running: "正在 {addr} 上托管",
            tooltip: "RustGit",
        }
    } else {
        Labels {
            open: "Open RustGit",
            stop: "Stop hosting",
            quit: "Quit RustGit",
            idle: "Not hosting",
            starting: "Hosting is starting…",
            running: "Hosting on {addr}",
            tooltip: "RustGit",
        }
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let l = labels("en");
    let status = MenuItem::with_id(app, "status", l.idle, false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", l.open, true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", l.stop, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", l.quit, true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &open, &stop, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(l.tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => crate::show_window(app),
            "stop" => {
                let app = app.clone();
                std::thread::spawn(move || {
                    serve::stop(&app);
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                crate::show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    let updater: TrayUpdater = Box::new(move |v: &TrayView| {
        let _ = status.set_text(&v.status);
        let _ = open.set_text(v.open);
        let _ = stop.set_text(v.stop);
        let _ = stop.set_enabled(v.stop_enabled);
        let _ = quit.set_text(v.quit);
        let _ = tray.set_tooltip(Some(v.tooltip.clone()));
    });
    *app.state::<AppState>().tray.lock().unwrap() = Some(updater);
    refresh(app);
    Ok(())
}

/// Re-apply labels and enabled state from the current language and host status.
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    let lang = state.ui_language.lock().unwrap().clone();
    let l = labels(&lang);
    let st = serve::status(&state);
    let text = if !st.running {
        l.idle.to_string()
    } else if let Some(addr) = &st.addr {
        l.running.replace("{addr}", addr)
    } else {
        l.starting.to_string()
    };
    let view = TrayView {
        tooltip: format!("{} – {}", l.tooltip, text),
        status: text,
        open: l.open,
        stop: l.stop,
        quit: l.quit,
        stop_enabled: st.running,
    };
    if let Some(update) = state.tray.lock().unwrap().as_ref() {
        update(&view);
    };
}
