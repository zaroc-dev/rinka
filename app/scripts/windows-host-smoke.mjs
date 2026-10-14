// Runs against a separate named pipe and generated projects. The host and
// wallpaper windows are removed even when an assertion fails.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { build } from "esbuild";

assert.equal(process.platform, "win32", "this smoke check requires Windows Explorer and a GPU");
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const binary = path.join(root, "target/debug/rinka-host.exe");
const directory = path.join(root, "target/windows-smoke", String(process.pid));
const pipe = `\\\\.\\pipe\\rinka-smoke-${process.pid}`;
const bundled = await build({
  entryPoints: [path.join(root, "app/src/main/host.ts")],
  bundle: true, platform: "node", format: "esm", write: false,
});
const { HostConnection, connectOrSpawn, defaultSocketPath } = await import(
  `data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`
);
assert.equal(defaultSocketPath(), `\\\\.\\pipe\\rinka-host-${process.env.USERNAME}`);
const project = async (name, content) => {
  const dir = path.join(directory, "projects", name);
  await mkdir(dir, { recursive: true });
  await writeFile(path.join(dir, "project.json"), JSON.stringify({
    format: "rinka.project", version: 1, title: name, content,
  }));
  return dir;
};
const color = await project("color", { type: "color", color: "#1e6f73" });
const image = await project("image", { type: "image", source: "generated.png", fit: "tile" });
execFileSync(binary, ["thumbnail"], {
  windowsHide: true,
  input: JSON.stringify([{ project: color, out: path.join(image, "generated.png"), width: 32, height: 32 }]),
});
const scene = await project("scene", {
  type: "scene", width: 64, height: 64, clear_color: [30 / 255, 111 / 255, 115 / 255],
  color_space: "gamma", layers: [],
});
const invalidScene = await project("invalid-scene", {
  type: "scene", width: 64, height: 64, clear_color: [0, 0, 0], color_space: "gamma",
  layers: [{ id: "missing", name: "missing", texture: "missing.png", origin: [32, 32], size: [64, 64] }],
});
const video = await project("video", { type: "video", source: "dummy.mp4" });
await writeFile(path.join(video, "dummy.mp4"), "");

let conn;
let pid;
const request = (cmd, args) => Promise.race([
  conn.request(cmd, args),
  delay(15_000, undefined, { ref: false }).then(() => { throw new Error(`${cmd} timed out`); }),
]);
try {
  const started = await connectOrSpawn(pipe, binary, path.join(directory, "host.log"));
  conn = started.conn;
  pid = started.hello.pid;
  console.log(JSON.stringify({ hello: started.hello, pipe, directory }));
  const outputs = await request("outputs");
  assert.ok(outputs.length, "host must discover monitors before accepting clients");
  const caps = await request("capabilities");
  console.log(JSON.stringify({ gpu: { name: caps.gpu.name, backend: caps.gpu.backend }, presentation: caps.presentation, outputs }));

  const ungreeted = await HostConnection.connect(pipe);
  try {
    await assert.rejects(ungreeted.request("outputs"), { code: "hello_required" });
    await assert.rejects(ungreeted.request("hello", { protocol: 999 }), { code: "unsupported_protocol" });
    await ungreeted.request("hello", { protocol: 1 });
    assert.equal((await ungreeted.request("outputs")).length, outputs.length);
  } finally { ungreeted.close(); }

  const duplicate = spawn(binary, ["serve", "--socket", pipe], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  let duplicateError = "";
  duplicate.stderr.on("data", (chunk) => { duplicateError += chunk; });
  const [exit] = await once(duplicate, "exit");
  assert.notEqual(exit, 0, "a second host must not claim the same pipe");
  assert.match(duplicateError, /already listening/);

  await request("assign", { project: color });
  for (const output of await request("outputs")) {
    assert.equal(output.assigned, color);
    assert.equal(output.surface.presenting, true);
    assert.deepEqual(output.surface.buffer_size, output.logical_size);
  }
  await request("assign", { project: image });
  for (const output of await request("outputs")) {
    assert.equal(output.assigned, image);
    assert.equal(output.surface.presenting, true);
  }
  await request("assign", { project: scene });
  await assert.rejects(request("assign", { project: invalidScene }), { code: "project" });
  await assert.rejects(request("assign", { project: video }), { code: "project" });
  for (const output of await request("outputs")) {
    assert.equal(output.assigned, scene, "failed assignments must keep the previous wallpaper");
    assert.equal(output.surface.presenting, true);
  }
  await request("reload", { project: scene });
  await request("pause");
  assert.equal((await request("capabilities")).paused, true);
  await request("resume");
  assert.equal((await request("capabilities")).paused, false);
  await assert.rejects(request("seek", { position: -1 }), { code: "bad_request" });
  await assert.rejects(request("assign", { output: "absent", project: color }), { code: "unknown_output" });
  conn.close();
  conn = await HostConnection.connect(pipe);
  await request("hello", { protocol: 1 });
  assert.equal((await request("outputs"))[0].assigned, scene, "wallpapers survive client disconnect");
  const cpuMilliseconds = () => Number(execFileSync("powershell.exe", [
    "-NoProfile", "-Command", `(Get-Process -Id ${pid}).TotalProcessorTime.TotalMilliseconds`,
  ], { windowsHide: true, encoding: "utf8" }).trim().replace(",", "."));
  const cpuBefore = cpuMilliseconds();
  await delay(5_000);
  console.log(JSON.stringify({ staticCpuMs: cpuMilliseconds() - cpuBefore, intervalSeconds: 5 }));
  if (process.argv.includes("--hold")) {
    console.log(`Holding generated wallpapers for 30 seconds (PID ${pid}).`);
    await delay(30_000);
  }
  await request("unassign");
  for (const output of await request("outputs")) {
    assert.equal(output.assigned, null);
    assert.equal(output.surface, null);
  }
  await request("shutdown");
  conn.close();
  conn = undefined;
  await delay(300);
  assert.throws(() => process.kill(pid, 0), "the first host must exit after shutdown");
  pid = undefined;

  await writeFile(path.join(directory, "config.json"), JSON.stringify({
    version: 1, assignments: Object.fromEntries(outputs.map((output) => [output.name, "color"])),
  }));
  const restored = spawn(binary, ["serve", "--socket", pipe, "--restore", directory], {
    windowsHide: true, stdio: ["ignore", "ignore", "pipe"],
  });
  pid = restored.pid;
  let restoreError = "";
  restored.stderr.on("data", (chunk) => { restoreError += chunk; });
  for (let i = 0; i < 100; i++) {
    if (restored.exitCode !== null) throw new Error(`restore failed: ${restoreError}`);
    try { conn = await HostConnection.connect(pipe); break; } catch { await delay(100); }
  }
  assert.ok(conn, "restored host must open its pipe");
  await request("hello", { protocol: 1 });
  for (const output of await request("outputs")) {
    assert.equal(output.assigned, color);
    assert.equal(output.surface.presenting, true);
  }
  console.log("Windows host smoke checks passed.");
} finally {
  let shutdownError;
  if (conn) {
    try { await request("shutdown"); } catch (error) { shutdownError = error; }
    finally { conn.close(); }
  }
  if (pid) {
    // The detached host should exit after its shutdown reply is flushed.
    for (let i = 0; i < 20; i++) {
      try { process.kill(pid, 0); } catch { pid = undefined; break; }
      await delay(100);
    }
    if (pid) { process.kill(pid); throw new Error("host did not exit after shutdown"); }
  }
  if (shutdownError) throw shutdownError;
}
