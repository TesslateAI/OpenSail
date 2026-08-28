/**
 * Portal management panels: thin wrappers that mount the existing
 * management surfaces (SecretVault, WorkspaceDetails, scope workspaces and
 * members) inside the portal shell. Each surface keeps its own internal
 * behavior; this module only supplies the context values the shell owns.
 */

import { useCallback } from "react";
import { SecretVault } from "../secrets/SecretVault.tsx";
import { WorkspaceDetails } from "../workspace-details/WorkspaceDetails.tsx";
import { ScopeWorkspaces } from "../scopes/ScopeWorkspaces.tsx";
import { ScopeMembers } from "../scopes/ScopeMembers.tsx";
import { AgentPresets } from "../scopes/AgentPresets.tsx";
import { useConsole } from "../console.tsx";
import { useRouter } from "../router.tsx";
import { PageHeader, StateView } from "../ui/primitives.tsx";

export function SecretVaultPage() {
  const { selectedScope, me } = useConsole();
  if (selectedScope === null || me === null) {
    return <StateView state="loading" title="Preparing secrets" />;
  }
  return (
    <section className="portal-panel">
      <PageHeader title="Secret vault" subtitle={`Secrets scoped to ${selectedScope.name}.`} />
      <SecretVault
        scopeId={selectedScope.id}
        scopeKind={selectedScope.kind}
        scopeName={selectedScope.name}
        canWrite={false}
        meUserId={me.userId}
      />
    </section>
  );
}

export type WorkspaceDetailsPageProps = {
  workspaceId: string;
};

export function WorkspaceDetailsPage({ workspaceId }: WorkspaceDetailsPageProps) {
  const { me, platformAdmin } = useConsole();
  const { navigate } = useRouter();

  const handleOpenConversation = useCallback(
    (conversationId: string) => {
      navigate(`/chat/${encodeURIComponent(conversationId)}`);
    },
    [navigate],
  );

  if (me === null) {
    return <StateView state="loading" title="Preparing workspace" />;
  }
  return (
    <section className="portal-panel">
      <WorkspaceDetails
        workspaceId={workspaceId}
        meUserId={me.userId}
        canViewDiagnostics={platformAdmin === true}
        onOpenConversation={handleOpenConversation}
      />
    </section>
  );
}

function ScopeWorkspaceSurface({ scopeId }: { scopeId: string }) {
  const { selectedScope, me } = useConsole();
  if (selectedScope === null || me === null) {
    return <StateView state="loading" title="Preparing workspaces" />;
  }
  return (
    <ScopeWorkspaces
      scopeId={scopeId}
      meUserId={me.userId}
      canOperate={selectedScope.capabilities.operateSessions}
      canManage={selectedScope.capabilities.manageMembers}
      subtitle={
        selectedScope.kind === "team"
          ? `Shared workspaces in ${selectedScope.name}.`
          : "Your personal workspaces."
      }
    />
  );
}

/** Workspaces are the ordinary user's durable execution resources. */
export function WorkspacesPage() {
  const { projectId, selectedScope } = useConsole();
  if (projectId === null || selectedScope === null) {
    return <StateView state="loading" title="Preparing workspaces" />;
  }
  return (
    <section className="portal-panel">
      <ScopeWorkspaceSurface scopeId={projectId} />
    </section>
  );
}

/** Team membership and shared agent presets; only meaningful in team scopes. */
export function TeamPage() {
  const { projectId, selectedScope } = useConsole();
  if (projectId === null || selectedScope === null) {
    return <StateView state="loading" title="Preparing team" />;
  }
  if (selectedScope.kind !== "team") {
    return (
      <StateView
        state="empty"
        title="Personal scope"
        detail="Switch to a team scope to manage team members and shared agent presets."
      />
    );
  }
  const canOperate = selectedScope.capabilities.operateSessions;
  const canManage = selectedScope.capabilities.manageMembers;
  return (
    <section className="portal-panel portal-scope-grid">
      <PageHeader title={selectedScope.name} subtitle="Team members and shared agent presets." />
      <div className="portal-scope-grid-item">
        <ScopeMembers scopeId={projectId} canManage={canManage} />
      </div>
      <div className="portal-scope-grid-item">
        <AgentPresets scopeId={projectId} canOperate={canOperate} />
      </div>
    </section>
  );
}

export type ScopesPageProps = {
  scopeId: string | null;
};

/** Compatibility landing for old scope links; primary navigation uses Workspaces/Team. */
export function ScopesPage({ scopeId }: ScopesPageProps) {
  const { projectId, selectedScope, me } = useConsole();
  const resolvedScopeId = scopeId ?? projectId;
  if (resolvedScopeId === null || selectedScope === null || me === null) {
    return <StateView state="loading" title="Preparing scope" />;
  }
  const canOperate = selectedScope.capabilities.operateSessions;
  const canManage = selectedScope.capabilities.manageMembers;
  return (
    <section className="portal-panel portal-scope-grid">
      <PageHeader title={selectedScope.name} subtitle="Scope resources and collaboration." />
      <div className="portal-scope-grid-item">
        <ScopeWorkspaces
          scopeId={resolvedScopeId}
          meUserId={me.userId}
          canOperate={canOperate}
          canManage={canManage}
        />
      </div>
      {selectedScope.kind === "team" ? (
        <>
          <div className="portal-scope-grid-item">
            <ScopeMembers scopeId={resolvedScopeId} canManage={canManage} />
          </div>
          <div className="portal-scope-grid-item">
            <AgentPresets scopeId={resolvedScopeId} canOperate={canOperate} />
          </div>
        </>
      ) : null}
    </section>
  );
}
