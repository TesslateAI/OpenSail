/**
 * VOIE-native typed presentation models, mirroring the interaction logic of
 * the reference mock's review/notification/composer surfaces.
 *
 * These are STRUCTURAL and VALUE-FREE: no mock data, no credentials, no
 * runtime imports. Consumer code (management shell, ChatHost) feeds plain
 * objects in and gets typed, kds-styled affordances out.
 */
import type { ReactNode } from "react";
import type { Tone } from "../../design-system/variants";

/* ------------------------------------------------------------------ */
/* Notifications                                                       */
/* Source: mock review-ui.js § NotificationBell + NOTIF_KINDS shape.   */
/* ------------------------------------------------------------------ */

/** Notification kinds the VOIE portal surfaces; tone + icon are derived. */
export type NotificationKind =
  | "approval_requested"
  | "review_commented"
  | "review_resolved"
  | "state_changed"
  | "run_finished"
  | "run_failed"
  | "access_changed"
  | "system";

export interface NotificationModel {
  id: string;
  kind: NotificationKind;
  title: string;
  body: string;
  /** Display timestamp label as the source data carries it ("2h ago"). */
  at: string;
  read: boolean;
  /** When set, the row offers a "Open the app" deep-link affordance. */
  targetId?: string;
}

/** Day bucketing for the drawer queue, as a pure function (mock `notifGroup`). */
export function notificationGroupLabel(at: string): NotificationGroup {
  if (/minute|hour|now/.test(at)) return "Today";
  if (/yesterday|^1 day/.test(at)) return "Yesterday";
  return "Earlier";
}

export type NotificationGroup = "Today" | "Yesterday" | "Earlier";

export const NOTIFICATION_GROUP_ORDER: readonly NotificationGroup[] = [
  "Today",
  "Yesterday",
  "Earlier",
];

export interface NotificationGrouped {
  group: NotificationGroup;
  items: NotificationModel[];
}

export function groupNotifications(items: ReadonlyArray<NotificationModel>): NotificationGrouped[] {
  return NOTIFICATION_GROUP_ORDER
    .map((group) => ({ group, items: items.filter((n) => notificationGroupLabel(n.at) === group) }))
    .filter((g) => g.items.length > 0);
}

export function unreadCount(items: ReadonlyArray<NotificationModel>): number {
  return items.filter((n) => !n.read).length;
}

/** Kind -> tone for the circular icon (mock NOTIF_KINDS tone mapping). */
export function toneForNotificationKind(kind: NotificationKind): Tone {
  switch (kind) {
    case "approval_requested": return "info";
    case "review_commented": return "info";
    case "review_resolved": return "ok";
    case "state_changed": return "neutral";
    case "run_finished": return "ok";
    case "run_failed": return "fail";
    case "access_changed": return "warn";
    case "system": return "neutral";
  }
}

/* ------------------------------------------------------------------ */
/* Review loop                                                         */
/* Source: mock review-ui.js § ReviewPanel + REVIEW_STATES shape.      */
/* ------------------------------------------------------------------ */

export type ReviewState = "none" | "in_review" | "changes_requested" | "approved";

export interface ReviewCommentModel {
  id: string;
  authorName: string;
  /** Short uppercase tag, e.g. "reviewer" / "builder". */
  role?: string;
  text: string;
  at: string;
  /** Replies are drawn as the other side of the conversation. */
  reply?: boolean;
  resolved?: boolean;
}

export interface ReviewSummaryModel {
  state: ReviewState;
  reviewerName?: string;
  submittedAtLabel?: string;
  comments: ReviewCommentModel[];
}

/** Tone used for the review state chip/badge. */
export function toneForReviewState(state: ReviewState): Tone {
  switch (state) {
    case "approved": return "ok";
    case "changes_requested": return "warn";
    case "in_review": return "info";
    case "none": return "neutral";
  }
}

/* ------------------------------------------------------------------ */
/* Composer controls (vocabulary, not data)                            */
/* Source: mock composer-tools.js § PERMISSION_MODES / EFFORT_LEVELS.  */
/* ------------------------------------------------------------------ */

export type PermissionMode = "ask" | "auto" | "off" | "full";

export interface PermissionModeSpec {
  id: PermissionMode;
  label: string;
  blurb: string;
  isDefault?: boolean;
  /** Renders loud amber, deliberately. */
  loud?: boolean;
}

export const PERMISSION_MODES: Record<PermissionMode, PermissionModeSpec> = {
  ask: {
    id: "ask",
    label: "Ask every time",
    blurb: "The agent checks with you before it changes a file or runs a command.",
  },
  auto: {
    id: "auto",
    label: "Auto",
    blurb: "Routine edits and commands run without asking. The default.",
    isDefault: true,
  },
  off: {
    id: "off",
    label: "Read only",
    blurb: "The agent can look at your app but cannot change or run anything.",
  },
  full: {
    id: "full",
    label: "Full access",
    blurb: "Nothing is checked with you first. Use only when you trust the task.",
    loud: true,
  },
};

export const PERMISSION_ORDER: readonly PermissionMode[] = ["ask", "auto", "off", "full"];

export type EffortLevel = "off" | "low" | "medium" | "high";

export interface EffortLevelSpec {
  id: EffortLevel;
  label: string;
  blurb: string;
}

export const EFFORT_LEVELS: readonly EffortLevelSpec[] = [
  { id: "off", label: "Off", blurb: "Answer directly, no extended reasoning." },
  { id: "low", label: "Low", blurb: "A little thinking. Fastest useful setting." },
  { id: "medium", label: "Medium", blurb: "Balanced. Good for most build steps." },
  { id: "high", label: "High", blurb: "Think hard. Slower and costs more tokens." },
];

export interface MCPServerModel {
  id: string;
  name: string;
  blurb: string;
  enabled: boolean;
}

/* ------------------------------------------------------------------ */
/* Chat: tool cards, plan steps, fan-out workers                       */
/* Source: mock surfaces.css § .tool-card/.file-chip; chat.css § .fan* */
/* ------------------------------------------------------------------ */

export type ToolRunStatus = "ok" | "fail" | "run";

export interface ToolCallModel {
  name: string;
  command?: string;
  output?: string;
  status: ToolRunStatus;
}

export interface FileChipModel {
  id: string;
  name: string;
}

export interface PlanStepModel {
  id: string;
  title: string;
  /** When set, renders the step as done with the current step's number. */
  done?: boolean;
}

export interface FanWorkerModel {
  id: string;
  name: string;
  role?: string;
  model?: string;
  task: string;
  /** Render a pending/result area when provided. */
  result?: string;
  pending?: boolean;
  tokens?: string;
}

/* ------------------------------------------------------------------ */
/* VOIE lifecycle vocabulary (management rail)                         */
/* Source: mock surfaces.css § .stages + ui.js § StageRail structure.  */
/* The stage IDs/labels are VOIE-native and deliberately NOT the mock's */
/* project data.                                                       */
/* ------------------------------------------------------------------ */

export type VoieStageId = "intake" | "collection" | "review" | "reporting" | "delivered";

export interface VoieStageSpec {
  id: VoieStageId;
  label: string;
  hint: string;
}

export const VOIE_STAGES: readonly VoieStageSpec[] = [
  { id: "intake", label: "Intake", hint: "Engagement opened; borrower data being requested." },
  { id: "collection", label: "Collection", hint: "Documents and bank data are being gathered." },
  { id: "review", label: "Review", hint: "Collected data is being checked for completeness." },
  { id: "reporting", label: "Reporting", hint: "Findings are being written up." },
  { id: "delivered", label: "Delivered", hint: "Final output released to the engagement team." },
];

export function stageIndexById(id: VoieStageId): number {
  return VOIE_STAGES.findIndex((s) => s.id === id);
}

/* ------------------------------------------------------------------ */
/* Generic entity card model for fleet/template/drive grids            */
/* Source: mock surfaces.css § .person-card/.tpl-card/.drive-card      */
/* ------------------------------------------------------------------ */

export interface EntityCardModel {
  id: string;
  name: string;
  sub?: string;
  badge?: ReactNode;
  /** Arbitrary preview rows rendered beneath the identity block. */
  details?: ReactNode;
}
