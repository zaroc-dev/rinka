// Electron main process: window, library, thumbnails, host connection and IPC.

import { app, BrowserWindow, dialog, ipcMain, Menu, net, protocol, shell } from "electron";
import { randomUUID } from "node:crypto";
import { createReadStream, existsSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { Readable } from "node:stream";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { SCENE_FPS_CHOICES } from "../shared/api";
import type { AppSettings, HostState, SceneFps, VideoDecoder, VideoEncoder, VideoFiles, OptimizeOptions, OptimizeResult, ImportResult, LibraryEntry, OutputInfo, PlasmaDesktopState, PreviewReport, Project } from "../shared/api";
import { connectOrSpawn, defaultSocketPath, findHostBinary, type HostConnection } from "./host";
import { ConfigStore, IMPORT_EXTENSIONS, Library, type StoredEntry } from "./library";
import { optimizedCopies, optimizedName, VideoOptimizer } from "./optimize";
import { Thumbnails } from "./thumbnails";
import { AppTray } from "./tray";
import { Workshop, workshopFolder } from "./workshop";

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

// One editor per data directory: launching again shows the running one,
// which may be hidden in the tray.
const primary = SMOKE || app.requestSingleInstanceLock();
if (!primary) app.exit(0);

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
// Packaged builds (electron-builder) ship the host in the resources directory.
const hostBinary = () => findHostBinary(repoRoot, app.isPackaged ? process.resourcesPath : undefined);

/** The window and tray icon: ICO on Windows (several sizes), PNG elsewhere. */
const appIcon = () => path.join(appDir, "dist", "resources", process.platform === "win32" ? "icon.ico" : "icon.png");

// Groups the editor's windows under Rinka in the Windows taskbar instead of
// Electron's own entry.
if (process.platform === "win32") app.setAppUserModelId("dev.zaroc.rinka");

let library: Library;
let config: ConfigStore;
let thumbnails: Thumbnails;
let workshop: Workshop;
let optimizer: VideoOptimizer;
let window: BrowserWindow | null = null;
let host: HostConnection | null = null;
let hostState: HostState = { state: "stopped" };
let tray: AppTray | null = null;
/** Set once the editor is really quitting; closing the window otherwise hides it to the tray. */
let quitting = false;

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

/** A video project now plays another file: refresh what shows it. */
async function afterVideoChange(id: string): Promise<void> {
  refreshThumbnails();
  libraryChanged();
  if (host && Object.values(config.config.assignments).includes(id)) {
    await host.request("reload", { project: library.dir(id) }).catch((e: Error) => console.warn("reload:", e.message));
  }
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
  tray?.update(trayState());
}

/** Stops or continues wallpaper animation; the last frame stays on screen. */
async function setPaused(paused: boolean): Promise<HostState> {
  if (!host || hostState.state !== "connected") throw new Error("The wallpaper host is not running.");
  await host.request(paused ? "pause" : "resume");
  if (hostState.state === "connected") setHostState({ ...hostState, paused });
  return hostState;
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

/** Hands the host-wide playback settings to the host. */
async function sendHostSettings(): Promise<void> {
  const s = appSettings();
  const patch = { scene_fps: s.sceneFps, volume: s.soundVolume, muted: s.soundMuted, video_decoder: s.videoDecoder };
  await host?.request("settings", patch).catch((e: Error) => {
    console.warn("host settings:", e.message);
  });
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
      presentation: { compositor: string | null; plasma_desktop?: boolean };
      paused: boolean;
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
      paused: caps.paused,
      plasmaDesktop: caps.presentation.plasma_desktop ?? false,
    });
    await sendHostSettings();
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
    // Only optimize and restore change which file a video plays; an editor
    // draft from before one of them must not switch it back.
    const disk = library.get(id).project.content;
    const c = project.content;
    if (disk.type === "video" && c.type === "video") {
      const { original: _, ...rest } = c;
      project = { ...project, content: { ...rest, source: disk.source, ...(disk.original ? { original: disk.original } : {}) } };
    }
    library.save(id, project);
    refreshThumbnails(600);
    // Outputs showing this project pick up the edit; others ignore it.
    if (host && Object.values(config.config.assignments).includes(id)) {
      await host.request("reload", { project: library.dir(id) });
    }
  });
  ipcMain.handle("video:files", async (_e, id: string): Promise<VideoFiles> => {
    const entry = library.get(id);
    const c = entry.project.content;
    if (c.type !== "video") throw new Error("not a video wallpaper");
    const original = c.original ? path.join(entry.dir, c.original) : null;
    return {
      current: await optimizer.probe(path.join(entry.dir, c.source)),
      original: original ? await optimizer.probe(original).catch(() => null) : null,
      running: optimizer.progress(id),
    };
  });
  ipcMain.handle("video:optimize", async (_e, id: string, options: OptimizeOptions): Promise<OptimizeResult> => {
    const before = library.get(id);
    const c = before.project.content;
    if (c.type !== "video") throw new Error("not a video wallpaper");
    // Always encode from the original, never from an earlier re-encode.
    const original = c.original ?? c.source;
    const input = path.join(before.dir, original);
    const source = await optimizer.probe(input);
    const height = options.maxHeight && options.maxHeight < source.height ? options.maxHeight : source.height;
    const fps = options.maxFps && source.frameRate && options.maxFps < source.frameRate - 0.01 ? options.maxFps : null;
    const name = optimizedName(before.dir, original, height, fps);
    const outcome = await optimizer.transcode(id, input, path.join(before.dir, name), options, appSettings().videoEncoder, (p) =>
      window?.webContents.send("video:progress", id, p),
    );
    // Re-read: the project may have been edited while it encoded.
    const now = library.get(id);
    const content = now.project.content;
    if (content.type !== "video") throw new Error("the wallpaper changed while its video was optimized");
    const previous = content.source;
    library.save(id, { ...now.project, content: { ...content, source: name, original } });
    // The previous copy, and any a stale save left behind.
    for (const copy of new Set([previous, ...optimizedCopies(now.dir, original)])) {
      if (copy !== original && copy !== name) rmSync(path.join(now.dir, copy), { force: true });
    }
    await afterVideoChange(id);
    return {
      entry: toEntry(library.get(id)),
      encoder: outcome.encoder,
      droppedAudio: outcome.dropped_audio,
      reducedTo8Bit: outcome.reduced_to_8_bit,
    };
  });
  ipcMain.handle("video:cancel", (_e, id: string) => optimizer.cancel(id));
  ipcMain.handle("video:restore", async (_e, id: string): Promise<LibraryEntry> => {
    const entry = library.get(id);
    const c = entry.project.content;
    if (c.type !== "video" || !c.original) throw new Error("this video has no original to restore");
    if (!existsSync(path.join(entry.dir, c.original))) throw new Error("the original file is missing");
    const { original, ...rest } = c;
    library.save(id, { ...entry.project, content: { ...rest, source: original } });
    for (const copy of new Set([c.source, ...optimizedCopies(entry.dir, original)])) {
      if (copy !== original) rmSync(path.join(entry.dir, copy), { force: true });
    }
    await afterVideoChange(id);
    return toEntry(library.get(id));
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
  ipcMain.handle("workshop:scan", () => workshop.scan(config.config.workshopFolder));
  ipcMain.handle("workshop:locate", async () => {
    if (!window) return null;
    const result = await dialog.showOpenDialog(window, {
      title: "Locate Wallpaper Engine or its workshop folder",
      defaultPath: config.config.workshopFolder,
      properties: ["openDirectory"],
    });
    const folder = result.filePaths[0];
    if (result.canceled || !folder) return null;
    workshopFolder(folder);
    config.config.workshopFolder = folder;
    config.save();
    return workshop.scan(folder);
  });
  ipcMain.handle("workshop:reset-folder", () => {
    delete config.config.workshopFolder;
    config.save();
    return workshop.scan();
  });
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
  ipcMain.handle("host:pause", (_e, paused: boolean) => setPaused(paused));
  ipcMain.handle("settings:get", () => appSettings());
  ipcMain.handle("settings:set", async (_e, patch: Partial<AppSettings>) => {
    if (typeof patch.trayIcon === "boolean") config.config.trayIcon = patch.trayIcon;
    if (patch.sceneFps !== undefined && SCENE_FPS_CHOICES.includes(patch.sceneFps)) config.config.sceneFps = patch.sceneFps;
    if (typeof patch.soundVolume === "number" && patch.soundVolume >= 0 && patch.soundVolume <= 1) {
      config.config.soundVolume = patch.soundVolume;
    }
    if (typeof patch.soundMuted === "boolean") config.config.soundMuted = patch.soundMuted;
    if (patch.videoDecoder !== undefined && VIDEO_DECODER_CHOICES.includes(patch.videoDecoder)) {
      config.config.videoDecoder = patch.videoDecoder;
    }
    if (patch.videoEncoder !== undefined && VIDEO_ENCODER_CHOICES.includes(patch.videoEncoder)) {
      config.config.videoEncoder = patch.videoEncoder;
    }
    config.save();
    updateTray();
    const hostPatch = [patch.sceneFps, patch.soundVolume, patch.soundMuted, patch.videoDecoder];
    if (hostPatch.some((v) => v !== undefined)) {
      await sendHostSettings();
    }
    return appSettings();
  });
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
  ipcMain.handle("host:plasma-desktop", async (_e, showRinka?: boolean): Promise<PlasmaDesktopState> => {
    if (!host) throw new Error("The wallpaper host is not running.");
    const state = await host.request<{
      plugin: string;
      desktops: { screen: number; plugin: string; geometry: [number, number, number, number] | null }[];
      restart: "restarted" | "needed" | null;
    }>("plasma_desktop", { show_rinka: showRinka });
    const outputs = await rawOutputs().catch(() => []);
    // Plasma and the host both report logical geometry.
    const outputAt = (g: [number, number, number, number] | null) =>
      g &&
      outputs.find(
        (o) => o.logical_position?.[0] === g[0] && o.logical_position?.[1] === g[1] && o.logical_size?.[0] === g[2] && o.logical_size?.[1] === g[3],
      )?.name;
    return {
      desktops: state.desktops.map((d) => ({ output: outputAt(d.geometry) ?? null, plugin: d.plugin, showsRinka: d.plugin === state.plugin })),
      restart: state.restart,
    };
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
    // Linux desktops take the icon from the desktop entry; Windows uses this.
    icon: appIcon(),
    backgroundColor: "#080f10",
    webPreferences: {
      preload: path.join(appDir, "dist", "preload.cjs"),
      contextIsolation: true,
      sandbox: true,
    },
  });
  window.on("close", (event) => {
    if (tray && !quitting) {
      event.preventDefault();
      window?.hide();
    }
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
function importArgs(argv = process.argv): string[] {
  const i = argv.indexOf("--import");
  return i < 0 ? [] : argv.slice(i + 1).filter((a) => !a.startsWith("--"));
}

function showWindow(): void {
  if (!window) return createWindow();
  if (window.isMinimized()) window.restore();
  window.show();
  window.focus();
}

// ---- tray and settings -----------------------------------------------------------

/** The host's decoders on this platform (see settings.rs). */
const VIDEO_DECODER_CHOICES: VideoDecoder[] =
  process.platform === "win32" ? ["auto", "d3d11va", "software"] : ["auto", "nvdec", "vaapi", "software"];
/** The host's encoders on this platform (see transcode.rs). */
const VIDEO_ENCODER_CHOICES: VideoEncoder[] =
  process.platform === "win32" ? ["auto", "nvenc", "software"] : ["auto", "nvenc", "vaapi", "software"];

function appSettings(): AppSettings {
  const fps = config.config.sceneFps as SceneFps;
  const volume = config.config.soundVolume;
  const decoder = config.config.videoDecoder as VideoDecoder;
  const encoder = config.config.videoEncoder as VideoEncoder;
  return {
    trayIcon: config.config.trayIcon !== false,
    sceneFps: SCENE_FPS_CHOICES.includes(fps) ? fps : 30,
    soundVolume: typeof volume === "number" && volume >= 0 && volume <= 1 ? volume : 0.5,
    soundMuted: config.config.soundMuted === true,
    videoDecoder: VIDEO_DECODER_CHOICES.includes(decoder) ? decoder : "auto",
    videoDecoderChoices: VIDEO_DECODER_CHOICES,
    videoEncoder: VIDEO_ENCODER_CHOICES.includes(encoder) ? encoder : "auto",
    videoEncoderChoices: VIDEO_ENCODER_CHOICES,
  };
}

function trayState() {
  return hostState.state === "connected" ? { connected: true, paused: hostState.paused } : { connected: false, paused: false };
}

/** Shows or removes the tray icon to match the settings. */
function updateTray(): void {
  const want = appSettings().trayIcon && !SMOKE;
  if (want && !tray) {
    tray = new AppTray(
      appIcon(),
      {
        open: showWindow,
        setPaused: (paused) => void setPaused(paused).catch((e: Error) => console.warn("pause:", e.message)),
        quit: () => {
          quitting = true;
          app.quit();
        },
        quitEntirely: () => {
          quitting = true;
          void (host?.request("shutdown") ?? Promise.resolve())
            .catch((e: Error) => console.warn("shutdown:", e.message))
            .finally(() => app.quit());
        },
      },
      trayState(),
    );
  } else if (!want && tray) {
    tray.destroy();
    tray = null;
  }
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

app.on("second-instance", (_event, argv) => {
  const toImport = importArgs(argv);
  if (toImport.length) importFiles(toImport);
  showWindow();
});

app.on("before-quit", () => {
  quitting = true;
  optimizer?.cancelAll();
});

app.whenReady().then(() => {
  if (!primary) return;
  library = new Library(path.join(app.getPath("userData"), "projects"));
  config = new ConfigStore(path.join(app.getPath("userData"), "config.json"));
  optimizer = new VideoOptimizer(hostBinary);
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
  updateTray();
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
