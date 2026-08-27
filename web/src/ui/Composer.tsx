/** Prompt composer: controlled textarea with chat-first key handling.
 *
 * Enter sends, Shift+Enter inserts a newline, Ctrl/Cmd+Enter also sends
 * (kept for muscle memory). While a run is active the primary action flips
 * to Stop, so the send affordance never hides the cancel path. Disable rules
 * live with the caller; empty prompts are blocked here.
 */

import { type KeyboardEvent } from "react";

export type ComposerProps = {
  value: string;
  onValueChange: (next: string) => void;
  onSubmit: (prompt: string) => void;
  /** Hard gate: viewer role, active run, or otherwise read-only. */
  disabled: boolean;
  /** True while the startRun request is in flight. */
  submitting: boolean;
  /** Reason shown while disabled; null falls back to role wording. */
  lockNote: string | null;
  /** True while a run is active; flips the primary action to Stop. */
  running: boolean;
  /** True while the cancel request is in flight. */
  cancelling: boolean;
  onCancel: () => void;
};

export function Composer({
  value,
  onValueChange,
  onSubmit,
  disabled,
  submitting,
  lockNote,
  running,
  cancelling,
  onCancel,
}: ComposerProps) {
  const trimmed = value.trim();
  const blocked = disabled || submitting || running || trimmed.length === 0;

  const submit = (): void => {
    if (blocked) return;
    onSubmit(trimmed);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    } else if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  };

  return (
    <form
      className="composer stack"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <label className="sr-only" htmlFor="voie-composer-prompt">
        Prompt
      </label>
      <textarea
        id="voie-composer-prompt"
        rows={3}
        value={value}
        onChange={(event) => onValueChange(event.target.value)}
        onKeyDown={handleKeyDown}
        disabled={disabled || submitting}
        placeholder={
          disabled
            ? (lockNote ?? "Read-only: this account cannot prompt the session.")
            : "Describe a task… (Enter sends, Shift+Enter breaks lines)"
        }
      />
      <div className="composer-row row spread">
        <span className="muted">
          {lockNote ??
            (disabled
              ? "Viewer access is read-only."
              : "Enter sends · Shift+Enter breaks lines")}
        </span>
        <div className="actions">
          {running ? (
            <button
              type="button"
              className={cancelling ? "btn btn-danger btn-disabled" : "btn btn-danger"}
              disabled={cancelling}
              onClick={onCancel}
            >
              {cancelling ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <button
              type="submit"
              className={blocked ? "btn btn-primary btn-disabled" : "btn btn-primary"}
              disabled={blocked}
            >
              {submitting ? "Sending…" : "Send"}
            </button>
          )}
        </div>
      </div>
    </form>
  );
}
