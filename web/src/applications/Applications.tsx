/** Application overview: Project-owned deployable with fixed Environments. */

import { useCallback } from "react";
import { listApplications } from "../api/applications.ts";
import type { ApplicationDto } from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { Badge, PageHeader, StateView } from "../ui/primitives.tsx";
import { Link } from "../router.tsx";

export function Applications() {
  const { projectId } = useConsole();
  const load = useCallback(
    async (signal: AbortSignal): Promise<ApplicationDto[]> => {
      if (projectId === null) return [];
      return listApplications(projectId, signal);
    },
    [projectId],
  );
  const resource = useResource(load);

  if (projectId === null) {
    return <StateView state="loading" title="Selecting project" />;
  }
  if (resource.error !== null) {
    return (
      <StateView
        state="error"
        title="Applications unavailable"
        detail={resource.error.message}
        onRetry={resource.reload}
      />
    );
  }
  if (resource.data === null) {
    return <StateView state="loading" title="Loading applications" />;
  }

  return (
    <section className="stack">
      <PageHeader
        title="Applications"
        subtitle="Agent-managed software projects. Project membership still authorizes every action."
      />
      {resource.data.length === 0 ? (
        <p className="muted">No applications yet. Ask the agent to create one in this Workspace.</p>
      ) : (
        <ul className="stack">
          {resource.data.map((application) => (
            <li key={application.id} className="card">
              <div className="row">
                <Link to={`/applications/${encodeURIComponent(application.id)}`}>
                  {application.name}
                </Link>
                <Badge>{application.state}</Badge>
              </div>
              <p className="muted mono">{application.slug}</p>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
