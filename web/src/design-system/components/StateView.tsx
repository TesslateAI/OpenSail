/**
 * StateView — centered empty / loading / error block with icon, title,
 * optional detail and an action slot.
 * Source: mock app.css § .state-view* ; mock ui.js § StateView
 */
import type { ReactNode } from "react";
import { cx } from "../cx";

export interface StateViewProps {
  icon?: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export function StateView({ icon, title, detail, action, className }: StateViewProps): ReactNode {
  return (
    <div className={cx("kds-state-view", className)}>
      {icon !== undefined ? <div className="kds-state-icon">{icon}</div> : null}
      <p className="kds-state-title">{title}</p>
      {detail !== undefined ? <p className="kds-state-detail">{detail}</p> : null}
      {action ?? null}
    </div>
  );
}
