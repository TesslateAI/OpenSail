/**
 * Project collaboration components: Personal/Team switching, workspace
 * listing/creation, and agent presets.
 */

export { AgentPresets, type AgentPresetsProps } from "./AgentPresets.tsx";
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
