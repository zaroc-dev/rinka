import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Fit, HostState, LibraryEntry, OutputInfo, Project } from "../shared/api";
import { FITS } from "../shared/api";
import { DisplayMap } from "./displays";
import { CloseIcon, InfoIcon, PauseIcon, PlayIcon, TrashIcon } from "./icons";
import { ReportView } from "./import";
import type { ImportReport } from "../shared/api";
import {
  canvas,
  onPreviewLoading,
  onPreviewStatus,
  onVideoState,
  previewStatus,
  seekVideo,
  setPreviewReference,
  showEntry,
  stopPreviewMedia,
  toggleVideo,
  updateProject,
  type VideoState,
} from "./preview";
import { api, clock, errorMessage, FIT_HINTS, FIT_LABELS, outputPixels } from "./util";

interface Props {
  entry: LibraryEntry;
  entries: LibraryEntry[];
  outputs: OutputInfo[];
  host: HostState;
  onEdited(id: string, project: Project): void;
  onDelete(id: string): void;
  onOutputsChanged(): void;
  onToast(message: string): void;
}

export function Wallpaper({ entry, entries, outputs, host, onEdited, onDelete, onOutputsChanged, onToast }: Props) {
  const [project, setProject] = useState<Project>(entry.project);
  const [applyOpen, setApplyOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [status, setStatus] = useState(previewStatus());
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const canvasHost = useRef<HTMLDivElement>(null);
  const saveTimer = useRef<number | undefined>(undefined);

  // Preview at the pixel density of the largest display.
  const reference = outputs
    .map(outputPixels)
    .filter((s): s is [number, number] => s !== null)
    .sort((a, b) => b[0] * b[1] - a[0] * a[1])[0];
  const referenceHeight = reference ? Math.min(reference[0], reference[1]) : 1080;

  useEffect(() => onPreviewStatus(setStatus), []);
  useEffect(() => onPreviewLoading(setLoading), []);
  const [video, setVideo] = useState<VideoState | null>(null);
  const [showOriginal, setShowOriginal] = useState(false);
  const [report, setReport] = useState<ImportReport | null>(null);
  useEffect(() => onVideoState(setVideo), []);
  // Preview playback ends with the view.
  useEffect(() => () => stopPreviewMedia(), []);

  useLayoutEffect(() => {
    canvasHost.current?.appendChild(canvas);
    return () => canvas.remove();
  }, []);

  // A different wallpaper resets the draft and loads its assets.
  useEffect(() => {
    setProject(entry.project);
    setApplyOpen(false);
    setLoadError(null);
    showEntry(entry).catch((e) => setLoadError(errorMessage(e)));
  }, [entry.id, entry.assetPrefix]);

  useEffect(() => {
    setPreviewReference(referenceHeight);
  }, [referenceHeight]);

  useEffect(() => () => window.clearTimeout(saveTimer.current), []);

  function edit(change: (p: Project) => Project): void {
    const next = change(project);
    setProject(next);
    updateProject(entry, next);
    onEdited(entry.id, next);
    window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      api.saveProject(entry.id, next).catch((e) => onToast(`Could not save: ${errorMessage(e)}`));
    }, 200);
  }

  async function toggle(o: OutputInfo): Promise<void> {
    setBusy(o.name);
    try {
      if (o.assignedId === entry.id) await api.unassign(o.name);
      else await api.assign(o.name, entry.id);
    } catch (e) {
      onToast(errorMessage(e));
    }
    setBusy(null);
    onOutputsChanged();
  }

  async function applyAll(): Promise<void> {
    setBusy("*");
    try {
      await api.assign(null, entry.id);
      onToast(`“${project.title}” is on every display`);
    } catch (e) {
      onToast(errorMessage(e));
    }
    setBusy(null);
    onOutputsChanged();
  }

  const c = project.content;
  const backdrop = c.type === "color" ? c.color : c.type === "scene" ? "#000" : c.background;
  const liveOn = outputs.filter((o) => o.assignedId === entry.id);

  return (
    <section class="stage" aria-label={project.title} style={{ background: backdrop }}>
      {/* The engine thumbnail stands in while the full image decodes. */}
      {entry.thumb && <img class="stage-placeholder" src={entry.thumb} alt="" />}
      <div class={`canvas-host${loading ? " is-loading" : ""}`} ref={canvasHost} />
      {loading && <span class="live-dot-label stage-loading">Loading full resolution…</span>}
      {showOriginal && project.provenance?.reference && (
        <img class="stage-reference" src={`rinka://app/library/${entry.id}/${project.provenance.reference}`} alt="Wallpaper Engine's preview" />
      )}
      {!status.ok && status.error && <p class="preview-error">Preview unavailable: {status.error}</p>}
      {loadError && <p class="preview-error">Could not load this image: {loadError}</p>}

      {liveOn.length > 0 && (
        <span class="live-dot-label stage-live">
          <span class="live-dot" />
          On {liveOn.map((o) => o.name).join(", ")}
        </span>
      )}

      <div class="bar">
        {c.type === "video" && video && video.duration > 0 && (
          <input
            id="seek"
            class="seek"
            type="range"
            min={0}
            max={video.duration}
            step={0.01}
            value={video.time}
            aria-label="Preview position"
            style={{ "--progress": `${(video.time / video.duration) * 100}%` }}
            onInput={(e) => seekVideo(Number((e.target as HTMLInputElement).value))}
          />
        )}
        <input
          id="title"
          class="bar-title"
          value={project.title}
          aria-label="Wallpaper name"
          onInput={(e) => edit((p) => ({ ...p, title: (e.target as HTMLInputElement).value }))}
        />
        <div class="bar-controls">
          {c.type === "scene" ? (
            <span class="scene-tools">
              {project.provenance?.reference && (
                <button
                  type="button"
                  class="button ghost"
                  aria-pressed={showOriginal}
                  title="Compare with Wallpaper Engine's own preview"
                  onClick={() => setShowOriginal((v) => !v)}
                >
                  Original
                </button>
              )}
              {project.provenance?.report && (
                <button
                  type="button"
                  class="icon-button"
                  aria-label="Import report"
                  title="Import report"
                  aria-pressed={report !== null}
                  onClick={() => {
                    if (report) return setReport(null);
                    setApplyOpen(false);
                    void fetch(`rinka://app/library/${entry.id}/${project.provenance!.report}`)
                      .then((r) => r.json())
                      .then(setReport)
                      .catch((e) => onToast(errorMessage(e)));
                  }}
                >
                  <InfoIcon />
                </button>
              )}
            </span>
          ) : c.type === "color" ? (
            <label class="swatch-input" title="Color">
              <input
                id="color"
                type="color"
                value={c.color}
                onInput={(e) => edit((p) => ({ ...p, content: { type: "color", color: (e.target as HTMLInputElement).value } }))}
              />
              <span class="mono">{c.color}</span>
            </label>
          ) : (
            <>
              {c.type === "video" && (
                <span class="transport">
                  <button
                    type="button"
                    class="icon-button"
                    aria-label={video?.playing ? "Pause preview" : "Play preview"}
                    title={video?.playing ? "Pause preview" : "Play preview"}
                    disabled={!video}
                    onClick={toggleVideo}
                  >
                    {video?.playing ? <PauseIcon /> : <PlayIcon />}
                  </button>
                  <span class="mono time">
                    {clock(video?.time ?? 0)} / {clock(video?.duration ?? entry.duration)}
                  </span>
                  {video && !video.exactColor && (
                    <span
                      class="approx"
                      title="The preview cannot decode this format itself, so the browser converts its colors. The wallpaper on your displays uses the exact conversion."
                    >
                      Approx. colors
                    </span>
                  )}
                </span>
              )}
              <div class="segmented fit" role="radiogroup" aria-label="Placement">
                {FITS.map((fit: Fit) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={c.fit === fit}
                    title={FIT_HINTS[fit]}
                    onClick={() => edit((p) => ({ ...p, content: { ...c, fit } }))}
                  >
                    {FIT_LABELS[fit]}
                  </button>
                ))}
              </div>
              <label class="swatch-input" title="Background, shown around and behind the image">
                <input
                  id="background"
                  type="color"
                  value={c.background}
                  onInput={(e) => edit((p) => ({ ...p, content: { ...c, background: (e.target as HTMLInputElement).value } }))}
                />
                <span class="sr-only">Background</span>
              </label>
            </>
          )}
        </div>
        <button
          type="button"
          class="icon-button danger"
          title="Delete from library"
          aria-label="Delete from library"
          onClick={() => {
            if (confirm(`Delete “${project.title}” from your library? Its files are removed too.`)) onDelete(entry.id);
          }}
        >
          <TrashIcon />
        </button>
        <button
          type="button"
          class="button primary"
          aria-expanded={applyOpen}
          onClick={() => {
            setReport(null);
            setApplyOpen((v) => !v);
          }}
        >
          Apply to
        </button>
        {report && (
          <div class="popover report-popover" role="dialog" aria-label="Import report">
            <div class="popover-head">
              <h2>Import report</h2>
              <button type="button" class="icon-button" aria-label="Close" onClick={() => setReport(null)}>
                <CloseIcon />
              </button>
            </div>
            <ReportView report={report} />
          </div>
        )}
        {applyOpen && (
          <div class="popover" role="dialog" aria-label="Choose displays">
            <DisplayMap
              outputs={outputs}
              entries={entries}
              current={entry}
              host={host}
              busy={busy}
              onToggle={toggle}
              onAll={applyAll}
              onStartHost={() => void api.startHost()}
            />
          </div>
        )}
      </div>
    </section>
  );
}
