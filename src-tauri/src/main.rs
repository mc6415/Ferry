// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK 2.54's GPU compositor doesn't keep transparent windows transparent, which
    // leaves grey squares behind our rounded corners. Fall back to its non-composited path
    // unless the user has chosen a setting themselves.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
        // SAFETY: runs first thing in main, before any other threads exist.
        unsafe { std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1") };
    }
    steam_sharing_lib::run()
}
