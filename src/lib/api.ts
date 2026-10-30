import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Platform = "windows" | "linux" | "unknown";

export interface Game {
  appid: string;
  name: string;
  installdir: string;
  sizeOnDisk: number;
  buildid: string;
  complete: boolean;
  isTool: boolean;
  local: { library: string; buildid: string } | null;
}

export interface Library {
  path: string;
  label: string;
  free_bytes: number | null;
  total_bytes: number | null;
}

export type CopyEvent =
  | {
      kind: "progress";
      gameIndex: number;
      gameCount: number;
      appid: string;
      name: string;
      phase: "scanning" | "copying" | "finishing";
      bytesDone: number;
      bytesTotal: number;
      filesDone: number;
      filesTotal: number;
      skippedFiles: number;
      currentFile: string;
    }
  | { kind: "gameDone"; appid: string; name: string; ok: boolean; message: string }
  | { kind: "finished"; cancelled: boolean; succeeded: number; failed: number };

export const api = {
  listGames: (source: string) => invoke<{ steamapps: string; games: Game[] }>("list_games", { source }),
  localLibraries: () => invoke<Library[]>("local_libraries"),
  detectPlatforms: (steamapps: string, installdirs: Record<string, string>) =>
    invoke<Record<string, Platform>>("detect_platforms", { steamapps, installdirs }),
  steamRunning: () => invoke<boolean>("steam_running"),
  hostOs: () => invoke<string>("host_os"),
  startCopy: (steamapps: string, targetLibrary: string, appids: string[]) =>
    invoke<void>("start_copy", { steamapps, targetLibrary, appids }),
  cancelCopy: () => invoke<void>("cancel_copy"),
  onCopyEvent: (cb: (e: CopyEvent) => void): Promise<UnlistenFn> =>
    listen<CopyEvent>("copy-event", (e) => cb(e.payload)),
};

export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "?";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) {
    n /= 1000;
    i++;
  }
  return `${n.toFixed(i >= 3 ? 1 : 0)} ${units[i]}`;
}

export function formatDuration(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "–";
  const s = Math.round(secs);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

/** Art from Steam's cache on the share (served by the Rust `ssimg` protocol). */
export const localImageUrl = (appid: string) => convertFileSrc(appid, "ssimg");

/** Fallback when the share has no cached art. */
export const cdnImageUrl = (appid: string) =>
  `https://cdn.cloudflare.steamstatic.com/steam/apps/${appid}/header.jpg`;
