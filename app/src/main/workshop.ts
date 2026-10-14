// Wallpaper Engine workshop items: discovery, dry-run reports and import.
//
// Conversion runs in the native host (`rinka-host import`), outside the
// editor. Each item is first checked without writing; importing writes a
// new library project atomically.

import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import type { ImportReport, WorkshopItem, WorkshopScan } from "../shared/api";

const APP_ID = "431960";

/** Steam library roots, from the default install and libraryfolders.vdf. */
function steamLibraries(): string[] {
  const home = os.homedir();
  const roots = [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"]
    .map((p) => path.join(home, p))
    .filter((p) => existsSync(path.join(p, "steamapps")));
  const libraries = new Set(roots.map((r) => path.resolve(r)));
  for (const root of roots) {
    try {
      const vdf = readFileSync(path.join(root, "steamapps", "libraryfolders.vdf"), "utf8");
      for (const m of vdf.matchAll(/"path"\s+"([^"]+)"/g)) libraries.add(path.resolve(m[1]!.replace(/\\\\/g, "\\")));
    } catch {
      // No library list; the default root is enough.
    }
  }
  return [...libraries];
}

export function findWorkshop(): { workshopDirs: string[]; assetsDir: string | null } {
  const libraries = steamLibraries();
  const workshopDirs = libraries
    .map((l) => path.join(l, "steamapps", "workshop", "content", APP_ID))
    .filter((d) => existsSync(d));
  const assetsDir =
    libraries.map((l) => path.join(l, "steamapps", "common", "wallpaper_engine", "assets")).find((d) => existsSync(d)) ??
    null;
  return { workshopDirs, assetsDir };
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

  constructor(
    private readonly hostBinary: () => string | null,
    private readonly importedIds: () => Map<string, string>,
    private readonly onItem: (item: WorkshopItem) => void,
  ) {}

  /** Lists items and checks those without a report, a few at a time. */
  scan(): WorkshopScan {
    const { workshopDirs, assetsDir } = findWorkshop();
    this.dirs = workshopDirs;
    this.assetsDir = assetsDir;
    const imported = this.importedIds();
    for (const root of workshopDirs) {
      for (const id of readdirSync(root)) {
        const dir = path.join(root, id);
        if (!statSync(dir).isDirectory()) continue;
        const meta = readProject(dir);
        if (!meta) continue;
        const existing = this.items.get(id);
        const item: WorkshopItem = {
          id,
          dir,
          title: meta.title,
          type: meta.type,
          preview: meta.preview ? `rinka://app/workshop/${id}/${encodeURI(meta.preview)}` : null,
          report: existing?.report ?? null,
          error: existing?.error ?? null,
          importedId: imported.get(id) ?? null,
        };
        this.items.set(id, item);
        if (!item.report && !item.error && !this.queue.includes(id)) this.queue.push(id);
      }
    }
    this.pump();
    return { workshopDirs, assetsDir, items: [...this.items.values()] };
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
      this.run(item.dir, null, false)
        .then((report) => {
          item.report = report;
        })
        .catch((e: Error) => {
          item.error = e.message;
        })
        .finally(() => {
          this.running--;
          this.onItem(item);
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
      const child = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"] });
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
