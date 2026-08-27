import { useCallback, useState } from "react";
import {
  getWorkspaceDetails,
  getWorkspaceScope,
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
  const scopeId = workspaceResource.data?.scopeId ?? null;
  const scopeResource = useResource(
    useCallback(
      (signal: AbortSignal) =>
        scopeId === null ? Promise.resolve(null) : getWorkspaceScope(scopeId, signal),
      [scopeId],
    ),
    [scopeId],
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
        scopeId === null ? Promise.resolve([]) : listWorkspaceAgentPresets(scopeId, signal),
      [scopeId],
    ),
    [scopeId],
  );
  const [deleted, setDeleted] = useState(false);

  const handleChanged = useCallback((): void => {
    workspaceResource.reload();
    scopeResource.reload();
    conversationsResource.reload();
    presetsResource.reload();
    onChanged?.();
  }, [
    conversationsResource.reload,
    onChanged,
    presetsResource.reload,
    scopeResource.reload,
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
  const scope =
    scopeResource.loading || scopeResource.error !== null ? null : scopeResource.data;


  return (
    <>
      <PageHeader title={workspaceTitle(workspace)} subtitle="Workspace details" />
      <div className="stack">
        <FactsSection workspace={workspace} />
        <StateSection workspace={workspace} />
        <CreatorSection
          workspace={workspace}
          scope={scope}
          scopeLoading={scopeResource.loading}
          meUserId={meUserId}
        />
        <SharingSection
          scope={scope}
          loading={scopeResource.loading}
          error={scopeResource.error}
          onRetry={scopeResource.reload}
        />
        <ConversationsSection
          conversations={conversationsResource.data ?? []}
          loading={conversationsResource.loading}
          error={conversationsResource.error}
          onRetry={conversationsResource.reload}
          scopeId={workspace.scopeId}
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
          scope={scope}
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
