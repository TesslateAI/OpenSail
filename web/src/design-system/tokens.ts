/**
 * VOIE parity design tokens, typed mirror of `tokens.css`.
 *
 * Every custom property declared in tokens.css has a `Tokens` entry so
 * consumers can read raw values in JS (e.g. for chart geometry) or emit
 * `var(--kds-*)` references without scattering magic strings. The CSS file
 * is the single source of truth at runtime; this module only mirrors it.
 *
 * Source: the reference mock/src/tokens.css (names kds-prefixed).
 */

export const brandColors = {
  brandBlue: "#00338D",
  brandMediumBlue: "#0091DA",
  brandLightBlue: "#6D2077",
  brandCobalt: "#005EB8",
  brandGreen: "#00A3A1",
  brandPink: "#C6007E",
} as const;

export const surfaces = {
  background: "#fcfcfd",
  foreground: "#1c2024",
  card: "#ffffff",
  cardForeground: "#1c2024",
  popover: "#ffffff",
  primary: "#00338D",
  primaryForeground: "#ffffff",
  secondary: "#eef2fb",
  secondaryForeground: "#00338D",
  muted: "#f4f5f7",
  mutedForeground: "#616b78",
  faint: "#8a93a0",
  accent: "#ececf0",
  accentForeground: "#262a30",
  border: "#e6e8ee",
  input: "#e0e3ea",
} as const;

export const status = {
  ok: "#0f8a5f",
  okBg: "#e7f6ef",
  warn: "#b7791f",
  warnBg: "#fdf5e6",
  fail: "#c62f36",
  failBg: "#fdeeee",
  info: "#0091DA",
  infoBg: "#e8f5fd",
  pending: "#6b46c1",
  pendingBg: "#f1ecfd",
} as const;

export const sidebar = {
  sidebar: "#ffffff",
  sidebarForeground: "#1c2024",
  sidebarMuted: "#6b7280",
  sidebarAccent: "#f1f3f8",
  sidebarBorder: "#edeff4",
} as const;

export const darkSurfaces = {
  background: "#161719",
  foreground: "#ececee",
  card: "#202225",
  cardForeground: "#ececee",
  popover: "#202225",
  primary: "#4d8fe8",
  primaryForeground: "#0b1220",
  secondary: "#232733",
  secondaryForeground: "#cfe0f8",
  muted: "#232427",
  mutedForeground: "#9ba1ab",
  faint: "#737a85",
  accent: "#2c2e33",
  accentForeground: "#ececee",
  border: "#303236",
  input: "#303236",
} as const;

export const darkStatus = {
  ok: "#4ec08a",   okBg: "rgba(78,192,138,.13)",
  warn: "#e0a44a", warnBg: "rgba(224,164,74,.13)",
  fail: "#e5706f", failBg: "rgba(229,112,111,.13)",
  info: "#58b6ee", infoBg: "rgba(88,182,238,.13)",
  pending: "#a78bfa", pendingBg: "rgba(167,139,250,.13)",
} as const;

export const darkSidebar = {
  sidebar: "#1d1e21",
  sidebarForeground: "#ececee",
  sidebarMuted: "#9ba1ab",
  sidebarAccent: "#2a2c30",
  sidebarBorder: "#2a2c30",
} as const;

export const fontFamilies = {
  sans: '"Inter Variable", -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif',
  heading: '"Hellix", "Inter Variable", -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif',
  mono: '"Fira Code", ui-monospace, "Cascadia Code", Menlo, Consolas, monospace',
} as const;

export const radii = {
  base: "1.1rem",
  sm: "0.55rem",
  md: "0.85rem",
  lg: "1.35rem",
  xl: "2rem",
  xl2: "2.5rem",
  pill: "999px",
} as const;

export const geometry = {
  sidebarWidth: "280px",
  sidebarWidthCollapsed: "48px",
  topbarHeight: "56px",
  navRowHeight: "33px",
  chatColumnWidth: "44%",
  barHeight: "46px",
  composerRadius: "26px",
  composerMaxLinesHeight: "200px",
  notifPanelWidth: "392px",
  menuPopoverWidth: "318px",
  menuPopoverWideWidth: "352px",
  previewTabletWidth: "768px",
  previewPhoneWidth: "390px",
} as const;

export const zIndex = {
  popoverScrim: 40,
  popover: 41,
  notifScrim: 90,
  notifPanel: 91,
  modalOverlay: 100,
  toast: 200,
} as const;

export const motion = {
  fast: "150ms",
  normal: "200ms",
  easeOutQuart: "cubic-bezier(0.165, 0.84, 0.44, 1)",
} as const;

/** Responsive breakpoints used by the media queries in primitives.css. */
export const breakpoints = {
  gridWide: 1100,   // grid-4 collapses to 2 columns
  gridNarrow: 900,  // grid-2/3 collapse to 1 column
  loginAside: 980,  // login aside hides
} as const;

/** Emit `var(--kds-<name>)` from a `Tokens` key. */
export function cssVar(name: keyof Tokens): string {
  return `var(--kds-${name})`;
}

/** Full token key set, covering both `Tokens` and `tokens.css`. */
export type Tokens = typeof tokens;
export const tokens = {
  ...brandColors,
  ...surfaces,
  ...status,
  ...sidebar,
  ...fontFamilies,
  ...radii,
  ...geometry,
  ...motion,
  ring: "color-mix(in srgb, var(--kds-primary) 40%, transparent)",
  loudWarn: "#b7791f",
} as const;

/** Surface token names that a `Tone`-driven component can restyle. */
export const toneTokens = {
  ok: { fg: "var(--kds-ok)", bg: "var(--kds-ok-bg)" },
  warn: { fg: "var(--kds-warn)", bg: "var(--kds-warn-bg)" },
  fail: { fg: "var(--kds-fail)", bg: "var(--kds-fail-bg)" },
  info: { fg: "var(--kds-info)", bg: "var(--kds-info-bg)" },
  pending: { fg: "var(--kds-pending)", bg: "var(--kds-pending-bg)" },
  neutral: { fg: "var(--kds-muted-foreground)", bg: "var(--kds-muted)" },
} as const;
