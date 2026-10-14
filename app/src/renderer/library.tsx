import { useMemo } from "preact/hooks";
import type { LibraryEntry, OutputInfo } from "../shared/api";
import { CompactIcon, GridIcon, ListIcon } from "./icons";
import { clock, detail, displaysShowing, kindLabel, resolution } from "./util";

export type ViewMode = "grid" | "compact" | "list";
export const VIEW_MODES: readonly ViewMode[] = ["grid", "compact", "list"];
export type Filter = "all" | "image" | "video" | "scene" | "color" | "live";
export type Sort = "recent" | "name";
export const SORTS: readonly Sort[] = ["recent", "name"];

interface Props {
  entries: LibraryEntry[];
  outputs: OutputInfo[];
  query: string;
  view: ViewMode;
  filter: Filter;
  sort: Sort;
  onView(v: ViewMode): void;
  onFilter(f: Filter): void;
  onSort(s: Sort): void;
  onOpen(id: string): void;
  onAddImages(): void;
}

export function Library(props: Props) {
  const { entries, outputs, query, view, filter, sort } = props;

  const counts = useMemo(
    () => ({
      all: entries.length,
      image: entries.filter((e) => e.project.content.type === "image").length,
      video: entries.filter((e) => e.project.content.type === "video").length,
      scene: entries.filter((e) => e.project.content.type === "scene").length,
      color: entries.filter((e) => e.project.content.type === "color").length,
      live: entries.filter((e) => displaysShowing(e, outputs).length > 0).length,
    }),
    [entries, outputs],
  );

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries
      .filter((e) => {
        if (filter === "live") return displaysShowing(e, outputs).length > 0;
        return filter === "all" || e.project.content.type === filter;
      })
      .filter((e) => !q || e.project.title.toLowerCase().includes(q))
      .sort((a, b) => (sort === "name" ? a.project.title.localeCompare(b.project.title) : b.added - a.added));
  }, [entries, outputs, query, filter, sort]);

  const filters: [Filter, string][] = [
    ["all", "All"],
    ["image", "Images"],
    ["video", "Videos"],
    ["scene", "Scenes"],
    ["color", "Colors"],
    ["live", "On display"],
  ];

  return (
    <section class="library" aria-label="Library">
      <div class="library-head">
        <div class="library-title">
          <h1>Library</h1>
          <span class="count">
            {entries.length} {entries.length === 1 ? "wallpaper" : "wallpapers"}
          </span>
        </div>
        <div class="library-tools">
          <div class="chips" role="tablist" aria-label="Filter">
            {filters.map(([f, label]) => (
              <button type="button" role="tab" class="chip" aria-selected={filter === f} onClick={() => props.onFilter(f)}>
                {label}
                <span class="chip-count">{counts[f]}</span>
              </button>
            ))}
          </div>
          <label class="sort">
            <span class="sr-only">Sort</span>
            <select id="sort" value={sort} onChange={(e) => props.onSort((e.target as HTMLSelectElement).value as Sort)}>
              <option value="recent">Recently added</option>
              <option value="name">Name</option>
            </select>
          </label>
          <div class="segmented" role="group" aria-label="View">
            {(
              [
                ["grid", "Large grid", <GridIcon />],
                ["compact", "Compact grid", <CompactIcon />],
                ["list", "List", <ListIcon />],
              ] as const
            ).map(([mode, label, icon]) => (
              <button type="button" class="icon-button" aria-pressed={view === mode} title={label} aria-label={label} onClick={() => props.onView(mode)}>
                {icon}
              </button>
            ))}
          </div>
        </div>
      </div>

      {shown.length === 0 ? (
        <Empty filtered={entries.length > 0} onAddImages={props.onAddImages} />
      ) : view === "list" ? (
        <ListView entries={shown} outputs={outputs} onOpen={props.onOpen} />
      ) : (
        <ul class={`wall wall-${view}`}>
          {shown.map((e) => (
            <li key={e.id}>
              <Tile entry={e} outputs={outputs} onOpen={props.onOpen} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function Thumb({ entry, class: cls = "" }: { entry: LibraryEntry; class?: string }) {
  const c = entry.project.content;
  const fallback = c.type === "color" ? c.color : c.type === "scene" ? "#000" : c.background;
  return (
    <div class={`thumb ${cls}`} style={{ background: fallback }}>
      {entry.thumb ? <img src={entry.thumb} alt="" loading="lazy" decoding="async" /> : c.type !== "color" && <span class="thumb-pending" />}
      {c.type === "video" && <span class="thumb-kind">{clock(entry.duration) || "Video"}</span>}
    </div>
  );
}

function DisplayBadges({ entry, outputs }: { entry: LibraryEntry; outputs: OutputInfo[] }) {
  const on = displaysShowing(entry, outputs);
  if (on.length === 0) return null;
  return (
    <span class="badges">
      {on.map((o) => (
        <span class="badge" title={`Showing on ${o.name}`}>
          {o.name}
        </span>
      ))}
    </span>
  );
}

function Tile({ entry, outputs, onOpen }: { entry: LibraryEntry; outputs: OutputInfo[]; onOpen(id: string): void }) {
  const live = displaysShowing(entry, outputs).length > 0;
  return (
    <button type="button" class={`tile${live ? " is-live" : ""}`} onClick={() => onOpen(entry.id)}>
      <Thumb entry={entry} />
      <span class="tile-text">
        <span class="tile-title">{entry.project.title}</span>
        <span class="tile-meta">
          <span class="mono">{detail(entry)}</span>
          <DisplayBadges entry={entry} outputs={outputs} />
        </span>
      </span>
    </button>
  );
}

function ListView({ entries, outputs, onOpen }: { entries: LibraryEntry[]; outputs: OutputInfo[]; onOpen(id: string): void }) {
  const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
  return (
    <div class="list" aria-label="Wallpapers">
      <div class="list-row list-header" aria-hidden="true">
        <span />
        <span>Name</span>
        <span>Type</span>
        <span>Resolution</span>
        <span>On display</span>
        <span>Added</span>
      </div>
      {entries.map((e) => (
        <button type="button" class="list-row" onClick={() => onOpen(e.id)}>
          <Thumb entry={e} class="thumb-small" />
          <span class="list-title">{e.project.title}</span>
          <span class="muted">{kindLabel(e)}</span>
          <span class="mono muted">{e.project.content.type === "color" ? e.project.content.color : detail(e)}</span>
          <span>
            <DisplayBadges entry={e} outputs={outputs} />
          </span>
          <span class="muted tabular">{date.format(e.added)}</span>
        </button>
      ))}
    </div>
  );
}

function Empty({ filtered, onAddImages }: { filtered: boolean; onAddImages(): void }) {
  return (
    <div class="empty">
      {filtered ? (
        <p>Nothing matches this filter.</p>
      ) : (
        <>
          <p class="empty-title">Your library is empty</p>
          <p>Drop images (PNG, JPEG) or videos (MP4, WebM, MKV, MOV) here, or add them from disk.</p>
          <button type="button" class="button primary" onClick={onAddImages}>
            Add images
          </button>
        </>
      )}
    </div>
  );
}
