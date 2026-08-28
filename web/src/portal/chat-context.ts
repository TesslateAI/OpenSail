/**
 * Shared portal-chrome context: the recent-conversation ledger that both the
 * sidebar (reader) and the chat surface (new-conversation discovery) consume
 * from one polling source.
 */

import { createContext, useContext } from "react";
import type { WorkspaceConversationDto } from "../api/workspace-details.ts";

/** One recent chat row projected for the product sidebar. */
export type RecentChat = WorkspaceConversationDto;

export type PortalChatContextValue = {
  /** Product conversations of the active scope, newest first. */
  readonly chats: readonly RecentChat[];
  /** True until the first successful ledger load for the active scope. */
  readonly loading: boolean;
  /** Set when the ledger load failed; retry re-runs the load. */
  readonly error: boolean;
  /** Re-runs the ledger load after a failure. */
  readonly retry: () => void;
};

const PortalChatContext = createContext<PortalChatContextValue>({
  chats: [],
  loading: true,
  error: false,
  retry: () => {},
});

export const PortalChatProvider = PortalChatContext.Provider;

export function usePortalChats(): PortalChatContextValue {
  return useContext(PortalChatContext);
}

/** Newest-first ordering with a stable id tiebreak for equal timestamps. */
export function compareChatsDesc(
  left: RecentChat,
  right: RecentChat,
): number {
  const leftAt = left.createdAt ?? "";
  const rightAt = right.createdAt ?? "";
  if (leftAt !== rightAt) return rightAt.localeCompare(leftAt);
  return left.id.localeCompare(right.id);
}

/** Human label for one chat row: server title when named, else a conversation short id. */
export function chatLabel(chat: RecentChat): string {
  const title = chat.title?.trim();
  if (title !== undefined && title !== "") return title;
  return `Conversation ${chat.id.slice(0, 8)}`;
}
