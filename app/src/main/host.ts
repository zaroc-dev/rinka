// Client for the native playback host's control socket.
//
// The host is a separate process that owns the wallpaper surfaces. It is
// started detached so wallpapers keep playing after the editor closes, and
// the editor reconnects to an already running host on start.

import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, statSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { userInfo } from "node:os";

export const PROTOCOL_VERSION = 1;

export class HostError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export function defaultSocketPath(): string | null {
  if (process.platform === "win32") {
    const user = process.env.USERNAME ?? userInfo().username;
    return `\\\\.\\pipe\\rinka-host-${user}`;
  }
  const runtime = process.env.XDG_RUNTIME_DIR;
  return runtime ? path.join(runtime, "rinka", "host.sock") : null;
}

/**
 * Finds the host binary: $RINKA_HOST_BIN, else the one shipped in a packaged
 * app's resources, else the newest workspace build.
 */
export function findHostBinary(repoRoot: string, resourcesDir?: string): string | null {
  if (process.env.RINKA_HOST_BIN) return process.env.RINKA_HOST_BIN;
  const exe = process.platform === "win32" ? "rinka-host.exe" : "rinka-host";
  if (resourcesDir && existsSync(path.join(resourcesDir, exe))) return path.join(resourcesDir, exe);
  const candidates = ["release", "debug"]
    .map((profile) => path.join(repoRoot, "target", profile, exe))
    .filter((p) => existsSync(p))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return candidates[0] ?? null;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

export interface Hello {
  protocol: number;
  host_version: string;
  engine_version: string;
  platform: string;
  pid: number;
}

export class HostConnection {
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private buffer = "";
  private closedHandlers: (() => void)[] = [];

  private constructor(private readonly socket: net.Socket) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.onData(chunk));
    socket.on("close", () => {
      for (const p of this.pending.values()) p.reject(new HostError("disconnected", "host disconnected"));
      this.pending.clear();
      for (const h of this.closedHandlers) h();
    });
    socket.on("error", () => {});
  }

  static connect(socketPath: string): Promise<HostConnection> {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(socketPath);
      socket.once("connect", () => resolve(new HostConnection(socket)));
      socket.once("error", reject);
    });
  }

  onClose(handler: () => void): void {
    this.closedHandlers.push(handler);
  }

  close(): void {
    this.socket.end();
  }

  request<T = unknown>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.socket.write(`${JSON.stringify({ id, cmd, ...args })}\n`);
    });
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let newline: number;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      let msg: { id: number; ok: boolean; result?: unknown; error?: { code: string; message: string } };
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      const p = this.pending.get(msg.id);
      if (!p) continue;
      this.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.result);
      else p.reject(new HostError(msg.error?.code ?? "unknown", msg.error?.message ?? "host error"));
    }
  }
}

/** Connects to a running host, or starts one and waits for its socket. */
export async function connectOrSpawn(
  socketPath: string,
  binary: string | null,
  logFile: string,
): Promise<{ conn: HostConnection; hello: Hello; spawned: boolean }> {
  const handshake = async (conn: HostConnection) => {
    try {
      return await conn.request<Hello>("hello", { protocol: PROTOCOL_VERSION, client: "rinka-app" });
    } catch (error) {
      conn.close();
      throw error;
    }
  };
  try {
    const conn = await HostConnection.connect(socketPath);
    return { conn, hello: await handshake(conn), spawned: false };
  } catch (e) {
    if (e instanceof HostError) throw e;
  }
  if (!binary) throw new HostError("no_binary", "rinka-host binary not found; build it with `cargo build -p rinka-host`");

  mkdirSync(path.dirname(logFile), { recursive: true });
  const log = openSync(logFile, "a");
  const child = (() => {
    try {
      return spawn(binary, ["serve", "--socket", socketPath], {
        detached: true,
        windowsHide: true,
        stdio: ["ignore", log, log],
      });
    } finally { closeSync(log); }
  })();
  child.unref();
  let exited: number | null = null;
  let spawnError: Error | null = null;
  child.once("exit", (code) => (exited = code ?? -1));
  child.once("error", (error) => (spawnError = error));

  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 100));
    if (spawnError) throw new HostError("spawn_failed", String(spawnError));
    if (exited !== null) throw new HostError("spawn_failed", `rinka-host exited with ${exited}; see ${logFile}`);
    try {
      const conn = await HostConnection.connect(socketPath);
      return { conn, hello: await handshake(conn), spawned: true };
    } catch (e) {
      if (e instanceof HostError) throw e;
    }
  }
  throw new HostError("spawn_timeout", `rinka-host did not open ${socketPath}; see ${logFile}`);
}
