import { useState } from "react";
import {
  deleteWorkspace,
  replaceWorkspace,
  type WorkspaceDetailsDto,
  type WorkspaceProjectSharingDto,
} from "../api/workspace-details.ts";
import { Card } from "../ui/primitives.tsx";
import { workspaceTitle } from "./model.ts";
import { ConfirmDialog } from "./ConfirmDialog.tsx";

export type LifecycleSectionProps = {
  workspace: WorkspaceDetailsDto;
  project: WorkspaceProjectSharingDto | null;
  onChanged: () => void;
  onDeleted: () => void;
};

type DialogKind = "replace" | "delete";

/** Lifecycle controls with an explicit confirmation before each mutation. */
export function LifecycleSection({
  workspace,
  project,
  onChanged,
  onDeleted,
}: LifecycleSectionProps) {
  const [dialog, setDialog] = useState<DialogKind | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const canReplace = project?.capabilities.operateSessions === true;
  const canDelete = project?.capabilities.manageMembers === true;
  const title = workspaceTitle(workspace);

  const openDialog = (kind: DialogKind): void => {
    if (busy) return;
    setError(null);
    setNotice(null);
    setDialog(kind);
  };

  const closeDialog = (): void => {
    if (busy) return;
    setError(null);
    setDialog(null);
  };

  const confirmReplace = async (): Promise<void> => {
    if (busy || !canReplace) return;
    setBusy(true);
    setError(null);
    try {
      await replaceWorkspace(workspace.id);
      setDialog(null);
      setNotice("Workspace replacement accepted. The workspace keeps its identity and history.");
      onChanged();
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setBusy(false);
    }
  };

  const confirmDelete = async (): Promise<void> => {
    if (busy || !canDelete) return;
    setBusy(true);
    setError(null);
    try {
      await deleteWorkspace(workspace.id);
      setDialog(null);
      setNotice("Workspace deleted.");
      onDeleted();
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Card title="Replace or delete">
        <div className="stack">
          <div className="stack stack-tight">
            <strong>Replace workspace</strong>
            <p className="muted">
              Replace the workspace environment while keeping its identity, saved data, and
              conversation history.
            </p>
            <div className="actions">
              {canReplace ? (
                <button type="button" className="btn" disabled={busy} onClick={() => openDialog("replace")}>
                  Replace workspace
                </button>
              ) : (
                <span className="muted">Requires the server-granted operate capability.</span>
              )}
            </div>
          </div>
          <div className="stack stack-tight">
            <strong>Delete workspace</strong>
            <p className="muted">
              Delete this workspace permanently. The service refuses deletion while conversations
              still reference it.
            </p>
            <div className="actions">
              {canDelete ? (
                <button
                  type="button"
                  className="btn btn-danger"
                  disabled={busy}
                  onClick={() => openDialog("delete")}
                >
                  Delete workspace
                </button>
              ) : (
                <span className="muted">Requires the server-granted management capability.</span>
              )}
            </div>
          </div>
          {notice !== null ? <p className="muted" role="status">{notice}</p> : null}
          {project === null ? (
            <p className="muted">Workspace permissions are unavailable until the Project loads.</p>
          ) : null}
        </div>
      </Card>
      {dialog === "replace" ? (
        <ConfirmDialog
          title="Replace workspace environment?"
          message={`Replace “${title}”? The workspace identity, saved data, and conversation history stay attached. Active work may pause while the service replaces the environment.`}
          confirmLabel="Replace workspace"
          busy={busy}
          error={error}
          onConfirm={() => void confirmReplace()}
          onCancel={closeDialog}
        />
      ) : null}
      {dialog === "delete" ? (
        <ConfirmDialog
          title="Delete workspace?"
          message={`Delete “${title}” permanently? This cannot be undone, and the service refuses the operation while conversations still reference the workspace.`}
          confirmLabel="Delete workspace"
          danger
          busy={busy}
          error={error}
          onConfirm={() => void confirmDelete()}
          onCancel={closeDialog}
        />
      ) : null}
    </>
  );
}
