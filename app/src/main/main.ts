// Electron main process: window, library, thumbnails, host connection and IPC.

import { app, BrowserWindow, dialog, ipcMain, Menu, net, protocol, shell } from "electron";
import { randomUUID } from "node:crypto";
import { createReadStream, existsSync, renameSync, statSync, writeFileSync } from "node:fs";
import { Readable } from "node:stream";
import path from "node:path";
import { pathToFileURL } from "node:url";
import type { HostState, ImportResult, LibraryEntry, OutputInfo, PreviewReport, Project } from "../shared/api";
import { connectOrSpawn, defaultSocketPath, findHostBinary, type HostConnection } from "./host";
import { ConfigStore, IMPORT_EXTENSIONS, Library, type StoredEntry } from "./library";
import { Thumbnails } from "./thumbnails";
import { Workshop } from "./workshop";

const SMOKE = process.env.RINKA_SMOKE === "1";

// Data lives in ~/.config/rinka. Libraries from before the rename
// (~/.config/wpe-app) move there on first start.
{
  const userData = process.env.RINKA_USER_DATA ?? path.join(app.getPath("appData"), "rinka");
  const legacy = path.join(app.getPath("appData"), "wpe-app");
  if (!process.env.RINKA_USER_DATA && !existsSync(userData) && existsSync(legacy)) {
    try {
      renameSync(legacy, userData);
    } catch (e) {
      console.warn("could not move the old library:", (e as Error).message);
    }
  }
  app.setPath("userData", userData);
}

protocol.registerSchemesAsPrivileged([
  { scheme: "rinka", privileges: { standard: true, secure: true, supportFetchAPI: true } },
]);
// WebGPU is not enabled by default for every Linux GPU/driver combination.
app.commandLine.appendSwitch("enable-unsafe-webgpu");
// Chromium's GPU sandbox only allows the standard Vulkan ICD directories.
// NixOS keeps drivers under /run/opengl-driver, so inside the sandbox Dawn
// finds no Vulkan driver and silently falls back to SwiftShader, which the
// preview rejects. The renderer sandbox stays enabled. RINKA_GPU_SANDBOX=1
// keeps the GPU sandbox regardless.
if (
  process.platform === "linux" &&
  process.env.RINKA_GPU_SANDBOX !== "1" &&
  existsSync("/run/opengl-driver/share/vulkan/icd.d")
) {
  app.commandLine.appendSwitch("disable-gpu-sandbox");
}

const appDir = app.getAppPath();
const repoRoot = path.resolve(appDir, "..");
const rendererDir = path.join(appDir, "dist", "renderer");
const hostBinary = () => findHostBinary(repoRoot);

let library: Library;
let config: ConfigStore;
let thumbnails: Thumbnails;
let workshop: Workshop;
let window: BrowserWindow | null = null;
let host: HostConnection | null = null;
let hostState: HostState = { state: "stopped" };

// ---- library -------------------------------------------------------------------

function toEntry(e: StoredEntry): LibraryEntry {
  const thumb = thumbnails.file(e);
  let assetUrl: string | null = null;
  let revision = "0";
  if (e.project.content.type === "image" || e.project.content.type === "video") {
    try {
      revision = String(Math.round(statSync(path.join(e.dir, e.project.content.source)).mtimeMs));
    } catch {
      revision = "missing";
    }
    assetUrl = `rinka://app/library/${e.id}/${e.project.content.source}?v=${revision}`;
  }
  return {
    id: e.id,
    project: e.project,
    added: e.added,
    thumb: thumb ? `rinka://app/thumbs/${thumb}` : null,
    assetUrl,
    assetPrefix: `${e.id}/${revision}/`,
    sourceSize: thumbnails.sourceSize(e.id),
    duration: thumbnails.duration(e.id),
    videoColor: thumbnails.videoColor(e.id),
  };
}

function listEntries(): LibraryEntry[] {
  return library.list().map(toEntry);
}

function libraryChanged(): void {
  window?.webContents.send("library:changed", listEntries());
}

let thumbTimer: NodeJS.Timeout | undefined;
function refreshThumbnails(delay = 0): void {
  clearTimeout(thumbTimer);
  thumbTimer = setTimeout(() => {
    thumbnails.refresh(library.list()).catch((e: Error) => console.warn("thumbnails:", e.message));
  }, delay);
}

function importFiles(paths: string[]): ImportResult {
  const { imported, skipped } = library.importFiles(paths);
  for (const s of skipped) console.log(`import skipped ${s.path}: ${s.reason}`);
  if (imported.length) refreshThumbnails();
  return { imported: imported.map(toEntry), skipped };
}

// ---- host ----------------------------------------------------------------------

function setHostState(state: HostState): void {
  hostState = state;
  window?.webContents.send("host:state", state);
}

interface RawOutput {
  name: string;
  description: string | null;
  logical_position: [number, number] | null;
  logical_size: [number, number] | null;
  transform: string | null;
  assigned: string | null;
  surface: { buffer_size: [number, number]; scale: number; presenting: boolean } | null;
}

async function rawOutputs(): Promise<RawOutput[]> {
  return host ? host.request<RawOutput[]>("outputs") : [];
}

/** Re-applies saved assignments, e.g. after the host restarted. */
async function restoreAssignments(): Promise<void> {
  for (const output of await rawOutputs()) {
    const id = config.config.assignments[output.name];
    if (!id) continue;
    const dir = library.dir(id);
    if (output.assigned !== dir) {
      await host?.request("assign", { output: output.name, project: dir }).catch((e: Error) => {
        console.warn(`restoring ${output.name}:`, e.message);
      });
    }
  }
}

async function startHost(): Promise<HostState> {
  if (host) return hostState;
  const socket = defaultSocketPath();
  if ((process.platform !== "linux" && process.platform !== "win32") || !socket) {
    setHostState({ state: "unavailable", reason: "wallpaper playback is not implemented on this platform yet" });
    return hostState;
  }
  setHostState({ state: "connecting" });
  try {
    const logFile = path.join(app.getPath("userData"), "logs", "host.log");
    const { conn, hello } = await connectOrSpawn(socket, hostBinary(), logFile);
    const caps = await conn.request<{
      gpu: { name: string; backend: string };
      presentation: { compositor: string | null };
    }>("capabilities");
    host = conn;
    conn.onClose(() => {
      host = null;
      setHostState({ state: "stopped" });
    });
    setHostState({
      state: "connected",
      pid: hello.pid,
      protocol: hello.protocol,
      hostVersion: hello.host_version,
      gpu: `${caps.gpu.name} (${caps.gpu.backend})`,
      compositor: caps.presentation.compositor,
    });
    await restoreAssignments();
  } catch (e) {
    host = null;
    setHostState({ state: "unavailable", reason: (e as Error).message });
  }
  return hostState;
}

// ---- IPC -----------------------------------------------------------------------

function registerIpc(): void {
  ipcMain.handle("app:info", () => ({
    libraryPath: library.root,
    electron: process.versions.electron,
    chrome: process.versions.chrome,
  }));
  ipcMain.handle("library:list", () => listEntries());
  ipcMain.handle("library:create-color", (_e, title: string, color: string) => {
    const entry = library.createColor(title, color);
    refreshThumbnails();
    return toEntry(entry);
  });
  ipcMain.handle("library:import-files", async (_e, paths?: string[], kind: "image" | "video" = "image") => {
    if (!paths) {
      const extensions = IMPORT_EXTENSIONS[kind].map((e) => e.slice(1));
      const result = window
        ? await dialog.showOpenDialog(window, {
            title: kind === "video" ? "Add videos" : "Add images",
            properties: ["openFile", "multiSelections"],
            filters: [{ name: kind === "video" ? "Videos" : "Images", extensions }],
          })
        : { canceled: true, filePaths: [] };
      if (result.canceled) return { imported: [], skipped: [] };
      paths = result.filePaths;
    }
    return importFiles(paths);
  });
  ipcMain.handle("library:save", async (_e, id: string, project: Project) => {
    library.save(id, project);
    refreshThumbnails(600);
    // Outputs showing this project pick up the edit; others ignore it.
    if (host && Object.values(config.config.assignments).includes(id)) {
      await host.request("reload", { project: library.dir(id) });
    }
  });
  ipcMain.handle("library:delete", async (_e, id: string) => {
    for (const [output, assigned] of Object.entries(config.config.assignments)) {
      if (assigned !== id) continue;
      delete config.config.assignments[output];
      await host?.request("unassign", { output }).catch(() => {});
    }
    config.save();
    library.remove(id);
    refreshThumbnails();
  });
  ipcMain.handle("library:reveal", () => shell.openPath(library.root));
  ipcMain.handle("workshop:scan", () => workshop.scan());
  ipcMain.handle("workshop:import", async (_e, id: string, allowPartial: boolean) => {
    const item = workshop.get(id);
    if (!item) throw new Error("This workshop item is no longer available.");
    const projectId = randomUUID();
    const report = await workshop.run(item.dir, library.dir(projectId), allowPartial);
    let entry: LibraryEntry | null = null;
    if (report.written) {
      entry = toEntry(library.get(projectId));
      item.importedId = projectId;
      item.report = report;
      window?.webContents.send("workshop:item", item);
      refreshThumbnails();
    }
    return { report, entry };
  });

  ipcMain.handle("host:state", () => hostState);
  ipcMain.handle("host:start", () => startHost());
  ipcMain.handle("host:stop", async () => {
    await host?.request("shutdown").catch(() => {});
    return hostState;
  });
  ipcMain.handle("host:outputs", async (): Promise<OutputInfo[]> =>
    (await rawOutputs()).map((o) => ({
      name: o.name,
      description: o.description,
      position: o.logical_position,
      logicalSize: o.logical_size,
      scale: o.surface?.scale ?? null,
      transform: o.transform,
      assignedId: o.assigned ? library.idForDir(o.assigned) : null,
      presenting: o.surface?.presenting ?? false,
      bufferSize: o.surface?.buffer_size ?? null,
    })),
  );
  ipcMain.handle("host:assign", async (_e, output: string | null, id: string) => {
    if (!host) throw new Error("The wallpaper host is not running.");
    await host.request("assign", { output: output ?? undefined, project: library.dir(id) });
    const names = output ? [output] : (await rawOutputs()).map((o) => o.name);
    for (const name of names) config.config.assignments[name] = id;
    config.save();
  });
  ipcMain.handle("host:unassign", async (_e, output: string | null) => {
    if (!host) throw new Error("The wallpaper host is not running.");
    await host.request("unassign", { output: output ?? undefined });
    if (output) delete config.config.assignments[output];
    else config.config.assignments = {};
    config.save();
  });

  ipcMain.on("preview:report", (_e, report: PreviewReport) => {
    console.log(`preview: ${JSON.stringify(report)}`);
    if (SMOKE) void finishSmoke(report);
  });
}

// ---- smoke mode ------------------------------------------------------------------

/** Smoke mode: capture the window once the preview reported, then exit. */
async function finishSmoke(report: PreviewReport): Promise<void> {
  await new Promise((r) => setTimeout(r, Number(process.env.RINKA_SMOKE_DELAY ?? 500)));
  // Debug hook: run a script in the page (it may return a promise) before
  // the capture, and print its result.
  if (process.env.RINKA_SMOKE_EVAL && window) {
    const value: unknown = await window.webContents.executeJavaScript(process.env.RINKA_SMOKE_EVAL).catch((e: Error) => `threw: ${e.message}`);
    console.log(`eval: ${JSON.stringify(value)}`);
  }
  const out = process.env.RINKA_SMOKE_OUT ?? path.join(app.getPath("userData"), "smoke.png");
  if (window) writeFileSync(out, (await window.webContents.capturePage()).toPNG());
  const hostResult = process.env.RINKA_SMOKE_HOST === "1" ? await smokeHost() : null;
  console.log(
    `smoke: ${JSON.stringify({
      ok: report.ok && (hostResult?.ok ?? true),
      host: hostResult,
      capture: out,
      electron: process.versions.electron,
      chrome: process.versions.chrome,
      platform: process.platform,
      hostState,
    })}`,
  );
  host?.close();
  app.exit(report.ok && (hostResult?.ok ?? true) ? 0 : 1);
}

/**
 * Smoke mode with RINKA_SMOKE_HOST=1: start (or reuse) the host, show the first
 * project on every output, and leave the host running so the caller can check
 * that wallpapers survive the editor exiting.
 */
async function smokeHost(): Promise<{ ok: boolean; [key: string]: unknown }> {
  const state = await startHost();
  if (state.state !== "connected" || !host) return { ok: false, state };
  const [entry] = library.list();
  if (!entry) return { ok: false, error: "empty library" };
  await host.request("assign", { project: entry.dir });
  const outputs = await rawOutputs();
  return {
    ok: outputs.length > 0 && outputs.every((o) => o.assigned === entry.dir && o.surface?.presenting),
    state,
    outputs: outputs.map((o) => ({ name: o.name, buffer: o.surface?.buffer_size })),
  };
}

// ---- startup ---------------------------------------------------------------------

function createWindow(): void {
  // The editor has its own controls; no File/Edit/View menu bar.
  Menu.setApplicationMenu(null);
  window = new BrowserWindow({
    width: 1360,
    height: 860,
    minWidth: 760,
    minHeight: 520,
    title: "Rinka",
    backgroundColor: "#080f10",
    webPreferences: {
      preload: path.join(appDir, "dist", "preload.cjs"),
      contextIsolation: true,
      sandbox: true,
    },
  });
  window.on("closed", () => (window = null));
  // Smoke runs surface renderer console output for diagnosis.
  if (SMOKE) {
    window.webContents.on("console-message", (event) => {
      console.log(`renderer ${event.level}: ${event.message}`);
    });
  }
  void window.loadURL(process.env.RINKA_START_URL ?? "rinka://app/index.html");
}

/** `--import PATH...` adds images, videos or folders of them at startup. */
function importArgs(): string[] {
  const i = process.argv.indexOf("--import");
  return i < 0 ? [] : process.argv.slice(i + 1).filter((a) => !a.startsWith("--"));
}

function serve(request: Request): Response | Promise<Response> {
  const notFound = () => new Response("not found", { status: 404 });
  const url = new URL(request.url);
  const parts = decodeURIComponent(url.pathname).split("/").filter(Boolean);
  let file: string | null;
  if (parts[0] === "library" && parts.length >= 3) {
    try {
      file = library.resolve(parts[1]!, parts.slice(2).join("/"));
    } catch {
      file = null;
    }
  } else if (parts[0] === "workshop" && parts.length >= 3) {
    file = workshop.file(parts[1]!, parts.slice(2).join("/"));
  } else if (parts[0] === "thumbs" && parts.length === 2) {
    file = path.join(thumbnails.dir, path.basename(parts[1]!));
  } else {
    const candidate = path.normalize(path.join(rendererDir, ...parts));
    file = candidate.startsWith(rendererDir + path.sep) ? candidate : null;
  }
  if (!file || !existsSync(file)) return notFound();
  // Media elements need byte ranges to stream and seek.
  if (parts[0] === "library") return serveRange(file, request.headers.get("range"));
  return net.fetch(pathToFileURL(file).toString());
}

const MIME: Record<string, string> = {
  ".mp4": "video/mp4",
  ".m4v": "video/mp4",
  ".mov": "video/quicktime",
  ".mkv": "video/x-matroska",
  ".webm": "video/webm",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
};

function serveRange(file: string, range: string | null): Response {
  const size = statSync(file).size;
  const headers: Record<string, string> = {
    "Accept-Ranges": "bytes",
    "Content-Type": MIME[path.extname(file).toLowerCase()] ?? "application/octet-stream",
  };
  const m = range ? /^bytes=(\d*)-(\d*)$/.exec(range) : null;
  if (!m || (m[1] === "" && m[2] === "")) {
    headers["Content-Length"] = String(size);
    return new Response(Readable.toWeb(createReadStream(file)) as ReadableStream, { status: 200, headers });
  }
  let start = m[1] === "" ? Math.max(0, size - Number(m[2])) : Number(m[1]);
  let end = m[1] !== "" && m[2] !== "" ? Number(m[2]) : size - 1;
  end = Math.min(end, size - 1);
  if (start > end || start >= size) {
    return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${size}` } });
  }
  start = Math.max(0, start);
  headers["Content-Range"] = `bytes ${start}-${end}/${size}`;
  headers["Content-Length"] = String(end - start + 1);
  return new Response(Readable.toWeb(createReadStream(file, { start, end })) as ReadableStream, { status: 206, headers });
}

app.whenReady().then(() => {
  library = new Library(path.join(app.getPath("userData"), "projects"));
  config = new ConfigStore(path.join(app.getPath("userData"), "config.json"));
  thumbnails = new Thumbnails(path.join(app.getPath("userData"), "cache", "thumbnails"), hostBinary, libraryChanged);
  workshop = new Workshop(
    hostBinary,
    () =>
      new Map(
        library
          .list()
          .filter((e) => e.project.provenance?.source === "wallpaper-engine" && e.project.provenance.id)
          .map((e) => [e.project.provenance!.id!, e.id]),
      ),
    (item) => window?.webContents.send("workshop:item", item),
  );
  if (library.list().length === 0) library.createColor("Deep teal", "#1e6f73");
  const toImport = importArgs();
  if (toImport.length) {
    const { imported, skipped } = importFiles(toImport);
    console.log(`imported ${imported.length}, skipped ${skipped.length}`);
  }

  protocol.handle("rinka", serve);
  registerIpc();
  createWindow();
  refreshThumbnails();
  if (SMOKE) setTimeout(() => app.exit(2), 60_000);
  // Wallpapers belong to the host; the editor only connects to it.
  if (!SMOKE || process.env.RINKA_SMOKE_CONNECT === "1") void startHost();
});

// Closing the editor leaves the host and its wallpapers running.
app.on("window-all-closed", () => {
  host?.close();
  app.quit();
});
