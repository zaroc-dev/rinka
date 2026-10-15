// Discovery and rescan checks with isolated Steam libraries; no local artwork.
import assert from "node:assert/strict";
import { after, test } from "node:test";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { buildSync } from "esbuild";

const require = createRequire(import.meta.url);
const app = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const scratch = mkdtempSync(path.join(tmpdir(), "rinka-workshop-"));
after(() => {
  assert.ok(path.resolve(scratch).startsWith(path.resolve(tmpdir(), "rinka-workshop-")));
  rmSync(scratch, { recursive: true, force: true });
});
const steam = path.join(scratch, "Steam");
const extra = path.join(scratch, "Other library");
const content = (root) => path.join(root, "steamapps", "workshop", "content", "431960");
const assets = path.join(steam, "steamapps", "common", "wallpaper_engine", "assets");
for (const [root, id] of [[steam, "100"], [extra, "200"]]) {
  const item = path.join(content(root), id);
  mkdirSync(item, { recursive: true });
  writeFileSync(path.join(item, "project.json"), JSON.stringify({ title: id, type: "scene" }));
}
mkdirSync(assets, { recursive: true });
writeFileSync(path.join(steam, "steamapps", "libraryfolders.vdf"),
  `"libraryfolders" { "0" { "path" ${JSON.stringify(steam)} } "1" { "path" ${JSON.stringify(extra)} } }`);

function load(source) {
  const { outputFiles } = buildSync({ entryPoints: [path.join(app, source)], bundle: true, platform: "node", format: "cjs", write: false });
  const module = { exports: {} };
  runInNewContext(outputFiles[0].text, {
    module, exports: module.exports, console, Buffer,
    process: { platform: "win32", env: {}, pid: process.pid },
    require: (name) => name === "node:child_process"
      ? { execFileSync: () => `SteamPath    REG_SZ    ${steam}\r\n` }
      : name === "node:os" ? { homedir: () => scratch } : require(name),
  });
  return module.exports;
}
const { findWorkshop, workshopFolder, Workshop } = load("src/main/workshop.ts");
const plain = (value) => JSON.parse(JSON.stringify(value));

test("Windows registry and VDF discovery deduplicate library aliases", () => {
  assert.deepEqual(plain(findWorkshop()), { workshopDirs: [content(steam), content(extra)], assetsDir: assets });
});

test("the folder picker accepts Steam, its installation, and workshop content", () => {
  for (const folder of [steam, path.join(steam, "steamapps"), path.dirname(assets), content(steam)]) {
    assert.deepEqual(plain(workshopFolder(folder)), { workshopDirs: [content(steam)], assetsDir: assets });
    assert.equal(findWorkshop(folder).workshopDirs.length, 2);
  }
});

test("invalid selections fail; disconnected saved folders retain automatic discovery", () => {
  assert.throws(() => workshopFolder(scratch), /Choose a Steam library/);
  assert.deepEqual(plain(findWorkshop(path.join(scratch, "missing"))), plain(findWorkshop()));
});

test("manual folder selection survives saving and reopening configuration", () => {
  const { ConfigStore } = load("src/main/library.ts");
  const file = path.join(scratch, "config.json");
  const config = new ConfigStore(file);
  config.config.workshopFolder = content(steam);
  config.save();
  assert.equal(new ConfigStore(file).config.workshopFolder, content(steam));
  assert.equal(JSON.parse(readFileSync(file, "utf8")).version, 1);
});

test("rescans keep active checks and discard results from a removed folder", async () => {
  const manual = path.join(scratch, "manual", "431960");
  mkdirSync(path.join(manual, "300"), { recursive: true });
  writeFileSync(path.join(manual, "300", "project.json"), JSON.stringify({ title: "300", type: "scene" }));
  const pending = [];
  const updates = [];
  const workshop = new Workshop(() => null, () => new Map(), (item) => updates.push(item.id));
  workshop.run = () => new Promise((resolve) => pending.push(resolve));
  assert.equal(workshop.scan().items.length, 2);
  assert.equal(pending.length, 2);
  assert.equal(workshop.scan(manual).items.length, 3);
  assert.equal(pending.length, 3);
  assert.equal(workshop.scan().items.length, 2);
  assert.equal(pending.length, 3);
  for (const resolve of pending) resolve({ status: "supported" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(updates.sort(), ["100", "200"]);
});
