/**
 * Menu primitives — popover menus, menu rows, option rows, menu-back,
 * plus-trigger. Effectless: open state and scrim wiring belong to the caller.
 * Sources:
 *   - mock chat.css § .popover-scrim/.mode-menu/.mode-option/.menu-row/.menu-back
 *   - mock chat.css § .mode-tag/.menu-foot/.plus-trigger
 */
import type { ReactNode } from "react";
import { cx } from "../cx";

export interface MenuProps {
  wide?: boolean;
  className?: string;
  children: ReactNode;
}

export function Menu({ wide, className, children }: MenuProps): ReactNode {
  return <div className={cx("kds-menu", wide && "kds-wide", className)}>{children}</div>;
}

export interface MenuRowProps {
  icon?: ReactNode;
  children: ReactNode;
  trailing?: ReactNode;
  onClick?: () => void;
}

export function MenuRow({ icon, children, trailing, onClick }: MenuRowProps): ReactNode {
  return (
    <button type="button" className="kds-menu-row" onClick={onClick}>
      {icon ?? null}
      {children}
      <div className="kds-spacer" />
      {trailing ?? null}
    </button>
  );
}

export function MenuSeparator(): ReactNode {
  return <div className="kds-menu-sep" />;
}

export interface MenuOptionProps {
  icon?: ReactNode;
  /** Optional colored circular icon backdrop. */
  iconTone?: string;
  label: ReactNode;
  /** Small uppercase tag, e.g. "default" or "quiet". */
  tag?: ReactNode;
  tagQuiet?: boolean;
  blurb?: ReactNode;
  active?: boolean;
  loud?: boolean;
  trailing?: ReactNode;
  onClick?: () => void;
}

export function MenuOption({
  icon,
  iconTone,
  label,
  tag,
  tagQuiet,
  blurb,
  active,
  loud,
  trailing,
  onClick,
}: MenuOptionProps): ReactNode {
  return (
    <button
      type="button"
      className={cx("kds-menu-option", active && "kds-active")}
      onClick={onClick}
    >
      {icon !== undefined ? (
        <span className="kds-menu-icon" style={iconTone !== undefined ? { background: iconTone } : undefined}>
          {icon}
        </span>
      ) : null}
      <span className="kds-menu-option-body">
        <span className={cx("kds-menu-option-label", loud && "kds-loud")}>
          {label}
          {tag !== undefined ? (
            <span className={cx("kds-menu-tag", tagQuiet && "kds-quiet")}>{tag}</span>
          ) : null}
        </span>
        {blurb !== undefined ? <span className="kds-menu-option-blurb">{blurb}</span> : null}
      </span>
      {trailing ?? null}
    </button>
  );
}

export interface MenuBackProps {
  label: string;
  onBack: () => void;
}

export function MenuBack({ label, onBack }: MenuBackProps): ReactNode {
  return (
    <button type="button" className="kds-menu-back" onClick={onBack}>
      <span className="kds-menu-back-caret">{label}</span>
    </button>
  );
}

export interface MenuFootProps {
  children: ReactNode;
}

export function MenuFoot({ children }: MenuFootProps): ReactNode {
  return <div className="kds-menu-foot">{children}</div>;
}

/** Scroll-cancelling scrim behind a popover. Source: chat.css § .popover-scrim */
export function PopoverScrim({ onClick }: { onClick: () => void }): ReactNode {
  return <div className="kds-popover-scrim" onClick={onClick} />;
}
