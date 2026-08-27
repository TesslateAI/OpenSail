/**
 * ComposerShell — the chat composer box: rounded input, attachment/action
 * row with the send action pinned right (wrap, never scroll).
 * Source: mock surfaces.css § .composer and .send-btn ; chat.css § .composer-icon.
 * Fully controlled: `value`/`onChange`/`onSend` owned by the caller.
 */
import type { KeyboardEvent, ReactNode } from "react";
import { cx } from "../../design-system/cx";
import type { FileChipModel } from "../presentation/models";
import { FileChip } from "./ToolCallCard";

export interface ComposerShellProps {
  value: string;
  onChange: (next: string) => void;
  onSend: () => void;
  /** When true, the send control becomes a stop control. */
  running?: boolean;
  onStop?: () => void;
  sendIcon: ReactNode;
  stopIcon: ReactNode;
  attachments?: ReadonlyArray<FileChipModel>;
  /** Action controls rendered before the spacer (icons/pills). */
  actions?: ReactNode;
  note?: ReactNode;
  placeholder?: string;
}

export function ComposerShell({
  value,
  onChange,
  onSend,
  running = false,
  onStop,
  sendIcon,
  stopIcon,
  attachments,
  actions,
  note,
  placeholder = "Write a message…",
}: ComposerShellProps): ReactNode {
  const canSend = running || value.trim().length > 0;
  const handleKey = (e: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (canSend) {
        if (running) onStop?.();
        else onSend();
      }
    }
  };

  return (
    <div className="kds-composer">
      <div className="kds-composer-box">
        <textarea
          className="kds-composer-input"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKey}
          placeholder={placeholder}
          rows={1}
        />
        <div className="kds-composer-actions">
          {attachments?.map((f) => <FileChip key={f.id} file={f} />) ?? null}
          {actions ?? null}
          <div className="kds-spacer" />
          <button
            type="button"
            className={cx("kds-send-btn", running && "kds-stop")}
            disabled={!canSend}
            onClick={() => { if (running) onStop?.(); else onSend(); }}
            aria-label={running ? "Stop" : "Send"}
          >
            {running ? stopIcon : sendIcon}
          </button>
        </div>
      </div>
      {note !== undefined ? <div className="kds-composer-note">{note}</div> : null}
    </div>
  );
}
