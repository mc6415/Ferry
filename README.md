# Steam Sharing

Copy Steam games from a shared library on another PC straight into your local Steam library —
uncompressed, over SMB, much faster than Steam's network transfer.

## How it works

1. Point it at the shared Steam folder (Steam root, library root, or `steamapps` all work).
   - Windows: `\\MY-PC\SteamShare`
   - Linux: mount the share first (file manager or `/etc/fstab` with `cifs`), then pick the mount.
2. It reads every `appmanifest_*.acf`, shows the games with size, install status and platform.
3. Selected games are copied to `steamapps/common/<installdir>` in the chosen local library, then
   matching depot manifests, and **the app manifest last** so Steam never sees a half-copied game.
4. Restart Steam — the games show up as installed.

Details:
- Files are written as `*.sscopy-partial` and renamed when complete; re-running a copy skips files
  whose size + modified time already match, so interrupted copies resume and updates are quick.
- 4 files copy in parallel; there's a free-space check before each game.
- On Linux targets the executable bit is restored for ELF binaries and scripts (SMB often drops it).
- Games installed natively for Linux are flagged when running on Windows (they won't run there).
- Files removed from a game on the share are not deleted locally.

## Develop

```sh
npm install
npm run tauri dev
cd src-tauri && cargo test
```

## Build

```sh
npm run tauri build
```

Build on each OS you want a binary for (Linux → `.deb`/`.rpm`/AppImage, Windows → `.msi`/`.exe`).
Windows needs the WebView2 runtime (preinstalled on Windows 10/11) and the MSVC build tools.

## Releases

Every push to `main` builds Linux and Windows installers with GitHub Actions
(`.github/workflows/release.yml`) and publishes them to the release for the current version
(`v` + `version` in `src-tauri/tauri.conf.json`). Pushing again replaces that release's files.
To start a new release, bump `version` in `src-tauri/tauri.conf.json` (and ideally
`src-tauri/Cargo.toml` / `package.json` to match).
