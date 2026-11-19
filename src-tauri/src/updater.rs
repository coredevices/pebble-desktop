//! Signed self-updates from eng-dash (`/api/desktop/update/...`, see README).
//! The updater plugin downloads, checks the minisign signature against the
//! pubkey in tauri.conf.json, and swaps the .app bundle in place; this module
//! only decides when to check and asks before restarting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

const PERIOD: Duration = Duration::from_secs(6 * 60 * 60);

static CHECKING: Mutex<()> = Mutex::const_new(());
/// A newer bundle is already installed on disk; only a restart is missing.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Anonymous check (production track) at launch and every few hours. Runs from
/// Rust so a broken web page can still be fixed by an update.
pub fn spawn(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check(&app, None).await;
            tokio::time::sleep(PERIOD).await;
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

async fn check(app: &AppHandle, id_token: Option<String>) {
    // Wait rather than skip: a signed-in check from Index must not be lost to
    // an anonymous one that happens to be running.
    let _guard = CHECKING.lock().await;
    if INSTALLED.load(Ordering::SeqCst) {
        // Re-ask only on the slow periodic tick, not on every Index page load.
        if id_token.is_none() {
            prompt_restart(app);
        }
        return;
    }
    if let Err(error) = install(app, id_token).await {
        eprintln!("update check failed: {error}");
    }
}

async fn install(
    app: &AppHandle,
    id_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = app.updater_builder();
    if let Some(token) = id_token {
        builder = builder.header("Authorization", format!("Bearer {token}"))?;
    }
    // Default comparator: only strictly newer versions, so eng-dash can't roll users back.
    let Some(mut update) = builder.build()?.check().await? else {
        return Ok(());
    };
    // The bearer is for eng-dash only; the download is a signed R2 URL.
    update.headers.remove("Authorization");
    update.download_and_install(|_, _| {}, || {}).await?;
    INSTALLED.store(true, Ordering::SeqCst);
    prompt_restart(app);
    Ok(())
}

fn prompt_restart(app: &AppHandle) {
    let handle = app.clone();
    app.dialog()
        .message("A new version of Pebble is installed. Restart now to use it, or it will start next time you open Pebble.")
        .title("Pebble updated")
        .buttons(MessageDialogButtons::OkCancelCustom("Restart".into(), "Later".into()))
        .show(move |restart| {
            if restart {
                handle.restart();
            }
        });
}
