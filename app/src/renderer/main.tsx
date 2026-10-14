import { render } from "preact";
import { useCallback, useEffect, useRef, useState } from "preact/hooks";
import type { AppInfo, AppSettings, HostState, LibraryEntry, OutputInfo, Project, RinkaApi } from "../shared/api";
import { BackIcon, CloseIcon, GearIcon, ImportIcon, PlusIcon, SearchIcon } from "./icons";
import { type Filter, Library, SORTS, type Sort, VIEW_MODES, type ViewMode } from "./library";
import { onPreviewStatus, previewStatus, startPreview } from "./preview";
import { api, errorMessage, outputPixels, readPref, resolution, writePref } from "./util";
import { Wallpaper } from "./wallpaper";
import { ImportView } from "./import";

declare global {
  interface Window {
    rinka: RinkaApi;
  }
}

function hslToHex(h: number, s: number, l: number): string {
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    const c = l - s * Math.min(l, 1 - l) * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(c * 255).toString(16).padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

function idFromHash(): string | null {
  return /^#\/w\/([0-9a-f-]{36})$/.exec(location.hash)?.[1] ?? null;
}

function App() {
  const [entries, setEntries] = useState<LibraryEntry[]>([]);
  const [host, setHost] = useState<HostState>({ state: "connecting" });
  const [outputs, setOutputs] = useState<OutputInfo[]>([]);
  const [openId, setOpenId] = useState<string | null>(idFromHash);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [importing, setImporting] = useState(false);
  const [newOpen, setNewOpen] = useState(false);
  const [dropping, setDropping] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [view, setView] = useState<ViewMode>(readPref("view", VIEW_MODES, "grid"));
  const [sort, setSort] = useState<Sort>(readPref("sort", SORTS, "recent"));
  const [filter, setFilter] = useState<Filter>("all");
  const toastTimer = useRef<number | undefined>(undefined);
  const dragDepth = useRef(0);

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 3800);
  }, []);

  const refreshOutputs = useCallback(async () => {
    setOutputs(await api.outputs().catch(() => []));
  }, []);

  useEffect(() => {
    void api.listProjects().then(setEntries);
    api.onLibraryChanged((next) =>
      // Keep in-flight edits of the open wallpaper; saves arrive shortly.
      setEntries((prev) => next.map((n) => ({ ...n, project: prev.find((p) => p.id === n.id)?.project ?? n.project }))),
    );
    void api.hostState().then(setHost);
    api.onHostState(setHost);
    void startPreview().catch(() => {});
  }, []);

  useEffect(() => {
    void refreshOutputs();
  }, [host, refreshOutputs]);

  // The open wallpaper lives in the URL hash, so reloads keep it.
  useEffect(() => {
    const hash = openId ? `#/w/${openId}` : "";
    if (location.hash !== hash) history.replaceState(null, "", hash || location.pathname);
  }, [openId]);

  async function addFiles(paths?: string[], kind?: "image" | "video"): Promise<void> {
    setNewOpen(false);
    try {
      const { imported, skipped } = await api.importFiles(paths, kind);
      if (imported.length) setEntries((prev) => [...prev, ...imported]);
      const parts = [];
      if (imported.length) parts.push(`Added ${imported.length} ${imported.length === 1 ? "wallpaper" : "wallpapers"}`);
      if (skipped.length) {
        const dupes = skipped.filter((s) => s.reason === "already in the library").length;
        parts.push(dupes === skipped.length ? `${dupes} already in your library` : `skipped ${skipped.length}: ${skipped[0]!.reason}`);
      }
      if (parts.length) showToast(parts.join(" · "));
      if (imported.length === 1) setOpenId(imported[0]!.id);
    } catch (e) {
      showToast(errorMessage(e));
    }
  }

  async function newColor(): Promise<void> {
    setNewOpen(false);
    const entry = await api.createColor("New color", hslToHex(Math.floor(Math.random() * 360), 0.42, 0.32));
    setEntries((prev) => [...prev, entry]);
    setOpenId(entry.id);
  }

  async function remove(id: string): Promise<void> {
    await api.deleteProject(id);
    setEntries((prev) => prev.filter((e) => e.id !== id));
    setOpenId(null);
    void refreshOutputs();
  }

  function onEdited(id: string, project: Project): void {
    setEntries((prev) => prev.map((e) => (e.id === id ? { ...e, project } : e)));
  }

  const open = entries.find((e) => e.id === openId) ?? null;
  const live = outputs.filter((o) => o.presenting).length;

  return (
    <div
      class={`app${open ? " is-stage" : ""}`}
      onDragEnter={(e) => {
        if (!e.dataTransfer?.types.includes("Files")) return;
        dragDepth.current++;
        setDropping(true);
      }}
      onDragLeave={() => {
        if (--dragDepth.current <= 0) setDropping(false);
      }}
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault();
        dragDepth.current = 0;
        setDropping(false);
        const paths = [...(e.dataTransfer?.files ?? [])].map((f) => api.pathForFile(f)).filter(Boolean);
        if (paths.length) void addFiles(paths);
      }}
    >
      <header class="topbar">
        <button
          type="button"
          class="wordmark"
          onClick={() => {
            setOpenId(null);
            setImporting(false);
          }}
          title="Library"
        >
          rinka
        </button>
        {open && (
          <button type="button" class="button glass" onClick={() => setOpenId(null)}>
            <BackIcon />
            Library
          </button>
        )}
        {!open && !importing && (
          <label class="search">
            <SearchIcon />
            <span class="sr-only">Search the library</span>
            <input id="search" type="search" placeholder="Search wallpapers" value={query} onInput={(e) => setQuery((e.target as HTMLInputElement).value)} />
          </label>
        )}
        <div class="topbar-actions">
          <div class="menu-anchor">
            <button type="button" class="button primary" aria-expanded={newOpen} aria-haspopup="menu" onClick={() => setNewOpen((v) => !v)}>
              <PlusIcon />
              New
            </button>
            {newOpen && (
              <div class="menu" role="menu" onMouseLeave={() => setNewOpen(false)}>
                <button type="button" role="menuitem" onClick={() => void addFiles(undefined, "image")}>
                  <span>Image</span>
                  <span class="menu-hint">PNG or JPEG from disk</span>
                </button>
                <button type="button" role="menuitem" onClick={() => void addFiles(undefined, "video")}>
                  <span>Video</span>
                  <span class="menu-hint">MP4, WebM, MKV or MOV, looped and muted</span>
                </button>
                <button type="button" role="menuitem" onClick={() => void newColor()}>
                  <span>Solid color</span>
                  <span class="menu-hint">One color across the display</span>
                </button>
              </div>
            )}
          </div>
          <button
            type="button"
            class="button"
            aria-pressed={importing && !open}
            title="Import wallpapers from Wallpaper Engine"
            onClick={() => {
              setOpenId(null);
              setImporting((v) => !v);
            }}
          >
            <ImportIcon />
            Import
          </button>
          <button type="button" class={`host-pill is-${host.state}`} onClick={() => setSettingsOpen(true)} title="Playback host">
            <span class="live-dot" />
            {host.state === "connected"
              ? live > 0
                ? `Live on ${live} ${live === 1 ? "display" : "displays"}`
                : "Host running"
              : host.state === "connecting"
                ? "Connecting…"
                : host.state === "stopped"
                  ? "Wallpapers off"
                  : "Host unavailable"}
          </button>
          <button type="button" class="icon-button" aria-label="Settings" title="Settings" onClick={() => setSettingsOpen(true)}>
            <GearIcon />
          </button>
        </div>
      </header>

      <main class="content">
        {open ? (
          <Wallpaper
            entry={open}
            entries={entries}
            outputs={outputs}
            host={host}
            onEdited={onEdited}
            onDelete={(id) => void remove(id)}
            onOutputsChanged={() => void refreshOutputs()}
            onToast={showToast}
          />
        ) : importing ? (
          <ImportView
            onImported={(entry) => setEntries((prev) => [...prev.filter((e) => e.id !== entry.id), entry])}
            onOpen={(id) => {
              setImporting(false);
              setOpenId(id);
            }}
            onToast={showToast}
          />
        ) : (
          <Library
            entries={entries}
            outputs={outputs}
            query={query}
            view={view}
            filter={filter}
            sort={sort}
            onView={(v) => {
              setView(v);
              writePref("view", v);
            }}
            onFilter={setFilter}
            onSort={(s) => {
              setSort(s);
              writePref("sort", s);
            }}
            onOpen={setOpenId}
            onAddImages={() => void addFiles(undefined, "image")}
          />
        )}
      </main>

      {settingsOpen && <Settings host={host} outputs={outputs} onClose={() => setSettingsOpen(false)} />}
      {dropping && (
        <div class="drop-overlay">
          <p>Drop images or videos to add them to your library</p>
        </div>
      )}
      {toast && (
        <div class="toast" role="status">
          {toast}
        </div>
      )}
    </div>
  );
}

function Settings({ host, outputs, onClose }: { host: HostState; outputs: OutputInfo[]; onClose(): void }) {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [preview, setPreview] = useState(previewStatus());
  useEffect(() => {
    void api.appInfo().then(setInfo);
    void api.appSettings().then(setSettings);
    return onPreviewStatus(setPreview);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div class="sheet-backdrop" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div class="sheet" role="dialog" aria-modal="true" aria-label="Settings">
        <div class="sheet-head">
          <h2>Settings</h2>
          <button type="button" class="icon-button" aria-label="Close settings" onClick={onClose}>
            <CloseIcon />
          </button>
        </div>

        <div class="sheet-body">
        <section class="sheet-section">
          <h3>Playback host</h3>
          <p class="muted">Draws your wallpapers. It keeps running when this window closes.</p>
          <dl class="facts">
            <dt>Status</dt>
            <dd>
              <span class={`host-state is-${host.state}`}>
                {host.state === "connected" ? "Running" : host.state === "connecting" ? "Connecting" : host.state === "stopped" ? "Stopped" : "Unavailable"}
              </span>
              {host.state === "unavailable" && <span class="muted"> · {host.reason}</span>}
            </dd>
            {host.state === "connected" && (
              <>
                <dt>Process</dt>
                <dd class="mono">
                  pid {host.pid} · v{host.hostVersion} · protocol {host.protocol}
                </dd>
                <dt>GPU</dt>
                <dd>{host.gpu}</dd>
                <dt>Compositor</dt>
                <dd>{host.compositor ?? "unknown"}</dd>
              </>
            )}
          </dl>
          {host.state === "connected" && (
            <label class="check">
              <input type="checkbox" checked={host.paused} onChange={(e) => void api.setPaused(e.currentTarget.checked)} />
              <span>
                Pause animation
                <span class="muted small">Wallpapers keep their current frame and use no GPU time.</span>
              </span>
            </label>
          )}
          {host.state === "connected" ? (
            <button type="button" class="button danger" onClick={() => void api.stopHost()}>
              Stop host and remove wallpapers
            </button>
          ) : (
            <button type="button" class="button primary" disabled={host.state === "connecting"} onClick={() => void api.startHost()}>
              Start host
            </button>
          )}
        </section>

        <section class="sheet-section">
          <h3>Displays</h3>
          {outputs.length === 0 ? (
            <p class="muted">Displays appear here while the host is running.</p>
          ) : (
            <table class="displays-table">
              <thead>
                <tr>
                  <th>Output</th>
                  <th>Pixels</th>
                  <th>Scale</th>
                  <th>Rotation</th>
                </tr>
              </thead>
              <tbody>
                {outputs.map((o) => (
                  <tr>
                    <td>
                      <span class="mono">{o.name}</span>
                      <span class="muted small">{o.description}</span>
                    </td>
                    <td class="mono">{resolution(outputPixels(o))}</td>
                    <td class="mono">{o.scale ?? "–"}</td>
                    <td>{o.transform === "Normal" ? "None" : (o.transform?.replace("_", "") ?? "–") + (o.transform && o.transform !== "Normal" ? "°" : "")}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section class="sheet-section">
          <h3>Preview</h3>
          <dl class="facts">
            <dt>Renderer</dt>
            <dd>{preview.ok ? `WebGPU · ${preview.adapter}` : (preview.error ?? "Starting")}</dd>
            {preview.ok && (
              <>
                <dt>Surface</dt>
                <dd class="mono">{preview.surfaceFormat}</dd>
                <dt>Engine</dt>
                <dd class="mono">{preview.engineVersion}</dd>
              </>
            )}
          </dl>
        </section>

        <section class="sheet-section">
          <h3>Tray</h3>
          <label class="check">
            <input
              type="checkbox"
              checked={settings?.trayIcon ?? true}
              disabled={!settings}
              onChange={(e) => void api.setAppSettings({ trayIcon: e.currentTarget.checked }).then(setSettings)}
            />
            <span>
              Show tray icon
              <span class="muted small">Closing this window keeps Rinka in the tray. Without it, closing quits the editor.</span>
            </span>
          </label>
        </section>

        <section class="sheet-section">
          <h3>Library</h3>
          <dl class="facts">
            <dt>Location</dt>
            <dd class="mono small break">{info?.libraryPath}</dd>
            <dt>Electron</dt>
            <dd class="mono">
              {info?.electron} · Chromium {info?.chrome}
            </dd>
          </dl>
          <button type="button" class="button" onClick={() => void api.revealLibrary()}>
            Open library folder
          </button>
        </section>
        </div>
      </div>
    </div>
  );
}

render(<App />, document.getElementById("app")!);
