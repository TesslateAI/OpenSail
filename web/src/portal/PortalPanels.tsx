/**
 * Portal management panels: thin wrappers that mount the existing
 * management surfaces (SecretVault, WorkspaceDetails, project workspaces and
 * members) inside the portal shell. Each surface keeps its own internal
 * behavior; this module only supplies the context values the shell owns.
 */

import { useCallback } from "react";
import { SecretVault } from "../secrets/SecretVault.tsx";
import { WorkspaceDetails } from "../workspace-details/WorkspaceDetails.tsx";
import { ProjectWorkspaces } from "../projects/ProjectWorkspaces.tsx";
import { AgentPresets } from "../projects/AgentPresets.tsx";
import { useConsole } from "../console.tsx";
import { appHref, useRouter } from "../router.tsx";
import { PageHeader, StateView } from "../ui/primitives.tsx";

export function SecretVaultPage() {
  const { selectedProject, me } = useConsole();
  if (selectedProject === null || me === null) {
    return <StateView state="loading" title="Preparing secrets" />;
  }
  return (
    <section className="portal-panel">
      <PageHeader title="Secret vault" subtitle={`Secrets scoped to ${selectedProject.name}.`} />
      <SecretVault
        projectId={selectedProject.id}
        projectKind={selectedProject.kind}
        projectName={selectedProject.name}
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
  const { me, platformAdmin, projectId } = useConsole();
  const { navigate } = useRouter();

  const handleOpenConversation = useCallback(
    (conversationId: string) => {
      navigate(appHref(`/chat/${encodeURIComponent(conversationId)}`, projectId));
    },
    [navigate, projectId],
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

function ProjectWorkspaceSurface({ projectId }: { projectId: string }) {
  const { selectedProject, me } = useConsole();
  if (selectedProject === null || me === null) {
    return <StateView state="loading" title="Preparing workspaces" />;
  }
  return (
    <ProjectWorkspaces
      projectId={projectId}
      meUserId={me.userId}
      canOperate={selectedProject.capabilities.operateSessions}
      canManage={selectedProject.capabilities.manageMembers}
      subtitle={
        selectedProject.kind === "team"
          ? `Shared workspaces in ${selectedProject.name}.`
          : "Your personal workspaces."
      }
    />
  );
}

/** Workspaces are the ordinary user's durable execution resources. */
export function WorkspacesPage() {
  const { projectId, selectedProject } = useConsole();
  if (projectId === null || selectedProject === null) {
    return <StateView state="loading" title="Preparing workspaces" />;
  }
  return (
    <section className="portal-panel">
      <ProjectWorkspaceSurface projectId={projectId} />
    </section>
  );
}

export { TeamPage } from "../team/TeamPage.tsx";

export type ProjectsPageProps = {
  projectId: string | null;
};

/** Compatibility landing for leftover /scopes links; primary navigation uses Workspaces/Team. */
export function ProjectsPage({ projectId: routeProjectId }: ProjectsPageProps) {
  const { projectId, selectedProject, me } = useConsole();
  const resolvedProjectId = routeProjectId ?? projectId;
  if (resolvedProjectId === null || selectedProject === null || me === null) {
    return <StateView state="loading" title="Preparing project" />;
  }
  const canOperate = selectedProject.capabilities.operateSessions;
  const canManage = selectedProject.capabilities.manageMembers;
  return (
    <section className="portal-panel portal-scope-grid">
      <PageHeader title={selectedProject.name} subtitle="Project resources." />
      <div className="portal-scope-grid-item">
        <ProjectWorkspaces
          projectId={resolvedProjectId}
          meUserId={me.userId}
          canOperate={canOperate}
          canManage={canManage}
        />
      </div>
      {selectedProject.kind === "team" ? (
        <div className="portal-scope-grid-item">
          <AgentPresets projectId={resolvedProjectId} canOperate={canOperate} />
        </div>
      ) : null}
    </section>
  );
}
