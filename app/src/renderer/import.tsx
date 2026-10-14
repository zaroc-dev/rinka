// Wallpaper Engine import: workshop items with their conversion reports.

import { useEffect, useMemo, useState } from "preact/hooks";
import type { ImportReport, ImportReportItem, ImportStatus, LibraryEntry, WorkshopItem, WorkshopScan } from "../shared/api";
import { CloseIcon } from "./icons";
import { api, errorMessage } from "./util";

const STATUS_LABEL: Record<ImportStatus, string> = {
  supported: "Ready to import",
  partial: "Imports with changes",
  unsupported: "Missing features",
};

/** Whether converting would produce a project at all. */
function convertible(r: ImportReport): boolean {
  return r.items.some((i) => i.status !== "unsupported" && (i.kind === "image layer" || i.kind.endsWith("wallpaper")));
}

export function StatusBadge({ item }: { item: WorkshopItem }) {
  if (item.importedId) return <span class="status is-imported">In library</span>;
  if (item.error) return <span class="status is-unsupported">Can't read</span>;
  if (!item.report) return <span class="status is-checking">Checking…</span>;
  return <span class={`status is-${item.report.status}`}>{STATUS_LABEL[item.report.status]}</span>;
}

type Filter = "all" | ImportStatus | "imported";

export function ImportView({ onImported, onOpen, onToast }: { onImported(entry: LibraryEntry): void; onOpen(id: string): void; onToast(m: string): void }) {
  const [scan, setScan] = useState<WorkshopScan | null>(null);
  const [items, setItems] = useState<Map<string, WorkshopItem>>(new Map());
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    api.onWorkshopItem((item) => setItems((prev) => new Map(prev).set(item.id, item)));
    void api.scanWorkshop().then((s) => {
      setScan(s);
      setItems(new Map(s.items.map((i) => [i.id, i])));
    });
  }, []);

  const list = useMemo(
    () =>
      [...items.values()]
        .filter((i) => {
          if (filter === "all") return true;
          if (filter === "imported") return !!i.importedId;
          return i.report?.status === filter && !i.importedId;
        })
        .sort((a, b) => a.title.localeCompare(b.title)),
    [items, filter],
  );
  const count = (f: Filter) =>
    [...items.values()].filter((i) => (f === "all" ? true : f === "imported" ? !!i.importedId : i.report?.status === f && !i.importedId)).length;
  const checking = [...items.values()].filter((i) => !i.report && !i.error).length;
  const current = selected ? items.get(selected) ?? null : null;

  return (
    <section class="library" aria-label="Import from Wallpaper Engine">
      <div class="library-head">
        <div class="library-title">
          <h1>Wallpaper Engine</h1>
          <span class="count">
            {scan ? `${items.size} workshop items` : "Looking for your workshop items…"}
            {checking > 0 && ` · checking ${checking}`}
          </span>
        </div>
        <div class="chips" role="tablist" aria-label="Filter">
          {(
            [
              ["all", "All"],
              ["supported", "Ready"],
              ["partial", "With changes"],
              ["unsupported", "Missing features"],
              ["imported", "In library"],
            ] as [Filter, string][]
          ).map(([f, label]) => (
            <button type="button" role="tab" class="chip" aria-selected={filter === f} onClick={() => setFilter(f)}>
              {label}
              <span class="chip-count">{count(f)}</span>
            </button>
          ))}
        </div>
      </div>
      {scan && scan.workshopDirs.length === 0 ? (
        <div class="empty">
          <p class="empty-title">No Wallpaper Engine workshop items found</p>
          <p>Install Wallpaper Engine through Steam and subscribe to wallpapers in the workshop; they appear here once downloaded.</p>
        </div>
      ) : (
        <>
          {scan && !scan.assetsDir && (
            <p class="notice">Wallpaper Engine's own files were not found, so built-in effects can't be converted. Install Wallpaper Engine in Steam to include them.</p>
          )}
          <ul class="wall wall-grid">
            {list.map((item) => (
              <li key={item.id}>
                <button type="button" class="tile" onClick={() => setSelected(item.id)}>
                  <div class="thumb">{item.preview && <img src={item.preview} alt="" loading="lazy" />}</div>
                  <span class="tile-text">
                    <span class="tile-title">{item.title}</span>
                    <span class="tile-meta">
                      <span class="muted">{item.type || "unknown"}</span>
                      <StatusBadge item={item} />
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      {current && (
        <ItemSheet
          item={current}
          onClose={() => setSelected(null)}
          onImported={(entry) => {
            onImported(entry);
            onToast(`Imported “${current.title}”`);
          }}
          onOpen={onOpen}
          onToast={onToast}
        />
      )}
    </section>
  );
}

function ItemSheet({
  item,
  onClose,
  onImported,
  onOpen,
  onToast,
}: {
  item: WorkshopItem;
  onClose(): void;
  onImported(e: LibraryEntry): void;
  onOpen(id: string): void;
  onToast(m: string): void;
}) {
  const [busy, setBusy] = useState(false);
  const r = item.report;

  async function run(allowPartial: boolean) {
    setBusy(true);
    try {
      const { report, entry } = await api.importWorkshopItem(item.id, allowPartial);
      if (entry) onImported(entry);
      else onToast(report.status === "supported" ? "Nothing was imported" : "This wallpaper can't be converted");
    } catch (e) {
      onToast(errorMessage(e));
    }
    setBusy(false);
  }

  return (
    <div class="sheet-backdrop" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div class="sheet" role="dialog" aria-modal="true" aria-label={item.title}>
        <div class="sheet-head">
          <div class="sheet-title">
            <h2>{item.title}</h2>
            <span class="muted small mono">
              {item.type || "unknown"} · workshop {item.id}
            </span>
          </div>
          <button type="button" class="icon-button" aria-label="Close" onClick={onClose}>
            <CloseIcon />
          </button>
        </div>
        <div class="sheet-body">
          {item.preview && (
            <div class="sheet-preview">
              <img src={item.preview} alt="" />
              <StatusBadge item={item} />
            </div>
          )}
          {item.error && <p class="notice">{item.error}</p>}
          {r ? <ReportView report={r} /> : !item.error && <p class="muted">Checking what converts…</p>}
        </div>
        <div class="sheet-foot">
          <span class="muted small">
            {item.importedId
              ? "Already in your library"
              : r?.status === "supported"
                ? "Converts completely"
                : r && convertible(r)
                  ? "Unsupported parts are left out"
                  : ""}
          </span>
          {item.importedId ? (
            <button type="button" class="button primary" onClick={() => onOpen(item.importedId!)}>
              Open in library
            </button>
          ) : r && r.status === "supported" ? (
            <button type="button" class="button primary" disabled={busy} onClick={() => void run(false)}>
              {busy ? "Importing…" : "Import"}
            </button>
          ) : r && convertible(r) ? (
            <button type="button" class="button primary" disabled={busy} onClick={() => void run(true)}>
              {busy ? "Importing…" : "Import anyway"}
            </button>
          ) : (
            <button type="button" class="button" disabled>
              {r ? "Can't be converted yet" : "Checking…"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

const GROUPS: [ImportStatus, string][] = [
  ["unsupported", "Not converted"],
  ["partial", "Converted with changes"],
  ["supported", "Converted"],
];

export function ReportView({ report }: { report: ImportReport }) {
  const { supported, partial, unsupported } = report.summary;
  const total = Math.max(1, supported + partial + unsupported);
  return (
    <div class="report">
      <div class="report-summary">
        <p>
          {report.status === "supported"
            ? "Everything in this wallpaper converts."
            : report.status === "partial"
              ? "This wallpaper converts, but some behavior is approximated."
              : "Some parts of this wallpaper can't be converted yet. Importing leaves them out."}
        </p>
        <div class="report-bar" aria-hidden="true">
          {supported > 0 && <span class="is-supported" style={{ flexGrow: supported / total }} />}
          {partial > 0 && <span class="is-partial" style={{ flexGrow: partial / total }} />}
          {unsupported > 0 && <span class="is-unsupported" style={{ flexGrow: unsupported / total }} />}
        </div>
        <div class="report-legend">
          <span class="is-supported">{supported} converted</span>
          <span class="is-partial">{partial} with changes</span>
          <span class="is-unsupported">{unsupported} not converted</span>
        </div>
      </div>
      {GROUPS.map(([status, label]) => {
        const items = report.items.filter((i) => i.status === status);
        if (items.length === 0) return null;
        return (
          <details class={`report-group is-${status}`} open={status !== "supported"}>
            <summary>
              <span class="report-dot" />
              {label}
              <span class="chip-count">{items.length}</span>
            </summary>
            <ul>
              {groupItems(items).map(([names, group]) => (
                <li class="report-item">
                  <div class="report-item-head">
                    <span class="report-kind">{group[0]!.kind}</span>
                    {group.length > 1 && <span class="report-times">× {group.length}</span>}
                  </div>
                  {(group.length > 1 ? names : group[0]!.name) && (
                    <span class="report-name">{group.length > 1 ? names : group[0]!.name}</span>
                  )}
                  {(group[0]!.notes?.length ?? 0) > 0 && (
                    <ul class="report-notes">
                      {group[0]!.notes!.map((n) => (
                        <Note text={n} />
                      ))}
                    </ul>
                  )}
                </li>
              ))}
            </ul>
          </details>
        );
      })}
    </div>
  );
}

/** A note; compiler output goes behind a disclosure. */
function Note({ text }: { text: string }) {
  const split = /^(.*? could not be translated): ([\s\S]*)$/.exec(text);
  if (!split) return <li>{text}</li>;
  return (
    <li>
      {split[1]}
      <details class="report-technical">
        <summary>Technical details</summary>
        <pre>{split[2]}</pre>
      </details>
    </li>
  );
}

/** Groups items with the same kind and notes, e.g. 23 particle systems. */
function groupItems(items: ImportReportItem[]): [string, ImportReportItem[]][] {
  const groups = new Map<string, ImportReportItem[]>();
  for (const i of items) {
    const key = `${i.kind}|${(i.notes ?? []).join("|")}`;
    groups.set(key, [...(groups.get(key) ?? []), i]);
  }
  return [...groups.values()].map((g) => [g.map((i) => i.name).filter(Boolean).slice(0, 3).join(", "), g]);
}
