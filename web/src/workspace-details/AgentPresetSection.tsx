import type { WorkspaceAgentPresetDto } from "../api/workspace-details.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { shortId } from "./model.ts";

export type AgentPresetSectionProps = {
  presets: readonly WorkspaceAgentPresetDto[];
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
};

/** Read-only view of the agent presets available in the workspace scope. */
export function AgentPresetSection({
  presets,
  loading,
  error,
  onRetry,
}: AgentPresetSectionProps) {
  return (
    <Card title="Agent preset">
      {loading ? (
        <StateView state="loading" title="Loading agent presets" />
      ) : error !== null ? (
        <StateView
          state="error"
          title="Could not load agent presets"
          detail={error.message}
          onRetry={onRetry}
        />
      ) : presets.length === 0 ? (
        <StateView
          state="empty"
          title="No agent presets available"
          detail="The scope has no named preset for new conversations yet."
        />
      ) : (
        <>
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Model</th>
                <th scope="col">Max tokens</th>
                <th scope="col">Bash</th>
                <th scope="col">ID</th>
              </tr>
            </thead>
            <tbody>
              {presets.map((preset) => (
                <tr key={preset.id}>
                  <td>{preset.name.trim() === "" ? "Unnamed preset" : preset.name}</td>
                  <td className="mono">
                    {preset.model === null || preset.model.trim() === "" ? "Server default" : preset.model}
                  </td>
                  <td className="mono">{preset.maxTokens === null ? "—" : preset.maxTokens}</td>
                  <td>
                    {preset.bashEnabled ? (
                      <Badge tone="ok">Allowed</Badge>
                    ) : (
                      <Badge tone="neutral">Denied</Badge>
                    )}
                  </td>
                  <td className="mono" title={preset.id}>
                    {shortId(preset.id)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted">
            Presets are scope defaults. Chat chooses a preset when a new conversation starts; this
            details view does not change the scope configuration.
          </p>
        </>
      )}
    </Card>
  );
}
