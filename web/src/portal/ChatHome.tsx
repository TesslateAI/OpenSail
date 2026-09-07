/**
 * ChatHome: the portal's central conversation seat.
 *
 * Root `/` is New Chat: the graph creates a durable empty Session immediately
 * and the location is replaced with `/chat/:conversationId` once that Session
 * appears in the ledger. Deep links into `/chat/:conversationId` (and the
 * `/sessions/:id` aliases) open that Session on the same host.
 */

import { useEffect, useRef, useState } from "react";
import { getSession } from "../api/api.ts";
import { ChatHost } from "../chat-host/ChatHost.tsx";
import { requestVoieNewChat, requestVoieOpenConversation } from "../connection-voie/adapter.ts";
import { useConsole } from "../console.tsx";
import { appHref, useRouter } from "../router.tsx";
import { StateView } from "../ui/primitives.tsx";
import { usePortalChats } from "./chat-context.ts";
import { conversationScopeDecision } from "./conversation-scope.ts";
import { lastWorkspace } from "./last-workspace.ts";

export type ChatHomeProps = {
  /** Present when the URL addressed a specific conversation. */
  conversationId?: string | undefined;
  /**
   * Bumps when the user navigates to New chat (`/`) from another route so
   * the keep-mounted DSH graph creates a fresh durable Session instead of
   * appending to the previous current Session. Conversation switches never
   * remount ChatHost; the live graph opens the named session.
   */
  newChatGeneration?: number;
  /**
   * False while a management page covers the keep-mounted chat graph.
   * New Chat and newcomer URL replacement must not steal Workspaces.
   */
  seatActive?: boolean;
};

type ScopeGate = "wait" | "ready" | "missing";

export function ChatHome({
  conversationId,
  newChatGeneration = 0,
  seatActive = true,
}: ChatHomeProps) {
  const { me, selectedProject, projectId } = useConsole();
  const { navigate } = useRouter();
  const { chats, loading } = usePortalChats();
  // Conversation ids observed when New Chat starts. The first ledger row
  // that was not in this set is the durable Session just created.
  const knownAtMountRef = useRef<Set<string> | null>(null);
  const openedRef = useRef(false);
  const [scopeGate, setScopeGate] = useState<ScopeGate>(
    conversationId === undefined ? "ready" : "wait",
  );

  const listed =
    conversationId !== undefined && chats.some((chat) => chat.id === conversationId);

  useEffect(() => {
    if (conversationId === undefined) {
      setScopeGate("ready");
      return;
    }
    if (projectId === null) {
      setScopeGate("wait");
      return;
    }
    const pending = conversationScopeDecision({
      conversationId,
      currentProjectId: projectId,
      chatsLoaded: !loading,
      listed,
    });
    if (pending.kind === "stay") {
      setScopeGate("ready");
      return;
    }
    if (pending.kind === "wait") {
      setScopeGate("wait");
      return;
    }
    let cancelled = false;
    setScopeGate("wait");
    void (async () => {
      let sessionProjectId: string | null = null;
      try {
        const session = await getSession(conversationId);
        sessionProjectId = session.projectId === "" ? null : session.projectId;
      } catch {
        sessionProjectId = null;
      }
      if (cancelled) return;
      const decision = conversationScopeDecision({
        conversationId,
        currentProjectId: projectId,
        chatsLoaded: true,
        listed,
        sessionProjectId,
      });
      if (decision.kind === "reload") {
        window.location.replace(decision.href);
        return;
      }
      setScopeGate(decision.kind === "missing" ? "missing" : "ready");
    })();
    return () => {
      cancelled = true;
    };
  }, [conversationId, listed, loading, projectId]);

  useEffect(() => {
    if (conversationId === undefined) return;
    if (scopeGate !== "ready") return;
    requestVoieOpenConversation(conversationId);
  }, [conversationId, scopeGate]);

  useEffect(() => {
    if (!seatActive) return;
    if (newChatGeneration === 0 || conversationId !== undefined) return;
    knownAtMountRef.current = new Set(chats.map((chat) => chat.id));
    openedRef.current = false;
    const workspaceId = lastWorkspace(projectId ?? "");
    window.setTimeout(() => {
      requestVoieNewChat(workspaceId === "" ? undefined : workspaceId);
    }, 0);
    // `chats` is snapshotted once per New-chat navigation, not per ledger poll.
  }, [conversationId, newChatGeneration, projectId, seatActive]);

  useEffect(() => {
    if (!seatActive) return;
    if (conversationId !== undefined) {
      openedRef.current = true;
      knownAtMountRef.current = new Set<string>();
      return;
    }
    if (knownAtMountRef.current === null && !loading && chats.length > 0) {
      knownAtMountRef.current = new Set(chats.map((chat) => chat.id));
      return;
    }
    if (knownAtMountRef.current === null) return;
    if (openedRef.current) return;
    const newcomer = chats.find((chat) => !knownAtMountRef.current!.has(chat.id));
    if (newcomer === undefined) return;
    openedRef.current = true;
    navigate(appHref(`/chat/${encodeURIComponent(newcomer.id)}`, projectId), true);
  }, [chats, conversationId, loading, navigate, projectId, seatActive]);

  if (conversationId !== undefined && scopeGate === "missing") {
    return (
      <div className="portal-chat">
        <StateView
          state="empty"
          title="Conversation not found"
          detail="This conversation is not in a scope you can open."
        />
      </div>
    );
  }

  if (selectedProject === null || projectId === null || me === null || scopeGate === "wait") {
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
  const remembered = lastWorkspace(projectId ?? "");
  const boundWorkspaceId = boundChat?.workspaceId || remembered;

  return (
    <div className="portal-chat">
      <div className="portal-chat-seat">
        <ChatHost
          scope={{
            id: selectedProject.id,
            name: selectedProject.name,
            kind: selectedProject.kind,
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
