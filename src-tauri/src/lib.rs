mod copy;
mod steam;
mod vdf;

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, State};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameList {
    steamapps: String,
    games: Vec<steam::Game>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// The `steamapps` folder of the connected share, used to serve cached game art.
#[derive(Default)]
struct ImageSource(Mutex<Option<PathBuf>>);

#[tauri::command]
async fn list_games(source: String, images: State<'_, ImageSource>) -> Result<GameList, String> {
    let (steamapps, games) = blocking(move || steam::list_games(&source)).await??;
    *images.0.lock().unwrap() = Some(PathBuf::from(&steamapps));
    Ok(GameList { steamapps, games })
}

/// Serves `ssimg://localhost/<appid>` (header art) and `ssimg://localhost/hero-<appid>`
/// (library banner) from Steam's local art cache on the share.
fn image_response(app: &AppHandle, request: &tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    use tauri::Manager;
    let not_found = || tauri::http::Response::builder().status(404).body(Vec::new()).unwrap();
    let path = request.uri().path().trim_start_matches('/');
    let (hero, appid) = match path.strip_prefix("hero-") {
        Some(id) => (true, id),
        None => (false, path),
    };
    if appid.is_empty() || !appid.bytes().all(|b| b.is_ascii_digit()) {
        return not_found();
    }
    let Some(steamapps) = app.state::<ImageSource>().0.lock().unwrap().clone() else {
        return not_found();
    };
    let found = if hero { steam::find_hero_image(&steamapps, appid) } else { steam::find_header_image(&steamapps, appid) };
    match found.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => tauri::http::Response::builder()
            .header("Content-Type", "image/jpeg")
            // Lets the page read pixels to pick each game's accent colour.
            .header("Access-Control-Allow-Origin", "*")
            .header("Cache-Control", "max-age=86400")
            .body(bytes)
            .unwrap(),
        None => not_found(),
    }
}

#[tauri::command]
async fn local_libraries() -> Result<Vec<steam::Library>, String> {
    blocking(steam::local_libraries).await
}

/// Detects platforms for several games in parallel (slow-ish over a network share).
#[tauri::command]
async fn detect_platforms(steamapps: String, installdirs: HashMap<String, String>) -> Result<HashMap<String, steam::Platform>, String> {
    blocking(move || {
        let common = PathBuf::from(steamapps).join("common");
        let items: Vec<_> = installdirs.into_iter().collect();
        let chunk = items.len().div_ceil(8).max(1);
        std::thread::scope(|s| {
            let handles: Vec<_> = items
                .chunks(chunk)
                .map(|c| {
                    let common = &common;
                    s.spawn(move || {
                        c.iter()
                            .map(|(appid, dir)| (appid.clone(), steam::detect_platform(&common.join(dir))))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        })
    })
    .await
}

#[tauri::command]
fn host_os() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
fn steam_running() -> bool {
    steam::is_steam_running()
}

/// Quits Steam (if running) and starts it again so it picks up newly copied games.
/// Goes through `steam://` links, so it works with any install (native, Flatpak, Windows).
#[tauri::command]
async fn restart_steam(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if steam::is_steam_running() {
        app.opener().open_url("steam://exit", None::<&str>).map_err(|e| e.to_string())?;
        let closed = blocking(|| {
            for _ in 0..60 {
                if !steam::is_steam_running() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            false
        })
        .await?;
        if !closed {
            return Err("Steam didn't close within 30 seconds".into());
        }
        // Give Steam a moment to release its files before relaunching.
        blocking(|| std::thread::sleep(std::time::Duration::from_secs(2))).await?;
    }
    app.opener().open_url("steam://open/games", None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn start_copy(app: AppHandle, state: State<'_, copy::JobState>, steamapps: String, target_library: String, appids: Vec<String>, workers: Option<usize>) -> Result<(), String> {
    let mut job = state.0.lock().unwrap();
    if job.is_some() {
        return Err("A copy is already running".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *job = Some(cancel.clone());
    std::thread::spawn(move || {
        let report = |e: copy::Event| {
            let _ = app.emit("copy-event", e);
        };
        copy::run(&PathBuf::from(steamapps), &PathBuf::from(target_library), &appids, workers.unwrap_or(copy::DEFAULT_WORKERS), &cancel, &report);
        use tauri::Manager;
        *app.state::<copy::JobState>().0.lock().unwrap() = None;
    });
    Ok(())
}

#[tauri::command]
fn cancel_copy(state: State<'_, copy::JobState>) {
    if let Some(c) = state.0.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

/// GTK still paints its own window background and frame under the webview, which shows up
/// as square corners around our rounded window. Make all of that transparent.
#[cfg(target_os = "linux")]
fn clear_gtk_background(win: &tauri::WebviewWindow) {
    use gtk::prelude::*;
    let Ok(gtk_win) = win.gtk_window() else { return };
    let css = gtk::CssProvider::new();
    if css
        .load_from_data(b"window, window.background, decoration, .titlebar { background: transparent; box-shadow: none; border: none; }")
        .is_err()
    {
        return;
    }
    if let Some(screen) = GtkWindowExt::screen(&gtk_win) {
        gtk::StyleContext::add_provider_for_screen(&screen, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    gtk_win.set_app_paintable(true);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|_app| {
            #[cfg(target_os = "linux")]
            {
                use tauri::Manager;
                if let Some(win) = _app.get_webview_window("main") {
                    clear_gtk_background(&win);
                }
            }
            Ok(())
        })
        .manage(copy::JobState::default())
        .manage(ImageSource::default())
        .register_asynchronous_uri_scheme_protocol("ssimg", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            // Reads can be slow over a network share, so keep them off the UI thread.
            tauri::async_runtime::spawn_blocking(move || responder.respond(image_response(&app, &request)));
        })
        .invoke_handler(tauri::generate_handler![
            list_games,
            local_libraries,
            detect_platforms,
            steam_running,
            restart_steam,
            host_os,
            start_copy,
            cancel_copy
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
