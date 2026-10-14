// Builds the engine for WebAssembly and generates JS bindings into
// src/renderer/wasm. Requires the Nix dev shell (cargo, wasm-bindgen).
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const app = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const root = path.dirname(app);
const run = (cmd, args) => execFileSync(cmd, args, { cwd: root, stdio: "inherit" });

run("cargo", ["build", "-p", "rinka-wasm", "--target", "wasm32-unknown-unknown", "--profile", "release-wasm"]);
run("wasm-bindgen", [
  "--target", "web",
  "--out-dir", path.join(app, "src/renderer/wasm"),
  path.join(root, "target/wasm32-unknown-unknown/release-wasm/rinka_wasm.wasm"),
]);
