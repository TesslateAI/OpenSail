/**
 * Secret vault: the scoped management surface for user secrets.
 *
 * Rows come from GET /api/projects/:projectId/secrets and are metadata only —
 * names, versions, timestamps, and server-authoritative `canWrite`.
 * Values are write-only: they exist only inside the create/update/rotate
 * request bodies and are never returned, refilled, displayed, cached, or
 * logged by this surface. The server owns all secret material (Azure Key
 * Vault under workload identity in production, encrypted server-owned
 * storage in local dev) and authorizes every action by scope membership.
 * No agent, activation, or chat tool receives vault values by default.
 */

import { useCallback, useEffect, useState, type ChangeEvent, type FormEvent } from "react";
import {
  createSecret,
  deleteSecret,
  fetchSecretAudit,
  listSecrets,
  rotateSecret,
  SECRET_AUDIT_ACTION_LABELS,
  SECRET_AUDIT_ACTION_TONES,
  updateSecret,
  type SecretAuditDto,
  type SecretListDto,
  type SecretMetadataDto,
} from "../api/secrets.ts";
import type { ProjectKind, Uuid } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { PROJECT_KIND_LABELS, shortId } from "../projects/model.ts";

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function formatDateTime(iso: string | null): string {
  if (iso === null) return "—";
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime())
    ? iso
    : parsed.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** "You" for the acting user, else the compact user id. */
function actorLabel(actor: Uuid, meUserId: Uuid | null): string {
  if (actor.trim() === "") return "—";
  return meUserId !== null && actor === meUserId ? "You" : shortId(actor);
}

export type SecretVaultProps = {
  projectId: Uuid;
  projectKind: ProjectKind;
  /** Display name of the scope; shown in the read-only note. */
  projectName: string;
  /** Server-emitted write capability for the scope (create gating). */
  canWrite: boolean;
  /** Acting user id for "You" attribution. */
  meUserId: Uuid | null;
};

type SecretDialogState =
  | { mode: "create" }
  | { mode: "update"; secret: SecretMetadataDto }
  | { mode: "rotate"; secret: SecretMetadataDto }
  | { mode: "audit"; secret: SecretMetadataDto };

export function SecretVault({
  projectId,
  projectKind,
  projectName,
  canWrite,
  meUserId,
}: SecretVaultProps) {
  const [dialog, setDialog] = useState<SecretDialogState | null>(null);

  const load = useCallback(
    (signal: AbortSignal): Promise<SecretListDto> => listSecrets(projectId, signal),
    [projectId],
  );
  const resource = useResource(load, [projectId]);

  const secrets = resource.data?.secrets ?? [];
  const listCanWrite = resource.data?.canWrite ?? false;
  /** Server capability wins once loaded; the mount prop only pre-fills the gate. */
  const canCreate = resource.data === null ? canWrite : listCanWrite;

  return (
    <Card
      title={`Secret vault (${secrets.length})`}
      actions={
        <>
          <Badge tone="accent">{PROJECT_KIND_LABELS[projectKind]}</Badge>
          <Badge tone={listCanWrite ? "ok" : "neutral"}>
            {listCanWrite ? "write" : "read-only"}
          </Badge>
          {canCreate ? (
            <button type="button" className="btn" onClick={() => setDialog({ mode: "create" })}>
              New secret
            </button>
          ) : null}
        </>
      }
    >
      <p className="muted">
        Secret values are write-only and stay in the vault: they are never returned to this page,
        and no agent, activation, or chat tool receives them by default. Values can only be
        created, replaced, or rotated here by members with write capability in this scope.
      </p>

      {dialog !== null ? (
        <SecretDialog
          state={dialog}
          projectId={projectId}
          meUserId={meUserId}
          onClose={() => setDialog(null)}
          onSaved={() => {
            setDialog(null);
            resource.reload();
          }}
        />
      ) : null}

      {resource.loading ? (
        <StateView state="loading" title="Loading secrets" />
      ) : resource.error !== null ? (
        <StateView
          state="error"
          title="Could not load secrets"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      ) : secrets.length === 0 ? (
        <StateView
          state="empty"
          title="No secrets yet"
          detail={
            canCreate
              ? "Create a secret to store a value this scope owns. Values never leave the vault."
              : "Secrets appear here once they are created in this scope."
          }
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Version</th>
              <th scope="col">Created</th>
              <th scope="col">Updated</th>
              <th scope="col">ID</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {secrets.map((secret) => (
              <tr key={secret.id}>
                <td>{secret.name.trim() === "" ? "—" : secret.name}</td>
                <td className="mono">v{secret.version}</td>
                <td>{formatDateTime(secret.createdAt)}</td>
                <td>{formatDateTime(secret.updatedAt)}</td>
                <td className="mono" title={secret.id}>
                  {shortId(secret.id)}
                </td>
                <td>
                  <span className="actions">
                    <button
                      type="button"
                      className="btn"
                      onClick={() => setDialog({ mode: "audit", secret })}
                    >
                      Audit
                    </button>
                    {secret.canWrite ? (
                      <>
                        <button
                          type="button"
                          className="btn"
                          onClick={() => setDialog({ mode: "update", secret })}
                        >
                          Update
                        </button>
                        <button
                          type="button"
                          className="btn"
                          onClick={() => setDialog({ mode: "rotate", secret })}
                        >
                          Rotate
                        </button>
                        <DeleteSecretButton
                          secret={secret}
                          disabled={dialog !== null}
                          onDeleted={() => resource.reload()}
                        />
                      </>
                    ) : null}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {!canCreate ? (
        <p className="muted">
          Changes to secrets in {projectName.trim() === "" ? "this scope" : projectName} need the
          write capability; reading and audit stay available.
        </p>
      ) : null}
    </Card>
  );
}

function DeleteSecretButton({
  secret,
  disabled,
  onDeleted,
}: {
  secret: SecretMetadataDto;
  disabled: boolean;
  onDeleted: () => void;
}) {
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const remove = useCallback(async (): Promise<void> => {
    if (deleting) return;
    const name = secret.name.trim();
    if (
      !window.confirm(
        `Delete the secret${name === "" ? "" : ` "${name}"`}? Its value is deleted too and cannot be recovered.`,
      )
    ) {
      return;
    }
    setDeleting(true);
    setError(null);
    try {
      await deleteSecret(secret.id);
      onDeleted();
    } catch (reason: unknown) {
      setError(errorOf(reason));
    } finally {
      setDeleting(false);
    }
  }, [deleting, onDeleted, secret.id, secret.name]);

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

function SecretDialog({
  state,
  projectId,
  meUserId,
  onClose,
  onSaved,
}: {
  state: SecretDialogState;
  projectId: Uuid;
  meUserId: Uuid | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  if (state.mode === "create") {
    return <CreateDialog projectId={projectId} onClose={onClose} onSaved={onSaved} />;
  }
  if (state.mode === "audit") {
    return <AuditDialog secret={state.secret} meUserId={meUserId} onClose={onClose} />;
  }
  return (
    <ValueDialog
      secret={state.secret}
      mode={state.mode}
      onClose={onClose}
      onSaved={onSaved}
    />
  );
}

function CreateDialog({
  projectId,
  onClose,
  onSaved,
}: {
  projectId: Uuid;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const readyToSubmit = !submitting && name.trim().length > 0 && value.length > 0;

  const submit = useCallback(async (): Promise<void> => {
    if (!readyToSubmit) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      await createSecret(projectId, { name: name.trim(), value });
      onSaved();
    } catch (reason: unknown) {
      setSubmitError(errorOf(reason));
    } finally {
      setSubmitting(false);
    }
  }, [name, onSaved, readyToSubmit, projectId, submitting, value]);

  const bindName = (event: ChangeEvent<HTMLInputElement>): void => setName(event.target.value);
  const bindValue = (event: ChangeEvent<HTMLInputElement>): void => setValue(event.target.value);

  return (
    <div
      role="presentation"
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div role="dialog" aria-modal="true" aria-labelledby="secret-create-title" className="modal">
        <form
          className="stack"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="modal-head">
            <h2 id="secret-create-title">New secret</h2>
          </div>
          <div className="modal-body stack">
            <p className="muted">
              The value is sent once to the server and never shown again — not here, and not to
              any agent or chat tool. There is no way to read a stored value back.
            </p>
            <div className="field">
              <label htmlFor="secret-create-name">Name</label>
              <input
                id="secret-create-name"
                value={name}
                disabled={submitting}
                required
                placeholder="e.g. provider-api-key"
                onChange={bindName}
              />
            </div>
            <div className="field">
              <label htmlFor="secret-create-value">Value</label>
              <input
                id="secret-create-value"
                type="password"
                value={value}
                disabled={submitting}
                required
                autoComplete="new-password"
                spellCheck={false}
                placeholder="Enter the secret value"
                onChange={bindValue}
              />
            </div>
            {submitError !== null ? (
              <p role="alert" className="muted">
                Creating the secret failed: {submitError} Nothing was stored; you can retry.
              </p>
            ) : null}
          </div>
          <div className="modal-actions">
            <button type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={!readyToSubmit}>
              {submitting ? "Creating…" : "Create secret"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

/**
 * Update/rotate value form. The value field always starts empty: the server
 * never returns a stored value, so the form can never refill or display one.
 * Update replaces the stored value under a new version; rotate additionally
 * records a rotate event and is the forced-rotation path.
 */
function ValueDialog({
  secret,
  mode,
  onClose,
  onSaved,
}: {
  secret: SecretMetadataDto;
  mode: "update" | "rotate";
  onClose: () => void;
  onSaved: () => void;
}) {
  const [value, setValue] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const readyToSubmit = !submitting && value.length > 0;

  const submit = useCallback(async (): Promise<void> => {
    if (!readyToSubmit) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      if (mode === "rotate") {
        await rotateSecret(secret.id, { value });
      } else {
        await updateSecret(secret.id, { value });
      }
      onSaved();
    } catch (reason: unknown) {
      setSubmitError(errorOf(reason));
    } finally {
      setSubmitting(false);
    }
  }, [mode, onSaved, readyToSubmit, secret.id, submitting, value]);

  const bindValue = (event: ChangeEvent<HTMLInputElement>): void => setValue(event.target.value);

  return (
    <div
      role="presentation"
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="secret-value-title"
        className="modal"
      >
        <form
          className="stack"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="modal-head">
            <h2 id="secret-value-title">
              {mode === "rotate" ? "Rotate secret value" : "Update secret value"}
            </h2>
          </div>
          <div className="modal-body stack">
            <p className="muted">
              <span className="mono">
                {secret.name.trim() === "" ? shortId(secret.id) : secret.name}
              </span>{" "}
              · version v{secret.version}. Entering a value{" "}
              {mode === "rotate" ? "rotates" : "replaces"} the stored secret and bumps the
              version. The current value is never shown and cannot be recovered.
            </p>
            <div className="field">
              <label htmlFor="secret-value-input">New value</label>
              <input
                id="secret-value-input"
                type="password"
                value={value}
                disabled={submitting}
                required
                autoComplete="new-password"
                spellCheck={false}
                placeholder={
                  mode === "rotate"
                    ? "Enter the fresh secret value"
                    : "Enter the replacement secret value"
                }
                onChange={bindValue}
              />
            </div>
            {submitError !== null ? (
              <p role="alert" className="muted">
                Saving the value failed: {submitError} Nothing changed; you can retry.
              </p>
            ) : null}
          </div>
          <div className="modal-actions">
            <button type="button" className="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={!readyToSubmit}>
              {submitting
                ? "Saving…"
                : mode === "rotate"
                  ? "Rotate secret"
                  : "Save new value"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

/**
 * Audit history for one secret: metadata events only (action, actor, time,
 * version). Values never appear in the audit trail; the endpoint is read-only.
 */
function AuditDialog({
  secret,
  meUserId,
  onClose,
}: {
  secret: SecretMetadataDto;
  meUserId: Uuid | null;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const loadAudit = useCallback(
    (signal: AbortSignal): Promise<SecretAuditDto> => fetchSecretAudit(secret.id, signal),
    [secret.id],
  );
  const audit = useResource(loadAudit, [secret.id]);
  const events = audit.data?.events ?? [];

  return (
    <div
      role="presentation"
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div role="dialog" aria-modal="true" aria-labelledby="secret-audit-title" className="modal">
        <div className="modal-head">
          <h2 id="secret-audit-title">Secret audit</h2>
        </div>
        <div className="modal-body stack">
          <p className="muted">
            <span className="mono">
              {secret.name.trim() === "" ? shortId(secret.id) : secret.name}
            </span>{" "}
            · version v{secret.version}. Events are metadata only; values never appear in the
            audit trail.
          </p>
          {audit.loading ? (
            <StateView state="loading" title="Loading audit events" />
          ) : audit.error !== null ? (
            <StateView
              state="error"
              title="Could not load audit events"
              detail={audit.error.message}
              onRetry={audit.reload}
            />
          ) : events.length === 0 ? (
            <StateView state="empty" title="No audit events yet" detail="Actions appear here once the secret is created." />
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Action</th>
                  <th scope="col">Actor</th>
                  <th scope="col">At</th>
                  <th scope="col">Version</th>
                </tr>
              </thead>
              <tbody>
                {events.map((event, index) => (
                  <tr key={`${event.at ?? ""}:${event.actor}:${index}`}>
                    <td>
                      <Badge tone={SECRET_AUDIT_ACTION_TONES[event.action]}>
                        {SECRET_AUDIT_ACTION_LABELS[event.action]}
                      </Badge>
                    </td>
                    <td className="mono" title={event.actor}>
                      {actorLabel(event.actor, meUserId)}
                    </td>
                    <td>{formatDateTime(event.at)}</td>
                    <td className="mono">{event.version === null ? "—" : `v${event.version}`}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
