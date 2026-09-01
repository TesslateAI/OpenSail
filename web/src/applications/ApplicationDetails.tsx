/** One Application: Environments, Releases, and health-gated publication. */

import { useCallback, useMemo, useState } from "react";
import {
  acceptApproval,
  activateDeployment,
  deployRelease,
  getApplication,
  listDeployments,
  listEnvironments,
  listReleases,
  restartDeployment,
  rollbackDeployment,
  startPreviewLogin,
  suspendApplication,
  archiveApplication,
  restoreApplication,
  deleteApplication,
} from "../api/applications.ts";
import type { ApplicationDto, DeploymentDto, EnvironmentDto, ReleaseDto, Uuid } from "../api/dto.ts";
import { ApiError, newIntentId } from "../api/http.ts";
import { useBoundedPoll, useResource } from "../hooks.ts";
import { Badge, PageHeader, StateView } from "../ui/primitives.tsx";

export type ApplicationDetailsProps = {
  applicationId: Uuid;
};

type Detail = {
  application: ApplicationDto;
  environments: EnvironmentDto[];
  releases: ReleaseDto[];
  deployments: Record<string, DeploymentDto[]>;
};

export function ApplicationDetails({ applicationId }: ApplicationDetailsProps) {
  const load = useCallback(
    async (signal: AbortSignal): Promise<Detail> => {
      const [application, environments, releases] = await Promise.all([
        getApplication(applicationId, signal),
        listEnvironments(applicationId, signal),
        listReleases(applicationId, signal),
      ]);
      const deploymentEntries = await Promise.all(
        environments.map(async (environment) => {
          const items = await listDeployments(environment.id, signal);
          return [environment.id, items] as const;
        }),
      );
      return {
        application,
        environments,
        releases,
        deployments: Object.fromEntries(deploymentEntries),
      };
    },
    [applicationId],
  );
  const resource = useResource(load);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingApproval, setPendingApproval] = useState<{
    id: Uuid;
    kind: string;
  } | null>(null);
  const [acceptedApproval, setAcceptedApproval] = useState<{
    id: Uuid;
    kind: string;
  } | null>(null);
  const [deleted, setDeleted] = useState(false);
  const inflight = useMemo(() => {
    const detail = resource.data;
    if (detail === null) return false;
    return (
      detail.releases.some((release) => release.state === "dispatched") ||
      Object.values(detail.deployments).some((items) =>
        items.some((item) => item.state === "materializing" || item.state === "starting"),
      )
    );
  }, [resource.data]);
  const pollInflight = useCallback(async () => {
    resource.reload();
  }, [resource.reload]);
  useBoundedPoll(pollInflight, 2000, inflight);

  if (resource.error !== null) {
    return (
      <StateView
        state="error"
        title="Application unavailable"
        detail={resource.error.message}
        onRetry={resource.reload}
      />
    );
  }
  if (deleted) {
    return (
      <StateView
        state="empty"
        title="Application deleted"
        detail="Local volumes were dropped. Delete does not create a final backup."
      />
    );
  }
  if (resource.data === null) {
    return <StateView state="loading" title="Loading application" />;
  }

  const { application, environments, releases } = resource.data;
  const readyRelease =
    [...releases].reverse().find((release) => release.state === "ready") ?? null;
  const dev = environments.find((environment) => environment.kind === "dev") ?? null;
  const prod = environments.find((environment) => environment.kind === "prod") ?? null;

  const approvalId = pendingApproval?.id ?? acceptedApproval?.id;

  const run = async (
    label: string,
    work: () => Promise<void>,
    options: { reload?: boolean } = {},
  ): Promise<void> => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await work();
      setNotice(label);
      if (options.reload !== false) resource.reload();
    } catch (reason: unknown) {
      if (reason instanceof ApiError && reason.status === 409 && reason.approvalId !== null) {
        setPendingApproval({ id: reason.approvalId, kind: label });
        setError("This action needs an explicit approval before it can continue.");
      } else {
        setError(reason instanceof Error ? reason.message : "request failed");
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="stack">
      <PageHeader
        title={application.name}
        subtitle={`${application.slug} · ${application.runtimeProfile}`}
        actions={
          <>
            <Badge>{application.state}</Badge>
            {application.state !== "suspended" &&
            application.state !== "archived" &&
            application.state !== "deleting" ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() =>
                  void run("Application suspended.", async () => {
                    await suspendApplication(application.id);
                  })
                }
              >
                Suspend
              </button>
            ) : null}
            {application.state !== "archived" && application.state !== "deleting" ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() =>
                  void run("Application archived.", async () => {
                    await archiveApplication(application.id);
                  })
                }
              >
                Archive
              </button>
            ) : null}
            {application.state === "archived" ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() =>
                  void run("Application restored.", async () => {
                    await restoreApplication(application.id);
                  })
                }
              >
                Restore
              </button>
            ) : null}
            {application.state !== "deleting" ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() =>
                  void run(
                    "Application deleted.",
                    async () => {
                      await deleteApplication(application.id, approvalId);
                      setAcceptedApproval(null);
                      setDeleted(true);
                    },
                    { reload: false },
                  )
                }
              >
                Delete
              </button>
            ) : null}
          </>
        }
      />
      {error !== null ? <p className="muted">{error}</p> : null}
      {notice !== null ? <p className="muted">{notice}</p> : null}
      <p className="muted">
        Suspend keeps local volumes. Archive pins Blob restore points and releases Fabric
        capacity. Restore allocates candidate LVs and switches after proof. Delete does
        not create a final backup.
      </p>
      {pendingApproval !== null ? (
        <div className="card">
          <p>
            Approval {pendingApproval.id.slice(0, 8)} is required for {pendingApproval.kind}.
          </p>
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() =>
              void run("Approval accepted.", async () => {
                await acceptApproval(pendingApproval.id);
                setAcceptedApproval(pendingApproval);
                setPendingApproval(null);
              })
            }
          >
            Approve
          </button>
        </div>
      ) : null}
      <div className="card">
        <h2>Environments</h2>
        <ul className="stack">
          {environments.map((environment) => {
            const deployments = resource.data?.deployments[environment.id] ?? [];
            const active = deployments.find((item) => item.id === environment.activeDeploymentId);
            const candidate = [...deployments]
              .reverse()
              .find((item) => item.id !== environment.activeDeploymentId);
            const cutover =
              candidate !== undefined && candidate.state === "healthy"
                ? candidate
                : active !== undefined && (active.state === "healthy" || active.state === "active")
                  ? active
                  : undefined;
            return (
              <li key={environment.id}>
                <span className="mono">{environment.kind}</span>{" "}
                <Badge>{environment.visibility}</Badge>{" "}
                <Badge>{environment.state}</Badge>{" "}
                <a href={`https://${environment.hostname}`}>{environment.hostname}</a>
                {environment.visibility === "private" ? (
                  <button
                    type="button"
                    className="btn"
                    disabled={busy}
                    onClick={() =>
                      void run("Private preview handshake started.", async () => {
                        const redirect = await startPreviewLogin(application.id, environment.id);
                        window.open(redirect, "_blank", "noopener,noreferrer");
                      })
                    }
                  >
                    Open private preview
                  </button>
                ) : null}
                {active !== undefined ? (
                  <p className="muted mono">
                    active {active.id.slice(0, 8)} · {active.state} · release{" "}
                    {active.releaseId.slice(0, 8)}
                  </p>
                ) : (
                  <p className="muted">No active Deployment.</p>
                )}
                {candidate !== undefined ? (
                  <p className="muted mono">
                    candidate {candidate.id.slice(0, 8)} · {candidate.state} · release{" "}
                    {candidate.releaseId.slice(0, 8)}
                  </p>
                ) : null}
                <div className="actions">
                  {environment.kind === "dev" && readyRelease !== null && dev !== null ? (
                    <button
                      type="button"
                      className="btn"
                      disabled={busy}
                      onClick={() =>
                        void run("Development deploy accepted.", async () => {
                          await deployRelease(dev.id, readyRelease.id, newIntentId());
                        })
                      }
                    >
                      Deploy latest Release to dev
                    </button>
                  ) : null}
                  {environment.kind === "prod" && readyRelease !== null && prod !== null ? (
                    <button
                      type="button"
                      className="btn"
                      disabled={busy}
                      onClick={() =>
                        void run("Production publish accepted.", async () => {
                          await deployRelease(
                            prod.id,
                            readyRelease.id,
                            newIntentId(),
                            approvalId,
                          );
                          setAcceptedApproval(null);
                        })
                      }
                    >
                      Publish exact Release to prod
                    </button>
                  ) : null}
                  {cutover !== undefined ? (
                    <button
                      type="button"
                      className="btn"
                      disabled={busy || (cutover.state !== "healthy" && cutover.state !== "active")}
                      onClick={() =>
                        void run("Cutover requested.", async () => {
                          await activateDeployment(cutover.id);
                        })
                      }
                    >
                      Activate
                    </button>
                  ) : null}
                  {active !== undefined ? (
                    <>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy}
                        onClick={() =>
                          void run("Restart requested.", async () => {
                            await restartDeployment(active.id);
                          })
                        }
                      >
                        Restart
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy}
                        onClick={() =>
                          void run("Rollback accepted.", async () => {
                            await rollbackDeployment(active.id, newIntentId(), approvalId);
                            setAcceptedApproval(null);
                          })
                        }
                      >
                        Rollback previous Release
                      </button>
                    </>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ul>
      </div>
      <div className="card">
        <h2>Releases</h2>
        {releases.length === 0 ? (
          <p className="muted">No releases yet.</p>
        ) : (
          <ul className="stack">
            {releases.map((release) => (
              <li key={release.id} className="mono">
                {release.id.slice(0, 8)} · {release.state} · gen {release.sourceExecGeneration ?? "—"}
                {release.artifactHash !== null ? ` · ${release.artifactHash.slice(0, 12)}` : ""}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
