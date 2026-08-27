/**
 * Scope switcher data model: the shared vocabulary and label resolution for
 * every scope component. Scopes are the product-level collaboration units
 * (`personal` single-user home, `team` multi-user surface); the control plane
 * persists them as projects, but components here only ever see scope shapes.
 *
 * Label helpers are the single source of the display vocabulary so every
 * surface renders "Personal"/"Team", role names, and creator attribution
 * identically.
 */

import type { ScopeKind, ScopeMemberDto, ScopeRole, ScopeSummaryDto, Uuid } from "../api/dto.ts";

/** Display names per scope kind; the only place this mapping lives. */
export const SCOPE_KIND_LABELS: Record<ScopeKind, string> = {
  personal: "Personal",
  team: "Team",
};

/** Display names per membership role; the only place this mapping lives. */
export const SCOPE_ROLE_LABELS: Record<ScopeRole, string> = {
  owner: "Owner",
  admin: "Admin",
  member: "Member",
  viewer: "Viewer",
};

/** Badge tone per membership role; mirrors the project roster mapping. */
export const SCOPE_ROLE_TONES: Record<ScopeRole, "accent" | "warn" | "ok" | "neutral"> = {
  owner: "accent",
  admin: "warn",
  member: "ok",
  viewer: "neutral",
};

/** One option in the switcher data model, fully resolved for rendering. */
export type ScopeOption = {
  scope: ScopeSummaryDto;
  label: string;
};

/** Complete switcher view model: grouped options and the selected option. */
export type ScopeSwitcherModel = {
  options: ScopeOption[];
  personal: ScopeOption[];
  team: ScopeOption[];
  selected: ScopeOption | null;
};

/**
 * Builds the switcher model from one scope listing. Server order is preserved
 * so the caller can keep backend-defined recency or alphabetical ordering.
 */
export function createScopeSwitcherModel(
  scopes: readonly ScopeSummaryDto[],
  selectedId: Uuid | null,
): ScopeSwitcherModel {
  const options = scopes.map((scope) => ({
    scope,
    label: scopeOptionLabel(scope),
  }));
  const personal = options.filter((option) => option.scope.kind === "personal");
  const team = options.filter((option) => option.scope.kind === "team");
  return {
    options,
    personal,
    team,
    selected: options.find((option) => option.scope.id === selectedId) ?? null,
  };
}

/** Compact id for cramped cells; identical to the console's shortId rule. */
export function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

/** Human label for one member: display name, else username, else subject. */
export function memberLabel(member: ScopeMemberDto): string {
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
  members: readonly ScopeMemberDto[],
): string {
  if (createdByUserId === null || createdByUserId.trim() === "") return "—";
  if (meUserId !== null && createdByUserId === meUserId) return "You";
  const member = members.find((entry) => entry.userId === createdByUserId);
  return member !== undefined ? memberLabel(member) : shortId(createdByUserId);
}

/** Switcher option label: name plus the kind suffix for disambiguation. */
export function scopeOptionLabel(scope: ScopeSummaryDto): string {
  const name = scope.name.trim();
  return name === "" ? shortId(scope.id) : `${name} · ${SCOPE_KIND_LABELS[scope.kind]}`;
}
