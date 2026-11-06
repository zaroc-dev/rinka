// "Optimize video": re-encodes a video project's file through the host
// (`rinka-host transcode`) and points the project at the result. The original
// file stays in the project so the change can be undone.
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, readdirSync, renameSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import type { OptimizeOptions, VideoEncoder, VideoFileInfo } from "../shared/api";

/** What `rinka-host transcode` reports when it finishes. */
export interface TranscodeOutcome {
  encoder: VideoEncoder;
  width: number;
  height: number;
  frame_rate: number;
  frames: number;
  bytes: number;
  dropped_audio: boolean;
  reduced_to_8_bit: boolean;
}

function run(bin: string, args: string[], onLine?: (line: string) => void): { child: ChildProcess; done: Promise<string> } {
  const child = spawn(bin, args, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  const done = new Promise<string>((resolve, reject) => {
    let out = "";
    let err = "";
    let pending = "";
    child.stdout!.setEncoding("utf8").on("data", (d: string) => {
      out += d;
      pending += d;
      const lines = pending.split("\n");
      pending = lines.pop()!;
      for (const line of lines) if (line) onLine?.(line);
    });
    child.stderr!.setEncoding("utf8").on("data", (d: string) => (err += d));
    child.on("error", reject);
    child.on("close", (code, signal) => {
      if (signal) return reject(new Error("cancelled"));
      if (code !== 0) {
        // anyhow prints "Error: …" then its causes; the last line is the root.
        const lines = err.trim().split("\n").filter((l) => !l.startsWith("["));
        return reject(new Error(lines.slice(-2).join(" ").replace(/^Error:\s*/, "").trim() || `exited with ${code}`));
      }
      resolve(out);
    });
  });
  return { child, done };
}

/** Re-encoded copies of `original` in `dir` (names from `optimizedName`). */
export function optimizedCopies(dir: string, original: string): string[] {
  const stem = path.basename(original, path.extname(original)).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`^${stem}-\\d+p\\d*(-\\d+)?\\.mp4$`);
  try {
    return readdirSync(path.join(dir, "assets"))
      .filter((f) => pattern.test(f))
      .map((f) => `assets/${f}`);
  } catch {
    return [];
  }
}

/** Asset name for the re-encoded copy of `original`, not yet in `dir`. */
export function optimizedName(dir: string, original: string, height: number, fps: number | null): string {
  const stem = path.basename(original, path.extname(original));
  const base = `${stem}-${height}p${fps ? Math.round(fps) : ""}`;
  for (let n = 1; ; n++) {
    const name = `assets/${n === 1 ? base : `${base}-${n}`}.mp4`;
    if (!existsSync(path.join(dir, name))) return name;
  }
}

export class VideoOptimizer {
  private readonly jobs = new Map<string, { child: ChildProcess; progress: number }>();

  constructor(private readonly hostBinary: () => string | null) {}

  private bin(): string {
    const bin = this.hostBinary();
    if (!bin) throw new Error("rinka-host is not built");
    return bin;
  }

  async probe(file: string): Promise<VideoFileInfo> {
    const out = await run(this.bin(), ["video-probe", file]).done;
    const info = JSON.parse(out) as { width: number; height: number; frame_rate: number | null; duration: number | null; codec: string };
    return {
      width: info.width,
      height: info.height,
      frameRate: info.frame_rate,
      duration: info.duration,
      codec: info.codec,
      bytes: statSync(file).size,
    };
  }

  progress(id: string): number | null {
    return this.jobs.get(id)?.progress ?? null;
  }

  /**
   * Re-encodes `input` into `output`. Writes a temporary file beside it and
   * moves it into place only when the encode succeeded.
   */
  async transcode(
    id: string,
    input: string,
    output: string,
    options: OptimizeOptions,
    encoder: VideoEncoder,
    onProgress: (progress: number) => void,
  ): Promise<TranscodeOutcome> {
    if (this.jobs.has(id)) throw new Error("this video is already being optimized");
    const part = path.join(path.dirname(output), `.${path.basename(output)}.part`);
    const args = ["transcode", input, "--out", part, "--encoder", encoder];
    if (options.maxHeight) args.push("--max-height", String(options.maxHeight));
    if (options.maxFps) args.push("--max-fps", String(options.maxFps));
    let outcome: TranscodeOutcome | null = null;
    const { child, done } = run(this.bin(), args, (line) => {
      const message = JSON.parse(line) as { progress?: number } & Partial<TranscodeOutcome>;
      if (typeof message.progress === "number") {
        const job = this.jobs.get(id);
        if (job) job.progress = message.progress;
        onProgress(message.progress);
      } else {
        outcome = message as TranscodeOutcome;
      }
    });
    this.jobs.set(id, { child, progress: 0 });
    try {
      await done;
      if (!outcome) throw new Error("the encoder returned no result");
      renameSync(part, output);
      return outcome;
    } catch (e) {
      rmSync(part, { force: true });
      throw e;
    } finally {
      this.jobs.delete(id);
    }
  }

  cancel(id: string): void {
    this.jobs.get(id)?.child.kill();
  }

  /** Stops every running encode (the editor is quitting). */
  cancelAll(): void {
    for (const job of this.jobs.values()) job.child.kill();
  }
}
