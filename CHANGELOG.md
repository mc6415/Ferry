# Changelog

What's new in each version of Ferry. When a version is released, its section here becomes the
release notes on GitHub.

## 0.5.2

- The stats tiles and the game banner can now be hidden, to make room for the list on small
  screens or with high display scaling (e.g. a handheld at 200%). Hidden stats shrink to a
  single line. Ferry remembers your choice.
- On a short window, both start hidden until you choose, and the layout is a little tighter.

## 0.5.1

- Upgrading on Windows is now one step: the installer installs straight over the older version
  instead of asking you to uninstall it first in a separate window.
- Windows releases now come as a `-setup.exe` only. The `.msi` is no longer published.

## 0.5.0

- Added a **Dynamic** option for **Parallel file copies**. It starts at 4 and adds more while
  that keeps making the copy faster (up to 16), then backs off when it stops helping. The copy
  view shows how many files are being copied at once.
- **Parallel file copies** now offers 4, 8, 16 or Dynamic. If you'd picked 1 or 2, it goes back
  to 4.
- Big files (over 64 MB) are now copied at most 4 at a time, whatever the **Parallel file
  copies** setting. Higher settings only add workers for smaller files, so big games no longer
  flood the write cache and stall.
- Added an **Unbuffered writes for big files** setting. It writes big files straight to the
  disk, skipping the system's write cache, for a steadier speed and lower memory use. Off by
  default.

## 0.4.3

- Added deleting the local copies of games from the app. Select installed games and press
  **Delete N local**. Handy for copying a game again, e.g. to test the parallel copies setting.
  Proton prefixes and Steam Cloud saves are kept.

## 0.4.2

- Added a **Settings** panel (the sliders button in the title bar).
- Added a **Parallel file copies** setting: how many files are copied at once (1, 2, 4, 8 or
  16; default 4). More can speed up games made of lots of small files.

## 0.4.1

- Fixed the copy progress showing "undefined" and "NaN" instead of file counts and sizes.

## 0.4.0

- **Steam Sharing is now Ferry**, with a new icon. Ferry installs as a separate app, so
  uninstall Steam Sharing afterwards. You'll need to pick your share again once.

## 0.3.1

- Fixed grid view cards being squashed.
- Fixed the game list disappearing while a copy was running.

## 0.3.0

- Added a banner with the highlighted game's artwork, and per-game accent colours.
- Added a live copy view with a speed graph, time left and a per-game queue.
- Added **New** badges for games added to the share since your last visit.
- Added progress on the taskbar, and a **Restart Steam** button once a copy finishes.
- Added full keyboard control: arrow keys, letter jumps, `Space` to select, `/` to search,
  `S` to review your selection and `Ctrl` + `Enter` to copy.
- Added a review of the selected games before copying (click "N selected").

## 0.2.0

- New look: a flat purple theme, cover art, a grid view, and a custom rounded title bar.
- Added stats for the share (games, total size, not installed, updates available).
- Added an app icon and desktop notifications when a copy finishes.

## 0.1.0

- First release: list the games on a shared Steam library and copy them into a local one.
