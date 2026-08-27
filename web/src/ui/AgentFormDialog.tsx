/** Agent configuration dialog: one form for both POST (create) and PATCH
 * (edit) against the real agent endpoints. The model is control-owned and
 * appears only as a read-only descriptor; edit mode cannot rename either.
 * Viewers can never mount it; the Agents page gates the triggers and this
 * component refuses to render as a second guard. */

import { useCallback, useEffect, useState, type ChangeEvent, type FormEvent } from "react";
import { createAgent, updateAgent } from "../api/api.ts";
import type { AgentSummaryDto, Uuid } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useConsole } from "../console.tsx";
import { StateView } from "./primitives.tsx";

export type AgentDialogState = { mode: "create" } | { mode: "edit"; agent: AgentSummaryDto };

/** Server-side clamp for `max_tokens` (create clamps, PATCH clamps). */
const MAX_TOKENS_LIMIT = 1024;

type AgentFormDialogProps = {
  projectId: Uuid;
  state: AgentDialogState;
  onClose: () => void;
  onSaved: (agent: AgentSummaryDto) => void;
};

export function AgentFormDialog({ projectId, state, onClose, onSaved }: AgentFormDialogProps) {
  const { canOperate } = useConsole();
  const editing = state.mode === "edit";
  const existing = editing ? state.agent : null;

  const [name, setName] = useState(existing?.name ?? "");
  const [systemPrompt, setSystemPrompt] = useState(existing?.systemPrompt ?? "");
  // Bash is the one Release 0 tool capability; the toggle is bounded on/off.
  const [bashEnabled, setBashEnabled] = useState(existing?.bashEnabled ?? true);
  const [maxTokens, setMaxTokens] = useState(String(existing?.maxTokens ?? MAX_TOKENS_LIMIT));
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  if (!canOperate) return null;

  const parsedMaxTokens = Number.parseInt(maxTokens.trim(), 10);
  const maxTokensValid =
    Number.isInteger(parsedMaxTokens) && parsedMaxTokens >= 1 && parsedMaxTokens <= MAX_TOKENS_LIMIT;
  const nameValid = editing || name.trim().length > 0;
  const readyToSubmit = !submitting && nameValid && maxTokensValid;

  const submit = async (): Promise<void> => {
    if (!readyToSubmit) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      const saved =
        state.mode === "create"
          ? await createAgent(projectId, {
              id: newIntentId(),
              name: name.trim(),
              systemPrompt: systemPrompt.trim(),
              bashEnabled,
              maxTokens: parsedMaxTokens,
            })
          : await updateAgent(state.agent.id, {
              systemPrompt: systemPrompt.trim(),
              bashEnabled,
              maxTokens: parsedMaxTokens,
            });
      onSaved(saved);
    } catch (reason: unknown) {
      setSubmitError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setSubmitting(false);
    }
  };

  const bind = (setter: (next: string) => void) => ({
    onChange: (event: ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) =>
      setter(event.target.value),
  });

  return (
    <div
      role="presentation"
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div role="dialog" aria-modal="true" aria-labelledby="agent-form-title" className="modal">
        <form
          className="stack"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="modal-head">
            <h2 id="agent-form-title">{editing ? "Edit agent" : "New agent"}</h2>
          </div>
          <div className="modal-body stack">
            <p className="muted">
              {editing
                ? "Update how runs configure prompts, tools, and tokens. The agent name and control-assigned model are fixed."
                : "Register an agent in this project; sessions pick it at creation time."}
            </p>
            <div className="field">
              <label htmlFor="agent-form-name">Name</label>
              <input
                id="agent-form-name"
                value={name}
                disabled={editing || submitting}
                required={!editing}
                placeholder="e.g. Research assistant"
                {...bind(setName)}
              />
            </div>
            {existing?.model != null && existing.model.trim() !== "" ? (
              <div className="field">
                <span className="muted">Model</span>
                <span className="mono">{existing.model}</span>
                <p className="muted">Assigned by the control plane; not editable here.</p>
              </div>
            ) : null}
            <div className="field">
              <label htmlFor="agent-form-prompt">System prompt</label>
              <textarea
                id="agent-form-prompt"
                rows={3}
                value={systemPrompt}
                disabled={submitting}
                placeholder="Instructions the agent starts every session with"
                {...bind(setSystemPrompt)}
              />
            </div>
            <div className="field">
              <span className="check-row">
                <input
                  id="agent-form-bash"
                  type="checkbox"
                  checked={bashEnabled}
                  disabled={submitting}
                  onChange={(event) => setBashEnabled(event.target.checked)}
                />
                <label htmlFor="agent-form-bash">Allow remote Bash</label>
              </span>
              <p className="muted">The only tool capability in Release 0; on by default.</p>
            </div>
            <div className="field">
              <label htmlFor="agent-form-tokens">Max tokens</label>
              <input
                id="agent-form-tokens"
                type="number"
                min={1}
                max={MAX_TOKENS_LIMIT}
                step={1}
                value={maxTokens}
                disabled={submitting}
                required
                {...bind(setMaxTokens)}
              />
              {!maxTokensValid ? (
                <p className="muted" role="alert">
                  Max tokens must be a whole number between 1 and {MAX_TOKENS_LIMIT}.
                </p>
              ) : null}
            </div>
            {submitError !== null ? (
              <p role="alert" className="muted">
                Saving the agent failed: {submitError}. Adjust nothing and try again, or cancel.
              </p>
            ) : null}
            {!nameValid && !editing ? (
              <StateView state="empty" title="A name is required" />
            ) : null}
          </div>
          <div className="modal-actions">
            <button type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={!readyToSubmit}>
              {submitting ? "Saving…" : editing ? "Save changes" : "Create agent"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
