fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "desktop_info",
            "mac_key_sync",
            "mac_key_reveal",
            "desktop_sign_in",
            "cancel_desktop_sign_in",
            "set_reminders",
            "open_item",
            "quit_app",
            "get_reminders",
            "reminder_action",
            "check_for_updates",
        ]),
    ))
    .expect("Tauri configuration is valid");
}
