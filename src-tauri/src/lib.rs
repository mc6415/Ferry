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

/// Serves `ssimg://localhost/<appid>` from Steam's local art cache on the share.
fn image_response(app: &AppHandle, request: &tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    use tauri::Manager;
    let not_found = || tauri::http::Response::builder().status(404).body(Vec::new()).unwrap();
    let appid = request.uri().path().trim_start_matches('/');
    if appid.is_empty() || !appid.bytes().all(|b| b.is_ascii_digit()) {
        return not_found();
    }
    let Some(steamapps) = app.state::<ImageSource>().0.lock().unwrap().clone() else {
        return not_found();
    };
    match steam::find_header_image(&steamapps, appid).and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => tauri::http::Response::builder()
            .header("Content-Type", "image/jpeg")
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

#[tauri::command]
fn start_copy(app: AppHandle, state: State<'_, copy::JobState>, steamapps: String, target_library: String, appids: Vec<String>) -> Result<(), String> {
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
        copy::run(&PathBuf::from(steamapps), &PathBuf::from(target_library), &appids, &cancel, &report);
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
            host_os,
            start_copy,
            cancel_copy
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
