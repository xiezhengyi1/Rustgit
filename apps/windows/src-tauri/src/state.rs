use std::collections::HashMap;
use std::process::{Child, ChildStdin};
use std::sync::Mutex;

use crate::serve::ServeInner;
use crate::settings::Settings;
use crate::tray::TrayUpdater;

/// A running `rgit --json` child. `stdin` stays open for the child's lifetime: the engine
/// treats a closed stdin as a request to stop, so it is dropped (closed) with the entry.
pub struct Run {
    pub child: Child,
    pub _stdin: Option<ChildStdin>,
}

#[derive(Default)]
pub struct AppState {
    pub runs: Mutex<HashMap<String, Run>>,
    pub serve: Mutex<ServeInner>,
    pub settings: Mutex<Settings>,
    /// set once the tray exists; applies new labels to its menu items
    pub tray: Mutex<Option<TrayUpdater>>,
    /// "en" or "zh", reported by the UI for the tray menu labels
    pub ui_language: Mutex<String>,
}
