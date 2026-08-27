import { useEffect } from "react";

export type ConfirmDialogProps = {
  title: string;
  message: string;
  confirmLabel: string;
  busy: boolean;
  error: string | null;
  danger?: boolean | undefined;
  onConfirm: () => void;
  onCancel: () => void;
};

/** Accessible confirmation modal used by replace and delete actions. */
export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  busy,
  error,
  danger = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [busy, onCancel]);

  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onClick={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="workspace-confirm-title">
        <div className="modal-head">
          <h2 id="workspace-confirm-title">{title}</h2>
        </div>
        <div className="modal-body stack">
          <p>{message}</p>
          {error !== null ? (
            <p className="muted" role="alert">
              {error}
            </p>
          ) : null}
        </div>
        <div className="modal-actions">
          <button type="button" className="btn" disabled={busy} onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className={danger ? "btn btn-danger" : "btn btn-primary"}
            disabled={busy}
            onClick={onConfirm}
          >
            {busy ? "Working…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
