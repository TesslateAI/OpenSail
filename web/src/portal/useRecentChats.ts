/**
 * Recent-chats source for the portal shell.
 *
 * One source of truth: the canonical conversations ledger
 * `GET /api/sessions`, membership-scoped by the control plane. Scope
 * isolation is purely a client-side concern: each row keeps its owning
 * `projectId` and this loader keeps only the active scope's rows. There is
 * no scope-workspace fan-out fallback; a failed read surfaces on the hook's
 * `error` field with a `retry` handle instead of collapsing into an empty
 * list, and the bounded poll keeps the sidebar and the root last-chat
 * affordance fresh while a first message lazily starts a conversation
 * through the mounted surface.
 */

import { useCallback, useMemo } from "react";
import { useResource, useBoundedPoll } from "../hooks.ts";
import { fetchJson } from "../api/http.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "../api/validate.ts";
import { compareChatsDesc, type RecentChat } from "./chat-context.ts";

const POLL_INTERVAL_MS = 4000;

/**
 * Decodes one ledger row (the server sessions projection) into the sidebar
 * shape: ids come back verbatim, a missing server title degrades to the
 * short-id label, and absent workspace or agent bindings stay empty strings.
 */
function normalizeLedgerRow(raw: unknown): RecentChat | null {
  const record = isRecord(raw) ? raw : {};
  const id = asStr(record.id) ?? asStr(record.conversationId);
  if (id === null || id === "") return null;
  return {
    id,
    workspaceId: asStr(record.workspaceId) ?? "",
    agentId: asStr(record.agentId) ?? "",
    projectId: asStr(record.projectId),
    title: asStr(record.title),
    running: asBoolOr(record.running, false),
    headRevision: asNum(record.headRevision) ?? 0,
    createdAt: asStr(record.createdAt),
  };
}

/**
 * Loads the active scope's slice of the conversations ledger. The endpoint
 * is the real server projection; nothing here synthesizes rows when a read
 * fails — the rejection propagates to the caller.
 */
async function loadScopeChats(
  scopeId: string,
  signal: AbortSignal,
): Promise<RecentChat[]> {
  const raw = await fetchJson("/api/sessions", { signal });
  const items = arrayAt(isRecord(raw) ? raw : {}, "items");
  const chats: RecentChat[] = [];
  for (const item of items) {
    const chat = normalizeLedgerRow(item);
    // Client-side UX isolation only: the ledger spans every scope the user
    // belongs to; the sidebar shows just the selected scope's rows.
    if (chat !== null && chat.projectId === scopeId) chats.push(chat);
  }
  return chats.sort(compareChatsDesc);
}

/** Shape exposed to the shell: data plus honest failure reporting. */
export type RecentChatsState = {
  readonly chats: readonly RecentChat[];
  readonly loading: boolean;
  /** Latest ledger failure; null again once a later attempt succeeds. */
  readonly error: Error | null;
  /** Manual retry for a surfaced failure; the poll also auto-retries. */
  readonly retry: () => void;
};

/**
 * Polls the recent-conversation ledger for one scope. Mount only once per
 * shell (the sidebar owner) so the two consumers share a single request loop.
 */
export function useRecentChats(scopeId: string | null): RecentChatsState {
  // useResource keys on scopeId: switching scopes swaps the ledger wholesale.
  const load = useCallback(
    async (signal: AbortSignal): Promise<readonly RecentChat[]> =>
      scopeId === null ? [] : loadScopeChats(scopeId, signal),
    [scopeId],
  );
  const resource = useResource<readonly RecentChat[]>(load, [scopeId]);
  const data = resource.data ?? [];

  const tick = useCallback(
    async (_signal: AbortSignal): Promise<void> => {
      resource.reload();
    },
    [resource.reload],
  );
  useBoundedPoll(tick, POLL_INTERVAL_MS, scopeId !== null);

  return useMemo(
    () => ({
      chats: data,
      loading: resource.loading,
      error: scopeId === null ? null : resource.error,
      retry: resource.reload,
    }),
    [data, resource.loading, resource.error, resource.reload, scopeId],
  );
}

/**
 * Resolves the workspace display context for a bound conversation without a
 * second round trip: ids come straight from the ledger rows.
 */
export function workspaceIdOfChat(
  chats: readonly RecentChat[],
  conversationId: string,
): string | undefined {
  return chats.find((chat) => chat.id === conversationId)?.workspaceId;
}
