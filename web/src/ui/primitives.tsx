/**
 * Shared chrome primitives for the VOIE console shell and pages.
 *
 * This module is a thin SHIM over the kds design system
 * (`../design-system/`). It keeps the console's historical component API
 * (PageHeader / Card variants / Badge tones / StateView kinds) so the ~39
 * existing call sites keep compiling unchanged, while the rendered markup and
 * class names are the premium `kds-*` ones ported from the reference mock.
 *
 * New code should prefer importing from `../design-system` directly; this
 * shim exists to retire the duplicate primitives layer without a big-bang
 * rewrite of every call site.
 */

import type { ReactNode } from "react";
import { Badge as KdsBadge } from "../design-system/components/Badge";
import { Button as KdsButton } from "../design-system/components/Button";
import { Card as KdsCard, PageHeader as KdsPageHeader } from "../design-system/components/Card";
import { StateView as KdsStateView } from "../design-system/components/StateView";
import type { Tone } from "../design-system/variants";

// --- PageHeader -----------------------------------------------------------

export type PageHeaderProps = {
  title: string;
  subtitle?: string | undefined;
  actions?: ReactNode | undefined;
};

export function PageHeader({ title, subtitle, actions }: PageHeaderProps) {
  return (
    <KdsPageHeader
      title={title}
      {...(subtitle === undefined ? {} : { subtitle })}
      {...(actions === undefined ? {} : { actions })}
    />
  );
}

// --- Card -----------------------------------------------------------------

export const CARD_VARIANTS = ["default", "terminal", "failure", "unknown"] as const;
export type CardVariant = (typeof CARD_VARIANTS)[number];

export type CardProps = {
  title?: string | undefined;
  actions?: ReactNode | undefined;
  variant?: CardVariant | undefined;
  /** Set to "kds-flush" so tables meet the card ring (the mock's table idiom). */
  bodyClass?: string | undefined;
  className?: string | undefined;
  children: ReactNode;
};

/** Variant -> extra class on the kds card ring. */
const CARD_VARIANT_CLASS: Record<CardVariant, string | undefined> = {
  default: undefined,
  terminal: "kds-card-terminal",
  failure: "kds-card-failure",
  unknown: "kds-card-unknown",
};

export function Card({
  title,
  actions,
  variant = "default",
  bodyClass,
  className,
  children,
}: CardProps) {
  const merged = [CARD_VARIANT_CLASS[variant], className]
    .filter((part) => part !== undefined && part !== "")
    .join(" ");
  return (
    <KdsCard
      {...(title === undefined ? {} : { title })}
      {...(actions === undefined ? {} : { actions })}
      {...(merged === "" ? {} : { className: merged })}
      {...(bodyClass === undefined ? {} : { bodyClass })}
    >
      {children}
    </KdsCard>
  );
}

// --- Badge ----------------------------------------------------------------

/** The console's historical five tones; the kds system adds `info` and
 * `pending`, which are re-exported here so pages can reach them too. */
export const BADGE_TONES = ["neutral", "ok", "warn", "fail", "accent", "info", "pending"] as const;
export type BadgeTone = (typeof BADGE_TONES)[number];

export type BadgeProps = {
  tone?: BadgeTone | undefined;
  /** Optional 6px leading dot; inherits the tone via `currentColor`. */
  dot?: boolean | undefined;
  children: ReactNode;
};

export function Badge({ tone = "neutral", dot = false, children }: BadgeProps) {
  return (
    <KdsBadge tone={tone as Tone} dot={dot}>
      {children}
    </KdsBadge>
  );
}

// --- StateView ------------------------------------------------------------

export const STATE_KINDS = ["loading", "error", "empty"] as const;
export type StateKind = (typeof STATE_KINDS)[number];

export type StateViewProps = {
  state: StateKind;
  title?: string | undefined;
  detail?: string | undefined;
  onRetry?: (() => void) | undefined;
};

const DEFAULT_STATE_TITLES: Record<StateKind, string> = {
  loading: "Loading",
  error: "Something went wrong",
  empty: "Nothing here yet",
};

/** The mock has no spinners and no skeletons: every state is the same centred
 * column with a 42px muted icon circle. Loading reads as a settled tray. */
function StateGlyph({ state }: { state: StateKind }) {
  if (state === "error") {
    return (
      <svg width="19" height="19" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path
          d="M12 8.5v4.5M12 16.2v.3"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
        />
        <circle cx="12" cy="12" r="8.4" stroke="currentColor" strokeWidth="1.6" />
      </svg>
    );
  }
  if (state === "loading") {
    return (
      <svg width="19" height="19" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle cx="12" cy="12" r="8.4" stroke="currentColor" strokeWidth="1.6" opacity=".55" />
        <path d="M12 7.4V12l3 1.8" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
      </svg>
    );
  }
  return (
    <svg width="19" height="19" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M4.5 8.6h15v9.1a1.8 1.8 0 0 1-1.8 1.8H6.3a1.8 1.8 0 0 1-1.8-1.8V8.6Z"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
      <path
        d="M7.6 8.6V6.3a1.8 1.8 0 0 1 1.8-1.8h5.2a1.8 1.8 0 0 1 1.8 1.8v2.3"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function StateView({ state, title, detail, onRetry }: StateViewProps) {
  return (
    <div role={state === "error" ? "alert" : "status"}>
      <KdsStateView
        className={`kds-state-${state}`}
        icon={<StateGlyph state={state} />}
        title={title ?? DEFAULT_STATE_TITLES[state]}
        {...(detail === undefined ? {} : { detail })}
        {...(state === "error" && onRetry !== undefined
          ? {
              action: (
                <KdsButton size="sm" onClick={onRetry}>
                  Retry
                </KdsButton>
              ),
            }
          : {})}
      />
    </div>
  );
}
