/**
 * Project collaboration components: Personal/Team switching, workspace
 * listing/creation/sharing, member administration, and agent presets.
 * Components stay standalone so the shell can mount them without changes to
 * this lane's ownership boundaries.
 */

export { AgentPresets, type AgentPresetsProps } from "./AgentPresets.tsx";
export { ProjectMembers, type ProjectMembersProps } from "./ProjectMembers.tsx";
export { ProjectSwitcher, type ProjectSwitcherProps } from "./ProjectSwitcher.tsx";
export { ProjectWorkspaces, type ProjectWorkspacesProps } from "./ProjectWorkspaces.tsx";
export {
  createProjectSwitcherModel,
  creatorLabel,
  memberLabel,
  projectOptionLabel,
  shortId,
  PROJECT_KIND_LABELS,
  PROJECT_ROLE_LABELS,
  PROJECT_ROLE_TONES,
  type ProjectOption,
  type ProjectSwitcherModel,
} from "./model.ts";
