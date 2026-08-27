/**
 * GateBanner — calm approval affordance (never alarming).
 * Source: mock surfaces.css § .gate*, .gate.warn.
 */
import type { ReactNode } from "react";
import { cx } from "../cx";

export interface GateBannerProps {
  icon: ReactNode;
  title: ReactNode;
  text?: ReactNode;
  actions?: ReactNode;
  warn?: boolean;
}

export function GateBanner({ icon, title, text, actions, warn }: GateBannerProps): ReactNode {
  return (
    <div className={cx("kds-gate", warn && "kds-warn")}>
      <div className="kds-gate-icon">{icon}</div>
      <div className="kds-gate-body">
        <div className="kds-gate-title">{title}</div>
        {text !== undefined ? <div className="kds-gate-text">{text}</div> : null}
        {actions !== undefined ? <div className="kds-gate-actions">{actions}</div> : null}
      </div>
    </div>
  );
}

/** Toggle control (`.kds-switch`). Source: chat.css § .switch */
export interface SwitchProps {
  on: boolean;
  onChange: (next: boolean) => void;
  ariaLabel?: string;
}

export function Switch({ on, onChange, ariaLabel }: SwitchProps): ReactNode {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={ariaLabel}
      className={cx("kds-switch", on && "kds-on")}
      onClick={() => onChange(!on)}
    >
      <i />
    </button>
  );
}

/** Single toast item. Source: surfaces.css § .toast */
export interface ToastProps {
  children: ReactNode;
}

export function Toast({ children }: ToastProps): ReactNode {
  return <div className="kds-toast">{children}</div>;
}
