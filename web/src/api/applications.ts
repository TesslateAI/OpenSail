/**
 * Same-origin Application platform resources. Project remains the
 * authorization scope; these DTOs describe the agent-managed deployable.
 */

import type {
  ApplicationDto,
  ApprovalDto,
  DeploymentDto,
  EnvironmentDto,
  ReleaseDto,
  Uuid,
} from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asNum, asStr, isRecord } from "./validate.ts";

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

function optionalText(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

function normalizeApplication(raw: unknown): ApplicationDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, ""),
    workspaceId: textOr(record.workspaceId, ""),
    name: textOr(record.name, ""),
    slug: textOr(record.slug, ""),
    rootPath: textOr(record.rootPath, "."),
    runtimeProfile: textOr(record.runtimeProfile, "universal-v1"),
    state: textOr(record.state, "ready"),
    createdByUserId: textOr(record.createdByUserId, ""),
    createdAt: asStr(record.createdAt),
    updatedAt: asStr(record.updatedAt),
  };
}

function normalizeEnvironment(raw: unknown): EnvironmentDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    applicationId: textOr(record.applicationId, ""),
    kind: textOr(record.kind, ""),
    visibility: textOr(record.visibility, "private"),
    hostname: textOr(record.hostname, ""),
    revision: asNum(record.revision),
    activeDeploymentId: optionalText(record.activeDeploymentId),
    state: textOr(record.state, "ready"),
  };
}

function normalizeRelease(raw: unknown): ReleaseDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    applicationId: textOr(record.applicationId, ""),
    buildIntentId: textOr(record.buildIntentId, ""),
    sourceWorkspaceId: textOr(record.sourceWorkspaceId, ""),
    sourceExecGeneration: asNum(record.sourceExecGeneration),
    runtimeProfile: textOr(record.runtimeProfile, "universal-v1"),
    state: textOr(record.state, "reserved"),
    artifactBytes: asNum(record.artifactBytes),
    artifactHash: optionalText(record.artifactHash),
    testSummary: optionalText(record.testSummary),
    createdByUserId: textOr(record.createdByUserId, ""),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeDeployment(raw: unknown): DeploymentDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    environmentId: textOr(record.environmentId, ""),
    releaseId: textOr(record.releaseId, ""),
    deploymentIntentId: textOr(record.deploymentIntentId, ""),
    state: textOr(record.state, "creating"),
    desiredState: optionalText(record.desiredState),
    observedState: optionalText(record.observedState),
    lastErrorCode: optionalText(record.lastErrorCode),
    proven: record.proven === true,
    desiredRevision: asNum(record.desiredRevision),
    observedRevision: asNum(record.observedRevision),
    previousDeploymentId: optionalText(record.previousDeploymentId),
    createdByUserId: textOr(record.createdByUserId, ""),
    acceptedAt: asStr(record.acceptedAt),
    activeAt: asStr(record.activeAt),
  };
}

function normalizeApproval(raw: unknown): ApprovalDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, ""),
    applicationId: optionalText(record.applicationId),
    environmentId: optionalText(record.environmentId),
    releaseId: optionalText(record.releaseId),
    kind: textOr(record.kind, ""),
    state: textOr(record.state, "pending"),
    createdAt: asStr(record.createdAt),
  };
}

function envelopeRecord(raw: unknown, key: string): unknown {
  return isRecord(raw) ? raw[key] : raw;
}

export async function listApplications(projectId: Uuid, signal?: AbortSignal): Promise<ApplicationDto[]> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/applications`, { signal });
  return arrayAt(isRecord(raw) ? raw : {}, "items").map(normalizeApplication);
}

export async function getApplication(applicationId: Uuid, signal?: AbortSignal): Promise<ApplicationDto> {
  const raw = await fetchJson(`/api/applications/${encodeURIComponent(applicationId)}`, { signal });
  return normalizeApplication(envelopeRecord(raw, "application"));
}

export async function listEnvironments(applicationId: Uuid, signal?: AbortSignal): Promise<EnvironmentDto[]> {
  const raw = await fetchJson(`/api/applications/${encodeURIComponent(applicationId)}/environments`, { signal });
  return arrayAt(isRecord(raw) ? raw : {}, "items").map(normalizeEnvironment);
}

export async function listReleases(applicationId: Uuid, signal?: AbortSignal): Promise<ReleaseDto[]> {
  const raw = await fetchJson(`/api/applications/${encodeURIComponent(applicationId)}/releases`, { signal });
  return arrayAt(isRecord(raw) ? raw : {}, "items").map(normalizeRelease);
}

export async function listDeployments(environmentId: Uuid, signal?: AbortSignal): Promise<DeploymentDto[]> {
  const raw = await fetchJson(`/api/environments/${encodeURIComponent(environmentId)}/deployments`, { signal });
  return arrayAt(isRecord(raw) ? raw : {}, "items").map(normalizeDeployment);
}

export async function deployRelease(
  environmentId: Uuid,
  releaseId: Uuid,
  deploymentIntentId: Uuid,
  approvalId?: Uuid,
): Promise<unknown> {
  const body: Record<string, string> = {
    release_id: releaseId,
    deployment_intent_id: deploymentIntentId,
  };
  if (approvalId !== undefined) body.approval_id = approvalId;
  return fetchJson(`/api/environments/${encodeURIComponent(environmentId)}/deployments`, {
    method: "POST",
    body,
    timeoutMs: 30_000,
  });
}

export async function activateDeployment(deploymentId: Uuid): Promise<unknown> {
  return fetchJson(`/api/deployments/${encodeURIComponent(deploymentId)}/activate`, {
    method: "POST",
    body: {},
    timeoutMs: 60_000,
  });
}

export async function restartDeployment(deploymentId: Uuid): Promise<unknown> {
  return fetchJson(`/api/deployments/${encodeURIComponent(deploymentId)}/restart`, {
    method: "POST",
    body: {},
    timeoutMs: 30_000,
  });
}

export async function stopDeployment(deploymentId: Uuid): Promise<unknown> {
  return fetchJson(`/api/deployments/${encodeURIComponent(deploymentId)}/stop`, {
    method: "POST",
    body: {},
    timeoutMs: 30_000,
  });
}

export async function rollbackDeployment(deploymentId: Uuid, deploymentIntentId: Uuid, approvalId?: Uuid): Promise<unknown> {
  const body: Record<string, string> = { deployment_intent_id: deploymentIntentId };
  if (approvalId !== undefined) body.approval_id = approvalId;
  return fetchJson(`/api/deployments/${encodeURIComponent(deploymentId)}/rollback`, {
    method: "POST",
    body,
    timeoutMs: 30_000,
  });
}

export async function listApprovals(applicationId: Uuid, signal?: AbortSignal): Promise<ApprovalDto[]> {
  const raw = await fetchJson(`/api/applications/${encodeURIComponent(applicationId)}/approvals`, {
    signal,
  });
  return arrayAt(isRecord(raw) ? raw : {}, "items").map(normalizeApproval);
}

export async function acceptApproval(approvalId: Uuid): Promise<unknown> {
  return fetchJson(`/api/approvals/${encodeURIComponent(approvalId)}/accept`, {
    method: "POST",
    body: {},
  });
}

export async function suspendApplication(applicationId: Uuid): Promise<unknown> {
  return fetchJson(`/api/applications/${encodeURIComponent(applicationId)}`, {
    method: "PATCH",
    body: { state: "suspended" },
    timeoutMs: 30_000,
  });
}

/** Archive keeps Blob restore points and releases local Fabric volumes. */
export async function archiveApplication(applicationId: Uuid): Promise<unknown> {
  return fetchJson(`/api/applications/${encodeURIComponent(applicationId)}`, {
    method: "PATCH",
    body: { state: "archived" },
    timeoutMs: 180_000,
  });
}

/** Restore an archived Application onto candidate LVs from pinned Blob points. */
export async function restoreApplication(applicationId: Uuid): Promise<unknown> {
  return fetchJson(`/api/applications/${encodeURIComponent(applicationId)}`, {
    method: "PATCH",
    body: { state: "ready" },
    timeoutMs: 180_000,
  });
}

/** Delete does not create a final backup. May require delete_application approval. */
export async function deleteApplication(
  applicationId: Uuid,
  approvalId?: Uuid,
): Promise<unknown> {
  const body: Record<string, string> = {};
  if (approvalId !== undefined) body.approvalId = approvalId;
  return fetchJson(`/api/applications/${encodeURIComponent(applicationId)}`, {
    method: "DELETE",
    body,
    timeoutMs: 60_000,
  });
}

export async function startPreviewLogin(
  applicationId: Uuid,
  environmentId: Uuid,
): Promise<string> {
  const raw = await fetchJson(
    `/api/preview/login?applicationId=${encodeURIComponent(applicationId)}&environmentId=${encodeURIComponent(environmentId)}`,
  );
  const record = isRecord(raw) ? raw : {};
  const redirect = asStr(record.redirect);
  if (redirect === null || !redirect.startsWith("https://")) {
    throw new Error("preview login did not return an exact-host redirect");
  }
  return redirect;
}
