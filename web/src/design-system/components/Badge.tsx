/**
 * Badge — tone pill with optional dot.
 * Source: mock app.css § .badge* ; mock ui.js § Badge
 */
import type { ReactNode } from "react";
import { cx } from "../cx";
import type { Tone } from "../variants";

export interface BadgeProps {
  tone?: Tone;
  dot?: boolean;
  children: ReactNode;
  className?: string;
}

export function Badge({ tone = "neutral", dot = false, children, className }: BadgeProps): ReactNode {
  return (
    <span className={cx("kds-badge", tone !== "neutral" && `kds-badge-${tone}`, className)}>
      {dot ? <span className="kds-badge-dot" /> : null}
      {children}
    </span>
  );
}
