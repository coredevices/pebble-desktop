//! Signed self-updates from eng-dash (`/api/desktop/update/...`, see README),
//! the only update endpoint (GitHub releases host first-time downloads). The updater
//! plugin downloads and checks the minisign signature against the pubkey in
//! tauri.conf.json; this module decides when to check and when to apply.
//!
//! On by default: a new version downloads in the background, then installs and
//! restarts by itself once Pebble is out of the way (no window showing, or
//! you've stepped away from the computer). Settings can turn it off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const PERIOD: Duration = Duration::from_secs(6 * 60 * 60);
/// Per request: a stalled download gives up instead of blocking later checks.
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const FILE: &str = "updates.json";

/// One check (network + download) at a time.
static CHECKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// A downloaded update, held until it's a good moment to apply it. Locked only briefly.
static PENDING: std::sync::Mutex<Option<(Update, Vec<u8>)>> = std::sync::Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(true);

#[derive(serde::Serialize, serde::Deserialize)]
struct Prefs {
    auto: bool,
}

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(FILE))
}

/// Checks at launch and every few hours, and applies a downloaded update when
/// idle. Runs from Rust so a broken web page can still be fixed by an update.
pub fn spawn(app: &AppHandle) {
    let saved = path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Prefs>(&b).ok());
    ENABLED.store(saved.is_none_or(|p| p.auto), Ordering::SeqCst);
    if cfg!(debug_assertions) {
        return;
    }
    let checker = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check(&checker, None).await;
            tokio::time::sleep(PERIOD).await;
        }
    });
    let applier = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            apply_when_idle(&applier).await;
        }
    });
}

/// Called by Index with the signed-in user's Firebase ID token, so eng-dash
/// can serve that user's track (alpha/beta/internal), like firmware.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle, id_token: Option<String>) {
    if !cfg!(debug_assertions) {
        check(&app, id_token.filter(|t| !t.is_empty() && t.len() <= 16384)).await;
    }
}

pub fn pending_version() -> Option<String> {
    PENDING.lock().unwrap().as_ref().map(|(update, _)| update.version.clone())
}

#[tauri::command]
pub fn get_auto_update() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub async fn set_auto_update(app: AppHandle, enabled: bool) -> Result<(), String> {
    let file = path(&app).ok_or("No settings folder")?;
    std::fs::create_dir_all(file.parent().ok_or("No settings folder")?).map_err(|e| e.to_string())?;
    std::fs::write(file, serde_json::to_vec(&Prefs { auto: enabled }).unwrap()).map_err(|e| e.to_string())?;
    ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        PENDING.lock().unwrap().take();
    } else if !cfg!(debug_assertions) {
        check(&app, None).await;
    }
    Ok(())
}

async fn check(app: &AppHandle, id_token: Option<String>) {
    // Wait rather than skip: a signed-in check from Index must not be lost to
    // an anonymous one that happens to be running.
    let _checking = CHECKING.lock().await;
    let signed_in = id_token.is_some();
    // An anonymous check has nothing to add once an update is waiting; a signed-in
    // one may find a newer tester build, which then replaces it.
    if !ENABLED.load(Ordering::SeqCst) || (!signed_in && PENDING.lock().unwrap().is_some()) {
        return;
    }
    match download(app, id_token).await {
        Ok(Some(update)) if ENABLED.load(Ordering::SeqCst) => {
            crate::log(format!("update {} downloaded", update.0.version));
            *PENDING.lock().unwrap() = Some(update);
        }
        Ok(_) => {}
        Err(error) => crate::log(format!("update check failed: {error}")),
    }
}

async fn download(app: &AppHandle, id_token: Option<String>) -> Result<Option<(Update, Vec<u8>)>, Box<dyn std::error::Error>> {
    let mut builder = app.updater_builder().timeout(TIMEOUT);
    if let Some(token) = id_token {
        // eng-dash is the only endpoint (tauri.conf.json), so the token goes nowhere else.
        builder = builder.header("Authorization", format!("Bearer {token}"))?;
    }
    // Default comparator: only strictly newer versions, so eng-dash can't roll users back.
    let Some(mut update) = builder.build()?.check().await? else {
        return Ok(None);
    };
    // The bearer is for eng-dash only; the download is a signed R2 URL.
    update.headers.remove("Authorization");
    let bytes = update.download(|_, _| {}, || {}).await?;
    Ok(Some((update, bytes)))
}

/// Applies a downloaded update only when it's still wanted and nobody would notice the restart.
async fn apply_when_idle(app: &AppHandle) {
    if !ENABLED.load(Ordering::SeqCst) || !idle(app) {
        return;
    }
    let Some((update, bytes)) = PENDING.lock().unwrap().take() else {
        return;
    };
    crate::log(format!("installing update {}", update.version));
    // Windows runs the installer and exits here; macOS and Linux swap the files.
    match update.install(bytes) {
        Ok(()) => app.restart(),
        Err(error) => crate::log(format!("update install failed: {error}")),
    }
}

fn idle(app: &AppHandle) -> bool {
    let showing = app.webview_windows().values().any(|w| w.is_visible().unwrap_or(false));
    !showing || !crate::presence::present()
}
