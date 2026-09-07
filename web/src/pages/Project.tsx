/** Project metadata. Team membership lives on `/team`. */

import { useCallback, useState } from "react";
import { getProject, updateProject } from "../api/api.ts";
import type { ProjectDetailDto, Role } from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { appHref } from "../router.tsx";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

const ROLE_TONE: Record<Role, "accent" | "ok" | "neutral"> = {
  owner: "accent",
  admin: "ok",
  member: "ok",
  viewer: "neutral",
};

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

export function Project() {
  const {
    projectId,
    loading: bootLoading,
    error: bootError,
    reload: reloadBootstrap,
  } = useConsole();
  const [renaming, setRenaming] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [savingName, setSavingName] = useState(false);
  const [renameError, setRenameError] = useState<string | null>(null);

  const load = useCallback(
    async (signal: AbortSignal): Promise<ProjectDetailDto | null> => {
      if (projectId === null) return null;
      return getProject(projectId, signal);
    },
    [projectId],
  );
  const resource = useResource(load, [projectId]);

  const startRename = useCallback((current: string) => {
    setDraftName(current);
    setRenameError(null);
    setRenaming(true);
  }, []);

  const saveRename = useCallback(async (): Promise<void> => {
    if (projectId === null || savingName) return;
    const trimmed = draftName.trim();
    if (trimmed.length === 0) return;
    setSavingName(true);
    setRenameError(null);
    try {
      await updateProject(projectId, trimmed);
      setRenaming(false);
      reloadBootstrap();
      resource.reload();
    } catch (reason: unknown) {
      setRenameError(errorOf(reason));
    } finally {
      setSavingName(false);
    }
  }, [projectId, draftName, reloadBootstrap, resource, savingName]);

  const header = <PageHeader title="Project" subtitle="Details." />;

  if (projectId === null) {
    return (
      <>
        {header}
        {bootLoading ? (
          <StateView state="loading" title="Loading workspace" />
        ) : bootError !== null ? (
          <StateView
            state="error"
            title="Could not load projects"
            detail={bootError.message}
            onRetry={reloadBootstrap}
          />
        ) : (
          <StateView
            state="empty"
            title="No project selected"
            detail="Join a project to see its details."
          />
        )}
      </>
    );
  }

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading project" />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load project"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const project = resource.data;
  if (project === null) {
    return (
      <>
        {header}
        <StateView state="empty" title="Project not found" />
      </>
    );
  }

  const canManage = project.capabilities.manageMembers;

  return (
    <>
      <PageHeader
        title={project.name.trim() === "" ? "Project" : project.name}
        subtitle={`Project ${shortId(project.id)}`}
        actions={<Badge tone={ROLE_TONE[project.role]}>{project.role}</Badge>}
      />

      <Card
        title="Details"
        actions={
          canManage && !renaming ? (
            <button type="button" className="btn" onClick={() => startRename(project.name)}>
              Rename
            </button>
          ) : null
        }
      >
        {renaming ? (
          <form
            className="stack stack-tight"
            onSubmit={(event) => {
              event.preventDefault();
              void saveRename();
            }}
          >
            <div className="field">
              <label htmlFor="rename-project">Project name</label>
              <input
                id="rename-project"
                value={draftName}
                disabled={savingName}
                autoFocus
                onChange={(event) => setDraftName(event.target.value)}
              />
            </div>
            {renameError !== null ? (
              <p role="alert" className="muted">
                Renaming failed: {renameError}. The project keeps its current name until a retry
                succeeds.
              </p>
            ) : null}
            <div className="actions">
              <button
                type="button"
                className="btn"
                onClick={() => setRenaming(false)}
                disabled={savingName}
              >
                Cancel
              </button>
              <button
                type="submit"
                className={
                  savingName || draftName.trim().length === 0
                    ? "btn btn-primary btn-disabled"
                    : "btn btn-primary"
                }
                disabled={savingName || draftName.trim().length === 0}
              >
                {savingName ? "Saving…" : "Save name"}
              </button>
            </div>
          </form>
        ) : null}
        <div className="stack">
          <div className="row spread">
            <span className="muted">ID</span>
            <span className="mono">{project.id.trim() === "" ? "—" : project.id}</span>
          </div>
          <div className="row spread">
            <span className="muted">Your role</span>
            <span>{project.role}</span>
          </div>
          <div className="row spread">
            <span className="muted">Owner</span>
            <span className="mono" title={project.ownerUserId}>
              {shortId(project.ownerUserId)}
            </span>
          </div>
          <div className="row spread">
            <span className="muted">Created</span>
            <span>
              {project.createdAt === null || project.createdAt.trim() === ""
                ? "—"
                : project.createdAt}
            </span>
          </div>
        </div>
      </Card>

      {project.kind === "team" ? (
        <p className="muted">
          Team membership is managed on{" "}
          <a href={appHref("/team", project.id)}>the Team page</a>.
        </p>
      ) : null}
    </>
  );
}
