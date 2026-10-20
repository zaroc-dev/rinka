// Real Windows video playback, thumbnails, pause/seek/loop/restore and screen
// checks. Uses its own pipe and generated, lossless teal videos.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { build } from "esbuild";
import { root, directory as fixtures, generateFixtures } from "./video-fixtures.mjs";

assert.equal(process.platform, "win32");
generateFixtures();
const binary = process.env.RINKA_HOST_BIN ?? path.join(root, "target/windows-video/debug/rinka-host.exe");
const screenHelper = process.env.RINKA_VIDEO_SCREEN_BIN ?? path.join(path.dirname(binary), "examples/windows_video_screen.exe");
const screenChecks = !process.argv.includes("--no-screen");
const directory = path.join(root, "target/windows-video-smoke", String(process.pid));
const pipe = `\\\\.\\pipe\\rinka-video-smoke-${process.pid}`;
const bundled = await build({ entryPoints: [path.join(root, "app/src/main/host.ts")], bundle: true, platform: "node", format: "esm", write: false });
const { HostConnection, connectOrSpawn } = await import(`data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`);
const project = async (name, content) => {
  const dir = path.join(directory, "projects", name);
  await mkdir(dir, { recursive: true });
  await writeFile(path.join(dir, "project.json"), JSON.stringify({ format: "rinka.project", version: 1, title: name, content }));
  return dir;
};
const clip = await project("clip", { type: "video", source: "clip.mkv" });
const { copyFile } = await import("node:fs/promises");
await copyFile(path.join(fixtures, "bt709-limited.mkv"), path.join(clip, "clip.mkv"));
const broken = await project("broken", { type: "video", source: "empty.mp4" });
await writeFile(path.join(broken, "empty.mp4"), "");
const still = await project("still", { type: "color", color: "#1e6f73" });
let conn;
let pid;
let cover;
const request = (cmd, args) => Promise.race([
  conn.request(cmd, args),
  delay(15_000, undefined, { ref: false }).then(() => { throw new Error(`${cmd} timed out`); }),
]);
const until = async (check) => {
  for (let i = 0; i < 100; i++) { const value = await check(); if (value) return value; await delay(50); }
  throw new Error("video condition timed out");
};
const screenArgs = (mode, output) => [mode, ...output.logical_position.map(String), ...output.logical_size.map(String)];
try {
  const started = await connectOrSpawn(pipe, binary, path.join(directory, "host.log"));
  conn = started.conn; pid = started.hello.pid;
  await request("assign", { project: clip });
  const output = await until(async () => (await request("outputs")).find(o => o.video && !o.video.hidden && o.video.position != null));
  const state = async () => (await request("outputs")).find(o => o.name === output.name);
  assert.equal(output.video.codec, "ffv1");
  assert.deepEqual(output.video.size, [64, 64]);
  assert.equal(output.surface.presenting, true);
  await request("pause");
  const paused = (await state()).video.position;
  await delay(200);
  assert.equal((await state()).video.position, paused);
  await request("seek", { output: output.name, position: 0.6 });
  await until(async () => { const s = await state(); return s.video.paused && s.video.position >= 0.6 && s.video.position < 0.65; });
  await assert.rejects(request("assign", { output: output.name, project: broken }), { code: "project" });
  assert.equal((await state()).assigned, clip);
  await request("resume");
  await request("seek", { output: output.name, position: 1.0 });
  await delay(500);
  const looped = (await state()).video;
  assert.ok(looped.position < 0.8 && looped.position >= 0, "video must loop on the continuous timeline");

  if (screenChecks) {
    const capture = path.join(directory, "desktop.png");
    const screen = JSON.parse(execFileSync(screenHelper, [...screenArgs("capture", output), capture], { windowsHide: true, encoding: "utf8" }));
    assert.deepEqual(screen.rgb, [26, 112, 115], "real desktop pixels must match the BT.709 equation");
    console.log(JSON.stringify({ output: output.name, nativeScreenRgb: screen.rgb, capture }));

    cover = spawn(screenHelper, screenArgs("cover", output), { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let coverError = "";
    cover.stderr.on("data", chunk => { coverError += chunk; });
    await until(async () => (await state()).video.hidden);
    const hiddenAt = (await state()).video.position;
    await delay(250);
    assert.equal((await state()).video.position, hiddenAt, "covered video must stop its clock");
    if (cover.exitCode === null) await once(cover, "exit");
    assert.equal(cover.exitCode, 0, coverError);
    await until(async () => !(await state()).video.hidden);
    await delay(150);
    assert.equal((await state()).video.paused, false);
    cover = undefined;
  } else {
    console.log("Screen and covering-window checks skipped (--no-screen).");
  }

  conn.close(); conn = await HostConnection.connect(pipe);
  await request("hello", { protocol: 1 });
  assert.equal((await state()).assigned, clip, "playback survives disconnection");
  await request("assign", { project: still });
  for (const o of await request("outputs")) assert.equal(o.video, null);
  await request("shutdown"); conn.close(); conn = undefined;
  await delay(300); pid = undefined;

  await writeFile(path.join(directory, "config.json"), JSON.stringify({ version: 1, assignments: { [output.name]: "clip" } }));
  const restored = spawn(binary, ["serve", "--socket", pipe, "--restore", directory], { windowsHide: true, stdio: "ignore" });
  pid = restored.pid;
  await until(async () => { try { conn = await HostConnection.connect(pipe); return true; } catch { return false; } });
  await request("hello", { protocol: 1 });
  assert.equal((await state()).video.codec, "ffv1");
  assert.equal((await state()).surface.presenting, true);
  const thumbnail = path.join(directory, "video-thumb.png");
  const rendered = JSON.parse(execFileSync(binary, ["thumbnail"], { windowsHide: true, encoding: "utf8",
    input: JSON.stringify([{ project: clip, out: thumbnail, width: 64, height: 64 }]) }));
  assert.equal(rendered.ok, true);
  assert.deepEqual(rendered.source_size, [64, 64]);
  assert.deepEqual(rendered.color, { matrix: "bt709", full_range: false });
  assert.ok(rendered.duration > 1);
  assert.ok(existsSync(thumbnail));
  console.log(JSON.stringify({ thumbnail: rendered, pausedSeek: true, loop: true, coveredPause: screenChecks, restore: true }));
  console.log("Windows video smoke checks passed.");
} finally {
  if (cover?.exitCode === null) cover.kill();
  if (conn) { try { await request("shutdown"); } finally { conn.close(); } }
  if (pid) {
    for (let i = 0; i < 40; i++) { try { process.kill(pid, 0); } catch { pid = undefined; break; } await delay(50); }
    if (pid) { process.kill(pid); throw new Error("video smoke host did not shut down"); }
  }
}
