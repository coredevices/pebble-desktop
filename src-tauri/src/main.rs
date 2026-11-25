#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod activation;
mod cloud;
mod presence;
mod quick;
mod sign_in;
mod tray;
mod updater;

use activation::{Activation, KeyStatus, Operation};
use cloud::{Cloud, Error};
use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Mutex;
use url::Url;

struct Desktop {
    cloud: Cloud,
    activation: Mutex<Result<Activation, Error>>,
    sign_in: sign_in::SignIn,
    computer_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopInfo {
    platform: &'static str,
    mac_key_available: bool,
    computer_name: String,
    app_version: String,
    os_version: String,
    arch: &'static str,
    auto_update: bool,
    /// Downloaded and waiting to install, if any.
    pending_update: Option<String>,
    present: bool,
    /// Recent app log lines, for bug reports.
    logs: Vec<String>,
}

/// Recent app log lines (also printed to stderr), sent with bug reports.
static LOG: std::sync::Mutex<std::collections::VecDeque<String>> = std::sync::Mutex::new(std::collections::VecDeque::new());

pub fn log(line: String) {
    eprintln!("{line}");
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut log = LOG.lock().unwrap();
    if log.len() >= 200 {
        log.pop_front();
    }
    log.push_back(format!("{secs} {line}"));
}

fn os_version() -> String {
    let run = |cmd: &str, args: &[&str]| {
        let mut command = std::process::Command::new(cmd);
        command.args(args);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
        command.output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    };
    match std::env::consts::OS {
        "macos" => run("/usr/bin/sw_vers", &["-productVersion"]),
        "windows" => run("cmd", &["/c", "ver"]),
        _ => std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_default(),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Presence {
    present: bool,
    /// Stable per install, so each Mac gets its own presence document.
    device: String,
    name: String,
}

#[tauri::command]
async fn presence(state: tauri::State<'_, Desktop>) -> Result<Presence, ()> {
    let device = state.activation.lock().await.as_ref()
        .map(|a| a.installation_id().to_string())
        // No saved install id: fall back to the computer name, still one doc per Mac.
        .unwrap_or_else(|_| format!("mac-{}", state.computer_name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(64).collect::<String>()));
    Ok(Presence { present: presence::present(), device, name: state.computer_name.clone() })
}

#[tauri::command]
fn desktop_info(app: tauri::AppHandle, state: tauri::State<'_, Desktop>) -> DesktopInfo {
    DesktopInfo {
        platform: std::env::consts::OS,
        mac_key_available: cfg!(target_os = "macos"),
        computer_name: state.computer_name.clone(),
        app_version: app.package_info().version.to_string(),
        os_version: os_version(),
        arch: std::env::consts::ARCH,
        auto_update: updater::get_auto_update(),
        pending_update: updater::pending_version(),
        present: presence::present(),
        logs: LOG.lock().unwrap().iter().cloned().collect(),
    }
}

#[tauri::command]
async fn mac_key_sync(
    state: tauri::State<'_, Desktop>,
    id_token: String,
    uid: String,
    operation: Operation,
) -> Result<KeyStatus, Error> {
    if !cfg!(target_os = "macos") {
        return Err(Error::new("unsupported", "iMessage setup requires a Mac."));
    }
    state
        .activation
        .lock()
        .await
        .as_mut()
        .map_err(|error| error.clone())?
        .sync(
            &state.cloud,
            &id_token,
            &uid,
            operation,
            state.computer_name.clone(),
            || {
                pebble_mac_key::extract_key().map_err(|_| {
                    Error::new(
                        "extraction",
                        "Couldn't prepare this Mac for iMessage. Please try again.",
                    )
                })
            },
        )
        .await
}

#[tauri::command]
async fn mac_key_reveal(
    state: tauri::State<'_, Desktop>,
    id_token: String,
    uid: String,
) -> Result<String, Error> {
    if !cfg!(target_os = "macos") {
        return Err(Error::new("unsupported", "iMessage setup requires a Mac."));
    }
    state.cloud.account(&id_token, Some(&uid)).await?;
    tauri::async_runtime::spawn_blocking(|| {
        pebble_mac_key::extract_key().map_err(|_| {
            Error::new(
                "extraction",
                "Couldn't prepare this Mac for iMessage. Please try again.",
            )
        })
    })
    .await
    .map_err(Error::local)?
}

#[tauri::command]
async fn desktop_sign_in(
    app: tauri::AppHandle,
    state: tauri::State<'_, Desktop>,
) -> Result<String, Error> {
    state.sign_in.start(&app, &state.cloud).await
}

#[tauri::command]
async fn cancel_desktop_sign_in(state: tauri::State<'_, Desktop>) -> Result<(), Error> {
    state.sign_in.cancel().await;
    Ok(())
}

#[tauri::command]
fn set_reminders(app: tauri::AppHandle, reminders: Vec<tray::Reminder>, sync_down: Option<bool>) {
    quick::main_ready();
    tray::set(&app, reminders, sync_down.unwrap_or(false));
}

#[tauri::command]
fn open_item(app: tauri::AppHandle, id: Option<String>, page: Option<String>) {
    // Pages the menu bar may open, by name; never a path from the page.
    let id = id.filter(|id| cloud::valid_id(id));
    let path = match (id, page.as_deref()) {
        (Some(id), Some("recording")) => Some(format!("/recording/{id}")),
        (Some(id), _) => {
            tray::act(&app, id.clone(), tray::Op::Dismiss, None);
            Some(format!("/item/{id}"))
        }
        (None, Some("bug-report")) => Some("/bug-report".to_string()),
        _ => None,
    };
    tray::show_main(&app, path);
}

#[tauri::command]
fn get_reminders(app: tauri::AppHandle) -> tray::Snapshot {
    tray::snapshot(&app)
}

#[tauri::command]
fn reminder_action(app: tauri::AppHandle, id: String, op: tray::Op, due_at: Option<i64>) {
    tray::act(&app, id, op, due_at);
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

const TITLE_BAR_SCRIPT: &str = r#"
// Native feel (see html.desktop in the web app's globals.css): no page-style
// context menu (Reload, Back) except where text can be edited or copied.
document.documentElement.classList.add("desktop");
document.addEventListener("contextmenu", (e) => {
  const editable = e.target.closest?.("input, textarea, [contenteditable='true'], .select-text");
  if (!editable && String(getSelection()) === "") e.preventDefault();
});
// Only the Mac window has its title bar merged into the page.
if (navigator.userAgent.includes("Mac")) document.addEventListener("DOMContentLoaded", () => {
  document.documentElement.style.setProperty("--titlebar", "28px");
  const strip = document.createElement("div");
  strip.setAttribute("data-tauri-drag-region", "");
  strip.style.cssText = "position:fixed;top:0;left:0;right:0;height:28px;z-index:2147483647;background:var(--bg,#fbf4ef)";
  document.body.append(strip);
});
"#;

pub fn base_url() -> Url {
    let url = if cfg!(debug_assertions) { "http://localhost:3017" } else { "https://index.repebble.com" };
    Url::parse(url).expect("static URL")
}

/// Only the app's own origin: a debug build stays on the local server, so it never
/// drifts onto the public site (and its separate sign-in).
fn allowed(url: &Url) -> bool {
    url.origin() == base_url().origin()
}

fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        if output.status.success() {
            let name: String = String::from_utf8_lossy(&output.stdout)
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect();
            if !name.is_empty() {
                return name;
            }
        }
    }
    // Windows sets COMPUTERNAME; Linux has /etc/hostname.
    let other = std::env::var("COMPUTERNAME").ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|n| n.trim().chars().filter(|c| !c.is_control()).take(128).collect::<String>())
        .filter(|n| !n.is_empty());
    other.unwrap_or_else(|| if cfg!(target_os = "macos") { "My Mac".into() } else { "My computer".into() })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // An unreadable iMessage preference file must not prevent Index from opening.
            let activation = app.path().app_config_dir().map_err(Error::local)
                .and_then(|directory| Activation::load(activation::preferences_path(&directory)));
            app.manage(Desktop { cloud: Cloud::new(), activation: Mutex::new(activation), sign_in: sign_in::SignIn::default(), computer_name: computer_name() });
            // Localhost IPC exists only in a debug binary. It is never in release capabilities.
            #[cfg(debug_assertions)]
            app.add_capability(serde_json::json!({
                "identifier":"index-development", "windows":["main","tray","alert","quick"], "local":false,
                "remote":{"urls":["http://localhost:3017"]},
                "permissions":["allow-desktop-info","allow-mac-key-sync","allow-mac-key-reveal","allow-desktop-sign-in","allow-cancel-desktop-sign-in","allow-set-reminders","allow-open-item","allow-quit-app","allow-get-reminders","allow-reminder-action","allow-check-for-updates","allow-get-shortcuts","allow-set-shortcuts","allow-quick-submit","allow-hide-quick","allow-open-quick","allow-presence","allow-get-auto-update","allow-set-auto-update","core:window:allow-set-size","core:window:allow-start-dragging","core:window:allow-internal-toggle-maximize","clipboard-manager:allow-write-text"]
            }).to_string())?;
            let url = base_url().join("/signin")?.to_string();
            let navigation_app = app.handle().clone();
            let popup_app = app.handle().clone();
            let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title(if cfg!(debug_assertions) { "Pebble Dev" } else { "Pebble" }).inner_size(420.0, 1000.0).min_inner_size(420.0, 560.0);
            // Title bar merged into the page: traffic lights sit over the app's own
            // background, with a draggable strip above the content.
            #[cfg(target_os = "macos")]
            let main = main.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
            main
                .initialization_script(TITLE_BAR_SCRIPT)
                // Mostly hidden, yet it is what keeps reminders live for the menu bar:
                // don't let WebKit throttle or suspend it in the background.
                .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
                .on_page_load(|_, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) { quick::main_loading(); }
                })
                .on_navigation(move |url| {
                    if allowed(url) { return true; }
                    if matches!(url.scheme(), "https" | "http") { let _ = navigation_app.opener().open_url(url.as_str(), None::<&str>); }
                    false
                })
                .on_new_window(move |url, _| {
                    if matches!(url.scheme(), "https" | "http") { let _ = popup_app.opener().open_url(url.as_str(), None::<&str>); }
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            // Menu bar popover and due-reminder alert; preloaded so they open instantly.
            // Each page sizes its window to fit its content.
            for (label, path) in [("tray", "/tray"), ("alert", "/tray/alert"), ("quick", "/tray/quick")] {
                let nav_app = app.handle().clone();
                WebviewWindowBuilder::new(app, label, WebviewUrl::External(base_url().join(path)?))
                    .title("Pebble Reminders").inner_size(340.0, 120.0).resizable(false)
                    .decorations(false).transparent(true).shadow(true).always_on_top(true)
                    .skip_taskbar(true).visible(false).focused(false)
                    .visible_on_all_workspaces(true)
                    .on_navigation(move |url| {
                        if allowed(url) && url.path() == path { return true; }
                        if allowed(url) { tray::show_main(&nav_app, Some(url.path().to_string())); }
                        false
                    })
                    .build()?;
            }
            tray::install(app.handle())?;
            presence::install(app.handle());
            quick::install(app.handle());
            // Cmd+Q closes Pebble to the menu bar (macOS gives no hook to cancel a
            // real quit); "Quit Pebble" in the menu bar item exits for real.
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{Menu, MenuItem, MenuItemKind};
                let menu = Menu::default(app.handle())?;
                if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.first() {
                    app_menu.remove_at(app_menu.items()?.len() - 1)?; // the default Quit
                    app_menu.append(&MenuItem::with_id(app, "close-to-menu-bar", "Quit Pebble", true, Some("CmdOrCtrl+Q"))?)?;
                }
                app.set_menu(menu)?;
                app.on_menu_event(|app, event| {
                    if event.id() == "close-to-menu-bar" { tray::hide_app(app); }
                });
            }
            updater::spawn(app.handle());
            Ok(())
        })
        // Closing the window hides it so the menu bar item keeps updating.
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                tauri::WindowEvent::Focused(false) if matches!(window.label(), "tray" | "quick") => {
                    if let Some(window) = window.app_handle().get_webview_window(window.label()) {
                        tray::menu_blurred(&window.app_handle().clone(), window);
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![desktop_info, mac_key_sync, mac_key_reveal, desktop_sign_in, cancel_desktop_sign_in, set_reminders, open_item, quit_app, get_reminders, reminder_action, updater::check_for_updates, updater::get_auto_update, updater::set_auto_update, quick::get_shortcuts, quick::set_shortcuts, quick::quick_submit, quick::hide_quick, quick::open_quick, presence])
        .build(tauri::generate_context!())
        .expect("Pebble desktop failed to start")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                tray::show_main(_app, None);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pins_remote_origin() {
        assert!(allowed(&base_url().join("/settings").unwrap()));
        // Debug (tests) runs against the local server only.
        assert!(!allowed(&Url::parse("https://index.repebble.com/settings").unwrap()));
        for url in [
            "http://index.repebble.com",
            "https://index.repebble.com.evil.test",
            "https://index.repebble.com:444",
            "https://evil.test",
        ] {
            assert!(!allowed(&Url::parse(url).unwrap()));
        }
    }
}
