import type { WorkspaceDetailsDto } from "../api/workspace-details.ts";
import { Card } from "../ui/primitives.tsx";
import { shortId, workspaceTitle } from "./model.ts";

export type FactsSectionProps = {
  workspace: WorkspaceDetailsDto;
};

/** Product-facing workspace identity facts. */
export function FactsSection({ workspace }: FactsSectionProps) {
  return (
    <Card title="Workspace details">
      <table className="table">
        <tbody>
          <tr>
            <th scope="row">Name</th>
            <td>{workspaceTitle(workspace)}</td>
          </tr>
          <tr>
            <th scope="row">Workspace ID</th>
            <td className="mono" title={workspace.id}>
              {shortId(workspace.id)}
            </td>
          </tr>
        </tbody>
      </table>
    </Card>
  );
}
