// Types shared by the main process, preload and renderer.

export type Fit = "cover" | "contain" | "stretch" | "center" | "tile";
export const FITS: Fit[] = ["cover", "contain", "stretch", "center", "tile"];

export type Content =
  | { type: "color"; color: string }
  | { type: "image"; source: string; fit: Fit; background: string }
  | {
      type: "video";
      source: string;
      fit: Fit;
      background: string;
      /** The file `source` was re-encoded from, kept to undo it; not played. */
      original?: string;
    }
  | SceneContent;

/** A layered scene (imported from Wallpaper Engine); edited by the engine. */
export interface SceneContent {
  type: "scene";
  width: number;
  height: number;
  fit?: Fit;
  layers: {
    id: string;
    name: string;
    visible: boolean;
    effects: { id: string; name: string }[];
    /** Mesh asset (`rinka.mesh`) the layer is drawn through. */
    mesh?: { source: string };
  }[];
  textures: Record<string, { clamp?: boolean; nearest?: boolean; video?: { width: number; height: number } }>;
  shaders: Record<string, { vertex: string; fragment: string }>;
  /** Sound layers (`rinka_engine::project::SceneSound`). */
  sounds?: SceneSound[];
}

export interface SceneSound {
  id: string;
  name: string;
  files: string[];
  mode?: "loop" | "random" | "single";
  volume?: number;
  /** Random mode: seconds of silence between tracks. */
  pause?: [number, number];
}

export interface Provenance {
  source: string;
  id?: string;
  title?: string;
  report?: string;
  reference?: string;
}

export type Kind = Content["type"];

/** Color adjustments of the finished frame (`rinka_engine::project::ColorAdjust`). */
export interface ColorAdjust {
  /** Multiplier; 1 is unchanged. */
  brightness: number;
  /** Around mid gray; 1 is unchanged. */
  contrast: number;
  /** 0 is grayscale, 1 unchanged. */
  saturation: number;
  /** Rotation in degrees. */
  hue: number;
}

export const NEUTRAL_ADJUST: ColorAdjust = { brightness: 1, contrast: 1, saturation: 1, hue: 0 };

/** Mirrors `rinka_engine::Project` (manifest version 1). */
export interface Project {
  format: "rinka.project";
  version: 1;
  title: string;
  content: Content;
  /** Playback rate of video and scene animation; 1 when absent. */
  speed?: number;
  /** Absent when colors are unchanged. */
  adjust?: Partial<ColorAdjust>;
  provenance?: Provenance;
}

export interface LibraryEntry {
  /** Stable identifier; also the project directory name. */
  id: string;
  project: Project;
  /** When the project was added, in ms since the epoch. */
  added: number;
  /** Engine-rendered 16:9 thumbnail, once generated. */
  thumb: string | null;
  /** URL of the image or video asset. */
  assetUrl: string | null;
  /** Prefix that, with an asset path, names a specific asset revision. */
  assetPrefix: string;
  /** Image or video resolution, once known. */
  sourceSize: [number, number] | null;
  /** Video length in seconds, once known. */
  duration: number | null;
  /** How native playback converts this video's colors, once known. */
  videoColor: VideoColor | null;
}

export interface VideoColor {
  matrix: "bt601" | "bt709" | "bt2020-ncl";
  full_range: boolean;
}

/** One output as reported by the host, with its assignment resolved. */
export interface OutputInfo {
  name: string;
  description: string | null;
  position: [number, number] | null;
  logicalSize: [number, number] | null;
  scale: number | null;
  transform: string | null;
  /** Library id of the assigned project, if it is in the library. */
  assignedId: string | null;
  presenting: boolean;
  bufferSize: [number, number] | null;
}

export type HostState =
  | { state: "connecting" }
  | {
      state: "connected";
      pid: number;
      protocol: number;
      hostVersion: string;
      gpu: string;
      compositor: string | null;
      /** Wallpaper animation is paused; the last frame stays on screen. */
      paused: boolean;
    }
  | { state: "stopped" }
  | { state: "unavailable"; reason: string };

export interface PreviewReport {
  ok: boolean;
  error?: string;
  gpu?: unknown;
  surfaceFormat?: string;
  engineVersion?: string;
}

export interface ImportResult {
  imported: LibraryEntry[];
  /** Files that were skipped, with the reason. */
  skipped: { path: string; reason: string }[];
}

export type ImportStatus = "supported" | "partial" | "unsupported";

export interface ImportReportItem {
  source: string;
  name: string;
  kind: string;
  status: ImportStatus;
  native?: string;
  notes?: string[];
}

export interface ImportReport {
  status: ImportStatus;
  summary: { supported: number; partial: number; unsupported: number };
  source: { id: string | null; title: string; item_type: string };
  items: ImportReportItem[];
  written: boolean;
}

/** A Wallpaper Engine workshop item found on disk. */
export interface WorkshopItem {
  id: string;
  dir: string;
  title: string;
  type: string;
  /** URL of the item's own preview image. */
  preview: string | null;
  /** Dry-run report, once checked. */
  report: ImportReport | null;
  /** Error from checking, when the report could not be produced. */
  error: string | null;
  /** Library id when this item was already imported. */
  importedId: string | null;
}

export interface WorkshopScan {
  /** Manually selected source folder, or null for automatic discovery. */
  folder: string | null;
  /** Where items were looked for; empty when no Steam library was found. */
  workshopDirs: string[];
  assetsDir: string | null;
  items: WorkshopItem[];
}

export type SceneFps = 15 | 30;
export const SCENE_FPS_CHOICES: SceneFps[] = [15, 30];

/** Where the host decodes videos. GPU decoders fall back to software for streams they cannot play. */
export type VideoDecoder = "auto" | "nvdec" | "vaapi" | "d3d11va" | "software";
/** Which H.264 encoder "Optimize video" uses. Automatic tries the graphics card, then software. */
export type VideoEncoder = "auto" | "nvenc" | "vaapi" | "software";
export const VIDEO_ENCODER_LABELS: Record<VideoEncoder, string> = {
  auto: "Automatic",
  nvenc: "NVENC",
  vaapi: "VA-API",
  software: "Software",
};

/** Stream properties of a video file (`rinka-host video-probe`) and its size on disk. */
export interface VideoFileInfo {
  width: number;
  height: number;
  frameRate: number | null;
  duration: number | null;
  codec: string;
  bytes: number;
}

/** A video project's file, and the original it was re-encoded from. */
export interface VideoFiles {
  current: VideoFileInfo;
  original: VideoFileInfo | null;
  /** Progress 0..1 of a re-encode that is running, else null. */
  running: number | null;
}

/** Limits for "Optimize video"; null keeps the source's value. */
export interface OptimizeOptions {
  maxHeight: number | null;
  maxFps: number | null;
}

export interface OptimizeResult {
  entry: LibraryEntry;
  /** The encoder that wrote the file. */
  encoder: VideoEncoder;
  droppedAudio: boolean;
  reducedTo8Bit: boolean;
}

export const VIDEO_DECODER_LABELS: Record<VideoDecoder, string> = {
  auto: "Automatic",
  nvdec: "NVDEC",
  vaapi: "VA-API",
  d3d11va: "Direct3D 11",
  software: "Software",
};

/** Editor settings stored in config.json. */
export interface AppSettings {
  /** Show the tray icon; closing the window then hides it to the tray. */
  trayIcon: boolean;
  /** Frame rate of animated scenes, on the desktop and in the preview. */
  sceneFps: SceneFps;
  /** Gain of all scene sound, 0..1. */
  soundVolume: number;
  soundMuted: boolean;
  videoDecoder: VideoDecoder;
  /** The decoders this platform offers; not settable. */
  videoDecoderChoices: VideoDecoder[];
  videoEncoder: VideoEncoder;
  /** The encoders this platform offers; not settable. */
  videoEncoderChoices: VideoEncoder[];
}

export interface AppInfo {
  libraryPath: string;
  electron: string;
  chrome: string;
}

/** API exposed to the renderer by the preload script as `window.rinka`. */
export interface RinkaApi {
  appInfo(): Promise<AppInfo>;
  appSettings(): Promise<AppSettings>;
  setAppSettings(patch: Partial<AppSettings>): Promise<AppSettings>;
  listProjects(): Promise<LibraryEntry[]>;
  createColor(title: string, color: string): Promise<LibraryEntry>;
  /**
   * Imports image and video files, or folders of them. Without `paths`, opens
   * a file dialog for `kind`.
   */
  importFiles(paths?: string[], kind?: "image" | "video"): Promise<ImportResult>;
  saveProject(id: string, project: Project): Promise<void>;
  deleteProject(id: string): Promise<void>;
  revealLibrary(): Promise<void>;
  /** Lists Wallpaper Engine workshop items and starts checking them. */
  scanWorkshop(): Promise<WorkshopScan>;
  /** Opens a folder picker and remembers the selected workshop location. */
  locateWorkshop(): Promise<WorkshopScan | null>;
  resetWorkshopFolder(): Promise<WorkshopScan>;
  /** Converts an item into a library project. */
  importWorkshopItem(id: string, allowPartial: boolean): Promise<{ report: ImportReport; entry: LibraryEntry | null }>;
  onWorkshopItem(listener: (item: WorkshopItem) => void): void;
  /** Filesystem path of a dropped file. */
  pathForFile(file: File): string;
  onLibraryChanged(listener: (entries: LibraryEntry[]) => void): void;
  /** The current and original file of a video project. */
  videoFiles(id: string): Promise<VideoFiles>;
  /**
   * Re-encodes a video project's file smaller or at a lower frame rate. The
   * project then plays the new file; the original stays for `restoreVideo`.
   */
  optimizeVideo(id: string, options: OptimizeOptions): Promise<OptimizeResult>;
  cancelOptimize(id: string): Promise<void>;
  /** Plays the original file again and deletes the re-encoded one. */
  restoreVideo(id: string): Promise<LibraryEntry>;
  /** Returns a function that stops listening. */
  onOptimizeProgress(listener: (id: string, progress: number) => void): () => void;

  hostState(): Promise<HostState>;
  startHost(): Promise<HostState>;
  stopHost(): Promise<HostState>;
  outputs(): Promise<OutputInfo[]>;
  assign(output: string | null, id: string): Promise<void>;
  unassign(output: string | null): Promise<void>;
  onHostState(listener: (state: HostState) => void): void;
  /** Stops or continues wallpaper animation on every display. */
  setPaused(paused: boolean): Promise<HostState>;

  reportPreview(report: PreviewReport): void;
}
