/** Project: metadata, rename via PATCH /api/projects/:id, and full
 * membership lifecycle for the selected project — POST
 * /api/projects/:id/members adds or reroles one member and DELETE
 * /api/projects/:id/members/:userId removes one. Every action is gated by the
 * server-emitted manage-members capability; protections (durable owner,
 * last owner) live server-side and refusals surface verbatim. */

import { useCallback, useState, type ChangeEvent, type FormEvent } from "react";
import { addProjectMember, getProject, removeProjectMember, updateProject } from "../api/api.ts";
import { parseRole, ROLES, type ProjectDetailDto, type Role, type Uuid } from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

/** Badge tone per membership role; the only place this mapping lives. */
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

  // Membership form state; one submission at a time.
  const [memberUserId, setMemberUserId] = useState("");
  const [memberRole, setMemberRole] = useState<Role>("member");
  const [addingMember, setAddingMember] = useState(false);
  const [memberError, setMemberError] = useState<string | null>(null);
  const [busyMemberId, setBusyMemberId] = useState<string | null>(null);

  const load = useCallback(
    async (signal: AbortSignal): Promise<ProjectDetailDto | null> => {
      if (projectId === null) return null;
      // The detail resource embeds the membership roster; no second fetch.
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
      // The shell chrome (project picker, sidebar) renders the same name.
      reloadBootstrap();
      resource.reload();
    } catch (reason: unknown) {
      setRenameError(errorOf(reason));
    } finally {
      setSavingName(false);
    }
  }, [projectId, draftName, reloadBootstrap, resource, savingName]);

  const submitMember = useCallback(async (): Promise<void> => {
    if (projectId === null || addingMember || resource.data === null) return;
    const trimmed = memberUserId.trim();
    if (trimmed.length === 0) return;
    setAddingMember(true);
    setMemberError(null);
    try {
      await addProjectMember(projectId, trimmed, memberRole);
      setMemberUserId("");
      resource.reload();
    } catch (reason: unknown) {
      setMemberError(errorOf(reason));
    } finally {
      setAddingMember(false);
    }
  }, [addingMember, memberRole, memberUserId, projectId, resource]);

  const rerole = useCallback(
    async (userId: Uuid, role: Role): Promise<void> => {
      if (projectId === null || busyMemberId !== null || resource.data === null) return;
      setBusyMemberId(userId);
      setMemberError(null);
      try {
        await addProjectMember(projectId, userId, role);
        resource.reload();
      } catch (reason: unknown) {
        setMemberError(errorOf(reason));
        resource.reload();
      } finally {
        setBusyMemberId(null);
      }
    },
    [busyMemberId, projectId, resource],
  );

  const removeMember = useCallback(
    async (userId: Uuid): Promise<void> => {
      if (projectId === null || busyMemberId !== null || resource.data === null) return;
      setBusyMemberId(userId);
      setMemberError(null);
      try {
        await removeProjectMember(projectId, userId);
        resource.reload();
      } catch (reason: unknown) {
        setMemberError(errorOf(reason));
      } finally {
        setBusyMemberId(null);
      }
    },
    [busyMemberId, projectId, resource],
  );

  const header = <PageHeader title="Project" subtitle="Details and membership." />;

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
  if (resource.error !== null || resource.data === null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load the project"
          detail={resource.error?.message ?? "request failed"}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const project = resource.data;
  const canManage = project.capabilities.manageMembers;

  const handleMemberUserIdChange = (event: ChangeEvent<HTMLInputElement>): void =>
    setMemberUserId(event.target.value);
  const handleMemberRoleChange = (event: ChangeEvent<HTMLSelectElement>): void => {
    setMemberRole(parseRole(event.target.value));
  };
  const handleMemberSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void submitMember();
  };

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
          <div className="row spread">
            <span className="muted">You can</span>
            <span>
              {[
                project.capabilities.read ? "read" : null,
                project.capabilities.operateSessions ? "operate sessions" : null,
                project.capabilities.manageMembers ? "manage members" : null,
              ]
                .filter((part) => part !== null)
                .join(" · ") || "nothing beyond listing"}
            </span>
          </div>
        </div>
      </Card>

      <Card title={`Members (${project.members.length})`}>
        {project.members.length === 0 ? (
          <StateView state="empty" title="No members listed" />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Subject</th>
                <th scope="col">User</th>
                <th scope="col">Role</th>
                <th scope="col">Joined</th>
                {canManage ? <th scope="col">Actions</th> : null}
              </tr>
            </thead>
            <tbody>
              {project.members.map((member) => (
                <tr key={`${member.userId}:${member.role}`}>
                  <td>{member.subject.trim() === "" ? "—" : member.subject}</td>
                  <td className="mono" title={member.userId}>
                    {shortId(member.userId)}
                  </td>
                  <td>
                    {canManage && busyMemberId === member.userId ? (
                      <Badge tone="warn">working…</Badge>
                    ) : (
                      <Badge tone={ROLE_TONE[member.role]}>{member.role}</Badge>
                    )}
                  </td>
                  <td>
                    {member.createdAt === null || member.createdAt.trim() === ""
                      ? "—"
                      : member.createdAt}
                  </td>
                  {canManage ? (
                    <td>
                      <span className="row">
                        <select
                          aria-label={`Role for ${member.userId}`}
                          value={member.role}
                          disabled={busyMemberId !== null}
                          onChange={(event) => {
                            void rerole(member.userId, parseRole(event.target.value));
                          }}
                        >
                          {ROLES.map((role) => (
                            <option key={role} value={role}>
                              {role}
                            </option>
                          ))}
                        </select>
                        <button
                          type="button"
                          className="btn"
                          disabled={busyMemberId !== null}
                          onClick={() => void removeMember(member.userId)}
                        >
                          Remove
                        </button>
                      </span>
                    </td>
                  ) : null}
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {canManage ? (
          <form className="stack stack-tight" onSubmit={handleMemberSubmit}>
            <p className="muted">
              Adding an existing user reroles them; removals of the durable owner are refused by
              the control plane.
            </p>
            <div className="row">
              <input
                aria-label="User id"
                placeholder="User id (UUID)"
                value={memberUserId}
                disabled={addingMember || busyMemberId !== null}
                onChange={handleMemberUserIdChange}
              />
              <select
                aria-label="Role"
                value={memberRole}
                disabled={addingMember || busyMemberId !== null}
                onChange={handleMemberRoleChange}
              >
                {ROLES.map((role) => (
                  <option key={role} value={role}>
                    {role}
                  </option>
                ))}
              </select>
              <button
                type="submit"
                className={
                  addingMember || memberUserId.trim().length === 0
                    ? "btn btn-primary btn-disabled"
                    : "btn btn-primary"
                }
                disabled={addingMember || memberUserId.trim().length === 0}
              >
                {addingMember ? "Saving…" : "Add or update member"}
              </button>
            </div>
            {memberError !== null ? (
              <p role="alert" className="muted">
                The membership change was refused: {memberError} Nothing changed; you can retry.
              </p>
            ) : null}
          </form>
        ) : (
          <p className="muted">
            Membership changes need the manage-members capability in this project.
          </p>
        )}
      </Card>
    </>
  );
}
