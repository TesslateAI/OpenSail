/** Shared chrome primitives for the VOIE console shell and pages. */

import type { ReactNode } from "react";

// --- PageHeader -----------------------------------------------------------

export type PageHeaderProps = {
  title: string;
  subtitle?: string | undefined;
  actions?: ReactNode | undefined;
};

export function PageHeader({ title, subtitle, actions }: PageHeaderProps) {
  return (
    <header className="page-header">
      <div className="stack stack-tight">
        <h1 className="page-title">{title}</h1>
        {subtitle === undefined ? null : <p className="page-subtitle">{subtitle}</p>}
      </div>
      {actions === undefined ? null : <div className="actions">{actions}</div>}
    </header>
  );
}

// --- Card -----------------------------------------------------------------

export const CARD_VARIANTS = ["default", "terminal", "failure", "unknown"] as const;
export type CardVariant = (typeof CARD_VARIANTS)[number];

export type CardProps = {
  title?: string | undefined;
  actions?: ReactNode | undefined;
  variant?: CardVariant | undefined;
  children: ReactNode;
};

const CARD_CLASS: Record<CardVariant, string> = {
  default: "card",
  terminal: "card card-terminal",
  failure: "card card-failure",
  unknown: "card card-unknown",
};

export function Card({ title, actions, variant = "default", children }: CardProps) {
  const hasHead = title !== undefined || actions !== undefined;
  return (
    <section className={CARD_CLASS[variant]}>
      {hasHead ? (
        <div className="card-head">
          {title !== undefined ? <h2 className="card-title">{title}</h2> : null}
          {actions !== undefined ? <div className="actions">{actions}</div> : null}
        </div>
      ) : null}
      <div className="card-body">{children}</div>
    </section>
  );
}

// --- Badge ----------------------------------------------------------------

export const BADGE_TONES = ["neutral", "ok", "warn", "fail", "accent"] as const;
export type BadgeTone = (typeof BADGE_TONES)[number];

export type BadgeProps = {
  tone?: BadgeTone | undefined;
  children: ReactNode;
};

export function Badge({ tone = "neutral", children }: BadgeProps) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
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

export function StateView({ state, title, detail, onRetry }: StateViewProps) {
  return (
    <div className={`state-view state-${state}`} role={state === "error" ? "alert" : "status"}>
      {state === "loading" ? (
        <span className="state-icon" aria-hidden="true">
          <span className="spinner" />
        </span>
      ) : null}
      {state === "error" ? (
        <span className="state-icon state-icon-alert" aria-hidden="true">
          !
        </span>
      ) : null}
      {state === "empty" ? <span className="state-icon state-icon-empty" aria-hidden="true" /> : null}
      <p className="state-title">{title ?? DEFAULT_STATE_TITLES[state]}</p>
      {detail === undefined ? null : <p className="state-detail">{detail}</p>}
      {state === "error" && onRetry !== undefined ? (
        <button type="button" className="btn" onClick={onRetry}>
          Retry
        </button>
      ) : null}
    </div>
  );
}
