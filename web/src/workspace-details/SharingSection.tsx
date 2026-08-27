import type { WorkspaceScopeSharingDto } from "../api/workspace-details.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import {
  memberLabel,
  scopeKindLabel,
  scopeRoleLabel,
  sharedWithLabel,
  shortId,
} from "./model.ts";

export type SharingSectionProps = {
  scope: WorkspaceScopeSharingDto | null;
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
};

const ROLE_TONES: Record<WorkspaceScopeSharingDto["role"], "accent" | "ok" | "warn" | "neutral"> = {
  owner: "accent",
  admin: "warn",
  member: "ok",
  viewer: "neutral",
};

/** Scope membership and the sharing state visible to its members. */
export function SharingSection({ scope, loading, error, onRetry }: SharingSectionProps) {
  return (
    <Card title="Shared scope">
      {loading ? (
        <StateView state="loading" title="Loading sharing state" />
      ) : error !== null ? (
        <StateView
          state="error"
          title="Could not load sharing state"
          detail={error.message}
          onRetry={onRetry}
        />
      ) : scope === null ? (
        <StateView
          state="empty"
          title="Sharing state unavailable"
          detail="The workspace scope is not available to this account."
        />
      ) : (
        <>
          <table className="table">
            <tbody>
              <tr>
                <th scope="row">Scope</th>
                <td>
                  {scope.name.trim() === "" ? "Unnamed scope" : scope.name}
                  <span className="mono muted"> ({shortId(scope.id)})</span>
                </td>
              </tr>
              <tr>
                <th scope="row">Kind</th>
                <td>
                  <Badge tone={scope.kind === "team" ? "accent" : "neutral"}>
                    {scopeKindLabel(scope.kind)}
                  </Badge>
                </td>
              </tr>
              <tr>
                <th scope="row">Your role</th>
                <td>
                  <Badge tone={ROLE_TONES[scope.role]}>{scopeRoleLabel(scope.role)}</Badge>
                </td>
              </tr>
              <tr>
                <th scope="row">Visible to</th>
                <td>{sharedWithLabel(scope)}</td>
              </tr>
            </tbody>
          </table>
          <p className="muted">
            {scope.kind === "personal"
              ? "This personal workspace is visible only to you."
              : "Members of this team scope can see and use this workspace."}
          </p>
          {scope.kind === "team" ? (
            <div className="stack stack-tight">
              <strong>Scope members</strong>
              {scope.members.length === 0 ? (
                <p className="muted">No members are listed.</p>
              ) : (
                <ul className="row" aria-label="Scope members">
                  {scope.members.map((member) => (
                    <li key={member.userId}>
                      <Badge tone={ROLE_TONES[member.role]}>
                        {memberLabel(member)} · {scopeRoleLabel(member.role)}
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
