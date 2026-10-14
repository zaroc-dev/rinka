// A to-scale map of the connected displays, as the compositor arranges them.

import type { HostState, LibraryEntry, OutputInfo } from "../shared/api";
import { outputPixels, resolution } from "./util";

interface Props {
  outputs: OutputInfo[];
  entries: LibraryEntry[];
  /** The wallpaper being applied. */
  current: LibraryEntry;
  host: HostState;
  busy: string | null;
  onToggle(output: OutputInfo): void;
  onAll(): void;
  onStartHost(): void;
}

const MAP_W = 360;
const MAP_H = 170;

export function DisplayMap({ outputs, entries, current, host, busy, onToggle, onAll, onStartHost }: Props) {
  if (host.state !== "connected") {
    return (
      <div class="display-map-empty">
        <p>{host.state === "unavailable" ? host.reason : "The wallpaper host is not running."}</p>
        <button type="button" class="button" onClick={onStartHost} disabled={host.state === "connecting"}>
          Start host
        </button>
      </div>
    );
  }
  const placed = outputs.filter((o) => o.position && o.logicalSize);
  const minX = Math.min(...placed.map((o) => o.position![0]));
  const minY = Math.min(...placed.map((o) => o.position![1]));
  const maxX = Math.max(...placed.map((o) => o.position![0] + o.logicalSize![0]));
  const maxY = Math.max(...placed.map((o) => o.position![1] + o.logicalSize![1]));
  const scale = Math.min(MAP_W / (maxX - minX), MAP_H / (maxY - minY));
  const offsetX = (MAP_W - (maxX - minX) * scale) / 2;
  const offsetY = (MAP_H - (maxY - minY) * scale) / 2;
  const allOn = outputs.length > 0 && outputs.every((o) => o.assignedId === current.id);

  return (
    <div class="display-map-wrap">
      <div class="display-map" style={{ width: `${MAP_W}px`, height: `${MAP_H}px` }}>
        {placed.map((o) => {
          const showing = entries.find((e) => e.id === o.assignedId);
          const on = o.assignedId === current.id;
          return (
            <button
              type="button"
              class={`display${on ? " is-on" : ""}`}
              aria-pressed={on}
              disabled={busy !== null}
              title={on ? `Remove from ${o.name}` : `Show on ${o.name}`}
              style={{
                left: `${offsetX + (o.position![0] - minX) * scale}px`,
                top: `${offsetY + (o.position![1] - minY) * scale}px`,
                width: `${o.logicalSize![0] * scale}px`,
                height: `${o.logicalSize![1] * scale}px`,
              }}
              onClick={() => onToggle(o)}
            >
              {showing?.thumb && <img src={showing.thumb} alt="" />}
              {showing && !showing.thumb && showing.project.content.type === "color" && (
                <span class="display-fill" style={{ background: showing.project.content.color }} />
              )}
              <span class="display-label">
                <span class="mono">{o.name}</span>
                <span class="display-res mono">{resolution(outputPixels(o))}</span>
              </span>
              {busy === o.name && <span class="display-busy" />}
            </button>
          );
        })}
      </div>
      <div class="display-map-foot">
        <span class="muted">{allOn ? "Showing on every display" : "Select displays to show this wallpaper on"}</span>
        <button type="button" class="button" disabled={allOn || busy !== null} onClick={onAll}>
          All displays
        </button>
      </div>
    </div>
  );
}
