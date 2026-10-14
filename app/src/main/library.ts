// Project library and application configuration stored under userData.
//
// Projects are self-contained directories (`<id>/project.json` plus
// `assets/`), referenced by relative paths. Output assignments are
// application configuration, kept separate from portable project content.

import { createHash, randomUUID } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  closeSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import type { Content, Fit, ImportResult, Project } from "../shared/api";
import { FITS } from "../shared/api";

const MANIFEST = "project.json";
const HEX = /^#[0-9a-f]{6}$/;
const ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const IMAGE_EXTENSIONS = new Set([".png", ".jpg", ".jpeg"]);
const VIDEO_EXTENSIONS = new Set([".mp4", ".m4v", ".mov", ".mkv", ".webm"]);
export const IMPORT_EXTENSIONS = { image: [...IMAGE_EXTENSIONS], video: [...VIDEO_EXTENSIONS] };

export function writeAtomic(file: string, data: string | Buffer): void {
  const tmp = `${file}.${process.pid}.tmp`;
  writeFileSync(tmp, data);
  renameSync(tmp, file);
}

/**
 * Structural check before writing; the engine performs full validation.
 * Fields the editor does not edit (scene content, provenance) pass through.
 */
export function checkProject(p: unknown): Project {
  const project = p as Project;
  // "wpe.project" is the format id from before the project was named Rinka.
  const known = project?.format === "rinka.project" || (project?.format as string) === "wpe.project";
  if (!known || project.version !== 1 || typeof project.title !== "string") {
    throw new Error("invalid project");
  }
  const c = project.content as Content | undefined;
  let content: Content;
  if (c?.type === "scene" && Array.isArray(c.layers)) {
    content = c;
  } else if (c?.type === "color" && HEX.test(c.color)) {
    content = { type: "color", color: c.color };
  } else if (
    (c?.type === "image" || c?.type === "video") &&
    typeof c.source === "string" &&
    FITS.includes((c.fit ?? "cover") as Fit) &&
    HEX.test(c.background ?? "#000000")
  ) {
    content = { type: c.type, source: c.source, fit: c.fit ?? "cover", background: c.background ?? "#000000" };
  } else {
    throw new Error("invalid project content");
  }
  const checked: Project = { format: "rinka.project", version: 1, title: project.title, content };
  if (project.provenance) checked.provenance = project.provenance;
  return checked;
}

/** Detects supported media by signature rather than extension. */
function sniff(file: string): "image" | "video" | null {
  const fd = openSync(file, "r");
  try {
    const head = Buffer.alloc(12);
    readSync(fd, head, 0, 12, 0);
    if (head.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) return "image";
    if (head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff) return "image";
    // ISO base media (MP4, MOV): a box whose type is "ftyp".
    if (head.toString("latin1", 4, 8) === "ftyp") return "video";
    // Matroska and WebM: the EBML magic.
    if (head.subarray(0, 4).equals(Buffer.from([0x1a, 0x45, 0xdf, 0xa3]))) return "video";
    return null;
  } finally {
    closeSync(fd);
  }
}

function titleFromFile(file: string): string {
  const base = path.basename(file, path.extname(file)).replace(/[_\-.]+/g, " ").trim();
  return base ? base[0]!.toUpperCase() + base.slice(1) : "Untitled";
}

function safeAssetName(file: string): string {
  const ext = path.extname(file).toLowerCase();
  const stem = path
    .basename(file, path.extname(file))
    .normalize("NFKD")
    .replace(/[^A-Za-z0-9._-]+/g, "-")
    .replace(/^[-.]+|-+$/g, "");
  return `${stem || "image"}${ext}`;
}

function sha256(file: string): string {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

export interface StoredEntry {
  id: string;
  dir: string;
  project: Project;
  added: number;
}

export class Library {
  constructor(readonly root: string) {
    mkdirSync(root, { recursive: true });
  }

  dir(id: string): string {
    if (!ID.test(id)) throw new Error(`invalid project id ${id}`);
    return path.join(this.root, id);
  }

  list(): StoredEntry[] {
    const entries: StoredEntry[] = [];
    for (const id of readdirSync(this.root)) {
      if (!ID.test(id)) continue;
      try {
        const dir = this.dir(id);
        const project = checkProject(JSON.parse(readFileSync(path.join(dir, MANIFEST), "utf8")));
        entries.push({ id, dir, project, added: statSync(dir).birthtimeMs || statSync(dir).ctimeMs });
      } catch (e) {
        console.warn(`skipping library entry ${id}:`, (e as Error).message);
      }
    }
    return entries;
  }

  get(id: string): StoredEntry {
    const dir = this.dir(id);
    const project = checkProject(JSON.parse(readFileSync(path.join(dir, MANIFEST), "utf8")));
    return { id, dir, project, added: statSync(dir).birthtimeMs || statSync(dir).ctimeMs };
  }

  createColor(title: string, color: string): StoredEntry {
    const project = checkProject({ format: "rinka.project", version: 1, title, content: { type: "color", color } });
    const id = randomUUID();
    mkdirSync(this.dir(id));
    this.save(id, project);
    return this.get(id);
  }

  /**
   * Copies image and video files into new projects. Folders are scanned one
   * level deep. Files whose content already exists in the library are
   * skipped.
   */
  importFiles(paths: string[]): { imported: StoredEntry[]; skipped: ImportResult["skipped"] } {
    const files: string[] = [];
    for (const p of paths) {
      if (statSync(p).isDirectory()) {
        for (const name of readdirSync(p).sort((a, b) => a.localeCompare(b))) {
          const file = path.join(p, name);
          const ext = path.extname(name).toLowerCase();
          if ((IMAGE_EXTENSIONS.has(ext) || VIDEO_EXTENSIONS.has(ext)) && statSync(file).isFile()) files.push(file);
        }
      } else {
        files.push(p);
      }
    }

    const known = new Set<string>();
    for (const entry of this.list()) {
      if (entry.project.content.type !== "image" && entry.project.content.type !== "video") continue;
      const asset = path.join(entry.dir, entry.project.content.source);
      if (existsSync(asset)) known.add(sha256(asset));
    }

    const imported: StoredEntry[] = [];
    const skipped: ImportResult["skipped"] = [];
    for (const file of files) {
      try {
        const kind = sniff(file);
        if (!kind) {
          skipped.push({ path: file, reason: "not a supported image or video" });
          continue;
        }
        const hash = sha256(file);
        if (known.has(hash)) {
          skipped.push({ path: file, reason: "already in the library" });
          continue;
        }
        known.add(hash);
        const id = randomUUID();
        const dir = this.dir(id);
        const source = `assets/${safeAssetName(file)}`;
        mkdirSync(path.join(dir, "assets"), { recursive: true });
        copyFileSync(file, path.join(dir, source));
        this.save(
          id,
          checkProject({
            format: "rinka.project",
            version: 1,
            title: titleFromFile(file),
            content: { type: kind, source, fit: "cover", background: "#000000" },
          }),
        );
        imported.push(this.get(id));
      } catch (e) {
        skipped.push({ path: file, reason: (e as Error).message });
      }
    }
    return { imported, skipped };
  }

  save(id: string, project: Project): void {
    const p = checkProject(project);
    writeAtomic(path.join(this.dir(id), MANIFEST), `${JSON.stringify(p, null, 2)}\n`);
  }

  remove(id: string): void {
    rmSync(this.dir(id), { recursive: true, force: true });
  }

  /** Maps a host-reported project directory back to a library id. */
  idForDir(dir: string): string | null {
    const rel = path.relative(this.root, dir);
    return ID.test(rel) ? rel : null;
  }

  /** Absolute path of a project file, refusing paths outside the project. */
  resolve(id: string, relative: string): string | null {
    const dir = this.dir(id);
    const file = path.normalize(path.join(dir, relative));
    return file.startsWith(dir + path.sep) ? file : null;
  }
}

export interface Config {
  version: 1;
  /** Output name -> library id. */
  assignments: Record<string, string>;
  /** Show the tray icon (default true). */
  trayIcon?: boolean;
}

export class ConfigStore {
  config: Config;

  constructor(readonly file: string) {
    try {
      const c = JSON.parse(readFileSync(file, "utf8")) as Config;
      this.config = c.version === 1 && c.assignments ? c : { version: 1, assignments: {} };
    } catch {
      this.config = { version: 1, assignments: {} };
    }
  }

  save(): void {
    writeAtomic(this.file, `${JSON.stringify(this.config, null, 2)}\n`);
  }
}
