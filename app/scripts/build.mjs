// Bundles main, preload and renderer into dist/.
import { build } from "esbuild";
import { cpSync, existsSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const app = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const src = (p) => path.join(app, "src", p);
const dist = path.join(app, "dist");
mkdirSync(path.join(dist, "renderer"), { recursive: true });

if (!existsSync(src("renderer/wasm/rinka_wasm.js"))) {
  console.error("missing engine bindings: run `pnpm wasm` first");
  process.exit(1);
}

const common = { bundle: true, sourcemap: true, logLevel: "info", target: "es2023" };
await Promise.all([
  build({ ...common, entryPoints: [src("main/main.ts")], outfile: path.join(dist, "main.cjs"), platform: "node", format: "cjs", external: ["electron"] }),
  // Sandboxed preloads must be CommonJS.
  build({ ...common, entryPoints: [src("preload/preload.ts")], outfile: path.join(dist, "preload.cjs"), platform: "node", format: "cjs", external: ["electron"] }),
  build({
    ...common,
    entryPoints: [src("renderer/main.tsx"), src("renderer/main.css"), src("renderer/decode-worker.ts")],
    outdir: path.join(dist, "renderer"),
    platform: "browser",
    format: "esm",
    jsx: "automatic",
    jsxImportSource: "preact",
    loader: { ".woff2": "file", ".woff": "file" },
    assetNames: "assets/[name]-[hash]",
  }),
]);
cpSync(src("renderer/index.html"), path.join(dist, "renderer/index.html"));
cpSync(src("renderer/wasm/rinka_wasm_bg.wasm"), path.join(dist, "renderer/rinka_wasm_bg.wasm"));
