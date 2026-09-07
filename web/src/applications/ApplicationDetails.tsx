/** One Application: Environments, Releases, and health-gated publication. */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  acceptApproval,
  activateDeployment,
  deployRelease,
  getApplication,
  listApprovals,
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
import type {
  ApplicationDto,
  ApprovalDto,
  DeploymentDto,
  EnvironmentDto,
  ReleaseDto,
  Uuid,
} from "../api/dto.ts";
import { ApiError, newIntentId } from "../api/http.ts";
import { useBoundedPoll, useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

export type ApplicationDetailsProps = {
  applicationId: Uuid;
};

type ResumeAction = "publish_prod" | "rollback" | "delete" | "none";

type PendingGate = {
  id: Uuid;
  kind: string;
  resume: ResumeAction;
  releaseId: Uuid | null;
  deploymentId: Uuid | null;
};

type Detail = {
  application: ApplicationDto;
  environments: EnvironmentDto[];
  releases: ReleaseDto[];
  deployments: Record<string, DeploymentDto[]>;
  approvals: ApprovalDto[];
};

export function ApplicationDetails({ applicationId }: ApplicationDetailsProps) {
  const load = useCallback(
    async (signal: AbortSignal): Promise<Detail> => {
      const [application, environments, releases, approvals] = await Promise.all([
        getApplication(applicationId, signal),
        listEnvironments(applicationId, signal),
        listReleases(applicationId, signal),
        listApprovals(applicationId, signal),
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
        approvals,
      };
    },
    [applicationId],
  );
  const resource = useResource(load);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingApproval, setPendingApproval] = useState<PendingGate | null>(null);
  const [acceptedApproval, setAcceptedApproval] = useState<PendingGate | null>(null);
  const [deleted, setDeleted] = useState(false);

  useEffect(() => {
    const detail = resource.data;
    if (detail === null || pendingApproval !== null) return;
    const pending = detail.approvals.find((item) => item.state === "pending");
    if (pending === undefined) return;
    setPendingApproval(gateFromApproval(pending));
  }, [resource.data, pendingApproval]);

  const inflight = useMemo(() => {
    const detail = resource.data;
    if (detail === null) return false;
    return (
      pendingApproval !== null ||
      busy ||
      detail.releases.some((release) => release.state === "dispatched") ||
      Object.values(detail.deployments).some((items) =>
        items.some((item) => item.state === "creating"),
      )
    );
  }, [resource.data, pendingApproval, busy]);
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

  const continueGate = async (gate: PendingGate): Promise<void> => {
    if (gate.resume === "publish_prod" && prod !== null) {
      const releaseId = gate.releaseId ?? readyRelease?.id;
      if (releaseId === undefined || releaseId === null) {
        throw new Error("no Release is ready to publish");
      }
      await deployRelease(prod.id, releaseId, newIntentId(), gate.id);
      return;
    }
    if (gate.resume === "rollback" && gate.deploymentId !== null) {
      await rollbackDeployment(gate.deploymentId, newIntentId(), gate.id);
      return;
    }
    if (gate.resume === "delete") {
      await deleteApplication(application.id, gate.id);
      setDeleted(true);
    }
  };

  const run = async (
    label: string,
    work: () => Promise<void>,
    options: { reload?: boolean; resume?: ResumeAction; releaseId?: Uuid; deploymentId?: Uuid } = {},
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
        setPendingApproval({
          id: reason.approvalId,
          kind: options.resume === "delete" ? "delete_application" : "publish_production",
          resume: options.resume ?? "none",
          releaseId: options.releaseId ?? null,
          deploymentId: options.deploymentId ?? null,
        });
        setError("Approve the request, then the original action continues immediately.");
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
                    { reload: false, resume: "delete" },
                  )
                }
              >
                Delete
              </button>
            ) : null}
          </>
        }
      />
      {error !== null ? (
        <p className="muted" role="alert">
          {error}
        </p>
      ) : null}
      {notice !== null ? <p className="muted">{notice}</p> : null}
      {busy ? <p className="muted">Working…</p> : null}
      <p className="muted">
        Suspend keeps local volumes. Archive pins Blob restore points and releases Fabric
        capacity. Restore allocates candidate LVs and switches after proof. Delete does
        not create a final backup.
      </p>
      {pendingApproval !== null ? (
        <Card title="Approval required">
          <p>
            Approval {pendingApproval.id.slice(0, 8)} is waiting. Approving continues the
            original action; it does not stop at the approval itself.
          </p>
          <div className="actions">
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() =>
                void run(
                  continueLabel(pendingApproval),
                  async () => {
                    const gate = pendingApproval;
                    await acceptApproval(gate.id);
                    setAcceptedApproval(gate);
                    setPendingApproval(null);
                    await continueGate(gate);
                  },
                  { reload: pendingApproval.resume !== "delete" },
                )
              }
            >
              Approve and continue
            </button>
          </div>
        </Card>
      ) : null}
      <Card title="Environments">
        <ul className="resource-list">
          {environments.map((environment) => {
            const deployments = resource.data?.deployments[environment.id] ?? [];
            const active = deployments.find((item) => item.id === environment.activeDeploymentId);
            const candidate = [...deployments]
              .reverse()
              .find((item) => item.id !== environment.activeDeploymentId);
            const inFlight = deployments.find((item) => item.state === "creating");
            const publishing = inFlight !== undefined;
            const cutover =
              candidate !== undefined && candidate.state === "healthy"
                ? candidate
                : active !== undefined && (active.state === "healthy" || active.state === "active")
                  ? active
                  : undefined;
            return (
              <li key={environment.id} className="resource-item">
                <div className="resource-item-head">
                  <span className="mono">{environment.kind}</span>
                  <Badge>{environment.visibility}</Badge>
                  <Badge>{environment.state}</Badge>
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
                </div>
                <p className="muted">{environmentProgress(environment, deployments, pendingApproval)}</p>
                {active !== undefined ? (
                  <p className="muted mono">
                    active {active.id.slice(0, 8)} · {deploymentProgress(active)} · release{" "}
                    {active.releaseId.slice(0, 8)}
                  </p>
                ) : null}
                {candidate !== undefined ? (
                  <p className="muted mono">
                    candidate {candidate.id.slice(0, 8)} · {deploymentProgress(candidate)} · release{" "}
                    {candidate.releaseId.slice(0, 8)}
                  </p>
                ) : null}
                <div className="actions">
                  {environment.kind === "dev" && readyRelease !== null && dev !== null ? (
                    <button
                      type="button"
                      className="btn"
                      disabled={busy || publishing}
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
                      disabled={busy || publishing}
                      onClick={() =>
                        void run(
                          "Production publish accepted.",
                          async () => {
                            await deployRelease(
                              prod.id,
                              readyRelease.id,
                              newIntentId(),
                              approvalId,
                            );
                            setAcceptedApproval(null);
                          },
                          { resume: "publish_prod", releaseId: readyRelease.id },
                        )
                      }
                    >
                      Publish exact Release to prod
                    </button>
                  ) : null}
                  {cutover !== undefined ? (
                    <button
                      type="button"
                      className="btn"
                      disabled={busy || publishing || (cutover.state !== "healthy" && cutover.state !== "active")}
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
                        disabled={busy || publishing}
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
                        disabled={busy || publishing}
                        onClick={() =>
                          void run(
                            "Rollback accepted.",
                            async () => {
                              await rollbackDeployment(active.id, newIntentId(), approvalId);
                              setAcceptedApproval(null);
                            },
                            { resume: "rollback", deploymentId: active.id },
                          )
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
      </Card>
      <Card title="Releases">
        {releases.length === 0 ? (
          <p className="muted">No releases yet.</p>
        ) : (
          <ul className="resource-list">
            {releases.map((release) => (
              <li key={release.id} className="resource-item mono">
                {release.id.slice(0, 8)} · {release.state} · gen {release.sourceExecGeneration ?? "—"}
                {release.artifactHash !== null ? ` · ${release.artifactHash.slice(0, 12)}` : ""}
              </li>
            ))}
          </ul>
        )}
      </Card>
    </section>
  );
}

function gateFromApproval(approval: ApprovalDto): PendingGate {
  return {
    id: approval.id,
    kind: approval.kind,
    resume: resumeFromKind(approval.kind),
    releaseId: approval.releaseId,
    deploymentId: null,
  };
}

function resumeFromKind(kind: string): ResumeAction {
  if (kind === "delete_application") return "delete";
  if (kind === "publish_production") return "publish_prod";
  return "none";
}

function continueLabel(gate: PendingGate): string {
  switch (gate.resume) {
    case "delete":
      return "Application deleted.";
    case "rollback":
      return "Rollback accepted.";
    case "publish_prod":
      return "Production publish started.";
    case "none":
      return "Approval accepted.";
  }
}

function deploymentProgress(item: DeploymentDto): string {
  if (item.lastErrorCode !== null) {
    return `failed (${item.lastErrorCode})`;
  }
  if (item.state === "active") return "live";
  if (item.state === "healthy") return "healthy — Activate to switch traffic";
  if (item.state === "creating") {
    const observed = item.observedState ?? "starting";
    return `publishing (${observed})`;
  }
  if (item.state === "stopped") return "stopped";
  return item.state;
}

function environmentProgress(
  environment: EnvironmentDto,
  deployments: DeploymentDto[],
  pending: PendingGate | null,
): string {
  const failed = deployments.find((item) => item.lastErrorCode !== null);
  if (failed !== undefined) {
    return `Last error: ${failed.lastErrorCode}`;
  }
  const creating = deployments.find((item) => item.state === "creating");
  if (creating !== undefined) {
    return `Deployment in progress: ${deploymentProgress(creating)}`;
  }
  if (pending !== null && environment.kind === "prod" && pending.resume === "publish_prod") {
    return "Waiting for approval. Approving starts the production publish.";
  }
  const healthy = deployments.find((item) => item.state === "healthy");
  if (healthy !== undefined && environment.activeDeploymentId !== healthy.id) {
    return "Healthy candidate is ready. Activate to switch traffic.";
  }
  if (environment.activeDeploymentId === null) {
    return "No active Deployment.";
  }
  return "Ready.";
}
