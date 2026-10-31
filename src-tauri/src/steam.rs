//! Finding Steam installs/libraries and reading game manifests.

use crate::vdf;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct Library {
    /// Library root (the folder that contains `steamapps`).
    pub path: String,
    pub label: String,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalInstall {
    pub library: String,
    pub buildid: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub appid: String,
    pub name: String,
    pub installdir: String,
    pub size_on_disk: u64,
    pub buildid: String,
    /// StateFlags bit 4 set = fully installed on the source.
    pub complete: bool,
    /// Proton, Steam Linux Runtime, redistributables etc.
    pub is_tool: bool,
    pub local: Option<LocalInstall>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub appid: String,
    pub name: String,
    pub installdir: String,
    pub size_on_disk: u64,
    pub buildid: String,
    pub state_flags: u32,
    /// (depot id, manifest id)
    pub depots: Vec<(String, String)>,
}

pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc = vdf::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let app = doc
        .get("AppState")
        .ok_or_else(|| format!("{}: no AppState", path.display()))?;
    let field = |k: &str| app.str(k).unwrap_or_default().to_string();
    let depots = app
        .get("InstalledDepots")
        .map(|d| {
            d.entries()
                .iter()
                .filter_map(|(id, v)| Some((id.clone(), v.str("manifest")?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let m = Manifest {
        appid: field("appid"),
        name: field("name"),
        installdir: field("installdir"),
        size_on_disk: field("SizeOnDisk").parse().unwrap_or(0),
        buildid: field("buildid"),
        state_flags: field("StateFlags").parse().unwrap_or(0),
        depots,
    };
    if m.appid.is_empty() || m.installdir.is_empty() {
        return Err(format!("{}: missing appid or installdir", path.display()));
    }
    // installdir becomes a path component, so refuse anything that could escape `common`.
    if m.installdir.contains(['/', '\\']) || m.installdir == ".." || m.installdir == "." {
        return Err(format!("{}: suspicious installdir '{}'", path.display(), m.installdir));
    }
    Ok(m)
}

fn is_tool(m: &Manifest) -> bool {
    const TOOL_IDS: &[&str] = &["228980", "1070560", "1391110", "1628350", "1493710", "2180100"];
    let n = m.name.to_ascii_lowercase();
    TOOL_IDS.contains(&m.appid.as_str())
        || n.starts_with("proton")
        || n.starts_with("steam linux runtime")
        || n.starts_with("steamworks common")
        || n.contains("dedicated server")
}

/// Accepts a Steam root, a library root, or a `steamapps` folder and returns the `steamapps` folder.
pub fn resolve_steamapps(input: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(input.trim());
    for candidate in [p.join("steamapps"), p.join("SteamApps")] {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    if p.is_dir() && p.join("common").is_dir() {
        return Ok(p);
    }
    if !p.exists() {
        return Err(format!("Can't reach '{}'. Is the share mounted / reachable?", p.display()));
    }
    Err(format!("No steamapps folder found in '{}'", p.display()))
}

/// Candidate local Steam root folders for this OS.
fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p));
            }
        }
        if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey("SOFTWARE\\WOW6432Node\\Valve\\Steam") {
            if let Ok(p) = k.get_value::<String, _>("InstallPath") {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    }

    #[cfg(not(windows))]
    if let Some(home) = std::env::home_dir() {
        #[cfg(target_os = "macos")]
        roots.push(home.join("Library/Application Support/Steam"));
        roots.push(home.join(".local/share/Steam"));
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        roots.push(home.join("snap/steam/common/.local/share/Steam"));
    }

    roots
}

fn disk_space(path: &Path) -> (Option<u64>, Option<u64>) {
    (fs4::available_space(path).ok(), fs4::total_space(path).ok())
}

/// All local Steam libraries, deduplicated (symlinks like ~/.steam/steam resolve to the same place).
pub fn local_libraries() -> Vec<Library> {
    let mut seen = HashSet::new();
    let mut libs = Vec::new();
    let mut add = |path: PathBuf, label: String, libs: &mut Vec<Library>| {
        let Ok(canon) = fs::canonicalize(&path) else { return };
        if !canon.join("steamapps").is_dir() || !seen.insert(canon.clone()) {
            return;
        }
        let (free, total) = disk_space(&canon);
        libs.push(Library {
            path: display_path(&canon),
            label,
            free_bytes: free,
            total_bytes: total,
        });
    };

    for root in steam_roots() {
        let vdf_path = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(doc) = fs::read_to_string(&vdf_path).map_err(|e| e.to_string()).and_then(|t| vdf::parse(&t))
            && let Some(folders) = doc.get("libraryfolders") {
                for (_, entry) in folders.entries() {
                    // Old format: "1" "D:\\SteamLibrary"; new format: "1" { "path" "..." }
                    let (path, label) = match entry {
                        vdf::Value::Str(p) => (p.clone(), String::new()),
                        obj => match obj.str("path") {
                            Some(p) => (p.to_string(), obj.str("label").unwrap_or_default().to_string()),
                            None => continue,
                        },
                    };
                    add(PathBuf::from(path), label, &mut libs);
                }
            }
        add(root, String::new(), &mut libs);
    }
    libs
}

/// Strips Windows' `\\?\` verbatim prefix that canonicalize adds, so paths look normal in the UI.
pub fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    }
}

pub fn list_games(source: &str) -> Result<(String, Vec<Game>), String> {
    let steamapps = resolve_steamapps(source)?;
    let libs = local_libraries();
    let source_canon = fs::canonicalize(&steamapps).ok();

    let entries = fs::read_dir(&steamapps).map_err(|e| format!("{}: {e}", steamapps.display()))?;
    let mut games = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
            continue;
        }
        let Ok(m) = read_manifest(&entry.path()) else { continue };
        let local = libs.iter().find_map(|lib| {
            let lib_apps = Path::new(&lib.path).join("steamapps");
            // Don't report the source itself as a local install when testing on one machine.
            if source_canon.is_some() && fs::canonicalize(&lib_apps).ok() == source_canon {
                return None;
            }
            let lm = read_manifest(&lib_apps.join(&fname)).ok()?;
            Some(LocalInstall { library: lib.path.clone(), buildid: lm.buildid })
        });
        games.push(Game {
            is_tool: is_tool(&m),
            complete: m.state_flags & 4 != 0,
            appid: m.appid,
            name: m.name,
            installdir: m.installdir,
            size_on_disk: m.size_on_disk,
            buildid: m.buildid,
            local,
        });
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    Ok((display_path(&steamapps), games))
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Windows,
    Linux,
    Unknown,
}

/// Guesses whether an install is a Windows build (e.g. run through Proton) or a native Linux build,
/// by looking at file names near the top of the game folder.
pub fn detect_platform(game_dir: &Path) -> Platform {
    let mut linux_hint = false;
    for entry in walkdir::WalkDir::new(game_dir).max_depth(3).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".exe") && !name.contains("unins") && !name.contains("crashhandler") {
            return Platform::Windows;
        }
        if name.ends_with(".x86_64") || name.ends_with(".x86") || name.ends_with(".sh") || name.ends_with(".so") || name.contains(".so.") {
            linux_hint = true;
        }
    }
    if linux_hint { Platform::Linux } else { Platform::Unknown }
}

/// Finds game art Steam caches under `<steam root>/appcache/librarycache/<appid>`, preferring
/// landscape images. Newer clients put each image in a hashed subfolder; older ones keep them at
/// the top level, so both are checked.
pub fn find_header_image(steamapps: &Path, appid: &str) -> Option<PathBuf> {
    find_cached_image(
        steamapps,
        appid,
        &["library_header.jpg", "header.jpg", "library_capsule.jpg", "library_hero.jpg", "library_600x900.jpg"],
    )
}

/// The wide banner art Steam shows at the top of a game's library page.
pub fn find_hero_image(steamapps: &Path, appid: &str) -> Option<PathBuf> {
    find_cached_image(steamapps, appid, &["library_hero.jpg"])
}

/// Looks in Steam's art cache on the share for the first of `names` that exists.
fn find_cached_image(steamapps: &Path, appid: &str, names: &[&str]) -> Option<PathBuf> {
    let dir = steamapps.parent()?.join("appcache").join("librarycache").join(appid);
    let mut folders = vec![dir.clone()];
    folders.extend(fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    names
        .iter()
        .find_map(|name| folders.iter().map(|f| f.join(name)).find(|p| p.is_file()))
}

pub fn is_steam_running() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(procs) = fs::read_dir("/proc") else { return false };
        procs.flatten().any(|p| {
            fs::read_to_string(p.path().join("comm"))
                .map(|c| c.trim() == "steam")
                .unwrap_or(false)
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("steam.exe"))
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("pgrep")
            .args(["-x", "steam_osx"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod real_tests {
    #[test]
    #[ignore]
    fn scan_real_library() {
        let home = std::env::home_dir().unwrap();
        let (apps, games) = super::list_games(home.join(".local/share/Steam").to_str().unwrap()).unwrap();
        println!("steamapps={apps} games={} tools={} incomplete={}", games.len(),
            games.iter().filter(|g| g.is_tool).count(), games.iter().filter(|g| !g.complete).count());
        for g in games.iter().take(8) {
            let p = super::detect_platform(&std::path::Path::new(&apps).join("common").join(&g.installdir));
            println!("  {} {} {:.1}GB {:?} tool={}", g.appid, g.name, g.size_on_disk as f64/1e9, p, g.is_tool);
        }
        let with_art = games.iter().filter(|g| super::find_header_image(std::path::Path::new(&apps), &g.appid).is_some()).count();
        println!("art found for {with_art}/{}", games.len());
        for g in &games { if super::find_header_image(std::path::Path::new(&apps), &g.appid).is_none() { println!("  missing {} {}", g.appid, g.name); } }
        println!("libraries: {:?}", super::local_libraries());
    }
}
