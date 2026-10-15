// Types shared by the main process, preload and renderer.

export type Fit = "cover" | "contain" | "stretch" | "center" | "tile";
export const FITS: Fit[] = ["cover", "contain", "stretch", "center", "tile"];

export type Content =
  | { type: "color"; color: string }
  | { type: "image"; source: string; fit: Fit; background: string }
  | { type: "video"; source: string; fit: Fit; background: string }
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
  textures: Record<string, { clamp?: boolean; nearest?: boolean }>;
  shaders: Record<string, { vertex: string; fragment: string }>;
}

export interface Provenance {
  source: string;
  id?: string;
  title?: string;
  report?: string;
  reference?: string;
}

export type Kind = Content["type"];

/** Mirrors `rinka_engine::Project` (manifest version 1). */
export interface Project {
  format: "rinka.project";
  version: 1;
  title: string;
  content: Content;
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
  /** Where items were looked for; empty when no Steam library was found. */
  workshopDirs: string[];
  assetsDir: string | null;
  items: WorkshopItem[];
}

/** Editor settings stored in config.json. */
export interface AppSettings {
  /** Show the tray icon; closing the window then hides it to the tray. */
  trayIcon: boolean;
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
  /** Converts an item into a library project. */
  importWorkshopItem(id: string, allowPartial: boolean): Promise<{ report: ImportReport; entry: LibraryEntry | null }>;
  onWorkshopItem(listener: (item: WorkshopItem) => void): void;
  /** Filesystem path of a dropped file. */
  pathForFile(file: File): string;
  onLibraryChanged(listener: (entries: LibraryEntry[]) => void): void;

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
