import type { WorkspaceDetailsDto, WorkspaceProjectSharingDto } from "../api/workspace-details.ts";
import { Card } from "../ui/primitives.tsx";
import { creatorLabel, formatDate } from "./model.ts";

export type CreatorSectionProps = {
  workspace: WorkspaceDetailsDto;
  project: WorkspaceProjectSharingDto | null;
  projectLoading: boolean;
  meUserId: string | null;
};

/** Creator attribution without exposing infrastructure ownership details. */
export function CreatorSection({
  workspace,
  project,
  projectLoading,
  meUserId,
}: CreatorSectionProps) {
  const creator = projectLoading
    ? "Loading…"
    : creatorLabel(workspace, meUserId, project?.members ?? []);

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
            <td className="kds-datetime">{formatDate(workspace.createdAt)}</td>
          </tr>
        </tbody>
      </table>
    </Card>
  );
}
