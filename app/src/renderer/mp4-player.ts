// WebCodecs playback for the preview: demux MP4/MOV with mp4box.js, decode
// in software, and hand raw Y'CbCr planes to the engine so the preview
// converts colors exactly like native playback. Timing mirrors the native
// player: presentation timestamps, a timeline that keeps increasing across
// loops, and dropping frames that are already late.

import { createFile, DataStream, Endianness, MP4BoxBuffer } from "mp4box";

interface Chunk {
  data: Uint8Array;
  /** Presentation time in seconds within the file. */
  time: number;
  duration: number;
  key: boolean;
}

/** Decoded frames held ahead of presentation. */
const AHEAD = 6;
/**
 * Chunks queued in the decoder. Software decoders with frame threading and
 * B-frame reordering hold many chunks before emitting a frame, so input is
 * bounded separately from output.
 */
const MAX_QUEUED = 64;

export interface YuvPlanes {
  width: number;
  height: number;
  chromaWidth: number;
  chromaHeight: number;
  /** 0 BT.601, 1 BT.709, 2 BT.2020, 255 unspecified. */
  matrix: number;
  fullRange: boolean;
  data: Uint8Array;
  offsets: [number, number, number];
  strides: [number, number, number];
}

const CHROMA: Record<string, [number, number]> = { I420: [2, 2], I420A: [2, 2], I422: [2, 1], I444: [1, 1] };

export function matrixCode(m: string | null | undefined): number {
  switch (m) {
    case "bt709":
      return 1;
    case "bt601":
    case "bt470bg":
    case "smpte170m":
      return 0;
    case "bt2020-ncl":
      return 2;
    default:
      return 255;
  }
}

async function demux(bytes: ArrayBuffer): Promise<{ config: VideoDecoderConfig; chunks: Chunk[]; length: number }> {
  const file = createFile();
  let error: string | null = null;
  file.onError = (_module, message) => (error = message);
  // Parse the boxes only; samples are sliced from `bytes` using the sample
  // table, which works whatever the box order.
  // mp4box takes ownership of the buffers it parses (and may release
  // them), so it gets a copy.
  file.appendBuffer(MP4BoxBuffer.fromArrayBuffer(bytes.slice(0), 0));
  file.flush();
  const info = file.getInfo();
  const t = info.videoTracks[0];
  if (!t) throw new Error(error ?? "not an MP4 with a video track");

  // The decoder configuration record (avcC, hvcC, vpcC or av1C).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const trak = file.getTrackById(t.id) as any;
  let description: Uint8Array | undefined;
  for (const entry of trak.mdia.minf.stbl.stsd.entries) {
    const box = entry.avcC ?? entry.hvcC ?? entry.vpcC ?? entry.av1C;
    if (box) {
      const stream = new DataStream(undefined, 0, Endianness.BIG_ENDIAN);
      box.write(stream);
      description = new Uint8Array(stream.buffer, 8);
      break;
    }
  }
  const config: VideoDecoderConfig = {
    codec: t.codec.startsWith("vp08") ? "vp8" : t.codec,
    codedWidth: t.video!.width,
    codedHeight: t.video!.height,
    description,
    hardwareAcceleration: "prefer-software",
  };

  const chunks: Chunk[] = [];
  for (const s of file.getTrackSamplesInfo(t.id)) {
    if (s.offset + s.size > bytes.byteLength) {
      // A partial download: play what is there, like native playback.
      console.warn(`video is truncated: ${s.number} of ${t.nb_samples} samples present`);
      break;
    }
    chunks.push({
      data: new Uint8Array(bytes, s.offset, s.size),
      time: s.cts / s.timescale,
      duration: s.duration / s.timescale,
      key: s.is_sync,
    });
  }
  if (chunks.length === 0) throw new Error("the video track has no samples");
  // Samples are in decode order; the pass ends after the latest frame.
  const first = Math.min(...chunks.map((c) => c.time));
  const length = Math.max(...chunks.map((c) => c.time + c.duration)) - first;
  return { config, chunks, length };
}

export class Mp4Player {
  private decoder!: VideoDecoder;
  private next = 0;
  /** Timeline offset added to the current pass. */
  private offset = 0;
  private flushing = false;
  private frames: VideoFrame[] = [];
  private origin: { at: number; pts: number } | null = null;
  private pausedAt: number | null = null;
  private dropBefore: number | null = null;
  private raf = 0;
  private copying = false;
  private buffer = new Uint8Array(0);
  private closed = false;
  readonly duration: number;
  private readonly start: number;

  private constructor(
    private readonly config: VideoDecoderConfig,
    private readonly chunks: Chunk[],
    length: number,
    private readonly color: { matrix: number; fullRange: boolean } | null,
    private readonly present: (planes: YuvPlanes) => void,
    private readonly onError: (e: Error) => void,
  ) {
    this.duration = length;
    this.start = Math.min(...chunks.map((c) => c.time));
    this.createDecoder();
  }

  /** Throws when the file or codec cannot be played this way. */
  /**
   * `color` overrides the decoder's color description with what native
   * playback uses, so both convert identically.
   */
  static async open(
    url: string,
    color: { matrix: number; fullRange: boolean } | null,
    present: (planes: YuvPlanes) => void,
    onError: (e: Error) => void,
  ): Promise<Mp4Player> {
    if (typeof VideoDecoder === "undefined") throw new Error("WebCodecs is unavailable");
    const bytes = await (await fetch(url)).arrayBuffer();
    const { config, chunks, length } = await demux(bytes);
    const support = await VideoDecoder.isConfigSupported(config);
    if (!support.supported) throw new Error(`cannot decode ${config.codec} in software`);
    return new Mp4Player(config, chunks, length, color, present, onError);
  }

  private createDecoder(): void {
    this.decoder = new VideoDecoder({
      output: (frame) => {
        const pts = frame.timestamp / 1e6;
        if (this.closed || (this.dropBefore !== null && pts + 1e-4 < this.dropBefore)) {
          frame.close();
          return;
        }
        this.dropBefore = null;
        this.frames.push(frame);
        this.frames.sort((a, b) => a.timestamp - b.timestamp);
      },
      error: (e) => this.onError(e instanceof Error ? e : new Error(String(e))),
    });
    this.decoder.ondequeue = () => {
      if (!this.closed && this.pausedAt === null) this.fill();
    };
    this.decoder.configure(this.config);
  }

  private fill(): void {
    while (!this.flushing && this.frames.length < AHEAD && this.decoder.decodeQueueSize < MAX_QUEUED) {
      if (this.next >= this.chunks.length) {
        // End of pass: drain reordered frames, then loop.
        this.flushing = true;
        this.decoder
          .flush()
          .then(() => {
            this.offset += this.duration;
            this.next = 0;
            this.flushing = false;
          })
          .catch(() => {});
        return;
      }
      const c = this.chunks[this.next++]!;
      this.decoder.decode(
        new EncodedVideoChunk({
          type: c.key ? "key" : "delta",
          timestamp: Math.round((this.offset + c.time - this.start) * 1e6),
          duration: Math.round(c.duration * 1e6),
          data: c.data,
        }),
      );
    }
  }

  private position(now: number): number | null {
    if (this.pausedAt !== null) return this.pausedAt;
    return this.origin ? this.origin.pts + (now - this.origin.at) / 1000 : null;
  }

  /** Seconds into the video. */
  get time(): number {
    const p = this.position(performance.now()) ?? 0;
    return this.duration > 0 ? p % this.duration : p;
  }

  get playing(): boolean {
    return this.pausedAt === null;
  }

  private tick = (now: number): void => {
    if (this.closed || this.pausedAt !== null) return;
    this.fill();
    if (!this.copying && this.frames.length) {
      const first = this.frames[0]!;
      this.origin ??= { at: now, pts: first.timestamp / 1e6 };
      const position = this.position(now)!;
      let due: VideoFrame | null = null;
      while (this.frames.length && this.frames[0]!.timestamp / 1e6 <= position + 0.002) {
        due?.close();
        due = this.frames.shift()!;
      }
      if (due) void this.deliver(due);
    }
    this.raf = requestAnimationFrame(this.tick);
  };

  private async deliver(frame: VideoFrame): Promise<void> {
    this.copying = true;
    try {
      const chroma = CHROMA[frame.format ?? ""];
      if (!chroma) throw new Error(`unsupported frame format ${frame.format}`);
      const rect = frame.visibleRect!;
      const size = frame.allocationSize();
      if (this.buffer.length < size) this.buffer = new Uint8Array(size);
      const layout = await frame.copyTo(this.buffer);
      const [cx, cy] = chroma;
      this.present({
        width: rect.width,
        height: rect.height,
        chromaWidth: Math.ceil(rect.width / cx),
        chromaHeight: Math.ceil(rect.height / cy),
        matrix: this.color?.matrix ?? matrixCode(frame.colorSpace.matrix),
        fullRange: this.color?.fullRange ?? frame.colorSpace.fullRange ?? false,
        data: this.buffer,
        offsets: [layout[0]!.offset, layout[1]!.offset, layout[2]!.offset],
        strides: [layout[0]!.stride, layout[1]!.stride, layout[2]!.stride],
      });
    } catch (e) {
      this.onError(e instanceof Error ? e : new Error(String(e)));
    } finally {
      frame.close();
      this.copying = false;
    }
  }

  play(): void {
    if (this.closed) return;
    if (this.pausedAt !== null && this.origin) this.origin = { at: performance.now(), pts: this.pausedAt };
    this.pausedAt = null;
    cancelAnimationFrame(this.raf);
    this.raf = requestAnimationFrame(this.tick);
  }

  pause(): void {
    if (this.pausedAt !== null) return;
    this.pausedAt = this.position(performance.now()) ?? 0;
    cancelAnimationFrame(this.raf);
  }

  /** Restarts decoding from the keyframe before `seconds`. */
  seek(seconds: number): void {
    const t = Math.min(Math.max(seconds, 0), this.duration);
    this.decoder.reset();
    this.decoder.configure(this.config);
    for (const f of this.frames) f.close();
    this.frames = [];
    this.flushing = false;
    this.offset = 0;
    let k = 0;
    for (let i = 0; i < this.chunks.length; i++) {
      const c = this.chunks[i]!;
      if (c.key && c.time - this.start <= t) k = i;
    }
    this.next = k;
    this.dropBefore = t;
    this.origin = { at: performance.now(), pts: t };
    if (this.pausedAt !== null) {
      this.pausedAt = t;
      // Show the target frame while paused.
      this.raf = requestAnimationFrame((now) => {
        const paused = this.pausedAt;
        this.pausedAt = null;
        this.origin = { at: now, pts: t };
        this.tick(now);
        cancelAnimationFrame(this.raf);
        this.pausedAt = paused;
      });
    }
  }

  close(): void {
    this.closed = true;
    cancelAnimationFrame(this.raf);
    for (const f of this.frames) f.close();
    this.frames = [];
    if (this.decoder.state !== "closed") this.decoder.close();
  }
}
