/**
 * Display vocabulary for the conventional workspace details surface.
 *
 * Product labels stay separate from infrastructure labels: ordinary users
 * see workspace state, Project, creator, and sharing facts, while raw
 * underlay identifiers are reserved for the administrator diagnostics
 * component.
 */

import type {
  WorkspaceConversationDto,
  WorkspaceDetailsDto,
  WorkspaceLifecycleState,
  WorkspaceMemberDto,
  WorkspaceProjectSharingDto,
} from "../api/workspace-details.ts";

export type WorkspaceBadgeTone = "ok" | "warn" | "neutral";

export const PROJECT_KIND_LABELS: Record<WorkspaceProjectSharingDto["kind"], string> = {
  personal: "Personal",
  team: "Team-shared",
};

export const PROJECT_ROLE_LABELS: Record<WorkspaceProjectSharingDto["role"], string> = {
  owner: "Owner",
  admin: "Admin",
  member: "Member",
  viewer: "Viewer",
};

export function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

export function formatDate(value: string | null): string {
  if (value === null || value.trim() === "") return "—";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? value
    : parsed.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function stateLabel(state: WorkspaceLifecycleState): string {
  switch (state) {
    case "creating":
      return "Preparing";
    case "ready":
      return "Ready";
    case "fenced":
      return "Temporarily unavailable";
    case "archived":
      return "Archived";
  }
}

export function stateTone(state: WorkspaceLifecycleState): WorkspaceBadgeTone {
  return state === "ready" ? "ok" : "warn";
}

export function projectKindLabel(kind: WorkspaceProjectSharingDto["kind"]): string {
  return PROJECT_KIND_LABELS[kind];
}

export function projectRoleLabel(role: WorkspaceProjectSharingDto["role"]): string {
  return PROJECT_ROLE_LABELS[role];
}

export function memberLabel(member: WorkspaceMemberDto): string {
  const displayName = member.displayName?.trim() ?? "";
  if (displayName !== "") return displayName;
  const username = member.username?.trim() ?? "";
  if (username !== "") return username;
  const subject = member.subject.trim();
  return subject === "" ? shortId(member.userId) : subject;
}

export function creatorLabel(
  workspace: WorkspaceDetailsDto,
  meUserId: string | null,
  members: readonly WorkspaceMemberDto[],
): string {
  const creatorId = workspace.createdByUserId;
  if (creatorId === null || creatorId.trim() === "") return "Unknown creator";
  if (meUserId !== null && creatorId === meUserId) return "You";
  const member = members.find((candidate) => candidate.userId === creatorId);
  return member === undefined ? shortId(creatorId) : memberLabel(member);
}

export function conversationTitle(conversation: WorkspaceConversationDto): string {
  const title = conversation.title?.trim() ?? "";
  return title === "" ? `Conversation ${shortId(conversation.id)}` : title;
}

export function workspaceTitle(workspace: WorkspaceDetailsDto): string {
  return workspace.name.trim() === "" ? `Workspace ${shortId(workspace.id)}` : workspace.name;
}

export function sharedWithLabel(project: WorkspaceProjectSharingDto): string {
  if (project.kind === "personal") return "Only you";
  const count = project.members.length;
  return `${count} ${count === 1 ? "member" : "members"} of this Project`;
}
