/**
 * Project switcher data model: the shared vocabulary and label resolution for
 * Personal and Team collaboration surfaces. The control plane persists them
 * as projects (`projects.kind` = personal | team).
 *
 * Label helpers are the single source of the display vocabulary so every
 * surface renders "Personal"/"Team", role names, and creator attribution
 * identically.
 */

import {
  type ProjectKind,
  type ProjectMemberDto,
  type ProjectSummaryDto,
  type Role,
  type Uuid,
} from "../api/dto.ts";

/** Display names per project kind; the only place this mapping lives. */
export const PROJECT_KIND_LABELS: Record<ProjectKind, string> = {
  personal: "Personal",
  team: "Team",
};

/** Display names per membership role; the only place this mapping lives. */
export const PROJECT_ROLE_LABELS: Record<Role, string> = {
  owner: "Owner",
  admin: "Admin",
  member: "Member",
  viewer: "Viewer",
};

/** Badge tone per membership role; mirrors the project roster mapping. */
export const PROJECT_ROLE_TONES: Record<Role, "accent" | "warn" | "ok" | "neutral"> = {
  owner: "accent",
  admin: "warn",
  member: "ok",
  viewer: "neutral",
};

/** One option in the switcher data model, fully resolved for rendering. */
export type ProjectOption = {
  project: ProjectSummaryDto;
  label: string;
};

/** Complete switcher view model: grouped options and the selected option. */
export type ProjectSwitcherModel = {
  options: ProjectOption[];
  personal: ProjectOption[];
  team: ProjectOption[];
  selected: ProjectOption | null;
};

/**
 * Builds the switcher model from one project listing. Server order is preserved
 * so the caller can keep backend-defined recency or alphabetical ordering.
 */
export function createProjectSwitcherModel(
  projects: readonly ProjectSummaryDto[],
  selectedId: Uuid | null,
): ProjectSwitcherModel {
  const options = projects.map((project) => ({
    project,
    label: projectOptionLabel(project),
  }));
  const personal = options.filter((option) => option.project.kind === "personal");
  const team = options.filter((option) => option.project.kind === "team");
  return {
    options,
    personal,
    team,
    selected: options.find((option) => option.project.id === selectedId) ?? null,
  };
}

/** Compact id for cramped cells; identical to the console's shortId rule. */
export function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

/** Locale timestamp for workspace created-at cells. */
export function formatDate(value: string | null): string {
  if (value === null || value.trim() === "") return "—";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? value
    : parsed.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** Human label for one member: display name, else username, else subject. */
export function memberLabel(member: ProjectMemberDto): string {
  const display = member.displayName?.trim() ?? "";
  if (display !== "") return display;
  const username = member.username?.trim() ?? "";
  if (username !== "") return username;
  const subject = member.subject?.trim() ?? "";
  if (subject !== "") return subject;
  return shortId(member.userId);
}

/**
 * Creator attribution for one workspace: "You" for the acting user, else the
 * resolved member label, else the compact user id.
 */
export function creatorLabel(
  createdByUserId: Uuid | null,
  meUserId: Uuid | null,
  members: readonly ProjectMemberDto[],
): string {
  if (createdByUserId === null || createdByUserId.trim() === "") return "—";
  if (meUserId !== null && createdByUserId === meUserId) return "You";
  const member = members.find((entry) => entry.userId === createdByUserId);
  return member !== undefined ? memberLabel(member) : shortId(createdByUserId);
}

/** Switcher option label: name plus the kind suffix for disambiguation. */
export function projectOptionLabel(project: ProjectSummaryDto): string {
  const name = project.name.trim();
  return name === "" ? shortId(project.id) : `${name} · ${PROJECT_KIND_LABELS[project.kind]}`;
}
