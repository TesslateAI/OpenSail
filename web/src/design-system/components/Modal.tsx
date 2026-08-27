/**
 * Modal + Overlay. Presentational and effectless: open state and Escape/click
 * handling are owned by the consumer (see parity hooks for a ready-made
 * `useEscapeToClose`). Source: mock surfaces.css § .overlay/.modal*;
 * mock ui.js § Modal.
 */
import type { ReactNode } from "react";
import { cx } from "../cx";
import { IconButton } from "./Button";

export interface ModalProps {
  title: ReactNode;
  subtitle?: ReactNode;
  /** Rendered in the footer, right-aligned. */
  footer?: ReactNode;
  onClose: () => void;
  wide?: boolean;
  closeIcon: ReactNode;
  children: ReactNode;
}

export function Modal({ title, subtitle, footer, onClose, wide, closeIcon, children }: ModalProps): ReactNode {
  return (
    <div className="kds-overlay" onClick={onClose}>
      <div
        className={cx("kds-modal", wide && "kds-lg")}
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="kds-modal-head">
          <div style={{ flex: 1 }}>
            <h2 className="kds-modal-title">{title}</h2>
            {subtitle !== undefined ? <p className="kds-modal-sub">{subtitle}</p> : null}
          </div>
          <IconButton icon={closeIcon} ariaLabel="Close" onClick={onClose} />
        </div>
        <div className="kds-modal-body">{children}</div>
        {footer !== undefined ? <div className="kds-modal-foot">{footer}</div> : null}
      </div>
    </div>
  );
}
