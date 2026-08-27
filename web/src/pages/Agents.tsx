/** Agents: bounded listing of the selected project's agents with the real
 * configuration surface — POST /api/projects/:id/agents to register and
 * PATCH /api/agents/:id to edit model, system prompt, tools, and token cap.
 * Detail is loaded via GET /api/agents/:id; viewers see every action disabled. */

import { useCallback, useState } from "react";
import { getAgent, listAgents } from "../api/api.ts";
import type { AgentSummaryDto } from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";
import { AgentFormDialog, type AgentDialogState } from "../ui/AgentFormDialog.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

export function Agents() {
  const {
    projectId,
    canOperate,
    loading: bootLoading,
    error: bootError,
    reload: reloadBootstrap,
  } = useConsole();
  const [dialog, setDialog] = useState<AgentDialogState | null>(null);
  const [detailId, setDetailId] = useState<string | null>(null);

  const load = useCallback(async (signal: AbortSignal): Promise<AgentSummaryDto[]> => {
    if (projectId === null) return [];
    return listAgents(projectId, signal);
  }, [projectId]);
  const resource = useResource(load, [projectId]);

  const loadDetail = useCallback(
    async (signal: AbortSignal): Promise<AgentSummaryDto | null> => {
      if (detailId === null) return null;
      return getAgent(detailId, signal);
    },
    [detailId],
  );
  const detailResource = useResource(loadDetail, [detailId]);

  const header = (
    <PageHeader
      title="Agents"
      subtitle="Agents registered in the selected project."
      actions={
        <button
          type="button"
          className={canOperate ? "btn btn-primary" : "btn btn-disabled"}
          disabled={!canOperate}
          title={
            canOperate
              ? "Register an agent in this project"
              : "The server does not grant you the operate-sessions capability here"
          }
          onClick={() => setDialog({ mode: "create" })}
        >
          New agent
        </button>
      }
    />
  );

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
            detail="Join a project to see its agents."
          />
        )}
      </>
    );
  }
  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading agents" />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load agents"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const agents = resource.data ?? [];
  const selectedDetail = detailResource.data;

  return (
    <>
      {header}
      {dialog !== null && projectId !== null ? (
        <AgentFormDialog
          projectId={projectId}
          state={dialog}
          onClose={() => setDialog(null)}
          onSaved={() => {
            setDialog(null);
            resource.reload();
          }}
        />
      ) : null}

      {agents.length === 0 ? (
        <StateView
          state="empty"
          title="No agents yet"
          detail={
            canOperate
              ? "Create an agent to configure how runs behave in this project."
              : "Agents appear here once they are registered in this project."
          }
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
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {agents.map((agent) => (
                <tr key={agent.id}>
                  <td>{agent.name.trim() === "" ? "—" : agent.name}</td>
                  <td className="mono">
                    {agent.model === null || agent.model.trim() === "" ? (
                      <span className="muted">—</span>
                    ) : (
                      agent.model
                    )}
                  </td>
                  <td className="mono">{agent.maxTokens === null ? "—" : agent.maxTokens}</td>
                  <td>
                    {agent.bashEnabled ? (
                      <Badge tone="ok">allowed</Badge>
                    ) : (
                      <Badge tone="neutral">denied</Badge>
                    )}
                  </td>
                  <td className="mono" title={agent.id}>
                    {shortId(agent.id)}
                  </td>
                  <td>
                    <span className="actions">
                      <button
                        type="button"
                        className="btn"
                        onClick={() => setDetailId(agent.id)}
                      >
                        View
                      </button>
                      {canOperate ? (
                        <button
                          type="button"
                          className="btn"
                          onClick={() => setDialog({ mode: "edit", agent })}
                        >
                          Edit
                        </button>
                      ) : null}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {detailId !== null ? (
            <Card
              title="Agent detail"
              actions={
                <button type="button" className="btn" onClick={() => setDetailId(null)}>
                  Close
                </button>
              }
            >
              {detailResource.loading ? (
                <StateView state="loading" title="Loading agent detail" />
              ) : detailResource.error !== null ? (
                <StateView
                  state="error"
                  title="Could not load agent"
                  detail={detailResource.error.message}
                  onRetry={detailResource.reload}
                />
              ) : selectedDetail === null ? (
                <p className="muted">Select an agent to see its system prompt and full configuration.</p>
              ) : (
                <div className="stack">
                  <div className="row spread">
                    <span className="muted">Name</span>
                    <span>{selectedDetail.name.trim() === "" ? "—" : selectedDetail.name}</span>
                  </div>
                  <div className="row spread">
                    <span className="muted">ID</span>
                    <span className="mono">{selectedDetail.id}</span>
                  </div>
                  <div className="row spread">
                    <span className="muted">Model</span>
                    <span className="mono">{selectedDetail.model ?? "—"}</span>
                  </div>
                  <div className="row spread">
                    <span className="muted">Bash</span>
                    <span>{selectedDetail.bashEnabled ? "enabled" : "disabled"}</span>
                  </div>
                  <div className="row spread">
                    <span className="muted">Max tokens</span>
                    <span className="mono">{selectedDetail.maxTokens ?? "—"}</span>
                  </div>
                  <div className="stack-tight">
                    <span className="muted">System prompt</span>
                    <pre className="bash-output mono" style={{ borderTop: "none" }}>
                      {selectedDetail.systemPrompt === null || selectedDetail.systemPrompt.trim() === ""
                        ? "— no system prompt —"
                        : selectedDetail.systemPrompt}
                    </pre>
                  </div>
                </div>
              )}
            </Card>
          ) : null}
        </>
      )}
    </>
  );
}
