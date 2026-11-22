//! Global shortcuts that open the quick-entry bar under the menu bar item:
//! one to type, one to talk. The combos are adjustable from Index's settings
//! and kept in the app config directory.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const FILE: &str = "shortcuts.json";

/// The combos actually registered, which may be the defaults if the saved ones failed.
static ACTIVE: Mutex<Option<Shortcuts>> = Mutex::new(None);
/// The main window's page has loaded and can take text from the bar.
static MAIN_READY: AtomicBool = AtomicBool::new(false);

/// Main window navigation started: text sent now would be lost.
pub fn main_loading() {
    MAIN_READY.store(false, Ordering::SeqCst);
}
/// The main page has mounted and reported in.
pub fn main_ready() {
    MAIN_READY.store(true, Ordering::SeqCst);
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Shortcuts {
    pub text: String,
    pub voice: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self { text: "Control+Alt+KeyN".into(), voice: "Control+Alt+KeyV".into() }
    }
}

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(FILE))
}

fn saved(app: &AppHandle) -> Shortcuts {
    path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn parse(s: &Shortcuts) -> Result<(Shortcut, Shortcut), String> {
    let bad = |c: &str| format!("\"{c}\" isn't a shortcut Pebble can use.");
    let text: Shortcut = s.text.parse().map_err(|_| bad(&s.text))?;
    let voice: Shortcut = s.voice.parse().map_err(|_| bad(&s.voice))?;
    if text == voice {
        return Err("Type and talk need different shortcuts.".into());
    }
    Ok((text, voice))
}

fn register(app: &AppHandle, s: &Shortcuts) -> Result<(), String> {
    let (text, voice) = parse(s)?;
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    for (shortcut, mode) in [(text, "text"), (voice, "voice")] {
        shortcuts
            .on_shortcut(shortcut, move |app, _, event| {
                if event.state == ShortcutState::Pressed {
                    open(app, mode);
                }
            })
            .map_err(|e| {
                eprintln!("shortcuts: register {shortcut:?} failed: {e}");
                "That shortcut is already used by another app.".to_string()
            })?;
    }
    *ACTIVE.lock().unwrap() = Some(s.clone());
    Ok(())
}

/// At launch: the saved shortcuts, or the defaults if those can't be registered.
pub fn install(app: &AppHandle) {
    if let Err(error) = register(app, &saved(app)) {
        eprintln!("shortcuts: {error}; falling back to defaults");
        if let Err(error) = register(app, &Shortcuts::default()) {
            eprintln!("shortcuts: defaults failed too: {error}");
        }
    }
}

fn open(app: &AppHandle, mode: &'static str) {
    let Some(window) = app.get_webview_window("quick") else {
        return;
    };
    // Already open (e.g. the talk shortcut again, to stop): keep the app to
    // return to and the window where it is.
    if !window.is_visible().unwrap_or(false) {
        crate::tray::mark_opening(app, &window);
        crate::tray::show_under_bar(app, &window);
    }
    let _ = window.eval(format!("window.__pebbleQuick && window.__pebbleQuick({mode:?})"));
    crate::tray::focus_menu(window);
}

/// The menu bar popover's buttons for the same bar.
#[tauri::command]
pub fn open_quick(app: AppHandle, mode: String) {
    // Open first so focus moves straight to the bar, not to the main window.
    // Pebble is in front now; keep the app the popover was opened over.
    let tray = app.state::<crate::tray::Tray>();
    let front = *tray.front_app.lock().unwrap();
    open(&app, if mode == "voice" { "voice" } else { "text" });
    *tray.front_app.lock().unwrap() = front;
    if let Some(popover) = app.get_webview_window("tray") {
        let _ = popover.hide();
    }
}

#[tauri::command]
pub fn get_shortcuts(app: AppHandle) -> Shortcuts {
    ACTIVE.lock().unwrap().clone().unwrap_or_else(|| saved(&app))
}

#[tauri::command]
pub fn set_shortcuts(app: AppHandle, shortcuts: Shortcuts) -> Result<(), String> {
    let shortcuts = Shortcuts { text: shortcuts.text.chars().take(64).collect(), voice: shortcuts.voice.chars().take(64).collect() };
    let previous = ACTIVE.lock().unwrap().clone();
    let restore = || match &previous {
        Some(p) => { let _ = register(&app, p); }
        None => install(&app),
    };
    if let Err(error) = register(&app, &shortcuts) {
        restore();
        return Err(error);
    }
    let written = path(&app).and_then(|file| {
        std::fs::create_dir_all(file.parent()?).ok()?;
        std::fs::write(file, serde_json::to_vec(&shortcuts).ok()?).ok()
    });
    if written.is_none() {
        restore();
        return Err("Couldn't save the shortcuts.".into());
    }
    Ok(())
}

/// Text from the quick bar goes to the main window, which runs it like anything
/// typed or said there. The bar shows its own confirmation, then hides itself.
/// Errors (main page not up) let the bar keep the text instead of losing it.
#[tauri::command]
pub fn quick_submit(app: AppHandle, text: String, voice: bool) -> Result<(), String> {
    let text: String = text.chars().take(10_000).collect();
    if text.trim().is_empty() {
        return Ok(());
    }
    // ponytail: "ready" means the page mounted, not an ack per message; add a
    // round-trip ack if text still goes missing.
    let main = app.get_webview_window("main").filter(|_| MAIN_READY.load(Ordering::SeqCst)).ok_or("Pebble is still loading.")?;
    let payload = serde_json::json!({ "text": text, "voice": voice });
    main.eval(format!("window.__pebbleCompose({payload})")).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_quick(app: AppHandle) {
    if let Some(window) = app.get_webview_window("quick") {
        crate::tray::put_away(&app, &window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcuts_parse() {
        assert!(parse(&Shortcuts::default()).is_ok());
        assert!(parse(&Shortcuts { text: "Control+Alt+Space".into(), voice: "Control+Alt+Space".into() }).is_err());
        assert!(parse(&Shortcuts { text: "Nonsense+Q".into(), voice: "Alt+KeyV".into() }).is_err());
    }
}
