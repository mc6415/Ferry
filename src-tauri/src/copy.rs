//! Copies game folders + manifests from a source `steamapps` into a local library.

use crate::steam::{self, Manifest};
use filetime::FileTime;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BUF_SIZE: usize = 4 * 1024 * 1024;
/// Files copied at once when the caller doesn't say. More helps games with lots of small
/// files over SMB; fewer is kinder to hard drives.
pub const DEFAULT_WORKERS: usize = 4;
/// Upper bound on parallel file copies, whatever the caller asks for.
pub const MAX_WORKERS: usize = 32;
/// Files at least this big count as "big". Big files are never copied more than
/// `BIG_FILE_WORKERS` at a time (more just floods the write cache), and are the ones written
/// unbuffered when that's turned on. The other workers stick to smaller files.
const BIG_FILE: u64 = 64 * 1024 * 1024;
const BIG_FILE_WORKERS: usize = 4;
/// Unbuffered writes must line up with the disk's sectors. 4 KiB covers 512 B and 4 KiB sectors.
const ALIGN: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct CopyOptions {
    /// Files copied at once (clamped to 1..=MAX_WORKERS).
    pub workers: usize,
    /// Write big files straight to disk, bypassing the OS write cache.
    pub unbuffered: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self { workers: DEFAULT_WORKERS, unbuffered: false }
    }
}
const PARTIAL_SUFFIX: &str = ".sscopy-partial";
/// SMB and FAT round timestamps, so allow some slack when deciding a file is unchanged.
const MTIME_SLACK_SECS: i64 = 2;

#[derive(Debug, Clone, Serialize)]
// rename_all only renames the variant tags; rename_all_fields is needed for the fields the UI reads.
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum Event {
    Progress {
        game_index: usize,
        game_count: usize,
        appid: String,
        name: String,
        phase: &'static str,
        bytes_done: u64,
        bytes_total: u64,
        files_done: usize,
        files_total: usize,
        skipped_files: usize,
        current_file: String,
    },
    GameDone {
        appid: String,
        name: String,
        ok: bool,
        message: String,
    },
    Finished {
        cancelled: bool,
        succeeded: usize,
        failed: usize,
    },
}

#[derive(Debug)]
enum CopyError {
    Cancelled,
    Failed(String),
}

impl From<String> for CopyError {
    fn from(s: String) -> Self {
        CopyError::Failed(s)
    }
}

fn io_err(path: &Path, e: std::io::Error) -> CopyError {
    CopyError::Failed(format!("{}: {e}", path.display()))
}

struct FileItem {
    rel: PathBuf,
    size: u64,
    mtime: FileTime,
    #[cfg(unix)]
    mode: u32,
}

pub fn run(
    source_steamapps: &Path,
    target_library: &Path,
    appids: &[String],
    opts: CopyOptions,
    cancel: &AtomicBool,
    report: &(dyn Fn(Event) + Sync),
) {
    let opts = CopyOptions { workers: opts.workers.clamp(1, MAX_WORKERS), ..opts };
    let dst_apps = target_library.join("steamapps");
    let (mut ok, mut failed) = (0, 0);
    for (i, appid) in appids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let ctx = GameCtx { index: i, count: appids.len(), appid: appid.clone(), name: appid.clone() };
        let (name, result) = copy_game(source_steamapps, &dst_apps, ctx, opts, cancel, report);
        let (good, message) = match result {
            Ok(msg) => (true, msg),
            Err(CopyError::Cancelled) => (false, "Cancelled".into()),
            Err(CopyError::Failed(e)) => (false, e),
        };
        if good { ok += 1 } else { failed += 1 }
        report(Event::GameDone { appid: appid.clone(), name, ok: good, message });
    }
    report(Event::Finished { cancelled: cancel.load(Ordering::Relaxed), succeeded: ok, failed });
}

struct GameCtx {
    index: usize,
    count: usize,
    appid: String,
    name: String,
}

fn copy_game(
    src_apps: &Path,
    dst_apps: &Path,
    mut ctx: GameCtx,
    opts: CopyOptions,
    cancel: &AtomicBool,
    report: &(dyn Fn(Event) + Sync),
) -> (String, Result<String, CopyError>) {
    let manifest_name = format!("appmanifest_{}.acf", ctx.appid);
    let manifest = match steam::read_manifest(&src_apps.join(&manifest_name)) {
        Ok(m) => m,
        Err(e) => return (ctx.name, Err(e.into())),
    };
    ctx.name = manifest.name.clone();
    let name = ctx.name.clone();
    (name, copy_game_inner(src_apps, dst_apps, &manifest, &manifest_name, &ctx, opts, cancel, report))
}

#[allow(clippy::too_many_arguments)]
fn copy_game_inner(
    src_apps: &Path,
    dst_apps: &Path,
    manifest: &Manifest,
    manifest_name: &str,
    ctx: &GameCtx,
    opts: CopyOptions,
    cancel: &AtomicBool,
    report: &(dyn Fn(Event) + Sync),
) -> Result<String, CopyError> {
    let progress = |phase: &'static str, bytes_done, bytes_total, files_done, files_total, skipped_files, current_file: String| {
        report(Event::Progress {
            game_index: ctx.index,
            game_count: ctx.count,
            appid: ctx.appid.clone(),
            name: ctx.name.clone(),
            phase,
            bytes_done,
            bytes_total,
            files_done,
            files_total,
            skipped_files,
            current_file,
        })
    };

    let src_dir = src_apps.join("common").join(&manifest.installdir);
    let dst_dir = dst_apps.join("common").join(&manifest.installdir);
    if !src_dir.is_dir() {
        return Err(format!("Game folder not found on source: {}", src_dir.display()).into());
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&src_dir), fs::canonicalize(&dst_dir))
        && a == b {
            return Err("Source and destination are the same folder".to_string().into());
        }

    // 1. Scan source and work out what actually needs copying.
    progress("scanning", 0, 0, 0, 0, 0, String::new());
    let mut dirs = Vec::new();
    let mut links = Vec::new();
    let mut to_copy = Vec::new();
    let mut skipped = 0usize;
    for entry in walkdir::WalkDir::new(&src_dir).follow_links(false) {
        if cancel.load(Ordering::Relaxed) {
            return Err(CopyError::Cancelled);
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let rel = entry.path().strip_prefix(&src_dir).unwrap().to_path_buf();
        let ft = entry.file_type();
        if ft.is_dir() {
            dirs.push(rel);
        } else if ft.is_symlink() {
            links.push(rel);
        } else if ft.is_file() {
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            let item = FileItem {
                size: meta.len(),
                mtime: FileTime::from_last_modification_time(&meta),
                #[cfg(unix)]
                mode: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()),
                rel,
            };
            if is_up_to_date(&dst_dir.join(&item.rel), &item) {
                skipped += 1;
            } else {
                to_copy.push(item);
            }
            if (to_copy.len() + skipped).is_multiple_of(500) {
                progress("scanning", 0, 0, 0, to_copy.len() + skipped, skipped, String::new());
            }
        }
    }
    let bytes_total: u64 = to_copy.iter().map(|f| f.size).sum();
    let files_total = to_copy.len() + skipped;

    // 2. Make sure it fits (keep 1 GiB spare).
    fs::create_dir_all(&dst_dir).map_err(|e| io_err(&dst_dir, e))?;
    if let Ok(free) = fs4::available_space(&dst_dir) {
        let needed = bytes_total + (1 << 30);
        if free < needed {
            return Err(format!(
                "Not enough disk space: need {:.1} GB, {:.1} GB free",
                needed as f64 / 1e9,
                free as f64 / 1e9
            )
            .into());
        }
    }

    // 3. Directories and symlinks.
    for d in &dirs {
        let p = dst_dir.join(d);
        fs::create_dir_all(&p).map_err(|e| io_err(&p, e))?;
    }
    for l in &links {
        copy_symlink(&src_dir.join(l), &dst_dir.join(l))?;
    }

    // 4. Files, several at a time, biggest first so the tail end isn't one huge file on one
    //    worker. Big files go to at most BIG_FILE_WORKERS workers; the rest only take small ones.
    to_copy.sort_by_key(|f| std::cmp::Reverse(f.size));
    let (big, small) = to_copy.split_at(to_copy.partition_point(|f| f.size >= BIG_FILE));
    let workers = opts.workers;
    let big_workers = workers.min(BIG_FILE_WORKERS);
    let next_big = AtomicUsize::new(0);
    let next_small = AtomicUsize::new(0);
    let files_done = AtomicUsize::new(0);
    let bytes_done = AtomicU64::new(0);
    let current = Mutex::new(String::new());
    let first_err: Mutex<Option<CopyError>> = Mutex::new(None);
    let stop = AtomicBool::new(false);
    let workers_left = AtomicUsize::new(workers);

    let worker = |takes_big: bool| {
        // Aligned so it can be used for unbuffered writes.
        let mut store = vec![0u8; BUF_SIZE + ALIGN];
        let off = store.as_ptr().align_offset(ALIGN);
        let buf = &mut store[off..off + BUF_SIZE];
        loop {
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                break;
            }
            let item = takes_big
                .then(|| big.get(next_big.fetch_add(1, Ordering::Relaxed)))
                .flatten()
                .or_else(|| small.get(next_small.fetch_add(1, Ordering::Relaxed)));
            let Some(item) = item else { break };
            *current.lock().unwrap() = item.rel.display().to_string();
            let unbuffered = opts.unbuffered && item.size >= BIG_FILE;
            let r = copy_file(&src_dir.join(&item.rel), &dst_dir.join(&item.rel), item, buf, unbuffered, &bytes_done, cancel, &stop);
            match r {
                Ok(()) => {
                    files_done.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    stop.store(true, Ordering::Relaxed);
                    first_err.lock().unwrap().get_or_insert(e);
                    break;
                }
            }
        }
        workers_left.fetch_sub(1, Ordering::Relaxed);
    };

    std::thread::scope(|s| {
        for w in 0..workers {
            let worker = &worker;
            s.spawn(move || worker(w < big_workers));
        }
        // Progress reporter
        loop {
            let done = workers_left.load(Ordering::Relaxed) == 0;
            progress(
                "copying",
                bytes_done.load(Ordering::Relaxed),
                bytes_total,
                files_done.load(Ordering::Relaxed) + skipped,
                files_total,
                skipped,
                current.lock().unwrap().clone(),
            );
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(CopyError::Cancelled);
    }

    // 5. Depot manifests (lets Steam validate/patch without a full verify), then the app manifest
    //    last so Steam never sees a half-copied game as installed.
    progress("finishing", bytes_total, bytes_total, files_total, files_total, skipped, manifest_name.to_string());
    let dst_cache = dst_apps.join("depotcache");
    for (depot, mid) in &manifest.depots {
        let fname = format!("{depot}_{mid}.manifest");
        let src = src_apps.join("depotcache").join(&fname);
        if src.is_file() {
            let _ = fs::create_dir_all(&dst_cache);
            let _ = atomic_copy_small(&src, &dst_cache.join(&fname));
        }
    }
    atomic_copy_small(&src_apps.join(manifest_name), &dst_apps.join(manifest_name))?;

    let copied_gb = bytes_total as f64 / 1e9;
    Ok(if skipped > 0 {
        format!("Copied {copied_gb:.2} GB ({skipped} unchanged files skipped)")
    } else {
        format!("Copied {copied_gb:.2} GB")
    })
}

fn is_up_to_date(dst: &Path, item: &FileItem) -> bool {
    let Ok(meta) = fs::metadata(dst) else { return false };
    let mtime = FileTime::from_last_modification_time(&meta);
    meta.len() == item.size && (mtime.unix_seconds() - item.mtime.unix_seconds()).abs() <= MTIME_SLACK_SECS
}

fn partial_path(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(PARTIAL_SUFFIX);
    dst.with_file_name(name)
}

#[allow(clippy::too_many_arguments)]
fn copy_file(
    src: &Path,
    dst: &Path,
    item: &FileItem,
    buf: &mut [u8],
    unbuffered: bool,
    bytes_done: &AtomicU64,
    cancel: &AtomicBool,
    stop: &AtomicBool,
) -> Result<(), CopyError> {
    let tmp = partial_path(dst);
    let result = (|| {
        let mut input = File::open(src).map_err(|e| io_err(src, e))?;
        // Unbuffered if asked and the destination supports it, otherwise a normal write.
        let (mut output, mut direct) = match unbuffered.then(|| open_unbuffered(&tmp)) {
            Some(Ok(f)) => (f, true),
            _ => (File::create(&tmp).map_err(|e| io_err(&tmp, e))?, false),
        };
        let mut written = 0u64;
        #[cfg(unix)]
        let mut first_bytes: Option<[u8; 4]> = None;
        loop {
            if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                return Err(CopyError::Cancelled);
            }
            // Unbuffered writes need whole sectors, so fill the buffer; only the last chunk is short.
            let n = if direct { read_full(&mut input, buf) } else { input.read(buf) }.map_err(|e| io_err(src, e))?;
            if n == 0 {
                break;
            }
            #[cfg(unix)]
            if first_bytes.is_none() && n >= 4 {
                first_bytes = Some([buf[0], buf[1], buf[2], buf[3]]);
            }
            if direct {
                let aligned = n - n % ALIGN;
                match output.write_all(&buf[..aligned]) {
                    Ok(()) => {}
                    // Some filesystems accept the unbuffered open but not the writes: start over normally.
                    Err(_) if written == 0 => {
                        output = File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
                        direct = false;
                        output.write_all(&buf[..n]).map_err(|e| io_err(&tmp, e))?;
                    }
                    Err(e) => return Err(io_err(&tmp, e)),
                }
                if direct && aligned < n {
                    // The final partial sector goes through a normal handle.
                    drop(output);
                    output = OpenOptions::new().append(true).open(&tmp).map_err(|e| io_err(&tmp, e))?;
                    direct = false;
                    output.write_all(&buf[aligned..n]).map_err(|e| io_err(&tmp, e))?;
                }
            } else {
                output.write_all(&buf[..n]).map_err(|e| io_err(&tmp, e))?;
            }
            written += n as u64;
            bytes_done.fetch_add(n as u64, Ordering::Relaxed);
        }
        drop(output);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // SMB mounts often lose the executable bit, so restore it for ELF binaries and scripts.
            let looks_executable = matches!(first_bytes, Some([0x7f, b'E', b'L', b'F']) | Some([b'#', b'!', _, _]));
            let mut mode = (item.mode & 0o777) | 0o600;
            if looks_executable {
                mode |= (mode & 0o444) >> 2; // +x wherever readable
            }
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).map_err(|e| io_err(&tmp, e))?;
        }

        filetime::set_file_mtime(&tmp, item.mtime).map_err(|e| io_err(&tmp, e))?;
        fs::rename(&tmp, dst).map_err(|e| io_err(dst, e))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Reads until `buf` is full or the file ends (network reads often return less than asked).
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Opens `path` for writing without the OS write cache: FILE_FLAG_NO_BUFFERING on Windows (like
/// robocopy /J), O_DIRECT on Linux. Writes must then be whole, aligned sectors.
fn open_unbuffered(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        o.custom_flags(FILE_FLAG_NO_BUFFERING);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_DIRECT);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    return Err(io::Error::other("unbuffered writes aren't supported here"));
    #[cfg(any(windows, target_os = "linux"))]
    o.open(path)
}

fn copy_symlink(src: &Path, dst: &Path) -> Result<(), CopyError> {
    #[cfg(unix)]
    {
        let target = fs::read_link(src).map_err(|e| io_err(src, e))?;
        if fs::read_link(dst).ok().as_ref() == Some(&target) {
            return Ok(());
        }
        let _ = fs::remove_file(dst);
        std::os::unix::fs::symlink(&target, dst).map_err(|e| io_err(dst, e))?;
    }
    #[cfg(not(unix))]
    {
        // Creating symlinks on Windows needs admin/dev mode, so copy what it points at instead.
        let meta = fs::metadata(src).map_err(|e| io_err(src, e))?;
        if meta.is_file() {
            fs::copy(src, dst).map_err(|e| io_err(dst, e))?;
        }
    }
    Ok(())
}

fn atomic_copy_small(src: &Path, dst: &Path) -> Result<(), CopyError> {
    let tmp = partial_path(dst);
    fs::copy(src, &tmp).map_err(|e| io_err(&tmp, e))?;
    fs::rename(&tmp, dst).map_err(|e| io_err(dst, e))?;
    Ok(())
}

/// Shared handle so a running job can be cancelled from another command.
#[derive(Default)]
pub struct JobState(pub Mutex<Option<Arc<AtomicBool>>>);

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, content: &[u8]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn events_serialize_with_camel_case_fields() {
        let e = Event::Progress {
            game_index: 0,
            game_count: 2,
            appid: "42".into(),
            name: "Test".into(),
            phase: "copying",
            bytes_done: 1,
            bytes_total: 2,
            files_done: 3,
            files_total: 4,
            skipped_files: 0,
            current_file: "a".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "progress");
        for key in ["gameIndex", "gameCount", "bytesDone", "bytesTotal", "filesDone", "filesTotal", "skippedFiles", "currentFile"] {
            assert!(v.get(key).is_some(), "missing {key} in {v}");
        }
    }

    /// A temp dir on a real disk: /tmp is often tmpfs, which doesn't support O_DIRECT.
    fn disk_tempdir() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn item_for(path: &Path) -> FileItem {
        let meta = fs::metadata(path).unwrap();
        FileItem {
            rel: PathBuf::new(),
            size: meta.len(),
            mtime: FileTime::from_last_modification_time(&meta),
            #[cfg(unix)]
            mode: 0o644,
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn unbuffered_open_and_aligned_write_work_on_disk() {
        let dir = disk_tempdir();
        let path = dir.path().join("direct");
        let mut f = open_unbuffered(&path).expect("unbuffered open");
        let store = vec![7u8; ALIGN * 3];
        let off = store.as_ptr().align_offset(ALIGN);
        f.write_all(&store[off..off + ALIGN * 2]).expect("aligned unbuffered write");
        drop(f);
        assert_eq!(fs::metadata(&path).unwrap().len(), (ALIGN * 2) as u64);
    }

    #[test]
    fn unbuffered_copy_handles_any_size() {
        let dir = disk_tempdir();
        let mut store = vec![0u8; BUF_SIZE + ALIGN];
        let off = store.as_ptr().align_offset(ALIGN);
        let buf = &mut store[off..off + BUF_SIZE];
        // Empty, under a sector, exactly aligned, and several buffers plus an odd tail.
        for len in [0, 100, ALIGN * 3, BUF_SIZE, 2 * BUF_SIZE + 12345] {
            let src = dir.path().join(format!("src-{len}"));
            let dst = dir.path().join(format!("dst-{len}"));
            let data = pattern(len);
            fs::write(&src, &data).unwrap();
            let done = AtomicU64::new(0);
            let flag = AtomicBool::new(false);
            copy_file(&src, &dst, &item_for(&src), buf, true, &done, &flag, &flag).unwrap();
            assert_eq!(fs::read(&dst).unwrap(), data, "contents differ for {len} bytes");
            assert_eq!(done.load(Ordering::Relaxed), len as u64);
            assert!(!partial_path(&dst).exists());
        }
    }

    #[test]
    fn copies_mixed_sizes_with_many_workers_unbuffered() {
        let src = disk_tempdir();
        let dst = disk_tempdir();
        let apps = src.path().join("steamapps");
        write(&apps.join("appmanifest_7.acf"), br#""AppState" { "appid" "7" "name" "Mixed" "installdir" "Mixed" }"#);
        let big = pattern(BIG_FILE as usize + 5000);
        write(&apps.join("common/Mixed/big1.pak"), &big);
        write(&apps.join("common/Mixed/big2.pak"), &big[..BIG_FILE as usize]);
        for i in 0..200 {
            write(&apps.join(format!("common/Mixed/small/{i}.dat")), &pattern(i * 97));
        }
        fs::create_dir_all(dst.path().join("steamapps")).unwrap();

        let events = Mutex::new(Vec::new());
        let report = |e: Event| events.lock().unwrap().push(e);
        let opts = CopyOptions { workers: 16, unbuffered: true };
        run(&apps, dst.path(), &["7".into()], opts, &AtomicBool::new(false), &report);

        let out = dst.path().join("steamapps/common/Mixed");
        assert_eq!(fs::read(out.join("big1.pak")).unwrap(), big);
        assert_eq!(fs::read(out.join("big2.pak")).unwrap(), &big[..BIG_FILE as usize]);
        for i in 0..200 {
            assert_eq!(fs::read(out.join(format!("small/{i}.dat"))).unwrap(), pattern(i * 97));
        }
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, Event::GameDone { ok: true, .. })));
    }

    #[test]
    fn copies_game_then_skips_unchanged() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let apps = src.path().join("steamapps");
        write(
            &apps.join("appmanifest_42.acf"),
            br#""AppState" { "appid" "42" "name" "Test Game" "installdir" "Test Game" "StateFlags" "4"
               "InstalledDepots" { "43" { "manifest" "999" } } }"#,
        );
        write(&apps.join("depotcache/43_999.manifest"), b"depot");
        write(&apps.join("common/Test Game/game.bin"), &vec![7u8; 10 * 1024 * 1024]);
        write(&apps.join("common/Test Game/data/a.txt"), b"hello");
        write(&apps.join("common/Test Game/run.sh"), b"#!/bin/sh\necho hi\n");
        fs::create_dir_all(dst.path().join("steamapps")).unwrap();

        let events = Mutex::new(Vec::new());
        let report = |e: Event| events.lock().unwrap().push(e);
        let cancel = AtomicBool::new(false);
        run(&apps, dst.path(), &["42".into()], CopyOptions::default(), &cancel, &report);

        let out = dst.path().join("steamapps");
        assert_eq!(fs::read(out.join("common/Test Game/data/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::metadata(out.join("common/Test Game/game.bin")).unwrap().len(), 10 * 1024 * 1024);
        assert!(out.join("appmanifest_42.acf").is_file());
        assert!(out.join("depotcache/43_999.manifest").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(out.join("common/Test Game/run.sh")).unwrap().permissions().mode();
            assert!(mode & 0o100 != 0, "script should be executable");
        }
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, Event::GameDone { ok: true, .. })));

        // Second run: everything is unchanged so nothing should be copied.
        events.lock().unwrap().clear();
        run(&apps, dst.path(), &["42".into()], CopyOptions::default(), &cancel, &report);
        let ev = events.lock().unwrap();
        let msg = ev.iter().find_map(|e| match e {
            Event::GameDone { ok: true, message, .. } => Some(message.clone()),
            _ => None,
        });
        assert_eq!(msg.as_deref(), Some("Copied 0.00 GB (3 unchanged files skipped)"));
    }
}
