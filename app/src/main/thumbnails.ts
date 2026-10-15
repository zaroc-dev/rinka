// Engine-rendered thumbnails, cached on disk.
//
// A thumbnail's key covers the manifest, each asset's size and modification
// time, the host binary (standing in for the engine version) and the render
// size, so any change to those inputs produces a new thumbnail.

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import type { VideoColor } from "../shared/api";
import type { StoredEntry } from "./library";
import { writeAtomic } from "./library";

export const THUMB_SIZE = { width: 480, height: 270 };

interface Meta {
  key: string;
  sourceSize: [number, number] | null;
  duration?: number | null;
  color?: VideoColor | null;
}

export class Thumbnails {
  private meta: Record<string, Meta>;
  private running: Promise<void> | null = null;
  private again = false;

  constructor(
    readonly dir: string,
    private readonly hostBinary: () => string | null,
    private readonly onUpdate: () => void,
  ) {
    mkdirSync(dir, { recursive: true });
    try {
      this.meta = JSON.parse(readFileSync(path.join(dir, "meta.json"), "utf8"));
    } catch {
      this.meta = {};
    }
  }

  key(entry: StoredEntry): string {
    const hash = createHash("sha256");
    hash.update(JSON.stringify(entry.project));
    if (entry.project.content.type === "image" || entry.project.content.type === "video") {
      try {
        const s = statSync(path.join(entry.dir, entry.project.content.source));
        hash.update(`${s.size}:${s.mtimeMs}`);
      } catch {
        hash.update("missing");
      }
    }
    const bin = this.hostBinary();
    if (bin) hash.update(`${bin}:${statSync(bin).mtimeMs}`);
    hash.update(`${THUMB_SIZE.width}x${THUMB_SIZE.height}`);
    return hash.digest("hex").slice(0, 16);
  }

  /** File name of the current thumbnail, when it exists. */
  file(entry: StoredEntry): string | null {
    const meta = this.meta[entry.id];
    if (!meta || meta.key !== this.key(entry)) return null;
    const name = `${entry.id}-${meta.key}.png`;
    return existsSync(path.join(this.dir, name)) ? name : null;
  }

  sourceSize(id: string): [number, number] | null {
    return this.meta[id]?.sourceSize ?? null;
  }

  duration(id: string): number | null {
    return this.meta[id]?.duration ?? null;
  }

  videoColor(id: string): VideoColor | null {
    return this.meta[id]?.color ?? null;
  }

  /** Renders missing or stale thumbnails in one host process. */
  refresh(entries: StoredEntry[]): Promise<void> {
    if (this.running) {
      this.again = true;
      return this.running;
    }
    this.running = this.render(entries).finally(() => {
      this.running = null;
      if (this.again) {
        this.again = false;
        void this.refresh(entries);
      }
    });
    return this.running;
  }

  private async render(entries: StoredEntry[]): Promise<void> {
    const bin = this.hostBinary();
    if (!bin) return;
    const jobs = entries
      .filter((e) => !this.file(e))
      .map((e) => {
        const key = this.key(e);
        return { id: e.id, key, project: e.dir, out: path.join(this.dir, `${e.id}-${key}.png`), ...THUMB_SIZE };
      });
    if (jobs.length === 0) return;

    const results = await new Promise<string>((resolve, reject) => {
      const child = spawn(bin, ["thumbnail"], { windowsHide: true, stdio: ["pipe", "pipe", "inherit"] });
      let out = "";
      child.stdout.setEncoding("utf8").on("data", (d: string) => (out += d));
      child.on("error", reject);
      child.on("close", () => resolve(out));
      child.stdin.end(JSON.stringify(jobs.map(({ project, out, width, height }) => ({ project, out, width, height }))));
    });
    for (const line of results.split("\n").filter(Boolean)) {
      const r = JSON.parse(line) as {
        out: string;
        ok: boolean;
        error?: string;
        source_size?: [number, number];
        duration?: number;
        color?: VideoColor;
      };
      const job = jobs.find((j) => j.out === r.out);
      if (!job) continue;
      if (!r.ok) {
        console.warn(`thumbnail for ${job.id} failed: ${r.error}`);
        continue;
      }
      this.meta[job.id] = {
        key: job.key,
        sourceSize: r.source_size ?? null,
        duration: r.duration ?? null,
        color: r.color ?? null,
      };
    }
    this.prune(new Set(entries.map((e) => e.id)));
    writeAtomic(path.join(this.dir, "meta.json"), JSON.stringify(this.meta));
    this.onUpdate();
  }

  /** Removes thumbnails of deleted projects and superseded keys. */
  private prune(ids: Set<string>): void {
    for (const id of Object.keys(this.meta)) if (!ids.has(id)) delete this.meta[id];
    for (const name of readdirSync(this.dir)) {
      const m = /^(.+)-([0-9a-f]{16})\.png$/.exec(name);
      if (m && this.meta[m[1]!]?.key !== m[2]) rmSync(path.join(this.dir, name), { force: true });
    }
  }
}
