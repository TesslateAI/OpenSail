/**
 * Card — ring + squircle-ish radius, no shadow; head/body/actions anatomy.
 * PageHeader — title/subtitle/actions block used above every surface.
 * Source: mock app.css § .card* / .page-header* ; mock ui.js § Card/PageHeader
 */
import type { MouseEvent, ReactNode } from "react";
import { cx } from "../cx";

export interface CardProps {
  title?: ReactNode;
  actions?: ReactNode;
  /** Content body; when `bodyClass` is "kds-flush" the body sits directly on the ring. */
  bodyClass?: string;
  className?: string;
  onClick?: (event: MouseEvent<HTMLElement>) => void;
  children: ReactNode;
}

export function Card({ title, actions, children, className, bodyClass, onClick }: CardProps): ReactNode {
  const hasHead = title !== undefined || actions !== undefined;
  return (
    <section className={cx("kds-card", className)} onClick={onClick}>
      {hasHead ? (
        <div className="kds-card-head">
          {title !== undefined ? <h2 className="kds-card-title">{title}</h2> : null}
          {actions !== undefined ? <div className="kds-card-actions">{actions}</div> : null}
        </div>
      ) : null}
      <div className={cx("kds-card-body", bodyClass)}>{children}</div>
    </section>
  );
}

export interface PageHeaderProps {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
}

export function PageHeader({ title, subtitle, actions }: PageHeaderProps): ReactNode {
  return (
    <header className="kds-page-header">
      <div>
        <h1 className="kds-page-title">{title}</h1>
        {subtitle !== undefined ? <p className="kds-page-subtitle">{subtitle}</p> : null}
      </div>
      {actions !== undefined ? <div className="kds-page-actions">{actions}</div> : null}
    </header>
  );
}
