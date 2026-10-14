// Decodes images with the engine off the page's main thread.

import init, { decodeImage, decodeImageRaw } from "./wasm/rinka_wasm.js";

export interface DecodeRequest {
  id: number;
  bytes: ArrayBuffer;
  maxDimension: number;
  /** Keep stored values (gamma-space scene textures). */
  raw?: boolean;
}

export type DecodeResponse =
  | {
      id: number;
      ok: true;
      width: number;
      height: number;
      sourceWidth: number;
      sourceHeight: number;
      format: number;
      pixels: ArrayBuffer;
    }
  | { id: number; ok: false; error: string };

const ready = init({ module_or_path: new URL("rinka_wasm_bg.wasm", import.meta.url) });

self.onmessage = async (event: MessageEvent<DecodeRequest>) => {
  const { id, bytes, maxDimension, raw } = event.data;
  await ready;
  try {
    const image = (raw ? decodeImageRaw : decodeImage)(new Uint8Array(bytes), maxDimension);
    const pixels = image.takePixels();
    const response: DecodeResponse = {
      id,
      ok: true,
      width: image.width,
      height: image.height,
      sourceWidth: image.sourceWidth,
      sourceHeight: image.sourceHeight,
      format: image.format,
      pixels: pixels.buffer as ArrayBuffer,
    };
    image.free();
    (self as unknown as Worker).postMessage(response, [response.pixels]);
  } catch (e) {
    const response: DecodeResponse = { id, ok: false, error: e instanceof Error ? e.message : String(e) };
    (self as unknown as Worker).postMessage(response);
  }
};
