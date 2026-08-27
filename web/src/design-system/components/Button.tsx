/**
 * Button family — pills, ring-style, fully rounded per the reference.
 * Source: mock app.css § .btn* ; mock surfaces.css § .btn-danger ; mock ui.js § Btn
 */
import type { MouseEvent, ReactNode } from "react";
import { cx } from "../cx";
import type { ButtonVariant, Size } from "../variants";

export interface ButtonProps {
  variant?: ButtonVariant;
  size?: Size;
  icon?: ReactNode;
  block?: boolean;
  disabled?: boolean;
  className?: string;
  type?: "button" | "submit" | "reset";
  onClick?: (event: MouseEvent<HTMLButtonElement>) => void;
  children: ReactNode;
}

export function Button({
  variant = "default",
  size = "md",
  icon,
  block = false,
  disabled = false,
  className,
  type = "button",
  onClick,
  children,
}: ButtonProps): ReactNode {
  const cls = cx(
    "kds-btn",
    variant !== "default" && `kds-btn-${variant}`,
    size !== "md" && `kds-btn-${size}`,
    block && "kds-btn-block",
    className,
  );
  return (
    <button type={type} className={cls} disabled={disabled} onClick={onClick}>
      {icon ?? null}
      {children}
    </button>
  );
}

/** Square icon control with hover fill. Source: app.css § .icon-btn */
export interface IconButtonProps {
  icon: ReactNode;
  ariaLabel: string;
  onClick?: () => void;
  className?: string;
  children?: ReactNode;
}

export function IconButton({ icon, ariaLabel, onClick, className, children }: IconButtonProps): ReactNode {
  return (
    <button type="button" className={cx("kds-icon-btn", className)} aria-label={ariaLabel} onClick={onClick}>
      {icon}
      {children}
    </button>
  );
}

/** Bare text control in the brand colour. Source: review.css § .link-btn */
export interface LinkButtonProps {
  onClick?: () => void;
  className?: string;
  children: ReactNode;
}

export function LinkButton({ onClick, className, children }: LinkButtonProps): ReactNode {
  return (
    <button type="button" className={cx("kds-link-btn", className)} onClick={onClick}>
      {children}
    </button>
  );
}
