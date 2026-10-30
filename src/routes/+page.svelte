<script lang="ts">
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    api,
    cdnImageUrl,
    localImageUrl,
    formatBytes,
    formatDuration,
    type CopyEvent,
    type Game,
    type Library,
    type Platform,
  } from "$lib/api";

  type QueueItem = { appid: string; name: string; size: number; status: "pending" | "active" | "done" | "failed" | "cancelled"; message: string };
  type Progress = Extract<CopyEvent, { kind: "progress" }>;

  const store = {
    get: (k: string) => {
      try { return localStorage.getItem(k) ?? ""; } catch { return ""; }
    },
    set: (k: string, v: string) => {
      try { localStorage.setItem(k, v); } catch {}
    },
  };

  let hostOs = $state("linux");
  let source = $state(store.get("source"));
  let steamapps = $state("");
  let games = $state<Game[]>([]);
  let platforms = $state<Record<string, Platform>>({});
  let libs = $state<Library[]>([]);
  let target = $state(store.get("target"));
  let steamRunning = $state(false);
  let loading = $state(false);
  let error = $state("");

  let search = $state("");
  let hideTools = $state(true);
  let filter = $state<"all" | "missing" | "outdated">("all");
  let sort = $state<"name" | "size">("name");
  const selected = new SvelteSet<string>();
  // Image fallback chain: share cache → Steam CDN → placeholder.
  const imgCdn = new SvelteSet<string>();
  const imgFailed = new SvelteSet<string>();

  let queue = $state<QueueItem[]>([]);
  let progress = $state<Progress | null>(null);
  let copying = $state(false);
  let finishedSummary = $state("");
  let speed = $state(0);
  let lastSample: { t: number; bytes: number; appid: string } | null = null;

  const placeholder = $derived(hostOs === "windows" ? "\\\\MY-PC\\SteamShare" : "/mnt/steamshare");
  const targetLib = $derived(libs.find((l) => l.path === target));

  function status(g: Game): "installed" | "outdated" | "missing" {
    if (!g.local) return "missing";
    return g.local.buildid === g.buildid ? "installed" : "outdated";
  }

  const visible = $derived.by(() => {
    const q = search.trim().toLowerCase();
    let list = games.filter(
      (g) =>
        (!hideTools || !g.isTool) &&
        (filter === "all" || status(g) === filter) &&
        (!q || g.name.toLowerCase().includes(q) || g.appid === q),
    );
    if (sort === "size") list = [...list].sort((a, b) => b.sizeOnDisk - a.sizeOnDisk);
    return list;
  });

  const selectedGames = $derived(games.filter((g) => selected.has(g.appid)));
  const selectedBytes = $derived(selectedGames.reduce((n, g) => n + g.sizeOnDisk, 0));
  const notEnoughSpace = $derived(targetLib?.free_bytes != null && selectedBytes > targetLib.free_bytes);
  const allVisibleSelected = $derived(visible.length > 0 && visible.every((g) => selected.has(g.appid)));

  const overall = $derived.by(() => {
    if (!queue.length) return { pct: 0, eta: NaN };
    const total = queue.reduce((n, q) => n + q.size, 0) || 1;
    let done = queue.filter((q) => q.status === "done" || q.status === "failed").reduce((n, q) => n + q.size, 0);
    let remainingCurrent = 0;
    if (progress && progress.phase === "copying" && progress.bytesTotal > 0) {
      const item = queue.find((q) => q.appid === progress!.appid);
      const frac = progress.bytesDone / progress.bytesTotal;
      if (item) done += item.size * frac;
      remainingCurrent = progress.bytesTotal - progress.bytesDone;
    }
    const pendingBytes = queue.filter((q) => q.status === "pending").reduce((n, q) => n + q.size, 0);
    return { pct: Math.min(100, (done / total) * 100), eta: speed > 0 ? (remainingCurrent + pendingBytes) / speed : NaN };
  });

  function platformBadge(p: Platform | undefined): { text: string; cls: string; title: string } | null {
    if (!p || p === "unknown") return null;
    if (p === "linux")
      return hostOs === "windows"
        ? { text: "Linux build", cls: "warn", title: "Native Linux build — won't run on Windows; Steam will redownload it" }
        : { text: "Native", cls: "muted", title: "Native Linux build" };
    return hostOs === "windows"
      ? null
      : { text: "Proton", cls: "muted", title: "Windows build (runs through Proton)" };
  }

  async function connect() {
    if (!source.trim()) return;
    loading = true;
    error = "";
    try {
      const res = await api.listGames(source);
      steamapps = res.steamapps;
      games = res.games;
      store.set("source", source);
      for (const id of [...selected]) if (!games.some((g) => g.appid === id)) selected.delete(id);
      detectPlatforms();
    } catch (e) {
      error = String(e);
      games = [];
    } finally {
      loading = false;
    }
  }

  async function detectPlatforms() {
    const dirs: Record<string, string> = {};
    for (const g of games) if (!(g.appid in platforms)) dirs[g.appid] = g.installdir;
    if (!Object.keys(dirs).length) return;
    try {
      platforms = { ...platforms, ...(await api.detectPlatforms(steamapps, dirs)) };
    } catch {}
  }

  async function browse() {
    const dir = await open({ directory: true, title: "Choose the shared Steam folder" });
    if (typeof dir === "string") {
      source = dir;
      connect();
    }
  }

  async function loadLibraries() {
    libs = await api.localLibraries();
    if (!libs.some((l) => l.path === target)) target = libs[0]?.path ?? "";
  }

  function toggleAll() {
    if (allVisibleSelected) visible.forEach((g) => selected.delete(g.appid));
    else visible.forEach((g) => selected.add(g.appid));
  }

  async function startCopy() {
    if (!selectedGames.length || !target) return;
    store.set("target", target);
    queue = selectedGames.map((g) => ({ appid: g.appid, name: g.name, size: g.sizeOnDisk, status: "pending", message: "" }));
    progress = null;
    finishedSummary = "";
    speed = 0;
    lastSample = null;
    copying = true;
    try {
      await api.startCopy(steamapps, target, queue.map((q) => q.appid));
    } catch (e) {
      copying = false;
      error = String(e);
    }
  }

  function onEvent(e: CopyEvent) {
    if (e.kind === "progress") {
      progress = e;
      const item = queue.find((q) => q.appid === e.appid);
      if (item && item.status === "pending") item.status = "active";
      const now = performance.now();
      if (e.phase === "copying") {
        if (lastSample && lastSample.appid === e.appid && now - lastSample.t > 0) {
          const inst = ((e.bytesDone - lastSample.bytes) / (now - lastSample.t)) * 1000;
          speed = speed ? speed * 0.85 + inst * 0.15 : inst;
        }
        lastSample = { t: now, bytes: e.bytesDone, appid: e.appid };
      }
    } else if (e.kind === "gameDone") {
      const item = queue.find((q) => q.appid === e.appid);
      if (item) {
        item.status = e.ok ? "done" : e.message === "Cancelled" ? "cancelled" : "failed";
        item.message = e.message;
      }
      if (e.ok) selected.delete(e.appid);
    } else {
      copying = false;
      progress = null;
      for (const q of queue) if (q.status === "pending") q.status = "cancelled";
      finishedSummary = e.cancelled
        ? `Cancelled — ${e.succeeded} copied`
        : `${e.succeeded} copied${e.failed ? `, ${e.failed} failed` : ""}. Restart Steam to see them.`;
      loadLibraries();
      if (source) connect();
    }
  }

  onMount(() => {
    const unlisten = api.onCopyEvent(onEvent);
    api.hostOs().then((os) => (hostOs = os));
    loadLibraries();
    if (source) connect();
    const poll = () => api.steamRunning().then((r) => (steamRunning = r));
    poll();
    const timer = setInterval(poll, 4000);
    return () => {
      clearInterval(timer);
      unlisten.then((f) => f());
    };
  });
</script>

<div class="app">
  <header>
    <div class="brand">
      <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"><path fill="currentColor" d="M4 5h16a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-6v2h3v2H7v-2h3v-2H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Zm1 2v7h14V7H5Z"/></svg>
      <h1>Steam Sharing</h1>
    </div>
    <div class="pill" class:on={steamRunning} title={steamRunning ? "Steam only picks up new games when it starts. Quit Steam before copying, or restart it afterwards." : "Steam is not running"}>
      <span class="dot"></span>{steamRunning ? "Steam is running — restart it after copying" : "Steam not running"}
    </div>
  </header>

  <section class="source">
    <label for="src">Shared library</label>
    <div class="row">
      <input id="src" bind:value={source} placeholder={placeholder} onkeydown={(e) => e.key === "Enter" && connect()} spellcheck="false" />
      <button class="ghost" onclick={browse}>Browse…</button>
      <button class="primary" onclick={connect} disabled={loading || !source.trim()}>{loading ? "Scanning…" : games.length ? "Refresh" : "Connect"}</button>
    </div>
    {#if error}<p class="error">{error}</p>{/if}
    {#if steamapps && !error}<p class="hint">{games.length} games in <code>{steamapps}</code></p>{/if}
  </section>

  {#if games.length}
    <section class="toolbar">
      <input class="search" type="search" bind:value={search} placeholder="Search games…" />
      <div class="seg">
        <button class:active={filter === "all"} onclick={() => (filter = "all")}>All</button>
        <button class:active={filter === "missing"} onclick={() => (filter = "missing")}>Not installed</button>
        <button class:active={filter === "outdated"} onclick={() => (filter = "outdated")}>Outdated</button>
      </div>
      <select bind:value={sort} aria-label="Sort">
        <option value="name">Name</option>
        <option value="size">Size</option>
      </select>
      <label class="check"><input type="checkbox" bind:checked={hideTools} /> Hide tools</label>
    </section>

    <div class="list" role="list">
      <div class="list-head">
        <label class="check"><input type="checkbox" checked={allVisibleSelected} onchange={toggleAll} /> {visible.length} shown</label>
        <span>Size</span>
      </div>
      {#each visible as g (g.appid)}
        {@const st = status(g)}
        {@const pb = platformBadge(platforms[g.appid])}
        <label class="game" class:sel={selected.has(g.appid)} role="listitem">
          <input
            type="checkbox"
            checked={selected.has(g.appid)}
            onchange={(e) => (e.currentTarget.checked ? selected.add(g.appid) : selected.delete(g.appid))}
          />
          {#if imgFailed.has(g.appid)}
            <div class="thumb placeholder" aria-hidden="true">{g.name.replace(/[^\p{L}\p{N}]/gu, "").slice(0, 2).toUpperCase()}</div>
          {:else}
            <img
              class="thumb"
              src={imgCdn.has(g.appid) ? cdnImageUrl(g.appid) : localImageUrl(g.appid)}
              alt=""
              loading="lazy"
              onerror={() => (imgCdn.has(g.appid) ? imgFailed.add(g.appid) : imgCdn.add(g.appid))}
            />
          {/if}
          <div class="meta">
            <div class="name">{g.name}</div>
            <div class="badges">
              {#if st === "installed"}<span class="badge ok">Installed</span>{/if}
              {#if st === "outdated"}<span class="badge info" title="Local build {g.local?.buildid}, share has {g.buildid}">Update available</span>{/if}
              {#if !g.complete}<span class="badge warn" title="Not fully installed/updated on the share">Incomplete on share</span>{/if}
              {#if pb}<span class="badge {pb.cls}" title={pb.title}>{pb.text}</span>{/if}
              {#if g.isTool}<span class="badge muted">Tool</span>{/if}
            </div>
          </div>
          <div class="size">{formatBytes(g.sizeOnDisk)}</div>
        </label>
      {:else}
        <p class="empty">No games match.</p>
      {/each}
    </div>
  {:else if !loading}
    <div class="welcome">
      <p>Point this at the Steam folder shared from your other PC.</p>
      <p class="hint">{hostOs === "windows" ? "e.g. \\\\MY-PC\\SteamShare" : "Mount the SMB share first (e.g. via your file manager or /etc/fstab), then pick the mounted folder."}</p>
    </div>
  {/if}

  {#if queue.length}
    <section class="job">
      <div class="job-head">
        <strong>{copying ? (progress ? `Copying ${progress.gameIndex + 1} of ${progress.gameCount}` : "Starting…") : finishedSummary}</strong>
        {#if copying}
          <span class="hint">{formatBytes(speed)}/s · {formatDuration(overall.eta)} left</span>
          <button class="danger" onclick={() => api.cancelCopy()}>Cancel</button>
        {:else}
          <button class="ghost" onclick={() => (queue = [])}>Dismiss</button>
        {/if}
      </div>
      {#if copying}
        <div class="bar"><div style="width:{overall.pct}%"></div></div>
        {#if progress}
          <div class="current">
            <span>{progress.name}</span>
            <span class="hint">
              {#if progress.phase === "scanning"}Scanning files… {progress.filesTotal}
              {:else if progress.phase === "finishing"}Writing manifest
              {:else}{formatBytes(progress.bytesDone)} / {formatBytes(progress.bytesTotal)} · {progress.filesDone}/{progress.filesTotal} files{progress.skippedFiles ? ` (${progress.skippedFiles} unchanged)` : ""}{/if}
            </span>
          </div>
          <div class="bar thin"><div style="width:{progress.bytesTotal ? (progress.bytesDone / progress.bytesTotal) * 100 : 0}%"></div></div>
          <div class="file" title={progress.currentFile}>{progress.currentFile}</div>
        {/if}
      {/if}
      <ul class="queue">
        {#each queue as q (q.appid)}
          <li class={q.status}>
            <span class="qs">{({ pending: "•", active: "›", done: "✓", failed: "✕", cancelled: "–" })[q.status]}</span>
            <span class="qn">{q.name}</span>
            <span class="hint">{q.message || formatBytes(q.size)}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <footer>
    <div class="dest">
      <label for="target">Copy to</label>
      <select id="target" bind:value={target} onchange={() => store.set("target", target)}>
        {#each libs as l (l.path)}
          <option value={l.path}>{l.label || l.path} — {formatBytes(l.free_bytes)} free</option>
        {:else}
          <option value="">No local Steam library found</option>
        {/each}
      </select>
    </div>
    <div class="summary" class:bad={notEnoughSpace}>
      {selectedGames.length} selected · {formatBytes(selectedBytes)}
      {#if notEnoughSpace}<span> — not enough space</span>{/if}
    </div>
    <button class="primary big" onclick={startCopy} disabled={copying || !selectedGames.length || !target || notEnoughSpace}>
      {copying ? "Copying…" : `Copy ${selectedGames.length || ""} game${selectedGames.length === 1 ? "" : "s"}`}
    </button>
  </footer>
</div>

<style>
  :global(:root) {
    --bg: #0f141b;
    --panel: #161d27;
    --panel-2: #1c2531;
    --line: #263142;
    --text: #e6edf5;
    --dim: #8b98a9;
    --accent: #4fa3f7;
    --accent-2: #2f86e0;
    --ok: #4cc38a;
    --warn: #f0b452;
    --bad: #f06a6a;
    color-scheme: dark;
  }
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: var(--bg);
    color: var(--text);
    font: 14px/1.4 Inter, "Segoe UI", system-ui, -apple-system, sans-serif;
  }
  :global(*) { box-sizing: border-box; }

  .app { display: flex; flex-direction: column; height: 100vh; }

  header { display: flex; align-items: center; justify-content: space-between; padding: 14px 20px; border-bottom: 1px solid var(--line); }
  .brand { display: flex; align-items: center; gap: 10px; color: var(--accent); }
  h1 { font-size: 16px; margin: 0; color: var(--text); font-weight: 600; letter-spacing: 0.2px; }
  .pill { display: flex; align-items: center; gap: 8px; font-size: 12px; color: var(--dim); background: var(--panel); border: 1px solid var(--line); padding: 4px 10px; border-radius: 999px; }
  .pill .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--dim); }
  .pill.on { color: var(--warn); border-color: color-mix(in srgb, var(--warn) 40%, transparent); }
  .pill.on .dot { background: var(--warn); }

  section.source { padding: 16px 20px 10px; }
  .source label { display: block; font-size: 12px; color: var(--dim); margin-bottom: 6px; text-transform: uppercase; letter-spacing: 0.6px; }
  .row { display: flex; gap: 8px; }
  .row input { flex: 1; }

  input, select, button { font: inherit; color: inherit; }
  input:not([type="checkbox"]), select {
    background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; outline: none;
  }
  input:not([type="checkbox"]):focus, select:focus { border-color: var(--accent); }
  input[type="checkbox"] { accent-color: var(--accent); width: 16px; height: 16px; margin: 0; }
  button { border: 1px solid var(--line); background: var(--panel-2); border-radius: 8px; padding: 8px 14px; cursor: pointer; }
  button:hover:not(:disabled) { border-color: var(--accent); }
  button:disabled { opacity: 0.45; cursor: default; }
  button.primary { background: var(--accent-2); border-color: var(--accent-2); color: white; font-weight: 600; }
  button.primary:hover:not(:disabled) { background: var(--accent); }
  button.big { padding: 10px 22px; }
  button.danger { color: var(--bad); }
  button.ghost { background: transparent; }

  .error { color: var(--bad); margin: 8px 0 0; }
  .hint { color: var(--dim); font-size: 12px; margin: 6px 0 0; }
  code { font-family: ui-monospace, "Cascadia Code", monospace; font-size: 12px; }

  .toolbar { display: flex; gap: 10px; padding: 6px 20px 12px; align-items: center; flex-wrap: wrap; }
  .search { flex: 1; min-width: 180px; }
  .seg { display: flex; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 2px; }
  .seg button { border: none; background: transparent; padding: 6px 12px; border-radius: 6px; color: var(--dim); }
  .seg button.active { background: var(--panel-2); color: var(--text); }
  .check { display: flex; align-items: center; gap: 8px; color: var(--dim); cursor: pointer; user-select: none; }

  .list { flex: 1; overflow-y: auto; margin: 0 20px; border: 1px solid var(--line); border-radius: 10px; background: var(--panel); }
  .list-head { position: sticky; top: 0; z-index: 1; display: flex; justify-content: space-between; padding: 8px 16px; background: var(--panel-2); border-bottom: 1px solid var(--line); font-size: 12px; color: var(--dim); }
  .game { display: grid; grid-template-columns: 16px 120px 1fr auto; gap: 14px; align-items: center; padding: 8px 16px; border-bottom: 1px solid var(--line); cursor: pointer; }
  .game:last-child { border-bottom: none; }
  .game:hover { background: var(--panel-2); }
  .game.sel { background: color-mix(in srgb, var(--accent) 10%, var(--panel)); }
  .thumb { width: 120px; height: 56px; object-fit: cover; border-radius: 4px; background: var(--panel-2); }
  .placeholder { display: grid; place-items: center; color: var(--dim); font-weight: 600; letter-spacing: 1px; border: 1px solid var(--line); }
  .meta { min-width: 0; }
  .name { font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .badges { display: flex; gap: 6px; margin-top: 4px; flex-wrap: wrap; }
  .badge { font-size: 11px; padding: 1px 7px; border-radius: 4px; border: 1px solid var(--line); color: var(--dim); }
  .badge.ok { color: var(--ok); border-color: color-mix(in srgb, var(--ok) 40%, transparent); }
  .badge.info { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 40%, transparent); }
  .badge.warn { color: var(--warn); border-color: color-mix(in srgb, var(--warn) 40%, transparent); }
  .size { color: var(--dim); font-variant-numeric: tabular-nums; }
  .empty { padding: 30px; text-align: center; color: var(--dim); }
  .welcome { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; color: var(--dim); text-align: center; padding: 20px; }

  .job { margin: 12px 20px 0; padding: 14px 16px; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; max-height: 40vh; display: flex; flex-direction: column; gap: 8px; }
  .job-head { display: flex; align-items: center; gap: 12px; }
  .job-head strong { flex: 1; }
  .job-head .hint { margin: 0; }
  .bar { height: 8px; background: var(--panel-2); border-radius: 99px; overflow: hidden; }
  .bar.thin { height: 4px; }
  .bar > div { height: 100%; background: linear-gradient(90deg, var(--accent-2), var(--accent)); transition: width 0.25s ease; }
  .current { display: flex; justify-content: space-between; gap: 10px; }
  .current .hint { margin: 0; font-variant-numeric: tabular-nums; }
  .file { font-family: ui-monospace, monospace; font-size: 11px; color: var(--dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .queue { list-style: none; margin: 0; padding: 0; overflow-y: auto; }
  .queue li { display: grid; grid-template-columns: 18px 1fr auto; gap: 8px; padding: 3px 0; font-size: 13px; }
  .queue .hint { margin: 0; }
  .queue li.done .qs { color: var(--ok); }
  .queue li.failed .qs, .queue li.failed .hint { color: var(--bad); }
  .queue li.active .qs { color: var(--accent); }
  .queue li.pending, .queue li.cancelled { color: var(--dim); }
  .qn { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

  footer { display: flex; align-items: center; gap: 16px; padding: 14px 20px; margin-top: 12px; border-top: 1px solid var(--line); background: var(--panel); }
  .dest { display: flex; align-items: center; gap: 10px; min-width: 0; flex: 1; }
  .dest label { color: var(--dim); white-space: nowrap; }
  .dest select { min-width: 0; max-width: 420px; flex: 1; }
  .summary { color: var(--dim); font-variant-numeric: tabular-nums; }
  .summary.bad { color: var(--bad); }
</style>
