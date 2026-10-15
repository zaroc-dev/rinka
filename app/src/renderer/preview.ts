// The engine preview: one canvas and one WebGPU device for the app's
// lifetime. Views mount the canvas where they need it.

import init, { Preview, engineVersion } from "./wasm/rinka_wasm.js";
import type { LibraryEntry, Project } from "../shared/api";
import type { DecodeRequest, DecodeResponse } from "./decode-worker";
import { matrixCode, Mp4Player } from "./mp4-player";

/**
 * Longest side the preview decodes images to. Placement still uses the
 * original size, so only sharpness is affected, and the preview is drawn
 * smaller than any display anyway.
 */
const PREVIEW_MAX_DIMENSION = 4096;

let worker: Worker | null = null;
let nextDecode = 1;
const decodes = new Map<number, (r: DecodeResponse) => void>();

function decodeInWorker(bytes: ArrayBuffer, raw = false): Promise<Extract<DecodeResponse, { ok: true }>> {
  if (!worker) {
    worker = new Worker(new URL("decode-worker.js", import.meta.url), { type: "module" });
    worker.onmessage = (e: MessageEvent<DecodeResponse>) => {
      decodes.get(e.data.id)?.(e.data);
      decodes.delete(e.data.id);
    };
  }
  const w = worker;
  const id = nextDecode++;
  return new Promise((resolve, reject) => {
    decodes.set(id, (r) => {
      if (!r.ok) return reject(new Error(r.error));
      // WebAssembly memory never shrinks; recycle the worker after a very
      // large image so its memory is released.
      if (r.sourceWidth * r.sourceHeight > 24_000_000 && decodes.size === 0) {
        w.terminate();
        if (worker === w) worker = null;
      }
      resolve(r);
    });
    const request: DecodeRequest = { id, bytes, maxDimension: PREVIEW_MAX_DIMENSION, raw };
    w.postMessage(request, [bytes]);
  });
}

let loading = false;
const loadingListeners = new Set<(loading: boolean) => void>();

export function onPreviewLoading(listener: (loading: boolean) => void): () => void {
  loadingListeners.add(listener);
  listener(loading);
  return () => loadingListeners.delete(listener);
}

function setLoading(value: boolean): void {
  loading = value;
  for (const l of loadingListeners) l(value);
}

export const canvas = document.createElement("canvas");
canvas.className = "preview-canvas";

export interface PreviewStatus {
  ok: boolean;
  error?: string;
  adapter?: string;
  surfaceFormat?: string;
  engineVersion?: string;
}

let preview: Preview | null = null;
let starting: Promise<Preview> | null = null;
/** Height in pixels of the display the preview stands in for. */
let referenceHeight = 1080;
let status: PreviewStatus = { ok: false };
const statusListeners = new Set<(s: PreviewStatus) => void>();

export function previewStatus(): PreviewStatus {
  return status;
}

export function onPreviewStatus(listener: (s: PreviewStatus) => void): () => void {
  statusListeners.add(listener);
  return () => statusListeners.delete(listener);
}

function setStatus(s: PreviewStatus): void {
  status = s;
  for (const l of statusListeners) l(s);
}

function syncCanvasSize(): void {
  const rect = canvas.getBoundingClientRect();
  if (rect.width < 1 || rect.height < 1) return;
  const w = Math.round(rect.width * devicePixelRatio);
  const h = Math.round(rect.height * devicePixelRatio);
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
    preview?.resize(w, h);
  }
}

function draw(): void {
  if (!preview || !canvas.isConnected) return;
  syncCanvasSize();
  // The canvas fills the window, so render an output of the window's shape
  // at the reference display's pixel density: scaled modes look as they do
  // on screen, and Center/Tile keep the display's 1:1 pixel size.
  const k = referenceHeight / canvas.height;
  try {
    preview.render(Math.max(1, Math.round(canvas.width * k)), referenceHeight, sceneTime());
  } catch (e) {
    console.error(e);
  }
}

new ResizeObserver(draw).observe(canvas);

export function startPreview(): Promise<Preview> {
  starting ??= (async () => {
    try {
      await init({ module_or_path: new URL("rinka_wasm_bg.wasm", import.meta.url) });
      canvas.width = canvas.height = 16;
      const p = await Preview.create(canvas);
      preview = p;
      // wgpu does not surface the browser's adapter description; ask WebGPU.
      type Info = { vendor?: string; architecture?: string; description?: string };
      const gpuApi = (navigator as Navigator & { gpu: { requestAdapter(): Promise<{ info: Info } | null> } }).gpu;
      const info: Info = (await gpuApi.requestAdapter())?.info ?? {};
      const adapter = [info.vendor, info.architecture, info.description].filter(Boolean).join(" ") || "unknown adapter";
      setStatus({ ok: true, adapter, surfaceFormat: p.surfaceFormat(), engineVersion: engineVersion() });
      window.rinka.reportPreview({
        ok: true,
        gpu: { ...JSON.parse(p.gpuInfo()), browserAdapter: info },
        surfaceFormat: p.surfaceFormat(),
        engineVersion: engineVersion(),
      });
      return p;
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      setStatus({ ok: false, error });
      window.rinka.reportPreview({ ok: false, error });
      throw e;
    }
  })();
  return starting;
}

// ---- video -------------------------------------------------------------------
//
// Preview playback is independent of the wallpapers on the desktop. MP4/MOV
// play through WebCodecs: raw Y'CbCr planes go to the engine, which converts
// them exactly as native playback does. Other containers fall back to a
// <video> element, whose frames the browser converts to RGB itself; the UI
// marks those colors as approximate.

export interface VideoState {
  playing: boolean;
  time: number;
  duration: number;
  /** Whether colors go through the engine's conversion. */
  exactColor: boolean;
}

interface PreviewMedia {
  readonly playing: boolean;
  readonly time: number;
  readonly duration: number;
  readonly exactColor: boolean;
  play(): void;
  pause(): void;
  seek(seconds: number): void;
  close(): void;
}

let media: PreviewMedia | null = null;
const videoListeners = new Set<(s: VideoState | null) => void>();

function videoState(): VideoState | null {
  return media
    ? { playing: media.playing, time: media.time, duration: media.duration, exactColor: media.exactColor }
    : null;
}

let lastEmit = 0;
function emitVideo(force = false): void {
  const now = performance.now();
  if (!force && now - lastEmit < 200) return;
  lastEmit = now;
  const s = videoState();
  for (const l of videoListeners) l(s);
}

export function onVideoState(listener: (s: VideoState | null) => void): () => void {
  videoListeners.add(listener);
  listener(videoState());
  return () => videoListeners.delete(listener);
}

export function toggleVideo(): void {
  if (!media) return;
  if (media.playing) media.pause();
  else media.play();
  emitVideo(true);
}

export function seekVideo(seconds: number): void {
  media?.seek(seconds);
  emitVideo(true);
}

/** Stops all preview playback and animation. */
export function stopPreviewMedia(): void {
  stopScene();
  stopVideo();
}

/** Stops preview playback and releases the frame textures. */
export function stopVideo(): void {
  if (!media) return;
  media.close();
  media = null;
  preview?.clearVideoFrame();
  emitVideo(true);
}

async function openCodecMedia(entry: LibraryEntry, url: string, p: Preview): Promise<PreviewMedia> {
  let failed: ((e: Error) => void) | null = null;
  let firstFrame: (() => void) | null = null;
  const ready = new Promise<void>((resolve, reject) => {
    firstFrame = resolve;
    failed = reject;
  });
  const color = entry.videoColor
    ? { matrix: matrixCode(entry.videoColor.matrix), fullRange: entry.videoColor.full_range }
    : null;
  const player = await Mp4Player.open(
    url,
    color,
    (f) => {
      p.uploadYuvFrame(
        f.width,
        f.height,
        f.chromaWidth,
        f.chromaHeight,
        f.matrix,
        f.fullRange,
        f.data,
        f.offsets[0],
        f.strides[0],
        f.offsets[1],
        f.strides[1],
        f.offsets[2],
        f.strides[2],
      );
      draw();
      emitVideo();
      firstFrame?.();
    },
    (e) => {
      console.error("preview video:", e);
      failed?.(e);
    },
  );
  player.play();
  try {
    await Promise.race([ready, new Promise((_, reject) => setTimeout(() => reject(new Error("no frame decoded")), 8000))]);
  } catch (e) {
    player.close();
    throw e;
  }
  return {
    get playing() {
      return player.playing;
    },
    get time() {
      return player.time;
    },
    duration: player.duration,
    exactColor: true,
    play: () => player.play(),
    pause: () => player.pause(),
    seek: (t) => player.seek(t),
    close: () => player.close(),
  };
}

function mediaError(v: HTMLVideoElement): string {
  const code = v.error?.code;
  if (code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED || code === MediaError.MEDIA_ERR_DECODE) {
    return "the preview cannot decode this video format; wallpaper playback may still work";
  }
  return v.error?.message || "the video could not be loaded";
}

async function openElementMedia(url: string, p: Preview): Promise<PreviewMedia> {
  const v = document.createElement("video");
  v.muted = true;
  v.loop = true;
  v.playsInline = true;
  v.preload = "auto";
  v.src = url;
  await new Promise<void>((resolve, reject) => {
    v.onloadeddata = () => resolve();
    v.onerror = () => reject(new Error(mediaError(v)));
  });
  let closed = false;
  const onFrame = () => {
    if (closed) return;
    p.updateVideoFrame(v);
    draw();
    emitVideo();
    v.requestVideoFrameCallback(onFrame);
  };
  v.requestVideoFrameCallback(onFrame);
  p.updateVideoFrame(v);
  return {
    get playing() {
      return !v.paused;
    },
    get time() {
      return v.currentTime;
    },
    get duration() {
      return v.duration || 0;
    },
    exactColor: false,
    play: () => void v.play(),
    pause: () => v.pause(),
    seek: (t) => {
      v.currentTime = t;
    },
    close: () => {
      closed = true;
      v.pause();
      v.removeAttribute("src");
      v.load();
    },
  };
}

// Hidden windows do no preview work.
let pausedForHidden = false;
document.addEventListener("visibilitychange", () => {
  if (!media) return;
  if (document.hidden && media.playing) {
    media.pause();
    pausedForHidden = true;
  } else if (!document.hidden && pausedForHidden) {
    media.play();
    pausedForHidden = false;
  }
});

let generation = 0;

// ---- scenes --------------------------------------------------------------------

/** Frame rate of animated scenes in the preview. */
const SCENE_FPS = 30;
let sceneStart = performance.now();
let sceneLoop = 0;
let lastSceneFrame = 0;

function sceneTime(): number {
  return (performance.now() - sceneStart) / 1000;
}

function stopScene(): void {
  cancelAnimationFrame(sceneLoop);
  sceneLoop = 0;
}

function startScene(): void {
  stopScene();
  sceneStart = performance.now();
  const tick = (now: number) => {
    sceneLoop = requestAnimationFrame(tick);
    if (document.hidden || now - lastSceneFrame < 1000 / SCENE_FPS - 2) return;
    lastSceneFrame = now;
    draw();
  };
  sceneLoop = requestAnimationFrame(tick);
}

/** Fetches and decodes a scene's textures, shader sources and layer meshes. */
async function loadScene(entry: LibraryEntry, project: Project, p: Preview, token: number): Promise<boolean> {
  if (project.content.type !== "scene") return false;
  const base = `rinka://app/library/${entry.id}/`;
  const shaders = Object.values(project.content.shaders).flatMap((s) => [s.vertex, s.fragment]);
  const texts = await Promise.all(shaders.map(async (path) => [path, await (await fetch(base + path)).text()] as const));
  const meshPaths = project.content.layers.flatMap((l) => (l.mesh ? [l.mesh.source] : []));
  const meshes = await Promise.all(meshPaths.map(async (path) => [path, await (await fetch(base + path)).text()] as const));
  const textures = Object.keys(project.content.textures);
  const decoded = await Promise.all(
    textures.map(async (path) => {
      const bytes = await (await fetch(base + path)).arrayBuffer();
      return [path, await decodeInWorker(bytes, true)] as const;
    }),
  );
  if (token !== generation) return false;
  for (const [path, text] of texts) p.addShaderSource(entry.assetPrefix + path, text);
  for (const [path, text] of meshes) p.addMeshSource(entry.assetPrefix + path, text);
  for (const [path, d] of decoded) {
    p.addDecodedAsset(entry.assetPrefix + path, d.width, d.height, d.sourceWidth, d.sourceHeight, d.format, new Uint8Array(d.pixels));
  }
  return true;
}

/** Shows a library entry, fetching and decoding its asset when needed. */
export async function showEntry(entry: LibraryEntry, project: Project = entry.project): Promise<void> {
  const token = ++generation;
  const p = await startPreview();
  const keys: string[] = [];
  try {
    stopScene();
    if (project.content.type === "scene") {
      stopVideo();
      setLoading(true);
      if (!(await loadScene(entry, project, p, token))) return;
      p.setProject(JSON.stringify(project), entry.assetPrefix);
      if (p.isAnimated()) startScene();
      else draw();
      return;
    }
    if (project.content.type === "video" && entry.assetUrl) {
      stopVideo();
      setLoading(true);
      let m: PreviewMedia;
      try {
        m = await openCodecMedia(entry, entry.assetUrl, p);
      } catch (e) {
        console.info("preview: WebCodecs path unavailable, using <video>:", e);
        m = await openElementMedia(entry.assetUrl, p);
      }
      if (token !== generation) {
        m.close();
        return;
      }
      p.retainAssets([]);
      p.setProject(JSON.stringify(project), entry.assetPrefix);
      media = m;
      m.play();
      draw();
      emitVideo(true);
      return;
    }
    stopVideo();
    if (project.content.type === "image" && entry.assetUrl) {
      const key = entry.assetPrefix + project.content.source;
      keys.push(key);
      if (!p.hasAsset(key)) {
        setLoading(true);
        const bytes = await (await fetch(entry.assetUrl)).arrayBuffer();
        const d = await decodeInWorker(bytes);
        if (token !== generation) return;
        p.addDecodedAsset(key, d.width, d.height, d.sourceWidth, d.sourceHeight, d.format, new Uint8Array(d.pixels));
      }
    }
    if (token !== generation) return;
    p.retainAssets(keys);
    p.setProject(JSON.stringify(project), entry.assetPrefix);
    draw();
  } finally {
    if (token === generation) setLoading(false);
  }
}

/** Applies an edit to the project currently shown; its assets are loaded. */
export function updateProject(entry: LibraryEntry, project: Project): void {
  if (!preview) return;
  try {
    preview.setProject(JSON.stringify(project), entry.assetPrefix);
    draw();
  } catch {
    void showEntry(entry, project);
  }
}

/** Sets the pixel height of the display the preview stands in for. */
export function setPreviewReference(height: number): void {
  referenceHeight = Math.max(1, Math.round(height));
  draw();
}

export function redrawPreview(): void {
  draw();
}
