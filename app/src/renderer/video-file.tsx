// "Video file" popover: what the video project plays, and re-encoding it
// smaller or at a lower frame rate. The original stays in the project.
import { useEffect, useState } from "preact/hooks";
import type { LibraryEntry, OptimizeResult, VideoEncoder, VideoFileInfo, VideoFiles } from "../shared/api";
import { VIDEO_ENCODER_LABELS } from "../shared/api";
import { api, errorMessage } from "./util";

const HEIGHTS = [2160, 1440, 1080, 720];
const RATES = [60, 30];

interface Props {
  entry: LibraryEntry;
  /** Short side of the largest display, in pixels. */
  displayHeight: number;
  encoder: VideoEncoder;
  onChanged(entry: LibraryEntry): void;
}

function megabytes(bytes: number): string {
  return `${(bytes / 1e6).toFixed(bytes < 1e7 ? 1 : 0)} MB`;
}

const CODECS: Record<string, string> = { h264: "H.264", hevc: "HEVC", av1: "AV1", vp9: "VP9", vp8: "VP8" };

function describe(v: VideoFileInfo): string {
  const fps = v.frameRate ? ` · ${Math.round(v.frameRate * 100) / 100} fps` : "";
  return `${v.width} × ${v.height}${fps} · ${megabytes(v.bytes)} · ${CODECS[v.codec] ?? v.codec.toUpperCase()}`;
}

export function VideoFilePanel({ entry, displayHeight, encoder, onChanged }: Props) {
  const [files, setFiles] = useState<VideoFiles | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [result, setResult] = useState<OptimizeResult | null>(null);
  const [height, setHeight] = useState<number | null>(null);
  const [fps, setFps] = useState<number | null>(null);

  useEffect(() => {
    setError(null);
    void api
      .videoFiles(entry.id)
      .then((f) => {
        setFiles(f);
        setProgress(f.running);
      })
      .catch((e) => setError(errorMessage(e)));
  }, [entry.id, entry.assetPrefix]);

  useEffect(
    () =>
      api.onOptimizeProgress((id, p) => {
        if (id === entry.id) setProgress(p);
      }),
    [entry.id],
  );

  // Choices come from the original, which every re-encode starts from.
  const source = files?.original ?? files?.current ?? null;
  const heights = source ? HEIGHTS.filter((h) => h < source.height) : [];
  const rates = source?.frameRate ? RATES.filter((r) => r < source.frameRate! - 0.01) : [];

  // Default: the smallest size that still covers the largest display.
  useEffect(() => {
    if (!source) return;
    const covering = heights.filter((h) => h >= displayHeight);
    setHeight(covering.length ? covering[covering.length - 1]! : null);
    setFps(rates.includes(60) ? 60 : null);
  }, [source?.width, source?.height, source?.frameRate, displayHeight]);

  async function optimize(): Promise<void> {
    setError(null);
    setResult(null);
    setProgress(0);
    try {
      const r = await api.optimizeVideo(entry.id, { maxHeight: height, maxFps: fps });
      setResult(r);
      onChanged(r.entry);
    } catch (e) {
      const message = errorMessage(e);
      if (message !== "cancelled") setError(message);
    }
    setProgress(null);
  }

  async function restore(): Promise<void> {
    setError(null);
    setResult(null);
    try {
      onChanged(await api.restoreVideo(entry.id));
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  const running = progress !== null;
  const unchanged = height === null && fps === null;
  return (
    <div class="video-file">
      <h2>Video file</h2>
      {files ? (
        <dl class="facts">
          <dt>Playing</dt>
          <dd>{describe(files.current)}</dd>
          {files.original && (
            <>
              <dt>Original</dt>
              <dd>{describe(files.original)}</dd>
            </>
          )}
        </dl>
      ) : (
        !error && <p class="muted small">Reading the file…</p>
      )}

      {source && (heights.length > 0 || rates.length > 0) && (
        <>
          {heights.length > 0 && (
            <div class="setting-row stacked">
              <span>Resolution</span>
              <div class="segmented fit" role="radiogroup" aria-label="Resolution">
                {[null, ...heights].map((h) => (
                  <button type="button" role="radio" aria-checked={height === h} disabled={running} onClick={() => setHeight(h)}>
                    {h === null ? "Original" : `${h}p`}
                  </button>
                ))}
              </div>
            </div>
          )}
          {rates.length > 0 && (
            <div class="setting-row stacked">
              <span>Frame rate</span>
              <div class="segmented fit" role="radiogroup" aria-label="Frame rate">
                {[null, ...rates].map((r) => (
                  <button type="button" role="radio" aria-checked={fps === r} disabled={running} onClick={() => setFps(r)}>
                    {r === null ? "Original" : `${r} fps`}
                  </button>
                ))}
              </div>
            </div>
          )}
          <p class="muted small">
            Re-encodes to H.264 ({VIDEO_ENCODER_LABELS[encoder]}, change it in Settings). Smaller files decode with less work.
            The original stays in the project, so you can go back to it.
          </p>
          {running ? (
            <div class="optimize-progress">
              <progress max={1} value={progress} aria-label="Optimizing" />
              <span class="mono small">{Math.round(progress * 100)}%</span>
              <button type="button" class="button" onClick={() => void api.cancelOptimize(entry.id)}>
                Cancel
              </button>
            </div>
          ) : (
            <button type="button" class="button primary" disabled={unchanged} onClick={() => void optimize()}>
              Optimize
            </button>
          )}
        </>
      )}
      {source && heights.length === 0 && rates.length === 0 && (
        <p class="muted small">This video is already 720p at 30 fps or less; there is nothing to make smaller.</p>
      )}

      {result && (
        <p class="small">
          Encoded with {VIDEO_ENCODER_LABELS[result.encoder]}.
          {result.droppedAudio && " Its sound track was left out: wallpapers play muted."}
          {result.reducedTo8Bit && " Colors were reduced to 8 bits per channel."}
        </p>
      )}
      {files?.original && !running && (
        <button type="button" class="button" onClick={() => void restore()}>
          Restore original
        </button>
      )}
      {error && <p class="error small">{error}</p>}
    </div>
  );
}
