/**
 * Activity feed row, usage meter and progress/limit bars.
 * Sources:
 *   - mock surfaces.css § .feed*          (FeedRow)
 *   - mock surfaces.css § .meter-row*     (MeterRow)
 *   - mock app.css § .progress*           (ProgressBar)
 *   - mock chat.css § .limit-*            (LimitStrip, LimitMini)
 */
import type { ReactNode } from "react";
import { cx } from "../cx";
import type { Tone } from "../variants";

export interface FeedRowProps {
  icon: ReactNode;
  tone?: Tone;
  text: ReactNode;
  time: ReactNode;
}

export function FeedRow({ icon, tone = "neutral", text, time }: FeedRowProps): ReactNode {
  return (
    <div className="kds-feed-row">
      <div className={cx("kds-feed-dot", tone !== "neutral" && `kds-${tone}`)}>{icon}</div>
      <div className="kds-feed-body">
        <div className="kds-feed-text">{text}</div>
        <div className="kds-feed-time">{time}</div>
      </div>
    </div>
  );
}

export interface MeterRowProps {
  name: string;
  /** 0..100 fill fraction. */
  percent: number;
  value: ReactNode;
}

export function MeterRow({ name, percent, value }: MeterRowProps): ReactNode {
  const clamped = Math.max(0, Math.min(100, percent));
  return (
    <div className="kds-meter-row">
      <span className="kds-meter-name">{name}</span>
      <div className="kds-meter-track">
        <div className="kds-meter-fill" style={{ width: `${clamped}%` }} />
      </div>
      <span className="kds-meter-val">{value}</span>
    </div>
  );
}

export interface ProgressBarProps {
  /** 0..100 fill fraction. */
  percent: number;
  tone?: "primary" | "ok" | "warn";
}

export function ProgressBar({ percent, tone = "primary" }: ProgressBarProps): ReactNode {
  const clamped = Math.max(0, Math.min(100, percent));
  return (
    <div className="kds-progress">
      <div
        className={cx("kds-progress-bar", tone !== "primary" && `kds-${tone}`)}
        style={{ width: `${clamped}%` }}
      />
    </div>
  );
}

/** Compact token-limit rail. Source: chat.css § .limit-track/.limit-fill */
export interface LimitTrackProps {
  percent: number;
  tone?: "ok" | "warn" | "fail";
  small?: boolean;
}

export function LimitTrack({ percent, tone = "ok", small }: LimitTrackProps): ReactNode {
  const clamped = Math.max(0, Math.min(100, percent));
  return (
    <div className={cx("kds-limit-track", small && "kds-sm")}>
      <div
        className={cx("kds-limit-fill", `kds-${tone}`)}
        style={{ width: `${clamped}%` }}
      />
    </div>
  );
}

/** Budget strip that only speaks up near the cap. Source: chat.css § .limit-strip */
export interface LimitStripProps {
  text: ReactNode;
  tone?: "warn" | "fail";
}

export function LimitStrip({ text, tone = "warn" }: LimitStripProps): ReactNode {
  return (
    <div className={cx("kds-limit-strip", `kds-${tone}`)}>
      <span className="kds-limit-strip-text">{text}</span>
    </div>
  );
}

/** Inline budget readout. Source: chat.css § .limit-mini */
export interface LimitMiniProps {
  text: ReactNode;
  tone?: "warn" | "fail";
}

export function LimitMini({ text, tone }: LimitMiniProps): ReactNode {
  return (
    <span className={cx("kds-limit-mini", tone !== undefined && `kds-${tone}`)}>{text}</span>
  );
}
