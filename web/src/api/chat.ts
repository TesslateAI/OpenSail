/**
 * Narrow chat adapter seam for the native VOIE console.
 *
 * The console today drives chat through the durable run resources
 * (`POST /api/sessions/:id/runs` + `GET /api/runs/:id`). When the Core chat
 * API lands, this module is the single place to swap transports: callers
 * keep the same contract and the optimistic-bubble reconciliation stays
 * anchored to the canonical session log, not to a transport detail.
 *
 * Nothing in here imports DSH/Cursor shell, runtime, or branding.
 */

import { startRun } from "./api.ts";
import type { RunState, Uuid } from "./dto.ts";
import { newIntentId } from "./http.ts";
import { asNum, asStr, isRecord } from "./validate.ts";

export type ChatIntent = {
  /** Client-minted durable run identity. */
  runId: Uuid;
  /** Client-minted single-attempt caller identity. */
  intentId: Uuid;
};

export type ChatSubmitResult = ChatIntent & {
  accepted: boolean;
  state: RunState;
  reason: string | null;
};

/** Mints the two identities used by one explicit chat submission. */
export function newChatIntent(): ChatIntent {
  return { runId: newIntentId(), intentId: newIntentId() };
}

/**
 * Submits one chat prompt to the session. Today that is one durable run
 * attempt; the answer is an acceptance receipt and the caller polls
 * `/api/runs/:id` for timestamps and result.
 *
 * The third argument accepts either the old optional AbortSignal shape or a
 * pre-minted ChatIntent. Keeping both forms lets existing resource callers
 * remain stable while the native composer can render its identity before
 * the request resolves.
 */
function isAbortSignal(value: ChatIntent | AbortSignal): value is AbortSignal {
  return typeof AbortSignal !== "undefined" && value instanceof AbortSignal;
}
export function submitChatPrompt(
  sessionId: Uuid,
  prompt: string,
  signal?: AbortSignal,
): Promise<ChatSubmitResult>;

export function submitChatPrompt(
  sessionId: Uuid,
  prompt: string,
  intent: ChatIntent,
  signal?: AbortSignal,
): Promise<ChatSubmitResult>;
export async function submitChatPrompt(
  sessionId: Uuid,
  prompt: string,
  intentOrSignal?: ChatIntent | AbortSignal,
  signal?: AbortSignal,
): Promise<ChatSubmitResult> {
  let signalArgument = signal;
  let intent: ChatIntent;
  if (intentOrSignal === undefined) {
    intent = newChatIntent();
  } else if (isAbortSignal(intentOrSignal)) {
    signalArgument = intentOrSignal;
    intent = newChatIntent();
  } else {
    intent = intentOrSignal;
  }
  const receipt = await startRun(sessionId, { ...intent, prompt }, signalArgument);
  return {
    accepted: receipt.accepted,
    runId: receipt.runId,
    intentId: receipt.intentId,
    state: receipt.state,
    reason: receipt.reason,
  };
}

const FIRST_PROMPT_PREFIX = "voie:firstPrompt:";

function firstPromptKey(sessionId: Uuid): string {
  return `${FIRST_PROMPT_PREFIX}${sessionId}`;
}

/**
 * Hands the hero composer's first message to the session route: the hero
 * creates the session, stashes the prompt, and navigates; the session route
 * submits it exactly once. Storage failures degrade to a plain session open.
 */
export function storeFirstPrompt(sessionId: Uuid, prompt: string): void {
  try {
    window.localStorage.setItem(firstPromptKey(sessionId), prompt);
  } catch {
    // Storage may be unavailable; the session still opens.
  }
}

/**
 * Takes (and removes) the stashed first prompt for one session. The removal
 * is atomic with the read, so a refresh can never resubmit it.
 */
export function takeFirstPrompt(sessionId: Uuid): string | null {
  try {
    const value = window.localStorage.getItem(firstPromptKey(sessionId));
    if (value === null) return null;
    window.localStorage.removeItem(firstPromptKey(sessionId));
    return value;
  } catch {
    return null;
  }
}

/**
 * One accepted prompt kept only for optimistic rendering continuity. It is
 * never replayed: the run listing decides whether the identity is still
 * active, while the canonical event stream decides whether to reconcile it.
 */
export type PendingChatPrompt = ChatIntent & {
  prompt: string;
  /** Cursor observed before the run was submitted. */
  afterSeq: number;
};

const PENDING_PROMPT_PREFIX = "voie:pending-prompt:";

function pendingPromptKey(sessionId: Uuid): string {
  return `${PENDING_PROMPT_PREFIX}${sessionId}`;
}

/** Reads one validated optimistic prompt from browser storage. */
export function readPendingChatPrompt(sessionId: Uuid): PendingChatPrompt | null {
  try {
    const raw = window.localStorage.getItem(pendingPromptKey(sessionId));
    if (raw === null) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return null;
    const runId = asStr(parsed.runId);
    const intentId = asStr(parsed.intentId);
    const prompt = asStr(parsed.prompt);
    const afterSeq = asNum(parsed.afterSeq);
    if (runId === null || intentId === null || prompt === null || afterSeq === null) return null;
    return { runId, intentId, prompt, afterSeq };
  } catch {
    return null;
  }
}

/** Persists one optimistic prompt for refresh-safe rendering only. */
export function writePendingChatPrompt(
  sessionId: Uuid,
  pending: PendingChatPrompt,
): void {
  try {
    window.localStorage.setItem(pendingPromptKey(sessionId), JSON.stringify(pending));
  } catch {
    // Storage may be unavailable; in-memory rendering still works.
  }
}

/** Removes the optimistic prompt after canonical reconciliation or settlement. */
export function clearPendingChatPrompt(sessionId: Uuid): void {
  try {
    window.localStorage.removeItem(pendingPromptKey(sessionId));
  } catch {
    // Nothing to clean up when storage is unavailable.
  }
}
