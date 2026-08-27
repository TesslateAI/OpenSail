/**
 * Hero composer: the chat-first first-message surface for a project.
 *
 * The agent and workspace pickers live inside the composer itself — no
 * headless create dialog — so the first message starts a durable session
 * and run in one motion. The session is created through the existing
 * session resource API, the prompt is stashed for the session route to
 * submit once, and the browser lands on the new session. Viewers see the
 * composer disabled; projects without agents or workspaces get links to
 * the management surfaces instead of dead selects.
 */

import { useCallback, useState, type KeyboardEvent } from "react";
import {
  createSession,
  listAgents,
  listSessions,
  listWorkspaces,
  projectBoundWorkspaces,
} from "../api/api.ts";
import { storeFirstPrompt } from "../api/chat.ts";
import type { AgentSummaryDto, Uuid, WorkspaceSummaryDto } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { appHref, Link, useRouter } from "../router.tsx";
import { StateView } from "./primitives.tsx";

type HeroOptions = {
  agents: AgentSummaryDto[];
  workspaces: WorkspaceSummaryDto[];
};

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

export function HeroComposer({ projectId }: { projectId: Uuid }) {
  const { canOperate } = useConsole();
  const { navigate } = useRouter();
  const [prompt, setPrompt] = useState("");
  const [agentId, setAgentId] = useState("");
  const [workspaceId, setWorkspaceId] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const load = useCallback(async (signal: AbortSignal): Promise<HeroOptions> => {
    const [agents, sessions, workspaces] = await Promise.all([
      listAgents(projectId, signal),
      listSessions(projectId, signal),
      listWorkspaces(signal),
    ]);
    // Only workspaces this project owns or already drives may be picked;
    // foreign rows stay out of the choices instead of surfacing as a
    // selection the control plane would refuse.
    return { agents, workspaces: projectBoundWorkspaces(projectId, workspaces, sessions) };
  }, [projectId]);
  const options = useResource(load);

  const readyToSubmit =
    options.data !== null &&
    options.error === null &&
    agentId !== "" &&
    workspaceId !== "" &&
    prompt.trim().length > 0 &&
    !submitting;

  const submit = async (): Promise<void> => {
    if (!readyToSubmit) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      const session = await createSession(projectId, {
        id: newIntentId(),
        agentId,
        workspaceId,
      });
      // The first message travels with the session; the session route
      // submits it exactly once (the stash is consumed atomically).
      storeFirstPrompt(session.id, prompt.trim());
      navigate(appHref(`/sessions/${encodeURIComponent(session.id)}`, projectId));
    } catch (error: unknown) {
      setSubmitError(error instanceof Error ? error.message : "request failed");
      setSubmitting(false);
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void submit();
    } else if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      void submit();
    }
  };

  const agents = options.data?.agents ?? [];
  const workspaces = options.data?.workspaces ?? [];

  return (
    <form
      className="composer hero-composer stack"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <label className="sr-only" htmlFor="voie-hero-prompt">
        Prompt
      </label>
      <textarea
        id="voie-hero-prompt"
        rows={4}
        value={prompt}
        onChange={(event) => setPrompt(event.target.value)}
        onKeyDown={handleKeyDown}
        disabled={!canOperate || submitting}
        placeholder={
          canOperate
            ? "Describe a task… (Enter sends, Shift+Enter breaks lines)"
            : "Read-only: this account cannot operate sessions in this project."
        }
      />
      {options.loading ? (
        <StateView state="loading" title="Loading agents and workspaces" />
      ) : options.error !== null ? (
        <StateView
          state="error"
          title="Could not load agents or workspaces"
          detail={options.error.message}
          onRetry={options.reload}
        />
      ) : (
        <div className="hero-pickers row">
          <div className="field">
            <label htmlFor="voie-hero-agent">Agent</label>
            <select
              id="voie-hero-agent"
              value={agentId}
              disabled={!canOperate || submitting}
              onChange={(event) => setAgentId(event.target.value)}
            >
              <option value="" disabled>
                Select an agent…
              </option>
              {agents.map((agent) => (
                <option key={agent.id} value={agent.id} title={agent.id}>
                  {agent.name.trim() === "" ? "—" : agent.name} ({shortId(agent.id)})
                </option>
              ))}
            </select>
            {agents.length === 0 ? (
              <p className="muted">
                No agents in this project yet.{" "}
                <Link to={appHref("/agents", projectId)}>Open agents</Link>
              </p>
            ) : null}
          </div>
          <div className="field">
            <label htmlFor="voie-hero-workspace">Workspace</label>
            <select
              id="voie-hero-workspace"
              value={workspaceId}
              disabled={!canOperate || submitting}
              onChange={(event) => setWorkspaceId(event.target.value)}
            >
              <option value="" disabled>
                Select a workspace…
              </option>
              {workspaces.map((workspace) => (
                <option key={workspace.id} value={workspace.id} title={workspace.id}>
                  {workspace.fabricName === null || workspace.fabricName.trim() === "" ? "" : `${workspace.fabricName} — `}
                  {shortId(workspace.id)}
                </option>
              ))}
            </select>
            {workspaces.length === 0 ? (
              <p className="muted">
                No workspaces belong to this project yet.{" "}
                <Link to={appHref("/workspaces", projectId)}>Open workspaces</Link>
              </p>
            ) : null}
          </div>
        </div>
      )}
      {submitError !== null ? (
        <p role="alert" className="muted">
          Starting the session failed: {submitError}. Adjust nothing and try again.
        </p>
      ) : null}
      <div className="composer-row row spread">
        <span className="muted">
          {canOperate
            ? "Enter sends · Shift+Enter breaks lines"
            : "Viewer access is read-only."}
        </span>
        <div className="actions">
          <button
            type="submit"
            className={readyToSubmit ? "btn btn-primary" : "btn btn-primary btn-disabled"}
            disabled={!readyToSubmit}
          >
            {submitting ? "Starting…" : "Start chat"}
          </button>
        </div>
      </div>
    </form>
  );
}
