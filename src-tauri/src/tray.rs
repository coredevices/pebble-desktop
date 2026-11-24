//! Menu bar item. The main Index window is the only data source: it pushes its
//! Reminders list here, the popover (/tray) and alert (/tray/alert) pages read it
//! back, and their actions (done, snooze, open) are relayed to the main window.
//! Rust owns the clock, so the bar and alerts stay correct while windows are hidden.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tauri::tray::{MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow};

const MAX_REMINDERS: usize = 300;
const BAR_TITLE_CHARS: usize = 24;
/// Reminders that came due while the Mac slept longer than this don't pop up.
const ALERT_WINDOW_MS: i64 = 10 * 60_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: String,
    pub title: String,
    pub due_at: Option<i64>, // epoch milliseconds
    pub kind: String,        // item metadata type
}

impl Reminder {
    /// Mirrors the app's todoSubline: only reminder/scheduled items are timed.
    fn timed(&self) -> Option<i64> {
        matches!(self.kind.as_str(), "reminder" | "scheduled")
            .then_some(self.due_at)
            .flatten()
    }
}

#[derive(Default)]
pub struct Tray {
    reminders: Mutex<Vec<Reminder>>,
    firing: Mutex<Vec<String>>,
    alerted: Mutex<HashSet<(String, i64)>>,
    icon: Mutex<Option<TrayIcon>>,
    /// When each menu-like window (popover, quick bar) was last opened.
    shown_at: Mutex<HashMap<String, std::time::Instant>>,
    /// The main window can't reach Index; the list may be stale.
    sync_down: Mutex<bool>,
    /// When the last tick ran; a big gap means the Mac was asleep.
    last_check: Mutex<Option<i64>>,
    /// The app in front when a menu-like window opened, to hand focus back to.
    pub front_app: Mutex<Option<i32>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    reminders: Vec<Reminder>,
    firing: Vec<String>,
    sync_down: bool,
}

fn clean(text: &str, max: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!(
        "{}…",
        text.chars().take(max - 1).collect::<String>().trim_end()
    )
}

/// Untrusted page input: bound the size, drop bad ids. Order is the app's order.
pub fn sanitize(input: Vec<Reminder>) -> Vec<Reminder> {
    input
        .into_iter()
        .take(MAX_REMINDERS)
        .filter(|r| crate::cloud::valid_id(&r.id))
        .map(|r| Reminder {
            title: clean(&r.title, 200),
            kind: clean(&r.kind, 32),
            ..r
        })
        .collect()
}

fn relative(due_ms: i64, now_ms: i64) -> String {
    let m = (due_ms - now_ms) / 60_000;
    match m {
        // Same thresholds as the app's formatDue.
        ..=59 => format!("in {}m", m.max(0)),
        60..=1439 => format!("in {}h", m / 60),
        _ => format!("in {}d", m / 1440),
    }
}

/// Bar text for the soonest not-yet-due reminder; with nothing upcoming, the
/// most recently overdue one. None when there are no timed reminders.
pub fn bar_title(reminders: &[Reminder], now_ms: i64) -> Option<String> {
    let timed = || reminders.iter().filter_map(|r| r.timed().map(|due| (r, due)));
    let (next, due) = timed()
        .filter(|(_, due)| *due > now_ms)
        .min_by_key(|(_, due)| *due)
        .or_else(|| timed().max_by_key(|(_, due)| *due))?;
    let title = if next.title.is_empty() {
        "Reminder".into()
    } else {
        clean(&next.title, BAR_TITLE_CHARS)
    };
    let when = if due > now_ms { relative(due, now_ms) } else { "overdue".into() };
    Some(format!("{title} {when}"))
}

/// Reminders that just came due and haven't popped up for this due time yet.
/// `last_check` is the previous tick: after the Mac slept, everything that came
/// due while it was asleep pops up on wake, as if it had been awake.
pub fn newly_due(
    reminders: &[Reminder],
    alerted: &HashSet<(String, i64)>,
    now_ms: i64,
    last_check: Option<i64>,
) -> Vec<(String, i64)> {
    let since = last_check.map_or(now_ms - ALERT_WINDOW_MS, |t| t.min(now_ms - ALERT_WINDOW_MS));
    reminders
        .iter()
        .filter_map(|r| r.timed().map(|due| (r.id.clone(), due)))
        .filter(|(_, due)| *due <= now_ms && *due > since)
        .filter(|key| !alerted.contains(key))
        .collect()
}

/// Puts a window centered just below the bar item, or just above it when the
/// tray is at the bottom of the screen (Windows taskbar).
fn place_below(window: &WebviewWindow, rect: tauri::Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let at = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    let outer = window.outer_size().map(|s| (s.width as f64, s.height as f64)).unwrap_or_default();
    let x = at.x + size.width / 2.0 - outer.0 / 2.0;
    let mut y = at.y + size.height + 4.0 * scale;
    if let Ok(Some(screen)) = window.monitor_from_point(at.x, at.y) {
        if y + outer.1 > (screen.position().y + screen.size().height as i32) as f64 {
            y = at.y - outer.1 - 4.0 * scale;
        }
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn toggle_popover(app: &AppHandle, rect: tauri::Rect) {
    let Some(window) = app.get_webview_window("tray") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    place_below(&window, rect);
    mark_opening(app, &window);
    let _ = window.show();
    focus_menu(window);
}

/// Call before showing a menu-like window: showing it can blur it at once.
pub fn mark_opening(app: &AppHandle, window: &WebviewWindow) {
    app.state::<Tray>().shown_at.lock().unwrap().insert(window.label().to_string(), std::time::Instant::now());
    remember_front_app(app);
}

#[cfg(target_os = "macos")]
fn remember_front_app(app: &AppHandle) {
    use objc2_app_kit::NSWorkspace;
    let pid = NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier());
    *app.state::<Tray>().front_app.lock().unwrap() = pid.filter(|&p| p != std::process::id() as i32);
}
#[cfg(not(target_os = "macos"))]
fn remember_front_app(_: &AppHandle) {}

/// Hides a menu-like window after an action in it. If it had focus, macOS would
/// pass focus (and bring forward) the main window; give it back to the app the
/// user was in instead.
pub fn put_away(app: &AppHandle, window: &WebviewWindow) {
    let focused = window.is_focused().unwrap_or(false);
    let _ = window.hide();
    #[cfg(target_os = "macos")]
    if focused {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        if let Some(previous) = app.state::<Tray>().front_app.lock().unwrap().take()
            .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
        {
            previous.activateWithOptions(NSApplicationActivationOptions::empty());
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, focused);
}

/// Focuses a menu-like window. Activating the app hands focus to the main
/// window; take it back once that settles.
// ponytail: focus dance; an NSPanel (tauri-nspanel) avoids activation entirely.
pub fn focus_menu(window: WebviewWindow) {
    let _ = window.set_focus();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let _ = window.set_focus();
    });
}

/// Menu-like windows close when they lose focus, except for the focus
/// shuffle right after they open.
pub fn menu_blurred(app: &AppHandle, window: WebviewWindow) {
    const GRACE: std::time::Duration = std::time::Duration::from_millis(600);
    let shown = app.state::<Tray>().shown_at.lock().unwrap().get(window.label()).copied();
    if let Some(left) = shown.and_then(|at| GRACE.checked_sub(at.elapsed())) {
        // Maybe the shuffle, maybe a real click elsewhere: look again once it
        // settles. is_focused() is unreliable here, so ask whether Pebble is
        // still the active app.
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(left).await;
            if !pebble_active() {
                let _ = window.hide();
            }
        });
        return;
    }
    let _ = window.hide();
}

#[cfg(target_os = "macos")]
fn pebble_active() -> bool {
    objc2_app_kit::NSRunningApplication::currentApplication().isActive()
}
#[cfg(not(target_os = "macos"))]
fn pebble_active() -> bool {
    true
}

/// Shows a window just below the menu bar item.
pub fn show_under_bar(app: &AppHandle, window: &WebviewWindow) {
    let rect = app
        .state::<Tray>()
        .icon
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|i| i.rect().ok().flatten());
    if let Some(rect) = rect {
        place_below(window, rect);
    }
    let _ = window.show();
}

/// Shows the alert under the bar item while something is firing, hides it otherwise.
fn sync_alert(app: &AppHandle) {
    let firing = !app.state::<Tray>().firing.lock().unwrap().is_empty();
    let Some(window) = app.get_webview_window("alert") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    if firing && !visible {
        remember_front_app(app);
        show_under_bar(app, &window);
    } else if !firing && visible {
        put_away(app, &window);
    }
}

fn tick(app: &AppHandle) {
    let tray = app.state::<Tray>();
    let reminders = tray.reminders.lock().unwrap().clone();
    let now = chrono::Utc::now().timestamp_millis();
    {
        let mut alerted = tray.alerted.lock().unwrap();
        let mut firing = tray.firing.lock().unwrap();
        let last_check = tray.last_check.lock().unwrap().replace(now);
        for key in newly_due(&reminders, &alerted, now, last_check) {
            if !firing.contains(&key.0) {
                firing.push(key.0.clone());
            }
            alerted.insert(key);
        }
        // Completed, deleted or rescheduled elsewhere: stop alerting.
        firing.retain(|id| reminders.iter().any(|r| &r.id == id && r.timed().is_some_and(|due| due <= now)));
    }
    sync_alert(app);
    let Some(icon) = tray.icon.lock().unwrap().clone() else {
        return;
    };
    // Next reminder as text; with nothing upcoming, the Pebble favicon instead.
    // Sync down: flag it in the bar so a stale list isn't trusted.
    let warn = if *tray.sync_down.lock().unwrap() { "⚠︎ " } else { "" };
    match bar_title(&reminders, now).map(|t| format!("{warn}{t}")).or((!warn.is_empty()).then(|| warn.trim().to_string())) {
        // Only the macOS menu bar shows text; elsewhere keep the icon, text as its tooltip.
        Some(title) if !cfg!(target_os = "macos") => {
            let _ = icon.set_tooltip(Some(title));
        }
        None if !cfg!(target_os = "macos") => {
            let _ = icon.set_tooltip(Some("Pebble"));
        }
        Some(title) => {
            let _ = icon.set_icon(None);
            let _ = icon.set_title(Some(title));
        }
        None => {
            // set_title(None) leaves the old text on macOS; clear it explicitly.
            let _ = icon.set_title(Some(""));
            let _ = icon.set_icon(Some(tauri::include_image!("icons/tray.png")));
            let _ = icon.set_icon_as_template(true);
        }
    }
}

pub fn set(app: &AppHandle, reminders: Vec<Reminder>, sync_down: bool) {
    *app.state::<Tray>().sync_down.lock().unwrap() = sync_down;
    *app.state::<Tray>().reminders.lock().unwrap() = sanitize(reminders);
    tick(app);
}

pub fn snapshot(app: &AppHandle) -> Snapshot {
    let tray = app.state::<Tray>();
    let reminders = tray.reminders.lock().unwrap().clone();
    let firing = tray.firing.lock().unwrap().clone();
    let sync_down = *tray.sync_down.lock().unwrap();
    Snapshot { reminders, firing, sync_down }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Done,
    Snooze,
    Dismiss,
}

/// Popover/alert action. Applied locally at once, then written by the main window.
pub fn act(app: &AppHandle, id: String, op: Op, due_at: Option<i64>) {
    if !crate::cloud::valid_id(&id) {
        return;
    }
    let tray = app.state::<Tray>();
    tray.firing.lock().unwrap().retain(|f| f != &id);
    let write = match op {
        Op::Done => {
            tray.reminders.lock().unwrap().retain(|r| r.id != id);
            serde_json::json!({ "id": id, "op": "done" })
        }
        Op::Snooze => {
            let Some(due_at) = due_at else { return };
            for r in tray
                .reminders
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|r| r.id == id)
            {
                r.due_at = Some(due_at);
            }
            serde_json::json!({ "id": id, "op": "snooze", "dueAt": due_at })
        }
        Op::Dismiss => serde_json::Value::Null,
    };
    if !write.is_null() {
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.eval(format!(
                "window.__pebbleAction && window.__pebbleAction({write})"
            ));
        }
    }
    tick(app);
}

/// Shows the main window, routing in-app (no reload) to `path` when given.
/// Hide every window and the Dock icon; the menu bar item keeps running.
pub fn hide_app(app: &AppHandle) {
    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

pub fn show_main(app: &AppHandle, path: Option<String>) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(popover) = app.get_webview_window("tray") {
        let _ = popover.hide();
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let Some(path) = path {
        let path = serde_json::Value::String(path);
        let _ = window.eval(format!(
            "window.__pebbleOpen ? window.__pebbleOpen({path}) : location.assign({path})"
        ));
    }
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    app.manage(Tray::default());
    let icon = TrayIconBuilder::with_id("reminders")
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        // No native menu: any click opens the popover (it has Open and Quit).
        .on_tray_icon_event(|icon, event| {
            if let TrayIconEvent::Click {
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(icon.app_handle(), rect);
            }
        })
        .build(app)?;
    // Linux tray icons report no clicks, so the popover can't open: a menu instead.
    #[cfg(target_os = "linux")]
    {
        use tauri::menu::{MenuBuilder, MenuItem};
        let open = MenuItem::with_id(app, "open", "Open Pebble", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit Pebble", true, None::<&str>)?;
        icon.set_menu(Some(MenuBuilder::new(app).item(&open).item(&quit).build()?))?;
        icon.on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app, None),
            "quit" => app.exit(0),
            _ => {}
        });
    }
    *app.state::<Tray>().icon.lock().unwrap() = Some(icon);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            tick(&handle);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn r(id: &str, title: &str, due: Option<i64>, kind: &str) -> Reminder {
        Reminder {
            id: id.into(),
            title: title.into(),
            due_at: due,
            kind: kind.into(),
        }
    }

    #[test]
    fn bar_title_and_alerts() {
        let now = 1_000_000 * MIN;
        assert_eq!(bar_title(&[], now), None);
        let list = sanitize(vec![
            r("a", "Passed", Some(now - MIN), "reminder"),
            r("n", "No due", None, "reminder"),
            r("c", "Later", Some(now + 90 * MIN), "reminder"),
            r(
                "b",
                "Cosonic//Core Devices call",
                Some(now + 21 * MIN),
                "scheduled",
            ),
            r("x", "Untimed note", Some(now + 5 * MIN), "note"),
            r("old", "Long overdue", Some(now - 60 * MIN), "reminder"),
            r("bad/id", "Dropped", Some(now + MIN), "reminder"),
        ]);
        // App order is kept; the soonest timed future reminder wins the bar.
        assert_eq!(list.len(), 6);
        assert_eq!(list[0].id, "a");
        assert_eq!(
            bar_title(&list, now).unwrap(),
            "Cosonic//Core Devices c… in 21m"
        );
        // Nothing upcoming: the most recently overdue reminder shows instead.
        let overdue: Vec<_> = list.iter().filter(|r| r.due_at.is_some_and(|d| d < now)).cloned().collect();
        assert_eq!(bar_title(&overdue, now).unwrap(), "Passed overdue");
        // Only the just-due reminder fires, once per due time.
        let mut alerted = HashSet::new();
        let due = newly_due(&list, &alerted, now, None);
        assert_eq!(due, vec![("a".to_string(), now - MIN)]);
        alerted.extend(due);
        assert!(newly_due(&list, &alerted, now, Some(now - 5_000)).is_empty());
        // Snoozing changes the due time, so it fires again later.
        let snoozed = vec![r("a", "Passed", Some(now + 10 * MIN), "reminder")];
        assert_eq!(newly_due(&snoozed, &alerted, now + 10 * MIN, None).len(), 1);
        // Woke after two hours asleep: the reminder due an hour ago pops up too.
        let woke = newly_due(&list, &alerted, now, Some(now - 120 * MIN));
        assert_eq!(woke, vec![("old".to_string(), now - 60 * MIN)]);
    }
}
