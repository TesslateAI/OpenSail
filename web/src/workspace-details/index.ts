/**
 * Traditional workspace details surface.
 *
 * This module is intentionally unmounted by the console shell until its
 * host chooses a workspace. It owns only the conventional workspace
 * projection; chat/carrier and global navigation remain separate surfaces.
 */

export { WorkspaceDetails } from "./WorkspaceDetails.tsx";
export type { WorkspaceDetailsProps } from "./WorkspaceDetails.tsx";
export { AgentPresetSection } from "./AgentPresetSection.tsx";
export { CreatorSection } from "./CreatorSection.tsx";
export { StateSection } from "./StateSection.tsx";
export { ConversationsSection } from "./ConversationsSection.tsx";
export { DiagnosticsSection } from "./DiagnosticsSection.tsx";
export { FactsSection } from "./FactsSection.tsx";
export { LifecycleSection } from "./LifecycleSection.tsx";
export { SharingSection } from "./SharingSection.tsx";
