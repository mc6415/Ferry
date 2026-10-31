<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { fade, fly } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { open } from "@tauri-apps/plugin-dialog";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
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
  const imgLoaded = new SvelteSet<string>();
  const imgSrc = (appid: string) => (imgCdn.has(appid) ? cdnImageUrl(appid) : localImageUrl(appid));

  let view = $state<"list" | "grid">(store.get("view") === "grid" ? "grid" : "list");
  function setView(v: "list" | "grid") {
    view = v;
    store.set("view", v);
  }

  // Most recently selected game's art, blurred behind the window.
  const featured = $derived.by(() => {
    const id = [...selected].at(-1);
    return id && !imgFailed.has(id) ? imgSrc(id) : null;
  });

  const appWindow = getCurrentWindow();
  let maximized = $state(false);

  let queue = $state<QueueItem[]>([]);
  let progress = $state<Progress | null>(null);
  let copying = $state(false);
  let finishedSummary = $state("");
  let finishedOk = $state(false);
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

  // Filter-chip counts, respecting search + hide-tools but not the status filter itself.
  const counts = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const c = { all: 0, missing: 0, outdated: 0 };
    for (const g of games) {
      if (hideTools && g.isTool) continue;
      if (q && !g.name.toLowerCase().includes(q) && g.appid !== q) continue;
      c.all++;
      const s = status(g);
      if (s !== "installed") c[s]++;
    }
    return c;
  });

  // Disk meter for the target library: what's used now, and what the selection would add.
  const disk = $derived.by(() => {
    const t = targetLib;
    if (!t?.total_bytes || t.free_bytes == null) return null;
    const used = t.total_bytes - t.free_bytes;
    return {
      used: (used / t.total_bytes) * 100,
      adding: (Math.min(selectedBytes, t.free_bytes) / t.total_bytes) * 100,
    };
  });

  const icons = {
    folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
    search: "M11 4a7 7 0 1 0 0 14a7 7 0 1 0 0-14M20 20l-4.3-4.3",
    refresh: "M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7",
    link: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1",
    download: "M12 4v11m-4.5-4.5L12 15l4.5-4.5M5 20h14",
    drive: "M3 13h18v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1zM5 13l2-7h10l2 7M7 16h.01M11 16h.01",
    x: "M6 6l12 12M18 6L6 18",
    check: "M5 12.5l4.5 4.5L19 7.5",
    alert: "M12 8v5M12 16.5h.01M10.3 3.9L2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
    list: "M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01",
    grid: "M4 4h6.5v6.5H4zM13.5 4H20v6.5h-6.5zM4 13.5h6.5V20H4zM13.5 13.5H20V20h-6.5z",
    minimize: "M6 12h12",
    maximize: "M6 6h12v12H6z",
    restore: "M8.5 9.5h9v9h-9zM6 15V6.5h9",
  } as const;

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
    finishedOk = false;
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
      finishedOk = !e.cancelled && e.failed === 0 && e.succeeded > 0;
      if (!e.cancelled) notify(e.failed ? "Copy finished with errors" : "Copy complete", finishedSummary);
      loadLibraries();
      if (source) connect();
    }
  }

  /** Desktop notification, only when the window isn't focused (you'll see the in-app result otherwise). */
  async function notify(title: string, body: string) {
    try {
      if (await appWindow.isFocused()) return;
      let ok = await isPermissionGranted();
      if (!ok) ok = (await requestPermission()) === "granted";
      if (ok) sendNotification({ title, body });
    } catch {}
  }

  /** Marks an image loaded even if it finished before the load listener was attached. */
  function trackLoaded(appid: string) {
    return (img: HTMLImageElement) => {
      if (img.complete && img.naturalWidth) untrack(() => imgLoaded.add(appid));
    };
  }

  onMount(() => {
    const unlisten = api.onCopyEvent(onEvent);
    const syncMax = () => appWindow.isMaximized().then((m) => (maximized = m)).catch(() => {});
    syncMax();
    const unResize = appWindow.onResized(syncMax);
    api.hostOs().then((os) => (hostOs = os));
    loadLibraries();
    if (source) connect();
    const poll = () => api.steamRunning().then((r) => (steamRunning = r));
    poll();
    const timer = setInterval(poll, 4000);
    return () => {
      clearInterval(timer);
      unlisten.then((f) => f());
      unResize.then((f) => f());
    };
  });
</script>

{#snippet icon(name: keyof typeof icons, size = 16)}
  <svg class="ic" viewBox="0 0 24 24" width={size} height={size} aria-hidden="true"><path d={icons[name]} /></svg>
{/snippet}

<div class="frame" class:max={maximized}>
<div class="aurora" aria-hidden="true"><span></span><span></span><span></span></div>
<div class="grain" aria-hidden="true"></div>
<div class="ambient" aria-hidden="true">
  {#key featured}
    {#if featured}<img src={featured} alt="" transition:fade={{ duration: 700 }} />{/if}
  {/key}
</div>

<div class="app">
  <header data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <img class="logo" src="/logo.svg" alt="" width="40" height="40" draggable="false" data-tauri-drag-region />
      <div data-tauri-drag-region>
        <h1 data-tauri-drag-region>Steam Sharing</h1>
        <div class="tagline" data-tauri-drag-region>Copy games from another PC on your network</div>
      </div>
    </div>
    <div class="spacer" data-tauri-drag-region></div>
    <div class="pill" class:on={steamRunning} title={steamRunning ? "Steam only picks up new games when it starts. Quit Steam before copying, or restart it afterwards." : "Steam is not running"}>
      <span class="dot"></span>{steamRunning ? "Steam is running — restart it after copying" : "Steam not running"}
    </div>
    <div class="winctl">
      <button title="Minimise" onclick={() => appWindow.minimize()}>{@render icon("minimize", 14)}</button>
      <button title={maximized ? "Restore" : "Maximise"} onclick={() => appWindow.toggleMaximize()}>{@render icon(maximized ? "restore" : "maximize", 13)}</button>
      <button class="close" title="Close" onclick={() => appWindow.close()}>{@render icon("x", 14)}</button>
    </div>
  </header>

  <section class="source">
    <label for="src">Shared library</label>
    <div class="row">
      <div class="field">
        {@render icon("folder")}
        <input id="src" bind:value={source} placeholder={placeholder} onkeydown={(e) => e.key === "Enter" && connect()} spellcheck="false" />
      </div>
      <button class="ghost" onclick={browse}>Browse…</button>
      <button class="primary" onclick={connect} disabled={loading || !source.trim()}>
        {#if loading}<span class="spinner"></span>Scanning…
        {:else if games.length}{@render icon("refresh")}Refresh
        {:else}{@render icon("link")}Connect{/if}
      </button>
    </div>
    {#if error}<p class="error">{@render icon("alert", 14)}{error}</p>{/if}
    {#if steamapps && !error}<p class="hint"><strong>{games.length}</strong> games in <code>{steamapps}</code></p>{/if}
  </section>

  {#if games.length}
    <section class="toolbar">
      <div class="field search">
        {@render icon("search")}
        <input type="search" bind:value={search} placeholder="Search games…" />
      </div>
      <div class="seg">
        <button class:active={filter === "all"} onclick={() => (filter = "all")}>All <span class="count">{counts.all}</span></button>
        <button class:active={filter === "missing"} onclick={() => (filter = "missing")}>Not installed <span class="count">{counts.missing}</span></button>
        <button class:active={filter === "outdated"} onclick={() => (filter = "outdated")}>Outdated <span class="count">{counts.outdated}</span></button>
      </div>
      <select bind:value={sort} aria-label="Sort">
        <option value="name">Sort: Name</option>
        <option value="size">Sort: Size</option>
      </select>
      <label class="check"><input type="checkbox" bind:checked={hideTools} /> Hide tools</label>
      <div class="seg icons" role="group" aria-label="View">
        <button class:active={view === "list"} title="List view" onclick={() => setView("list")}>{@render icon("list")}</button>
        <button class:active={view === "grid"} title="Grid view" onclick={() => setView("grid")}>{@render icon("grid")}</button>
      </div>
    </section>

    <div class="list">
      <div class="list-head">
        <label class="check"><input type="checkbox" checked={allVisibleSelected} onchange={toggleAll} /> Select all <span class="dim">· {visible.length} shown</span></label>
        {#if view === "list"}<span>Size</span>{/if}
      </div>
      <div class="rows" class:grid={view === "grid"} role="list">
      {#each visible as g (g.appid)}
        {@const st = status(g)}
        {@const pb = platformBadge(platforms[g.appid])}
        <label
          class="game"
          class:sel={selected.has(g.appid)}
          role="listitem"
          animate:flip={{ duration: 220 }}
          in:fly={{ y: 8, duration: 200 }}
        >
          <input
            type="checkbox"
            checked={selected.has(g.appid)}
            onchange={(e) => (e.currentTarget.checked ? selected.add(g.appid) : selected.delete(g.appid))}
          />
          {#if imgFailed.has(g.appid)}
            <div class="thumb placeholder loaded" aria-hidden="true">{g.name.replace(/[^\p{L}\p{N}]/gu, "").slice(0, 2).toUpperCase()}</div>
          {:else}
            <div class="thumb" class:loaded={imgLoaded.has(g.appid)}>
              <img
                src={imgSrc(g.appid)}
                alt=""
                loading="lazy"
                {@attach trackLoaded(g.appid)}
                onload={() => imgLoaded.add(g.appid)}
                onerror={() => (imgCdn.has(g.appid) ? imgFailed.add(g.appid) : imgCdn.add(g.appid))}
              />
            </div>
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
        <div class="empty">
          {@render icon("search", 28)}
          <p>No games match{search ? ` “${search}”` : ""}.</p>
        </div>
      {/each}
      </div>
    </div>
  {:else if loading}
    <div class="welcome">
      <span class="spinner big"></span>
      <p class="hint">Scanning the shared library…</p>
    </div>
  {:else}
    <div class="welcome">
      <div class="hero">{@render icon("folder", 36)}</div>
      <h2>Connect to a shared library</h2>
      <p>Point this at the Steam folder shared from your other PC.</p>
      <p class="hint">{hostOs === "windows" ? "e.g. \\\\MY-PC\\SteamShare" : "Mount the SMB share first (e.g. via your file manager or /etc/fstab), then pick the mounted folder."}</p>
      <button class="primary" onclick={browse}>{@render icon("folder")}Choose folder…</button>
    </div>
  {/if}

  {#if queue.length}
    <section class="job" class:running={copying}>
      <div class="job-head">
        {#if finishedOk}
          <span class="done-mark" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="22" height="22"><circle cx="12" cy="12" r="10" /><path d="M7.5 12.5l3 3 6-6.5" /></svg>
            {#each Array(8) as _, i (i)}<i style="--a:{i * 45}deg"></i>{/each}
          </span>
        {/if}
        <strong>{copying ? (progress ? `Copying ${progress.gameIndex + 1} of ${progress.gameCount}` : "Starting…") : finishedSummary}</strong>
        {#if copying}
          <span class="stat">{formatBytes(speed)}/s</span>
          <span class="stat">{formatDuration(overall.eta)} left</span>
          <button class="danger" onclick={() => api.cancelCopy()}>{@render icon("x", 14)}Cancel</button>
        {:else}
          <button class="ghost" onclick={() => (queue = [])}>Dismiss</button>
        {/if}
      </div>
      {#if copying}
        <div class="bar"><div style="width:{overall.pct}%"></div></div>
        {#if progress}
          <div class="current">
            <span class="cur-name">{progress.name}</span>
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
            <span class="qs">
              {#if q.status === "active"}<span class="spinner small"></span>
              {:else if q.status === "done"}{@render icon("check", 12)}
              {:else if q.status === "failed"}{@render icon("x", 12)}
              {/if}
            </span>
            <span class="qn">{q.name}</span>
            <span class="hint">{q.message || formatBytes(q.size)}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <footer>
    <div class="dest">
      <label for="target">{@render icon("drive")}Copy to</label>
      <div class="dest-inner">
        <select id="target" bind:value={target} onchange={() => store.set("target", target)}>
          {#each libs as l (l.path)}
            <option value={l.path}>{l.label || l.path} — {formatBytes(l.free_bytes)} free</option>
          {:else}
            <option value="">No local Steam library found</option>
          {/each}
        </select>
        {#if disk}
          <div class="disk" class:bad={notEnoughSpace} title="{formatBytes(targetLib?.free_bytes)} free of {formatBytes(targetLib?.total_bytes)}">
            <div class="used" style="width:{disk.used}%"></div>
            <div class="adding" style="width:{disk.adding}%"></div>
          </div>
        {/if}
      </div>
    </div>
    <div class="summary" class:bad={notEnoughSpace}>
      <div class="sum-main">{selectedGames.length} selected</div>
      <div class="sum-sub">{notEnoughSpace ? "Not enough space" : selectedGames.length ? formatBytes(selectedBytes) : "Pick games above"}</div>
    </div>
    <button class="primary big" onclick={startCopy} disabled={copying || !selectedGames.length || !target || notEnoughSpace}>
      {#if copying}<span class="spinner"></span>Copying…
      {:else}{@render icon("download")}Copy {selectedGames.length || ""} game{selectedGames.length === 1 ? "" : "s"}{/if}
    </button>
  </footer>
</div>
</div>

<style>
  :global(:root) {
    --bg: #0e0b17;
    --panel: #15111f;
    --panel-2: #1c1729;
    --panel-3: #251e35;
    --line: #2a2240;
    --line-strong: #3d3258;
    --text: #eeeaf7;
    --dim: #9a90b0;
    --faint: #6a6082;
    --accent: #a78bfa;
    --accent-2: #7c3aed;
    --pink: #ec4899;
    --glow: #c084fc;
    --grad: linear-gradient(120deg, #8b5cf6, #c026d3 55%, #ec4899);
    --grad-soft: linear-gradient(120deg, color-mix(in srgb, #8b5cf6 55%, transparent), color-mix(in srgb, var(--line) 80%, transparent) 40%, color-mix(in srgb, #ec4899 45%, transparent));
    --ok: #4cc38a;
    --warn: #f0b452;
    --bad: #f26d6d;
    --radius: 10px;
    --shadow: 0 1px 2px rgb(0 0 0 / 0.3), 0 8px 24px -12px rgb(0 0 0 / 0.5);
    /* No color-scheme on the root: WebKit would paint an opaque dark canvas and hide the rounded corners. */
    background: transparent;
  }
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: transparent;
    color: var(--text);
    font: 14px/1.45 Inter, "Segoe UI Variable", "Segoe UI", system-ui, -apple-system, sans-serif;
    -webkit-font-smoothing: antialiased;
  }
  :global(*) { box-sizing: border-box; }
  :global(::selection) { background: color-mix(in srgb, var(--accent) 35%, transparent); }
  :global(::-webkit-scrollbar) { width: 10px; height: 10px; }
  :global(::-webkit-scrollbar-track) { background: transparent; }
  :global(::-webkit-scrollbar-thumb) { background: var(--line); border-radius: 99px; border: 2px solid transparent; background-clip: padding-box; }
  :global(::-webkit-scrollbar-thumb:hover) { background-color: var(--line-strong); }

  .ambient { position: fixed; inset: 0; z-index: 1; overflow: hidden; pointer-events: none; }
  .ambient img {
    position: absolute; inset: -10%; width: 120%; height: 120%; object-fit: cover;
    filter: blur(60px) saturate(1.5); opacity: 0.3;
  }
  .ambient::after { content: ""; position: absolute; inset: 0; background: radial-gradient(ellipse at 50% 30%, transparent, color-mix(in srgb, var(--bg) 70%, transparent) 90%); }

  /* Rounded window: the OS window is transparent and this box draws the visible window. */
  .frame {
    position: fixed; inset: 0; overflow: hidden; contain: paint; color-scheme: dark;
    background: var(--bg); border-radius: 12px;
  }
  .frame::after {
    content: ""; position: absolute; inset: 0; z-index: 10; border-radius: inherit; pointer-events: none;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, #a78bfa 22%, rgb(255 255 255 / 0.06));
  }
  .frame.max { border-radius: 0; }
  .frame.max::after { display: none; }
  .app { position: relative; z-index: 2; display: flex; flex-direction: column; height: 100%; }

  /* Slowly drifting colour blobs behind everything */
  .aurora { position: fixed; inset: 0; z-index: 0; overflow: hidden; pointer-events: none; }
  .aurora span { position: absolute; border-radius: 50%; opacity: 0.55; will-change: transform; }
  .aurora span:nth-child(1) {
    width: 70vw; height: 70vw; left: -20vw; top: -35vw;
    background: radial-gradient(circle, color-mix(in srgb, #7c3aed 45%, transparent), transparent 65%);
    animation: drift1 26s ease-in-out infinite alternate;
  }
  .aurora span:nth-child(2) {
    width: 60vw; height: 60vw; right: -25vw; top: -10vw;
    background: radial-gradient(circle, color-mix(in srgb, #db2777 32%, transparent), transparent 65%);
    animation: drift2 32s ease-in-out infinite alternate;
  }
  .aurora span:nth-child(3) {
    width: 65vw; height: 65vw; left: 15vw; bottom: -45vw;
    background: radial-gradient(circle, color-mix(in srgb, #4f46e5 35%, transparent), transparent 65%);
    animation: drift3 38s ease-in-out infinite alternate;
  }
  /* Fine film grain so the gradients don't band */
  .grain {
    position: fixed; inset: 0; z-index: 1; pointer-events: none; opacity: 0.06; mix-blend-mode: overlay;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='.9' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
  }

  .ic { fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }

  /* Header */
  header { display: flex; align-items: center; gap: 16px; padding: 10px 10px 10px 24px; min-height: 62px; position: relative; user-select: none; -webkit-user-select: none; }
  header::after {
    content: ""; position: absolute; left: 0; right: 0; bottom: 0; height: 1px;
    background: linear-gradient(90deg, transparent, color-mix(in srgb, #8b5cf6 70%, transparent) 25%, color-mix(in srgb, #ec4899 60%, transparent) 70%, transparent);
  }
  .spacer { flex: 1; align-self: stretch; }
  .winctl { display: flex; gap: 2px; margin-left: 4px; }
  .winctl button { width: 34px; height: 30px; padding: 0; border: none; background: transparent; color: var(--dim); border-radius: 7px; }
  .winctl button:hover:not(:disabled) { background: var(--panel-3); color: var(--text); }
  .winctl button.close:hover:not(:disabled) { background: var(--bad); color: white; }
  .brand { display: flex; align-items: center; gap: 12px; }
  .logo { display: block; width: 40px; height: 40px; margin: -2px; filter: drop-shadow(0 4px 12px color-mix(in srgb, #c026d3 55%, transparent)); }
  h1 {
    font-size: 16px; margin: 0; font-weight: 700; letter-spacing: 0.1px; width: fit-content;
    background: linear-gradient(90deg, #fff, #e9d5ff 45%, #f9a8d4); -webkit-background-clip: text; background-clip: text; color: transparent;
  }
  .tagline { font-size: 12px; color: var(--dim); }
  .pill { display: flex; align-items: center; gap: 8px; font-size: 12px; color: var(--dim); background: var(--panel); border: 1px solid var(--line); padding: 5px 12px; border-radius: 999px; white-space: nowrap; }
  .pill .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--faint); }
  .pill.on { color: var(--warn); background: color-mix(in srgb, var(--warn) 8%, var(--panel)); border-color: color-mix(in srgb, var(--warn) 35%, transparent); }
  .pill.on .dot { background: var(--warn); box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 25%, transparent); animation: pulse 2s ease-in-out infinite; }

  /* Source */
  section.source { padding: 18px 24px 12px; }
  .source > label { display: block; font-size: 11px; font-weight: 600; color: var(--dim); margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.8px; }
  .row { display: flex; gap: 8px; }

  /* Controls */
  input, select, button { font: inherit; color: inherit; }
  .field { position: relative; display: flex; align-items: center; flex: 1; min-width: 0; }
  .field > .ic { position: absolute; left: 12px; color: var(--faint); pointer-events: none; transition: color 0.15s; }
  .field:focus-within > .ic { color: var(--accent); }
  .field > input:not([type="checkbox"]) { width: 100%; padding-left: 36px; }
  input:not([type="checkbox"]), select {
    background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 9px 12px; outline: none; height: 38px;
    transition: border-color 0.15s, box-shadow 0.15s, background-color 0.15s;
  }
  input::placeholder { color: var(--faint); }
  input:not([type="checkbox"]):hover, select:hover { border-color: var(--line-strong); }
  input:not([type="checkbox"]):focus, select:focus { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent); }
  input[type="search"]::-webkit-search-cancel-button { filter: grayscale(1) brightness(1.4); cursor: pointer; }
  select {
    appearance: none; padding-right: 34px; cursor: pointer;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%239a90b0' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M6 9l6 6 6-6'/%3E%3C/svg%3E");
    background-repeat: no-repeat; background-position: right 12px center;
  }

  input[type="checkbox"] {
    appearance: none; flex-shrink: 0; width: 18px; height: 18px; margin: 0; cursor: pointer;
    display: grid; place-items: center;
    border: 1.5px solid var(--line-strong); border-radius: 5px; background: var(--bg);
    transition: background-color 0.12s, border-color 0.12s, box-shadow 0.12s;
  }
  input[type="checkbox"]:hover { border-color: var(--accent); }
  input[type="checkbox"]:focus-visible { outline: none; box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 30%, transparent); }
  input[type="checkbox"]::after {
    content: ""; width: 4px; height: 9px; border: solid white; border-width: 0 2px 2px 0;
    transform: translateY(-1px) rotate(45deg) scale(0); transition: transform 0.12s ease;
  }
  input[type="checkbox"]:checked { background: var(--grad); border-color: transparent; box-shadow: 0 0 10px -2px color-mix(in srgb, #c026d3 70%, transparent); }
  input[type="checkbox"]:checked::after { transform: translateY(-1px) rotate(45deg) scale(1); }

  button {
    display: inline-flex; align-items: center; justify-content: center; gap: 7px; height: 38px; white-space: nowrap;
    border: 1px solid var(--line); background: var(--panel-2); border-radius: 8px; padding: 0 14px; cursor: pointer;
    transition: background-color 0.15s, border-color 0.15s, color 0.15s, transform 0.05s, box-shadow 0.15s;
  }
  button:hover:not(:disabled) { border-color: var(--line-strong); background: var(--panel-3); }
  button:active:not(:disabled) { transform: translateY(1px); }
  button:focus-visible { outline: none; box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 30%, transparent); }
  button:disabled { opacity: 0.45; cursor: default; }
  button.primary {
    position: relative; overflow: hidden;
    background: var(--grad); border-color: transparent; color: white; font-weight: 600;
    box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.25), 0 4px 18px -6px color-mix(in srgb, #c026d3 75%, transparent);
  }
  button.primary::after {
    content: ""; position: absolute; inset: 0; transform: translateX(-120%);
    background: linear-gradient(100deg, transparent 30%, rgb(255 255 255 / 0.28) 50%, transparent 70%);
  }
  button.primary:hover:not(:disabled) {
    background: var(--grad); border-color: transparent; filter: brightness(1.1);
    box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.3), 0 6px 26px -6px color-mix(in srgb, #c026d3 90%, transparent);
  }
  button.primary:hover:not(:disabled)::after { transform: translateX(120%); transition: transform 0.6s ease; }
  button.primary:disabled { filter: saturate(0.5); }
  button.big { height: 44px; padding: 0 22px; font-size: 14.5px; border-radius: 10px; }
  button.danger { color: var(--bad); }
  button.danger:hover:not(:disabled) { border-color: color-mix(in srgb, var(--bad) 50%, transparent); background: color-mix(in srgb, var(--bad) 10%, var(--panel-2)); }
  button.ghost { background: transparent; }

  .error { display: flex; align-items: center; gap: 8px; color: var(--bad); margin: 10px 0 0; padding: 8px 12px; border-radius: 8px; background: color-mix(in srgb, var(--bad) 10%, transparent); border: 1px solid color-mix(in srgb, var(--bad) 25%, transparent); font-size: 13px; }
  .hint { color: var(--dim); font-size: 12px; margin: 8px 0 0; }
  .hint strong { color: var(--text); font-weight: 600; }
  .dim { color: var(--faint); }
  code { font-family: ui-monospace, "Cascadia Code", "JetBrains Mono", monospace; font-size: 11.5px; background: var(--panel); border: 1px solid var(--line); padding: 1px 6px; border-radius: 5px; }

  /* Toolbar */
  .toolbar { display: flex; gap: 10px; padding: 4px 24px 14px; align-items: center; flex-wrap: wrap; }
  .search { min-width: 200px; }
  .seg { display: flex; gap: 2px; background: var(--panel); border: 1px solid var(--line); border-radius: 9px; padding: 3px; height: 38px; }
  .seg button { height: auto; border: none; background: transparent; padding: 0 12px; border-radius: 6px; color: var(--dim); font-size: 13px; }
  .seg button:hover:not(.active) { background: var(--panel-2); color: var(--text); }
  .seg button.active { background: var(--panel-3); color: var(--text); box-shadow: 0 1px 2px rgb(0 0 0 / 0.35); }
  .count { font-size: 11px; font-variant-numeric: tabular-nums; padding: 0 6px; border-radius: 99px; background: var(--panel-2); color: var(--dim); line-height: 17px; }
  .seg button.active .count { background: var(--grad); color: white; }
  .check { display: flex; align-items: center; gap: 9px; color: var(--dim); cursor: pointer; user-select: none; font-size: 13px; }
  .check:hover { color: var(--text); }

  /* Game list */
  .list { flex: 1; min-height: 0; display: flex; flex-direction: column; overflow: hidden; margin: 0 24px; border-radius: var(--radius); background: linear-gradient(color-mix(in srgb, var(--panel) 78%, transparent), color-mix(in srgb, var(--panel) 78%, transparent)) padding-box, var(--grad-soft) border-box; border: 1px solid transparent; box-shadow: var(--shadow), 0 0 60px -30px color-mix(in srgb, #8b5cf6 60%, transparent); }
  .rows { flex: 1; min-height: 0; overflow-y: auto; scrollbar-gutter: stable; }
  .list-head {
    flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; padding: 10px 28px 10px 18px;
    background: color-mix(in srgb, var(--panel-2) 85%, transparent);
    border-bottom: 1px solid var(--line); font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.6px; color: var(--dim);
  }
  .list-head .check { font-size: 11px; }
  .game {
    position: relative; display: grid; grid-template-columns: 18px 128px 1fr auto; gap: 16px; align-items: center;
    padding: 10px 18px; border-bottom: 1px solid color-mix(in srgb, var(--line) 70%, transparent); cursor: pointer;
    transition: background-color 0.12s;
  }
  .game:last-child { border-bottom: none; }
  .rows::-webkit-scrollbar-thumb { border-width: 3px; }
  .game:hover { background: var(--panel-2); }
  .game:hover { background: color-mix(in srgb, var(--panel-3) 60%, transparent); }
  .game.sel { background: linear-gradient(90deg, color-mix(in srgb, #8b5cf6 16%, transparent), color-mix(in srgb, #ec4899 5%, transparent)); }
  .game.sel:hover { background: linear-gradient(90deg, color-mix(in srgb, #8b5cf6 22%, transparent), color-mix(in srgb, #ec4899 8%, transparent)); }
  .game.sel::before { content: ""; position: absolute; left: 0; top: 8px; bottom: 8px; width: 3px; border-radius: 0 3px 3px 0; background: linear-gradient(180deg, #a78bfa, #ec4899); box-shadow: 0 0 12px color-mix(in srgb, #c026d3 80%, transparent); }
  .thumb {
    position: relative; width: 128px; aspect-ratio: 460 / 215; border-radius: 6px; overflow: hidden;
    background: linear-gradient(100deg, var(--panel-2) 30%, var(--panel-3) 50%, var(--panel-2) 70%) 0 0 / 300% 100%;
    animation: shimmer 1.4s ease-in-out infinite;
    box-shadow: 0 2px 8px -2px rgb(0 0 0 / 0.6), inset 0 0 0 1px rgb(255 255 255 / 0.05);
  }
  .thumb.loaded { animation: none; background: var(--panel-2); }
  .thumb img { display: block; width: 100%; height: 100%; object-fit: cover; opacity: 0; transition: opacity 0.35s ease, transform 0.25s ease; }
  .thumb.loaded img { opacity: 1; }
  .game:hover .thumb img { transform: scale(1.05); }
  .placeholder {
    display: grid; place-items: center; color: var(--dim); font-weight: 700; letter-spacing: 1.5px; font-size: 15px;
    background: linear-gradient(135deg, var(--panel-3), var(--panel-2));
  }

  /* Grid view */
  .rows.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(210px, 1fr)); gap: 14px; padding: 16px 8px 16px 16px; align-content: start; }
  .rows.grid .game {
    grid-template-columns: 1fr auto; grid-template-areas: "thumb thumb" "meta size"; gap: 0 10px; align-items: start;
    padding: 0 0 12px; border: 1px solid var(--line); border-radius: 10px; overflow: hidden;
    background: color-mix(in srgb, var(--panel-2) 70%, transparent);
    transition: background-color 0.12s, border-color 0.15s, box-shadow 0.15s, transform 0.15s;
  }
  .rows.grid .game:hover { border-color: var(--line-strong); transform: translateY(-2px); box-shadow: 0 10px 24px -12px rgb(0 0 0 / 0.7); }
  .rows.grid .game.sel {
    background: linear-gradient(color-mix(in srgb, var(--panel-2) 92%, transparent), color-mix(in srgb, var(--panel-2) 92%, transparent)) padding-box, var(--grad) border-box;
    border-color: transparent; box-shadow: 0 12px 32px -12px color-mix(in srgb, #c026d3 75%, transparent);
  }
  .rows.grid .game.sel::before { display: none; }
  .rows.grid .game > input { position: absolute; top: 10px; left: 10px; z-index: 1; background: color-mix(in srgb, var(--bg) 70%, transparent); border-color: rgb(255 255 255 / 0.5); }
  .rows.grid .game > input:checked { background: var(--grad); border-color: transparent; }
  .rows.grid .thumb { grid-area: thumb; width: 100%; border-radius: 0; box-shadow: none; margin-bottom: 10px; }
  .rows.grid .meta { grid-area: meta; padding-left: 12px; }
  .rows.grid .name { font-size: 13.5px; }
  .rows.grid .size { grid-area: size; padding-right: 12px; font-size: 12px; line-height: 20px; }
  .rows.grid .empty { grid-column: 1 / -1; }

  .meta { min-width: 0; }
  .name { font-weight: 550; font-size: 14.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .badges { display: flex; gap: 6px; margin-top: 5px; flex-wrap: wrap; }
  .badge {
    display: inline-flex; align-items: center; gap: 5px; font-size: 11px; font-weight: 500; padding: 1px 8px; border-radius: 99px;
    color: var(--dim); background: var(--panel-2); border: 1px solid var(--line);
  }
  .badge.ok, .badge.info, .badge.warn { --c: var(--ok); color: var(--c); background: color-mix(in srgb, var(--c) 12%, transparent); border-color: color-mix(in srgb, var(--c) 28%, transparent); }
  .badge.info { --c: var(--accent); }
  .badge.warn { --c: var(--warn); }
  .badge.ok::before, .badge.info::before, .badge.warn::before { content: ""; width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
  .size { color: var(--dim); font-variant-numeric: tabular-nums; font-size: 13px; }
  .game.sel .size { color: var(--text); }
  .empty { display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 48px 20px; color: var(--faint); }
  .empty p { margin: 0; color: var(--dim); }

  /* Empty / loading state */
  .welcome { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; padding: 24px; gap: 2px; }
  .welcome h2 { margin: 14px 0 4px; font-size: 18px; font-weight: 650; }
  .welcome p { margin: 0; color: var(--dim); max-width: 440px; }
  .welcome .hint { margin: 6px 0 18px; }
  .hero {
    width: 72px; height: 72px; border-radius: 20px; display: grid; place-items: center; color: var(--accent);
    background: linear-gradient(135deg, color-mix(in srgb, var(--accent) 18%, var(--panel)), var(--panel));
    border: 1px solid color-mix(in srgb, var(--accent) 25%, var(--line));
    box-shadow: 0 10px 50px -8px color-mix(in srgb, #c026d3 55%, transparent);
  }

  /* Copy job */
  .job {
    margin: 14px 24px 0; padding: 16px 18px; border-radius: var(--radius); background: linear-gradient(color-mix(in srgb, var(--panel) 88%, transparent), color-mix(in srgb, var(--panel) 88%, transparent)) padding-box, var(--grad-soft) border-box; border: 1px solid transparent;
    max-height: 40vh; display: flex; flex-direction: column; gap: 10px; box-shadow: var(--shadow); animation: rise 0.2s ease;
  }
  .job.running { box-shadow: var(--shadow), 0 0 40px -16px color-mix(in srgb, #c026d3 70%, transparent); }
  .job-head { display: flex; align-items: center; gap: 10px; }
  .job-head strong { flex: 1; font-weight: 600; }
  .job-head button { height: 32px; font-size: 13px; }
  .stat { font-size: 12px; color: var(--dim); font-variant-numeric: tabular-nums; background: var(--panel-2); border: 1px solid var(--line); padding: 2px 9px; border-radius: 99px; }
  .bar { height: 8px; background: var(--panel-3); border-radius: 99px; overflow: hidden; }
  .bar.thin { height: 4px; }
  .bar > div {
    height: 100%; border-radius: inherit; transition: width 0.3s ease;
    background: linear-gradient(90deg, #8b5cf6, #ec4899, #8b5cf6) 0 0 / 200% 100%;
    box-shadow: 0 0 12px color-mix(in srgb, #d946ef 70%, transparent);
    animation: flow 2s linear infinite;
  }
  .current { display: flex; justify-content: space-between; gap: 12px; font-size: 13px; }
  .cur-name { font-weight: 550; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .current .hint { margin: 0; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .file { font-family: ui-monospace, "Cascadia Code", monospace; font-size: 11px; color: var(--faint); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .queue { list-style: none; margin: 0; padding: 8px 0 0; overflow-y: auto; border-top: 1px solid var(--line); }
  .queue li { display: grid; grid-template-columns: 18px 1fr auto; gap: 10px; align-items: center; padding: 4px 0; font-size: 13px; }
  .queue .hint { margin: 0; }
  .qs { width: 18px; height: 18px; border-radius: 50%; display: grid; place-items: center; border: 1.5px solid var(--line-strong); }
  .queue li.active .qs { border: none; }
  .queue li.done .qs { background: var(--ok); border-color: var(--ok); color: var(--bg); }
  .queue li.done .qs .ic, .queue li.failed .qs .ic { stroke-width: 3.5; }
  .queue li.failed .qs { background: var(--bad); border-color: var(--bad); color: var(--bg); }
  .queue li.failed .hint { color: var(--bad); }
  .queue li.cancelled .qs { border-style: dashed; }
  .queue li.pending, .queue li.cancelled { color: var(--dim); }
  .qn { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

  /* Footer */
  footer {
    display: flex; align-items: center; gap: 20px; padding: 14px 24px; margin-top: 14px;
    border-top: 1px solid var(--line); background: color-mix(in srgb, var(--panel) 70%, transparent);
    backdrop-filter: blur(18px) saturate(1.4); -webkit-backdrop-filter: blur(18px) saturate(1.4);
    box-shadow: 0 -8px 24px -16px rgb(0 0 0 / 0.6);
  }
  .dest { display: flex; align-items: center; gap: 12px; min-width: 0; flex: 1; }
  .dest label { display: flex; align-items: center; gap: 7px; color: var(--dim); white-space: nowrap; font-size: 13px; }
  .dest-inner { display: flex; flex-direction: column; gap: 6px; min-width: 0; max-width: 440px; flex: 1; }
  .dest select { width: 100%; }
  .disk { position: relative; display: flex; height: 4px; border-radius: 99px; background: var(--panel-3); overflow: hidden; }
  .disk .used { background: var(--line-strong); }
  .disk .adding { background: var(--grad); transition: width 0.25s ease; }
  .disk.bad .adding { background: var(--bad); }
  .summary { text-align: right; font-variant-numeric: tabular-nums; line-height: 1.3; }
  .sum-main { font-weight: 600; }
  .sum-sub { font-size: 12px; color: var(--dim); }
  .summary.bad .sum-sub { color: var(--bad); font-weight: 500; }

  .done-mark { position: relative; display: grid; place-items: center; color: var(--ok); }
  .done-mark svg { fill: none; stroke: currentColor; stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round; }
  .done-mark circle { stroke-dasharray: 63; stroke-dashoffset: 63; animation: draw 0.5s ease-out forwards; }
  .done-mark path { stroke-dasharray: 16; stroke-dashoffset: 16; animation: draw 0.3s 0.4s ease-out forwards; }
  .done-mark i {
    position: absolute; width: 4px; height: 4px; border-radius: 50%; opacity: 0;
    background: var(--accent); animation: burst 0.6s 0.5s ease-out;
  }
  .done-mark i:nth-child(odd) { background: var(--ok); }

  /* Spinners & motion */
  .spinner {
    width: 14px; height: 14px; border-radius: 50%; display: inline-block; flex-shrink: 0;
    border: 2px solid currentColor; border-right-color: transparent; animation: spin 0.7s linear infinite;
  }
  .spinner.small { width: 14px; height: 14px; color: var(--accent); }
  .spinner.big { width: 28px; height: 28px; border-width: 3px; color: var(--accent); margin-bottom: 6px; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @keyframes flow { to { background-position: -200% 0; } }
  @keyframes pulse { 50% { box-shadow: 0 0 0 5px color-mix(in srgb, var(--warn) 5%, transparent); } }
  @keyframes drift1 { to { transform: translate(18vw, 12vh) scale(1.15); } }
  @keyframes drift2 { to { transform: translate(-16vw, 18vh) scale(0.9); } }
  @keyframes drift3 { to { transform: translate(-10vw, -14vh) scale(1.1); } }
  @keyframes shimmer { from { background-position: 100% 0; } to { background-position: -50% 0; } }
  @keyframes draw { to { stroke-dashoffset: 0; } }
  @keyframes burst {
    from { opacity: 1; transform: rotate(var(--a)) translateY(0) scale(1); }
    to { opacity: 0; transform: rotate(var(--a)) translateY(-20px) scale(0.4); }
  }
  @keyframes rise { from { opacity: 0; transform: translateY(6px); } }
  @media (prefers-reduced-motion: reduce) {
    * { animation: none !important; transition: none !important; }
    .done-mark circle, .done-mark path { stroke-dashoffset: 0; }
  }
</style>
