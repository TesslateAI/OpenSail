import type { WorkspaceDetailsDto } from "../api/workspace-details.ts";
import { Badge, Card } from "../ui/primitives.tsx";
import { stateLabel, stateTone } from "./model.ts";

export type StateSectionProps = {
  workspace: WorkspaceDetailsDto;
};

/** Product-facing workspace lifecycle state. */
export function StateSection({ workspace }: StateSectionProps) {
  return (
    <Card title="State">
      <p>
        <Badge tone={stateTone(workspace.state)}>{stateLabel(workspace.state)}</Badge>
      </p>
      <p className="muted">
        Workspace state is managed by the service. Infrastructure diagnostics are available only
        in the administrator section when the server grants that capability.
      </p>
    </Card>
  );
}
