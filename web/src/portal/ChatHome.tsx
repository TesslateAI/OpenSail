/**
 * ChatHome: the portal's central conversation seat.
 *
 * Root `/` renders this as a new-chat surface: DSH is mounted with no
 * conversation id and the first message lazily creates the conversation
 * server-side (the browser never issues a headless create). Once the
 * product ledger shows a conversation that did not exist at mount time,
 * the location is replaced with `/chat/:conversationId` so the URL always
 * reflects the live thread. Deep links into `/chat/:conversationId` (and
 * the legacy `/sessions/:id` aliases) mount the same host bound to that
 * conversation.
 */

import { useEffect, useRef, useState } from "react";
import { ChatHost } from "../chat-host/ChatHost.tsx";
import { useConsole } from "../console.tsx";
import { useRouter } from "../router.tsx";
import { StateView } from "../ui/primitives.tsx";
import { usePortalChats } from "./chat-context.ts";

export type ChatHomeProps = {
  /** Present when the URL addressed a specific conversation. */
  conversationId?: string | undefined;
  /**
   * Bumps when the user navigates to New chat (`/`) from another route so
   * the keep-mounted DSH graph starts a fresh provisional session instead
   * of appending to the previous current session.
   */
  newChatGeneration?: number;
};

export function ChatHome({ conversationId, newChatGeneration = 0 }: ChatHomeProps) {
  const { me, selectedScope, projectId } = useConsole();
  const { navigate } = useRouter();
  const { chats, loading } = usePortalChats();
  const [seat, setSeat] = useState(0);
  const previousConversation = useRef(conversationId);

  // Known conversation ids observed at mount, before DSH starts. When the
  // ledger later gains a row that was absent here, the new-chat surface
  // replaces the URL with the freshly created conversation.
  const knownAtMountRef = useRef<Set<string> | null>(null);
  const promotedRef = useRef(false);

  useEffect(() => {
    const previous = previousConversation.current;
    previousConversation.current = conversationId;
    if (previous === conversationId) return;
    // URL promotion after the first message must keep the live graph.
    if (previous === undefined && conversationId !== undefined) return;
    setSeat((value) => value + 1);
  }, [conversationId]);

  useEffect(() => {
    if (newChatGeneration === 0 || conversationId !== undefined) return;
    knownAtMountRef.current = new Set(chats.map((chat) => chat.id));
    promotedRef.current = false;
    window.setTimeout(() => {
      window.dispatchEvent(new Event("voie-new-chat"));
    }, 0);
    // `chats` is snapshotted once per New-chat navigation, not per ledger poll.
  }, [conversationId, newChatGeneration]);

  useEffect(() => {
    // A deep-linked conversation never needs promotion.
    if (conversationId !== undefined) {
      promotedRef.current = true;
      knownAtMountRef.current = new Set<string>();
      return;
    }
    if (knownAtMountRef.current === null && !loading && chats.length > 0) {
      knownAtMountRef.current = new Set(chats.map((chat) => chat.id));
      return;
    }
    if (knownAtMountRef.current === null) return;
    if (promotedRef.current) return;
    const newcomer = chats.find((chat) => !knownAtMountRef.current!.has(chat.id));
    if (newcomer === undefined) return;
    promotedRef.current = true;
    navigate(`/chat/${encodeURIComponent(newcomer.id)}`, true);
  }, [chats, conversationId, loading, navigate]);

  if (selectedScope === null || projectId === null || me === null) {
    return (
      <StateView
        state="loading"
        title="Preparing chat"
        detail="Waiting for your scope context."
      />
    );
  }

  const boundChat = conversationId === undefined
    ? undefined
    : chats.find((chat) => chat.id === conversationId);
  const boundWorkspaceId = boundChat?.workspaceId ?? "";

  return (
    <div className="portal-chat">
      <div className="portal-chat-seat">
        <ChatHost
          key={seat}
          scope={{
            id: selectedScope.id,
            name: selectedScope.name,
            kind: selectedScope.kind,
          }}
          workspace={{ id: boundWorkspaceId, name: undefined }}
          conversationId={conversationId}
          account={{
            id: me.userId,
            displayName: me.displayName?.trim() || me.username?.trim() || me.userId,
          }}
        />
      </div>
    </div>
  );
}
