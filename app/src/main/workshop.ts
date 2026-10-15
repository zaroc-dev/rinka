// Wallpaper Engine workshop items: discovery, dry-run reports and import.
//
// Conversion runs in the native host (`rinka-host import`), outside the
// editor. Each item is first checked without writing; importing writes a
// new library project atomically.

import { execFileSync, spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import type { ImportReport, WorkshopItem, WorkshopScan } from "../shared/api";

const APP_ID = "431960";

/** Steam's registered install location, with standard Windows fallbacks. */
function windowsSteamRoots(): string[] {
  const roots: string[] = [];
  for (const [key, value] of [
    ["HKCU\\Software\\Valve\\Steam", "SteamPath"],
    ["HKLM\\SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath"],
    ["HKLM\\SOFTWARE\\Valve\\Steam", "InstallPath"],
  ] as const) {
    try {
      const result = execFileSync("reg.exe", ["query", key, "/v", value], {
        encoding: "utf8",
        windowsHide: true,
        timeout: 2_000,
        stdio: ["ignore", "pipe", "ignore"],
      });
      const install = /^\s*\S+\s+REG_SZ\s+(.+?)\s*$/m.exec(result)?.[1];
      if (install) roots.push(install);
    } catch {
      // Missing/inaccessible keys are normal; try the other locations.
    }
  }
  for (const directory of [process.env["ProgramFiles(x86)"], process.env.ProgramFiles]) {
    if (directory) roots.push(path.join(directory, "Steam"));
  }
  return roots;
}

/** Steam library roots, from the default install and libraryfolders.vdf. */
function steamLibraries(): string[] {
  const home = os.homedir();
  const roots = process.platform === "win32"
    ? windowsSteamRoots()
    : [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"].map((p) => path.join(home, p));
  const libraries = new Map<string, string>();
  const add = (root: string) => {
    try {
      if (!statSync(path.join(root, "steamapps")).isDirectory()) return;
      const resolved = realpathSync(root);
      const key = process.platform === "win32" ? resolved.toLowerCase() : resolved;
      if (!libraries.has(key)) libraries.set(key, resolved);
    } catch {
      // Disconnected Steam library drives are not available for scanning.
    }
  };
  for (const root of roots) add(root);
  for (const root of [...libraries.values()]) {
    try {
      const vdf = readFileSync(path.join(root, "steamapps", "libraryfolders.vdf"), "utf8");
      for (const m of vdf.matchAll(/"(?:path|\d+)"\s+"([^"]+)"/g)) {
        const library = m[1]!.replace(/\\\\/g, "\\");
        if (path.isAbsolute(library)) add(library);
      }
    } catch {
      // No library list; the default root is enough.
    }
  }
  return [...libraries.values()];
}

/** Resolve a selected Steam library, installation, or workshop content folder. */
export function workshopFolder(folder: string): { workshopDirs: string[]; assetsDir: string | null } {
  const workshopDirs: string[] = [];
  let assetsDir: string | null = null;
  for (let dir = realpathSync(folder); ; dir = path.dirname(dir)) {
    const workshop = [
      ...(path.basename(dir) === APP_ID ? [dir] : []),
      path.join(dir, "steamapps", "workshop", "content", APP_ID),
      path.join(dir, "workshop", "content", APP_ID),
    ].find((p) => existsSync(p) && statSync(p).isDirectory());
    if (workshop && !workshopDirs.length) workshopDirs.push(workshop);
    assetsDir ??= [
      ...(path.basename(dir).toLowerCase() === "wallpaper_engine" ? [path.join(dir, "assets")] : []),
      path.join(dir, "steamapps", "common", "wallpaper_engine", "assets"),
      path.join(dir, "common", "wallpaper_engine", "assets"),
    ].find((p) => existsSync(p) && statSync(p).isDirectory()) ?? null;
    if (path.dirname(dir) === dir) break;
  }
  if (!workshopDirs.length && !assetsDir) {
    throw new Error("Choose a Steam library, the wallpaper_engine installation, or the workshop's 431960 folder.");
  }
  return { workshopDirs, assetsDir };
}

export function findWorkshop(folder?: string): { workshopDirs: string[]; assetsDir: string | null } {
  const libraries = steamLibraries();
  const workshopDirs = libraries
    .map((l) => path.join(l, "steamapps", "workshop", "content", APP_ID))
    .filter((d) => existsSync(d));
  let assetsDir =
    libraries.map((l) => path.join(l, "steamapps", "common", "wallpaper_engine", "assets")).find((d) => existsSync(d)) ??
    null;
  if (folder) {
    try {
      const manual = workshopFolder(folder);
      workshopDirs.push(...manual.workshopDirs);
      assetsDir = manual.assetsDir ?? assetsDir;
    } catch {
      // A saved folder may be on a disconnected drive; keep automatic discovery.
    }
  }
  const unique = new Map(workshopDirs.map((dir) => {
    const resolved = realpathSync(dir);
    return [process.platform === "win32" ? resolved.toLowerCase() : resolved, resolved];
  }));
  return { workshopDirs: [...unique.values()], assetsDir };
}

function readProject(dir: string): { title: string; type: string; preview: string | null } | null {
  try {
    const text = readFileSync(path.join(dir, "project.json"), "utf8").replace(/^﻿/, "");
    const p = JSON.parse(text) as { title?: string; type?: string; preview?: string };
    const preview = p.preview && existsSync(path.join(dir, p.preview)) ? p.preview : null;
    return { title: p.title ?? "Untitled", type: (p.type ?? "").toLowerCase(), preview };
  } catch {
    return null;
  }
}

export class Workshop {
  private items = new Map<string, WorkshopItem>();
  private dirs: string[] = [];
  private assetsDir: string | null = null;
  private queue: string[] = [];
  private running = 0;
  private checking = new Set<WorkshopItem>();

  constructor(
    private readonly hostBinary: () => string | null,
    private readonly importedIds: () => Map<string, string>,
    private readonly onItem: (item: WorkshopItem) => void,
  ) {}

  /** Lists items and checks those without a report, a few at a time. */
  scan(folder?: string): WorkshopScan {
    const { workshopDirs, assetsDir } = findWorkshop(folder);
    this.dirs = workshopDirs;
    const sameAssets = this.assetsDir === assetsDir;
    this.assetsDir = assetsDir;
    const imported = this.importedIds();
    const previous = this.items;
    this.items = new Map();
    this.queue = [];
    for (const root of workshopDirs) {
      for (const id of readdirSync(root)) {
        const dir = path.join(root, id);
        if (!statSync(dir).isDirectory()) continue;
        const meta = readProject(dir);
        if (!meta) continue;
        const existing = previous.get(id);
        const item: WorkshopItem = existing?.dir === dir && sameAssets ? existing : {
          id,
          dir,
          title: meta.title,
          type: meta.type,
          preview: meta.preview ? `rinka://app/workshop/${id}/${encodeURI(meta.preview)}` : null,
          report: null,
          error: null,
          importedId: imported.get(id) ?? null,
        };
        item.importedId = imported.get(id) ?? null;
        this.items.set(id, item);
        if (!item.report && !item.error && !this.checking.has(item) && !this.queue.includes(id)) this.queue.push(id);
      }
    }
    this.pump();
    return { workshopDirs, assetsDir, folder: folder ?? null, items: [...this.items.values()] };
  }

  /** Absolute path of a file inside a workshop item, or null. */
  file(id: string, relative: string): string | null {
    const item = this.items.get(id);
    if (!item) return null;
    const file = path.normalize(path.join(item.dir, relative));
    return file.startsWith(item.dir + path.sep) ? file : null;
  }

  private pump(): void {
    while (this.running < 3 && this.queue.length) {
      const id = this.queue.shift()!;
      const item = this.items.get(id);
      if (!item) continue;
      this.running++;
      this.checking.add(item);
      this.run(item.dir, null, false)
        .then((report) => {
          item.report = report;
        })
        .catch((e: Error) => {
          item.error = e.message;
        })
        .finally(() => {
          this.running--;
          this.checking.delete(item);
          if (this.items.get(id) === item) this.onItem(item);
          this.pump();
        });
    }
  }

  /** Runs the converter; writes to `out` when given. */
  run(dir: string, out: string | null, allowPartial: boolean): Promise<ImportReport> {
    const bin = this.hostBinary();
    if (!bin) return Promise.reject(new Error("rinka-host is not built"));
    const args = ["import", dir];
    if (out) args.push("--out", out);
    if (this.assetsDir) args.push("--assets", this.assetsDir);
    if (allowPartial) args.push("--allow-partial");
    return new Promise((resolve, reject) => {
      const child = spawn(bin, args, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
      let out = "";
      let err = "";
      child.stdout.setEncoding("utf8").on("data", (d: string) => (out += d));
      child.stderr.setEncoding("utf8").on("data", (d: string) => (err += d));
      child.on("error", reject);
      child.on("close", (code) => {
        if (code !== 0) return reject(new Error(err.trim().split("\n").pop() || `import exited with ${code}`));
        try {
          resolve(JSON.parse(out) as ImportReport);
        } catch {
          reject(new Error("the converter returned no report"));
        }
      });
    });
  }

  get(id: string): WorkshopItem | undefined {
    return this.items.get(id);
  }
}
