/**
 * Deep-link scope for `/chat/:id`. The carrier boots against one project,
 * so a conversation in another scope looks like a blank New chat unless
 * the URL names that project before DSH mounts.
 */

export type ConversationScopeDecision =
  | { kind: "wait" }
  | { kind: "lookup" }
  | { kind: "stay" }
  | { kind: "reload"; href: string }
  | { kind: "missing" };

export type ConversationScopeInput = {
  conversationId: string;
  currentProjectId: string;
  /** True once the active-scope ledger finished its first load. */
  chatsLoaded: boolean;
  /** True when the conversation is already in the active-scope ledger. */
  listed: boolean;
  /**
   * Owning project from `GET /api/sessions/:id`. `null` means the
   * membership-scoped read refused or the row is gone. Omit (`undefined`)
   * until that read has settled.
   */
  sessionProjectId?: string | null;
};

/** Chat URL that carries the conversation's owning project. */
export function conversationHref(conversationId: string, projectId: string): string {
  return `/chat/${encodeURIComponent(conversationId)}?project=${encodeURIComponent(projectId)}`;
}

/**
 * Decides whether a deep-linked conversation can open in the current
 * scope, needs a session lookup, needs a full reload into its owning
 * project, or does not exist.
 */
export function conversationScopeDecision(
  input: ConversationScopeInput,
): ConversationScopeDecision {
  if (input.listed) return { kind: "stay" };
  if (!input.chatsLoaded) return { kind: "wait" };
  if (input.sessionProjectId === undefined) return { kind: "lookup" };
  if (input.sessionProjectId === null || input.sessionProjectId === "") {
    return { kind: "missing" };
  }
  if (input.sessionProjectId === input.currentProjectId) return { kind: "stay" };
  return {
    kind: "reload",
    href: conversationHref(input.conversationId, input.sessionProjectId),
  };
}
