//! Is someone using this Mac right now? The main window polls `presence` and
//! writes the answer to Firestore, so the phone can hold back watch
//! notifications while you're at the computer.

use std::sync::atomic::{AtomicBool, Ordering};

/// No keyboard or mouse input for this long counts as away. Long enough to read
/// a page or watch a video without the watch starting to buzz.
const IDLE_SECS: f64 = 120.0;

/// Set between the Mac saying it will sleep and it waking again.
static ASLEEP: AtomicBool = AtomicBool::new(false);

pub fn present() -> bool {
    !ASLEEP.load(Ordering::Relaxed) && !mac::locked() && mac::idle_secs() < IDLE_SECS
}

/// Tells the main window right away when the Mac sleeps or wakes, instead of at
/// its next poll (which a sleeping Mac would never reach).
pub fn install(app: &tauri::AppHandle) {
    mac::on_sleep(app.clone());
}

fn nudge(app: &tauri::AppHandle, asleep: bool) {
    use tauri::Manager;
    ASLEEP.store(asleep, Ordering::Relaxed);
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.eval("window.__pebblePresence && window.__pebblePresence()");
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void};
    use std::ptr::NonNull;

    type Ref = *const c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
        fn CGSessionCopyCurrentDictionary() -> Ref;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
        fn CFStringCreateWithCString(alloc: Ref, s: *const c_char, encoding: u32) -> Ref;
        fn CFBooleanGetValue(b: Ref) -> bool;
        fn CFRelease(r: Ref);
    }

    /// Seconds since the last key press, click or mouse move, in any app.
    /// Needs no permission (unlike watching the events themselves).
    pub fn idle_secs() -> f64 {
        // kCGEventSourceStateCombinedSessionState, kCGAnyInputEventType
        unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) }
    }

    /// The lock screen is up (locked by hand, or by the screen saver / display sleep).
    pub fn locked() -> bool {
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return true; // no console session: nobody is at this Mac
            }
            let key = CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), 0x0800_0100);
            let value = CFDictionaryGetValue(session, key);
            let locked = !value.is_null() && CFBooleanGetValue(value);
            CFRelease(key);
            CFRelease(session);
            locked
        }
    }

    pub fn on_sleep(app: tauri::AppHandle) {
        use block2::RcBlock;
        use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification};
        use objc2_foundation::NSNotification;
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        for (name, asleep) in unsafe { [(NSWorkspaceWillSleepNotification, true), (NSWorkspaceDidWakeNotification, false)] } {
            let app = app.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| super::nudge(&app, asleep));
            // Observers live as long as the app.
            std::mem::forget(unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) });
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    pub fn idle_secs() -> f64 { 0.0 }
    pub fn locked() -> bool { false }
    pub fn on_sleep(_: tauri::AppHandle) {}
}
