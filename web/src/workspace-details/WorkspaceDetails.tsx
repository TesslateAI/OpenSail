import { useCallback, useState } from "react";
import {
  getWorkspaceDetails,
  getWorkspaceProject,
  listWorkspaceAgentPresets,
  listWorkspaceConversations,
} from "../api/workspace-details.ts";
import type { Uuid } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { PageHeader, StateView } from "../ui/primitives.tsx";
import { AgentPresetSection } from "./AgentPresetSection.tsx";
import { ConversationsSection } from "./ConversationsSection.tsx";
import { CreatorSection } from "./CreatorSection.tsx";
import { DiagnosticsSection } from "./DiagnosticsSection.tsx";
import { FactsSection } from "./FactsSection.tsx";
import { LifecycleSection } from "./LifecycleSection.tsx";
import { SharingSection } from "./SharingSection.tsx";
import { StateSection } from "./StateSection.tsx";
import { shortId, workspaceTitle } from "./model.ts";

export type WorkspaceDetailsProps = {
  workspaceId: Uuid;
  /** Acting user id used only for creator attribution. */
  meUserId: Uuid | null;
  /** Server-derived platform-admin capability; ordinary users pass false. */
  canViewDiagnostics: boolean;
  onDeleted?: (() => void) | undefined;
  onChanged?: (() => void) | undefined;
  onOpenConversation?: ((conversationId: Uuid) => void) | undefined;
};

/**
 * Traditional details and management surface for one product Workspace.
 * Resource ownership and capabilities come from the server; this component
 * only projects them into conventional sections and confirms mutations.
 */
export function WorkspaceDetails({
  workspaceId,
  meUserId,
  canViewDiagnostics,
  onDeleted,
  onChanged,
  onOpenConversation,
}: WorkspaceDetailsProps) {
  const workspaceResource = useResource(
    useCallback((signal: AbortSignal) => getWorkspaceDetails(workspaceId, signal), [workspaceId]),
    [workspaceId],
  );
  const projectId = workspaceResource.data?.projectId ?? null;
  const projectResource = useResource(
    useCallback(
      (signal: AbortSignal) =>
        projectId === null ? Promise.resolve(null) : getWorkspaceProject(projectId, signal),
      [projectId],
    ),
    [projectId],
  );
  const loadedWorkspaceId = workspaceResource.data?.id ?? null;
  const conversationsResource = useResource(
    useCallback(
      (signal: AbortSignal) =>
        loadedWorkspaceId === null
          ? Promise.resolve([])
          : listWorkspaceConversations(loadedWorkspaceId, signal),
      [loadedWorkspaceId],
    ),
    [loadedWorkspaceId],
  );
  const presetsResource = useResource(
    useCallback(
      (signal: AbortSignal) =>
        projectId === null ? Promise.resolve([]) : listWorkspaceAgentPresets(projectId, signal),
      [projectId],
    ),
    [projectId],
  );
  const [deleted, setDeleted] = useState(false);

  const handleChanged = useCallback((): void => {
    workspaceResource.reload();
    projectResource.reload();
    conversationsResource.reload();
    presetsResource.reload();
    onChanged?.();
  }, [
    conversationsResource.reload,
    onChanged,
    presetsResource.reload,
    projectResource.reload,
    workspaceResource.reload,
  ]);

  const handleDeleted = useCallback((): void => {
    setDeleted(true);
    onDeleted?.();
  }, [onDeleted]);

  const loadingHeader = (
    <PageHeader title="Workspace details" subtitle={`Workspace ${shortId(workspaceId)}`} />
  );

  if (deleted) {
    return (
      <>
        {loadingHeader}
        <StateView state="empty" title="Workspace deleted" detail="This workspace is no longer available." />
      </>
    );
  }
  if (workspaceResource.loading) {
    return (
      <>
        {loadingHeader}
        <StateView state="loading" title="Loading workspace" />
      </>
    );
  }
  if (workspaceResource.error !== null) {
    return (
      <>
        {loadingHeader}
        <StateView
          state="error"
          title="Could not load workspace"
          detail={workspaceResource.error.message}
          onRetry={workspaceResource.reload}
        />
      </>
    );
  }
  const workspace = workspaceResource.data;
  if (workspace === null) {
    return (
      <>
        {loadingHeader}
        <StateView
          state="empty"
          title="Workspace unavailable"
          detail="The workspace is not visible in the current account."
        />
      </>
    );
  }
  const project =
    projectResource.loading || projectResource.error !== null ? null : projectResource.data;


  return (
    <>
      <PageHeader title={workspaceTitle(workspace)} subtitle="Workspace details" />
      <div className="stack">
        <FactsSection workspace={workspace} />
        <StateSection workspace={workspace} />
        <CreatorSection
          workspace={workspace}
          project={project}
          projectLoading={projectResource.loading}
          meUserId={meUserId}
        />
        <SharingSection
          project={project}
          loading={projectResource.loading}
          error={projectResource.error}
          onRetry={projectResource.reload}
        />
        <ConversationsSection
          conversations={conversationsResource.data ?? []}
          loading={conversationsResource.loading}
          error={conversationsResource.error}
          onRetry={conversationsResource.reload}
          projectId={workspace.projectId}
          onOpenConversation={onOpenConversation}
        />
        <AgentPresetSection
          presets={presetsResource.data ?? []}
          loading={presetsResource.loading}
          error={presetsResource.error}
          onRetry={presetsResource.reload}
        />
        <LifecycleSection
          workspace={workspace}
          project={project}
          onChanged={handleChanged}
          onDeleted={handleDeleted}
        />
        <DiagnosticsSection
          workspaceId={workspace.id}
          canViewDiagnostics={canViewDiagnostics}
        />
      </div>
    </>
  );
}
