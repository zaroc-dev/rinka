// Small stroke icons, drawn on a 20-unit grid.

import type { JSX } from "preact";

function Icon({ children, size = 18 }: { children: JSX.Element | JSX.Element[]; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      {children}
    </svg>
  );
}

export const GridIcon = () => (
  <Icon>
    <rect x="3" y="3" width="6" height="6" rx="1.2" />
    <rect x="11" y="3" width="6" height="6" rx="1.2" />
    <rect x="3" y="11" width="6" height="6" rx="1.2" />
    <rect x="11" y="11" width="6" height="6" rx="1.2" />
  </Icon>
);

export const CompactIcon = () => (
  <Icon>
    {[3, 8, 13].flatMap((y) => [3, 8, 13].map((x) => <rect x={x} y={y} width="4" height="4" rx="0.8" />))}
  </Icon>
);

export const ListIcon = () => (
  <Icon>
    <rect x="3" y="4" width="4" height="3" rx="0.8" />
    <path d="M10 5.5h7" />
    <rect x="3" y="8.5" width="4" height="3" rx="0.8" />
    <path d="M10 10h7" />
    <rect x="3" y="13" width="4" height="3" rx="0.8" />
    <path d="M10 14.5h7" />
  </Icon>
);

export const PlusIcon = () => (
  <Icon>
    <path d="M10 4v12M4 10h12" />
  </Icon>
);

export const ImportIcon = () => (
  <Icon>
    <path d="M10 3v9m0 0-3.5-3.5M10 12l3.5-3.5" />
    <path d="M4 13v2.5A1.5 1.5 0 0 0 5.5 17h9a1.5 1.5 0 0 0 1.5-1.5V13" />
  </Icon>
);

export const GearIcon = () => (
  <Icon>
    <circle cx="10" cy="10" r="2.6" />
    <path d="M10 2.5v2M10 15.5v2M17.5 10h-2M4.5 10h-2M15.3 4.7l-1.4 1.4M6.1 13.9l-1.4 1.4M15.3 15.3l-1.4-1.4M6.1 6.1 4.7 4.7" />
  </Icon>
);

export const BackIcon = () => (
  <Icon>
    <path d="M12 4.5 6.5 10l5.5 5.5" />
  </Icon>
);

export const TrashIcon = () => (
  <Icon size={16}>
    <path d="M4 6h12M8 6V4.5h4V6M5.5 6l.7 10h7.6l.7-10" />
  </Icon>
);

export const DisplayIcon = () => (
  <Icon size={16}>
    <rect x="2.5" y="4" width="15" height="10" rx="1.5" />
    <path d="M7.5 17h5M10 14v3" />
  </Icon>
);

export const SearchIcon = () => (
  <Icon size={16}>
    <circle cx="9" cy="9" r="5" />
    <path d="m13 13 3.5 3.5" />
  </Icon>
);

export const CloseIcon = () => (
  <Icon>
    <path d="m5 5 10 10M15 5 5 15" />
  </Icon>
);

export const PlayIcon = () => (
  <Icon size={16}>
    <path d="M6.5 4.5v11l9-5.5z" fill="currentColor" />
  </Icon>
);

export const PauseIcon = () => (
  <Icon size={16}>
    <path d="M7 4.5v11M13 4.5v11" stroke-width="2.4" />
  </Icon>
);

export const InfoIcon = () => (
  <Icon>
    <circle cx="10" cy="10" r="7" />
    <path d="M10 9v4.5" />
    <circle cx="10" cy="6.4" r="0.4" fill="currentColor" />
  </Icon>
);
