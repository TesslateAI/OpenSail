/**
 * Public scope-management components: Personal/Team switching, workspace
 * listing/creation/sharing, member administration, and agent presets.
 * Components stay standalone so the shell can mount them without changes to
 * this lane's ownership boundaries.
 */

export { AgentPresets, type AgentPresetsProps } from "./AgentPresets.tsx";
export { ScopeMembers, type ScopeMembersProps } from "./ScopeMembers.tsx";
export { ScopeSwitcher, type ScopeSwitcherProps } from "./ScopeSwitcher.tsx";
export { ScopeWorkspaces, type ScopeWorkspacesProps } from "./ScopeWorkspaces.tsx";
export {
  createScopeSwitcherModel,
  creatorLabel,
  memberLabel,
  scopeOptionLabel,
  shortId,
  SCOPE_KIND_LABELS,
  SCOPE_ROLE_LABELS,
  SCOPE_ROLE_TONES,
  type ScopeOption,
  type ScopeSwitcherModel,
} from "./model.ts";
