/**
 * Nav/chrome glyph set.
 *
 * The reference mock draws its nav with HugeIcons at size 17 and
 * strokeWidth 1.75 (absoluteStrokeWidth off, so the stroke is passed through
 * literally). This console has no icon package in `package.json` and adding
 * one is out of scope, so the glyphs the shell actually uses are redrawn here
 * as inline SVG on the same 24-unit grid with the same stroke geometry:
 * round caps, round joins, no fill.
 *
 * Every icon is a plain `<svg aria-hidden>` so it never enters the
 * accessibility tree — nav rows carry their own text label.
 */

import type { SVGProps } from "react";

export type IconProps = {
  /** Rendered box, in px. The mock's nav uses 17. */
  size?: number;
  /** Literal stroke width, not rescaled to the box (mock parity: 1.75). */
  strokeWidth?: number;
  className?: string | undefined;
};

type GlyphProps = IconProps & Omit<SVGProps<SVGSVGElement>, "children">;

function svgProps({
  size = 17,
  strokeWidth = 1.75,
  className,
  ...rest
}: GlyphProps): SVGProps<SVGSVGElement> {
  return {
    ...rest,
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth,
    strokeLinecap: "round",
    strokeLinejoin: "round",
    className,
    "aria-hidden": true,
    focusable: false,
  };
}

/** Workspaces / drives. */
export function IconFolder(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M3 8.5A2.5 2.5 0 0 1 5.5 6h3.1c.5 0 1 .22 1.32.6l1.06 1.28c.33.38.82.62 1.33.62h5.19A2.5 2.5 0 0 1 20 11v6.5a2.5 2.5 0 0 1-2.5 2.5h-12A2.5 2.5 0 0 1 3 17.5Z" />
      <path d="M3 9.5h17" />
    </svg>
  );
}

/** Applications / all apps (dashboard grid). */
export function IconGrid(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <rect x="3.2" y="3.2" width="7.6" height="7.6" rx="2.2" />
      <rect x="13.2" y="3.2" width="7.6" height="7.6" rx="2.2" />
      <rect x="3.2" y="13.2" width="7.6" height="7.6" rx="2.2" />
      <rect x="13.2" y="13.2" width="7.6" height="7.6" rx="2.2" />
    </svg>
  );
}

/** Secrets (padlock). */
export function IconLock(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <rect x="4" y="10" width="16" height="10.5" rx="3" />
      <path d="M7.75 10V7.6a4.25 4.25 0 0 1 8.5 0V10" />
      <path d="M12 14.2v2.4" />
    </svg>
  );
}

/** Settings (gear). */
export function IconGear(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <circle cx="12" cy="12" r="3.1" />
      <path d="M12 2.9l1.5 2.2 2.6-.5.6 2.6 2.4 1.1-1 2.4 1 2.4-2.4 1.1-.6 2.6-2.6-.5L12 21.1l-1.5-2.2-2.6.5-.6-2.6-2.4-1.1 1-2.4-1-2.4 2.4-1.1.6-2.6 2.6.5Z" />
    </svg>
  );
}

/** Users / people. */
export function IconUsers(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <circle cx="9.5" cy="8" r="3.5" />
      <path d="M3.5 19.4c0-2.9 2.7-4.9 6-4.9s6 2 6 4.9" />
      <path d="M16.2 5.2a3.4 3.4 0 0 1 0 6.4" />
      <path d="M18 14.9c1.6.7 2.6 2 2.6 3.6" />
    </svg>
  );
}

/** Teams (people inside a boundary). */
export function IconTeam(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <circle cx="12" cy="6.6" r="2.9" />
      <path d="M7.4 13.6a4.7 4.7 0 0 1 9.2 0" />
      <circle cx="5" cy="16.4" r="2.2" />
      <circle cx="19" cy="16.4" r="2.2" />
      <path d="M2.6 21.3c.3-1.4 1.3-2.3 2.4-2.3s2.1.9 2.4 2.3" />
      <path d="M16.6 21.3c.3-1.4 1.3-2.3 2.4-2.3s2.1.9 2.4 2.3" />
    </svg>
  );
}

/** Fabrics (server stack). */
export function IconServer(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <rect x="3" y="3.6" width="18" height="7" rx="2.4" />
      <rect x="3" y="13.4" width="18" height="7" rx="2.4" />
      <path d="M6.8 7.1h.01" />
      <path d="M6.8 16.9h.01" />
    </svg>
  );
}

/** Audit / auth (shield). */
export function IconShield(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M12 2.8 4.6 5.6v6c0 4.2 3 8 7.4 9.6 4.4-1.6 7.4-5.4 7.4-9.6v-6Z" />
    </svg>
  );
}

/** Health (gauge). */
export function IconGauge(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M3.4 17.4a9.4 9.4 0 1 1 17.2 0" />
      <path d="M12 17.2 15.6 10" />
      <circle cx="12" cy="17.9" r="1.5" />
    </svg>
  );
}

/** Chats / conversations. */
export function IconChat(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M20.4 12c0 4-3.8 7.2-8.4 7.2a9.9 9.9 0 0 1-2.6-.34L4.6 20.4l1.2-3.6A6.9 6.9 0 0 1 3.6 12c0-4 3.8-7.2 8.4-7.2s8.4 3.2 8.4 7.2Z" />
    </svg>
  );
}

/** New chat / create. */
export function IconPlus(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M12 5v14" />
      <path d="M5 12h14" />
    </svg>
  );
}

/** Disclosure chevron. */
export function IconChevronDown(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M6.5 9.5 12 15l5.5-5.5" />
    </svg>
  );
}

/** Sidebar collapse toggle (panel). */
export function IconPanel(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <rect x="3" y="4" width="18" height="16" rx="2.6" />
      <path d="M9.6 4v16" />
    </svg>
  );
}

/** Sign out. */
export function IconSignOut(props: GlyphProps) {
  return (
    <svg {...svgProps(props)}>
      <path d="M14.5 4.6h2.1A2.4 2.4 0 0 1 19 7v10a2.4 2.4 0 0 1-2.4 2.4h-2.1" />
      <path d="M10.6 8.2 6.8 12l3.8 3.8" />
      <path d="M6.9 12h8.2" />
    </svg>
  );
}
