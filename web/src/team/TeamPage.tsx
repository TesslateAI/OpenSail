/**
 * Ordinary /team surface: Team management only.
 */

import { useConsole } from "../console.tsx";
import { PageHeader, StateView } from "../ui/primitives.tsx";
import { TeamMembers } from "./TeamMembers.tsx";
import { PROJECT_ROLE_LABELS } from "../projects/model.ts";

export function TeamPage() {
  const { projectId, selectedProject, role } = useConsole();
  if (projectId === null || selectedProject === null) {
    return <StateView state="loading" title="Preparing team" />;
  }
  if (selectedProject.kind !== "team") {
    return (
      <StateView
        state="empty"
        title="Personal project"
        detail="Create a team or switch to one to manage members."
      />
    );
  }
  const canManage = selectedProject.capabilities.manageMembers;
  return (
    <section className="portal-panel">
      <PageHeader
        title={selectedProject.name}
        subtitle={`Your role: ${PROJECT_ROLE_LABELS[role]}`}
      />
      <TeamMembers
        projectId={projectId}
        ownerUserId={selectedProject.ownerUserId}
        canManage={canManage}
      />
    </section>
  );
}
