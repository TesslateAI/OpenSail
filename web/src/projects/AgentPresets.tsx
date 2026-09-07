/**
 * Agent presets: named agent-settings bundles scoped to one scope. Sessions
 * can be created from a preset's prompt/tool/token configuration; the model
 * stays control-owned and surfaces only as a read-only descriptor. Presets
 * are the scope-level defaults surface; operate-capable members manage them.
 */

import { useCallback, useEffect, useState, type ChangeEvent, type FormEvent } from "react";
import {
  createAgentPreset,
  deleteAgentPreset,
  listAgentPresets,
  updateAgentPreset,
} from "../api/api.ts";
import type { AgentPresetDto, Uuid } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { shortId } from "./model.ts";

/** Server-side clamp for `max_tokens`; matches the agent form limit. */
const MAX_TOKENS_LIMIT = 8192;

export type AgentPresetsProps = {
  projectId: Uuid;
  canOperate: boolean;
};

type PresetDialogState = { mode: "create" } | { mode: "edit"; preset: AgentPresetDto };

export function AgentPresets({ projectId, canOperate }: AgentPresetsProps) {
  const [dialog, setDialog] = useState<PresetDialogState | null>(null);

  const load = useCallback(
    async (signal: AbortSignal): Promise<AgentPresetDto[]> => listAgentPresets(projectId, signal),
    [projectId],
  );
  const resource = useResource(load, [projectId]);

  return (
    <Card
      title={`Agent presets (${resource.data?.length ?? 0})`}
      actions={
        canOperate ? (
          <button type="button" className="btn" onClick={() => setDialog({ mode: "create" })}>
            New preset
          </button>
        ) : null
      }
    >
      {dialog !== null && canOperate ? (
        <PresetFormDialog
          projectId={projectId}
          state={dialog}
          onClose={() => setDialog(null)}
          onSaved={() => {
            setDialog(null);
            resource.reload();
          }}
        />
      ) : null}
      {resource.loading ? (
        <StateView state="loading" title="Loading presets" />
      ) : resource.error !== null ? (
        <StateView
          state="error"
          title="Could not load presets"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      ) : (resource.data ?? []).length === 0 ? (
        <StateView
          state="empty"
          title="No presets yet"
          detail={
            canOperate
              ? "Create a preset to reuse agent settings across this scope."
              : "Presets appear here once they are created in this scope."
          }
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Model</th>
              <th scope="col">Max tokens</th>
              <th scope="col">Bash</th>
              <th scope="col">ID</th>
              {canOperate ? <th scope="col">Actions</th> : null}
            </tr>
          </thead>
          <tbody>
            {(resource.data ?? []).map((preset) => (
              <tr key={preset.id}>
                <td>{preset.name.trim() === "" ? "—" : preset.name}</td>
                <td className="mono">
                  {preset.model === null || preset.model.trim() === "" ? (
                    <span className="muted">—</span>
                  ) : (
                    preset.model
                  )}
                </td>
                <td className="mono">{preset.maxTokens === null ? "—" : preset.maxTokens}</td>
                <td>
                  {preset.bashEnabled ? (
                    <Badge tone="ok">allowed</Badge>
                  ) : (
                    <Badge tone="neutral">denied</Badge>
                  )}
                </td>
                <td className="mono" title={preset.id}>
                  {shortId(preset.id)}
                </td>
                {canOperate ? (
                  <td>
                    <span className="actions">
                      <button
                        type="button"
                        className="btn"
                        onClick={() => setDialog({ mode: "edit", preset })}
                      >
                        Edit
                      </button>
                      <DeletePresetButton
                        projectId={projectId}
                        preset={preset}
                        disabled={dialog !== null}
                        onDeleted={() => resource.reload()}
                      />
                    </span>
                  </td>
                ) : null}
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!canOperate ? (
        <p className="muted">
          Preset changes need the operate-sessions capability in this scope.
        </p>
      ) : null}
    </Card>
  );
}

function DeletePresetButton({
  projectId,
  preset,
  disabled,
  onDeleted,
}: {
  projectId: Uuid;
  preset: AgentPresetDto;
  disabled: boolean;
  onDeleted: () => void;
}) {
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const remove = useCallback(async (): Promise<void> => {
    if (deleting) return;
    setDeleting(true);
    setError(null);
    try {
      await deleteAgentPreset(projectId, preset.id);
      onDeleted();
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setDeleting(false);
    }
  }, [deleting, onDeleted, preset.id, projectId]);

  return (
    <span className="row">
      <button
        type="button"
        className="btn btn-danger"
        disabled={deleting || disabled}
        onClick={() => void remove()}
      >
        {deleting ? "Deleting…" : "Delete"}
      </button>
      {error !== null ? (
        <span role="alert" className="muted">
          {error}
        </span>
      ) : null}
    </span>
  );
}

function PresetFormDialog({
  projectId,
  state,
  onClose,
  onSaved,
}: {
  projectId: Uuid;
  state: PresetDialogState;
  onClose: () => void;
  onSaved: () => void;
}) {
  const editing = state.mode === "edit";
  const existing = editing ? state.preset : null;

  const [name, setName] = useState(existing?.name ?? "");
  const [systemPrompt, setSystemPrompt] = useState(existing?.systemPrompt ?? "");
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

  const parsedMaxTokens = Number.parseInt(maxTokens.trim(), 10);
  const maxTokensValid =
    Number.isInteger(parsedMaxTokens) && parsedMaxTokens >= 1 && parsedMaxTokens <= MAX_TOKENS_LIMIT;
  const readyToSubmit = !submitting && name.trim().length > 0 && maxTokensValid;

  const submit = useCallback(async (): Promise<void> => {
    if (!readyToSubmit) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      if (state.mode === "edit") {
        await updateAgentPreset(projectId, state.preset.id, {
          name: name.trim(),
          systemPrompt: systemPrompt.trim(),
          bashEnabled,
          maxTokens: parsedMaxTokens,
        });
      } else {
        await createAgentPreset(projectId, {
          id: newIntentId(),
          name: name.trim(),
          systemPrompt: systemPrompt.trim(),
          bashEnabled,
          maxTokens: parsedMaxTokens,
        });
      }
      onSaved();
    } catch (reason: unknown) {
      setSubmitError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setSubmitting(false);
    }
  }, [
    bashEnabled,
    name,
    onSaved,
    parsedMaxTokens,
    readyToSubmit,
    projectId,
    state,
    systemPrompt,
  ]);

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
      <div role="dialog" aria-modal="true" aria-labelledby="preset-form-title" className="modal">
        <form
          className="stack"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="modal-head">
            <h2 id="preset-form-title">{editing ? "Edit preset" : "New preset"}</h2>
          </div>
          <div className="modal-body stack">
            <p className="muted">
              {editing
                ? "Update the preset settings; sessions already created keep their own settings."
                : "Name a reusable agent configuration for sessions in this scope."}
            </p>
            <div className="field">
              <label htmlFor="preset-form-name">Name</label>
              <input
                id="preset-form-name"
                value={name}
                disabled={submitting}
                required
                placeholder="e.g. Research assistant"
                {...bind(setName)}
              />
            </div>
            {existing?.model != null && existing.model.trim() !== "" ? (
              <div className="field">
                <span className="muted">Model</span>
                <span className="mono">{existing.model}</span>
                <p className="muted">
                  Assigned by the control plane for this preset; not editable here.
                </p>
              </div>
            ) : null}
            <div className="field">
              <label htmlFor="preset-form-prompt">System prompt</label>
              <textarea
                id="preset-form-prompt"
                rows={3}
                value={systemPrompt}
                disabled={submitting}
                placeholder="Instructions agents start sessions with"
                {...bind(setSystemPrompt)}
              />
            </div>
            <div className="field">
              <span className="check-row">
                <input
                  id="preset-form-bash"
                  type="checkbox"
                  checked={bashEnabled}
                  disabled={submitting}
                  onChange={(event) => setBashEnabled(event.target.checked)}
                />
                <label htmlFor="preset-form-bash">Allow remote Bash</label>
              </span>
              <p className="muted">The only tool capability in Release 0; on by default.</p>
            </div>
            <div className="field">
              <label htmlFor="preset-form-tokens">Max tokens</label>
              <input
                id="preset-form-tokens"
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
                Saving the preset failed: {submitError}. Adjust nothing and try again, or cancel.
              </p>
            ) : null}
          </div>
          <div className="modal-actions">
            <button type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={!readyToSubmit}>
              {submitting ? "Saving…" : editing ? "Save changes" : "Create preset"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
