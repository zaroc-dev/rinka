// KDE Plasma draws its own desktop window above Rinka's wallpapers. The
// window shows them only while it uses Rinka's wallpaper plugin, which the
// host switches to (and back) on request; this shows that state.
import { useCallback, useEffect, useState } from "preact/hooks";
import type { HostState, OutputInfo, PlasmaDesktop, PlasmaDesktopState } from "../shared/api";
import { api, errorMessage } from "./util";

export interface Plasma {
  state: PlasmaDesktopState | null;
  busy: boolean;
  error: string | null;
  /** Switches every Plasma desktop to Rinka's plugin, or back. */
  show(showRinka: boolean): Promise<void>;
}

/** Plasma's desktop state while the host runs in a Plasma session, else null. */
export function usePlasmaDesktop(host: HostState): Plasma | null {
  const enabled = host.state === "connected" && host.plasmaDesktop;
  const [state, setState] = useState<PlasmaDesktopState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled) {
      setState(null);
      return;
    }
    api.plasmaDesktop().then(
      (s) => {
        setState(s);
        setError(null);
      },
      (e) => setError(errorMessage(e)),
    );
  }, [enabled]);

  const show = useCallback(async (showRinka: boolean) => {
    setBusy(true);
    try {
      setState(await api.plasmaDesktop(showRinka));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }, []);

  return enabled ? { state, busy, error, show } : null;
}

/** Displays showing a Rinka wallpaper that Plasma's desktop covers. */
function coveredOutputs(plasma: Plasma, outputs: OutputInfo[]): OutputInfo[] {
  const desktops = plasma.state?.desktops ?? [];
  return outputs.filter((o) => o.presenting && desktops.some((d) => d.output === o.name && !d.showsRinka));
}

const PLUGIN_NAMES: Record<string, string> = {
  "org.kde.image": "Image",
  "org.kde.slideshow": "Slideshow",
  "org.kde.color": "Plain color",
  "org.kde.potd": "Picture of the day",
};

function pluginName(desktop: PlasmaDesktop): string {
  return desktop.showsRinka ? "Rinka" : (PLUGIN_NAMES[desktop.plugin] ?? desktop.plugin);
}

function RestartNote({ plasma }: { plasma: Plasma }) {
  if (plasma.state?.restart !== "needed") return null;
  return (
    <p class="muted small">
      Plasma could not be restarted for you. Log out and back in, or run <span class="mono">systemctl --user restart plasma-plasmashell</span>, to see the
      wallpaper.
    </p>
  );
}

/** A notice in the library while Plasma hides a wallpaper Rinka is showing. */
export function PlasmaNotice({ plasma, outputs }: { plasma: Plasma | null; outputs: OutputInfo[] }) {
  if (!plasma) return null;
  const covered = coveredOutputs(plasma, outputs);
  if (covered.length === 0) return <RestartNote plasma={plasma} />;
  return (
    <div class="notice notice-action">
      <span>
        Plasma's desktop covers the wallpaper on {covered.map((o) => o.name).join(", ")}. Plasma restarts once to show it.
      </span>
      <button type="button" class="button primary" disabled={plasma.busy} onClick={() => void plasma.show(true)}>
        Show Rinka on the desktop
      </button>
    </div>
  );
}

/** The Settings section: which desktops show Rinka, and the switch. */
export function PlasmaSection({ plasma }: { plasma: Plasma | null }) {
  if (!plasma) return null;
  const desktops = plasma.state?.desktops ?? [];
  // One row per display; activities have a desktop each.
  const rows = new Map<string, PlasmaDesktop>();
  for (const d of desktops) {
    const key = d.output ?? "Not connected";
    const seen = rows.get(key);
    if (!seen || (seen.showsRinka && !d.showsRinka)) rows.set(key, d);
  }
  const anyRinka = desktops.some((d) => d.showsRinka);
  const allRinka = desktops.length > 0 && desktops.every((d) => d.showsRinka);
  return (
    <section class="sheet-section">
      <h3>Plasma desktop</h3>
      <p class="muted">
        Plasma's desktop sits above Rinka's wallpapers. It shows them while it uses the Rinka wallpaper type; icons, widgets and panels stay on top.
      </p>
      {rows.size > 0 && (
        <dl class="facts">
          {[...rows].map(([name, d]) => (
            <>
              <dt class="mono">{name}</dt>
              <dd>{pluginName(d)}</dd>
            </>
          ))}
        </dl>
      )}
      {plasma.error && <p class="notice">{plasma.error}</p>}
      <RestartNote plasma={plasma} />
      <div class="button-row">
        {!allRinka && (
          <button type="button" class="button primary" disabled={plasma.busy} onClick={() => void plasma.show(true)}>
            Show Rinka on the desktop
          </button>
        )}
        {anyRinka && (
          <button type="button" class="button" disabled={plasma.busy} onClick={() => void plasma.show(false)}>
            Use Plasma's wallpaper again
          </button>
        )}
      </div>
      {!allRinka && <p class="muted small">Plasma restarts once to apply this.</p>}
    </section>
  );
}
