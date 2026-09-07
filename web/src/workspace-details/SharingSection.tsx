import type { WorkspaceProjectSharingDto } from "../api/workspace-details.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import {
  memberLabel,
  projectKindLabel,
  projectRoleLabel,
  sharedWithLabel,
  shortId,
} from "./model.ts";

export type SharingSectionProps = {
  project: WorkspaceProjectSharingDto | null;
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
};

const ROLE_TONES: Record<WorkspaceProjectSharingDto["role"], "accent" | "ok" | "warn" | "neutral"> = {
  owner: "accent",
  admin: "warn",
  member: "ok",
  viewer: "neutral",
};

/** Project membership and the sharing state visible to its members. */
export function SharingSection({ project, loading, error, onRetry }: SharingSectionProps) {
  return (
    <Card title="Shared Project">
      {loading ? (
        <StateView state="loading" title="Loading sharing state" />
      ) : error !== null ? (
        <StateView
          state="error"
          title="Could not load sharing state"
          detail={error.message}
          onRetry={onRetry}
        />
      ) : project === null ? (
        <StateView
          state="empty"
          title="Sharing state unavailable"
          detail="The Project is not available to this account."
        />
      ) : (
        <>
          <table className="table">
            <tbody>
              <tr>
                <th scope="row">Project</th>
                <td>
                  {project.name.trim() === "" ? "Unnamed Project" : project.name}
                  <span className="mono muted"> ({shortId(project.id)})</span>
                </td>
              </tr>
              <tr>
                <th scope="row">Kind</th>
                <td>
                  <Badge tone={project.kind === "team" ? "accent" : "neutral"}>
                    {projectKindLabel(project.kind)}
                  </Badge>
                </td>
              </tr>
              <tr>
                <th scope="row">Your role</th>
                <td>
                  <Badge tone={ROLE_TONES[project.role]}>{projectRoleLabel(project.role)}</Badge>
                </td>
              </tr>
              <tr>
                <th scope="row">Visible to</th>
                <td>{sharedWithLabel(project)}</td>
              </tr>
            </tbody>
          </table>
          <p className="muted">
            {project.kind === "personal"
              ? "This personal workspace is visible only to you."
              : "Members of this team Project can see and use this workspace."}
          </p>
          {project.kind === "team" ? (
            <div className="stack stack-tight">
              <strong>Project members</strong>
              {project.members.length === 0 ? (
                <p className="muted">No members are listed.</p>
              ) : (
                <ul className="row" aria-label="Project members">
                  {project.members.map((member) => (
                    <li key={member.userId}>
                      <Badge tone={ROLE_TONES[member.role]}>
                        {memberLabel(member)} · {projectRoleLabel(member.role)}
                      </Badge>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ) : null}
        </>
      )}
    </Card>
  );
}
