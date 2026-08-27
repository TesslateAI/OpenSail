import type { WorkspaceDetailsDto, WorkspaceScopeSharingDto } from "../api/workspace-details.ts";
import { Card } from "../ui/primitives.tsx";
import { creatorLabel, formatDate } from "./model.ts";

export type CreatorSectionProps = {
  workspace: WorkspaceDetailsDto;
  scope: WorkspaceScopeSharingDto | null;
  scopeLoading: boolean;
  meUserId: string | null;
};

/** Creator attribution without exposing infrastructure ownership details. */
export function CreatorSection({
  workspace,
  scope,
  scopeLoading,
  meUserId,
}: CreatorSectionProps) {
  const creator = scopeLoading
    ? "Loading…"
    : creatorLabel(workspace, meUserId, scope?.members ?? []);

  return (
    <Card title="Creator">
      <table className="table">
        <tbody>
          <tr>
            <th scope="row">Created by</th>
            <td>{creator}</td>
          </tr>
          <tr>
            <th scope="row">Created</th>
            <td>{formatDate(workspace.createdAt)}</td>
          </tr>
        </tbody>
      </table>
    </Card>
  );
}
