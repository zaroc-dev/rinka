// Per-wallpaper playback speed and color adjustments. Both are stored in the
// project and applied by the engine, so the preview and the desktop agree.

import type { ColorAdjust, Project } from "../shared/api";
import { NEUTRAL_ADJUST } from "../shared/api";

interface Slider {
  key: keyof ColorAdjust;
  label: string;
  min: number;
  max: number;
  step: number;
  format(v: number): string;
}

const percent = (v: number) => `${Math.round(v * 100)}%`;

const COLOR_SLIDERS: Slider[] = [
  { key: "brightness", label: "Brightness", min: 0, max: 2, step: 0.01, format: percent },
  { key: "contrast", label: "Contrast", min: 0, max: 2, step: 0.01, format: percent },
  { key: "saturation", label: "Saturation", min: 0, max: 2, step: 0.01, format: percent },
  { key: "hue", label: "Hue", min: -180, max: 180, step: 1, format: (v) => `${v > 0 ? "+" : ""}${Math.round(v)}°` },
];

/** Speeds the slider snaps through. */
const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2];

export function colorAdjust(p: Project): ColorAdjust {
  return { ...NEUTRAL_ADJUST, ...p.adjust };
}

/** The project with these settings; neutral values are left out of the file. */
export function withPlayback(p: Project, speed: number, adjust: ColorAdjust): Project {
  const { speed: _s, adjust: _a, ...rest } = p;
  const neutral = (Object.keys(NEUTRAL_ADJUST) as (keyof ColorAdjust)[]).every((k) => adjust[k] === NEUTRAL_ADJUST[k]);
  return { ...rest, ...(speed !== 1 && { speed }), ...(!neutral && { adjust }) };
}

interface Props {
  project: Project;
  /** Whether the wallpaper moves, so speed matters. */
  moves: boolean;
  onChange(project: Project): void;
}

export function AdjustPanel({ project, moves, onChange }: Props) {
  const speed = project.speed ?? 1;
  const adjust = colorAdjust(project);
  const speedIndex = SPEEDS.indexOf(speed);
  const changed = speed !== 1 || project.adjust !== undefined;

  return (
    <div class="adjust">
      {moves && (
        <label class="adjust-row">
          <span>Speed</span>
          <input
            type="range"
            min={0}
            max={SPEEDS.length - 1}
            step={1}
            // A speed set elsewhere still shows near its place.
            value={speedIndex >= 0 ? speedIndex : SPEEDS.findIndex((s) => s >= speed)}
            aria-valuetext={`${speed}×`}
            onInput={(e) => onChange(withPlayback(project, SPEEDS[Number(e.currentTarget.value)]!, adjust))}
          />
          <span class="mono adjust-value">{speed}×</span>
        </label>
      )}
      {COLOR_SLIDERS.map((s) => (
        <label class="adjust-row">
          <span>{s.label}</span>
          <input
            type="range"
            min={s.min}
            max={s.max}
            step={s.step}
            value={adjust[s.key]}
            aria-valuetext={s.format(adjust[s.key])}
            onInput={(e) => onChange(withPlayback(project, speed, { ...adjust, [s.key]: Number(e.currentTarget.value) }))}
            onDblClick={() => onChange(withPlayback(project, speed, { ...adjust, [s.key]: NEUTRAL_ADJUST[s.key] }))}
          />
          <span class="mono adjust-value">{s.format(adjust[s.key])}</span>
        </label>
      ))}
      <button type="button" class="button ghost" disabled={!changed} onClick={() => onChange(withPlayback(project, 1, NEUTRAL_ADJUST))}>
        Reset
      </button>
    </div>
  );
}
