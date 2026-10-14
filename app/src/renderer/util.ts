import type { Fit, LibraryEntry, OutputInfo } from "../shared/api";

export const api = window.rinka;

export const FIT_LABELS: Record<Fit, string> = {
  cover: "Fill",
  contain: "Fit",
  stretch: "Stretch",
  center: "Center",
  tile: "Tile",
};

export const FIT_HINTS: Record<Fit, string> = {
  cover: "Scale to fill the display, cropping the edges",
  contain: "Scale to fit inside the display, showing the background around it",
  stretch: "Stretch to the display's exact shape",
  center: "Show at actual pixels, centered",
  tile: "Repeat at actual pixels from the top-left",
};

export function resolution(size: [number, number] | null | undefined): string {
  return size ? `${size[0]} × ${size[1]}` : "";
}

export function kindLabel(entry: LibraryEntry): string {
  return { image: "Image", video: "Video", color: "Color", scene: "Scene" }[entry.project.content.type];
}

/** `m:ss` for a duration in seconds. */
export function clock(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds)) return "";
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** One-line technical summary shown under a title. */
export function detail(entry: LibraryEntry): string {
  const c = entry.project.content;
  if (c.type === "color") return c.color;
  if (c.type === "scene") return `${c.layers.length} ${c.layers.length === 1 ? "layer" : "layers"} · ${c.width} × ${c.height}`;
  const size = resolution(entry.sourceSize);
  if (c.type === "video") return [clock(entry.duration), size].filter(Boolean).join(" · ") || "Video";
  return size || "Image";
}

export function displaysShowing(entry: LibraryEntry, outputs: OutputInfo[]): OutputInfo[] {
  return outputs.filter((o) => o.assignedId === entry.id);
}

/** The pixel size a display presents at. */
export function outputPixels(o: OutputInfo): [number, number] | null {
  if (o.bufferSize) return o.bufferSize;
  if (!o.logicalSize) return null;
  const s = o.scale ?? 1;
  return [Math.round(o.logicalSize[0] * s), Math.round(o.logicalSize[1] * s)];
}

export function errorMessage(e: unknown): string {
  const m = e instanceof Error ? e.message : String(e);
  return m.replace(/^Error invoking remote method '[^']+': (Error: )?/, "");
}

export function readPref<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const v = localStorage.getItem(`rinka.${key}`) as T | null;
    return v && allowed.includes(v) ? v : fallback;
  } catch {
    return fallback;
  }
}

export function writePref(key: string, value: string): void {
  try {
    localStorage.setItem(`rinka.${key}`, value);
  } catch {
    // Preferences are a convenience; ignore storage failures.
  }
}
