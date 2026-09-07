import type { WorkspaceConversationDto } from "../api/workspace-details.ts";
import { appHref, Link } from "../router.tsx";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { conversationTitle, formatDate, shortId } from "./model.ts";

export type ConversationsSectionProps = {
  conversations: readonly WorkspaceConversationDto[];
  loading: boolean;
  error: Error | null;
  onRetry: () => void;
  projectId: string | null;
  onOpenConversation?: ((conversationId: string) => void) | undefined;
};

/** Read-only list of recent conversations attached to this workspace. */
export function ConversationsSection({
  conversations,
  loading,
  error,
  onRetry,
  projectId,
  onOpenConversation,
}: ConversationsSectionProps) {
  const ordered = [...conversations].sort((left, right) => {
    const leftTime = left.createdAt === null ? Number.MIN_SAFE_INTEGER : Date.parse(left.createdAt);
    const rightTime =
      right.createdAt === null ? Number.MIN_SAFE_INTEGER : Date.parse(right.createdAt);
    const safeLeft = Number.isNaN(leftTime) ? Number.MIN_SAFE_INTEGER : leftTime;
    const safeRight = Number.isNaN(rightTime) ? Number.MIN_SAFE_INTEGER : rightTime;
    return safeRight - safeLeft;
  });

  return (
    <Card title="Recent conversations">
      {loading ? (
        <StateView state="loading" title="Loading conversations" />
      ) : error !== null ? (
        <StateView
          state="error"
          title="Could not load conversations"
          detail={error.message}
          onRetry={onRetry}
        />
      ) : ordered.length === 0 ? (
        <StateView
          state="empty"
          title="No conversations yet"
          detail="Start a conversation by selecting this workspace in the chat surface."
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Conversation</th>
              <th scope="col">Agent</th>
              <th scope="col">Status</th>
              <th scope="col">Head revision</th>
              <th scope="col">Created</th>
            </tr>
          </thead>
          <tbody>
            {ordered.map((conversation) => {
              const href = appHref(
                `/chat/${encodeURIComponent(conversation.id)}`,
                projectId,
              );
              const title = conversationTitle(conversation);
              const link =
                onOpenConversation === undefined ? (
                  <Link to={href}>{title}</Link>
                ) : (
                  <a
                    href={href}
                    onClick={(event) => {
                      event.preventDefault();
                      onOpenConversation(conversation.id);
                    }}
                  >
                    {title}
                  </a>
                );
              return (
                <tr key={conversation.id}>
                  <td title={conversation.id}>
                    {link}
                    <span className="mono muted"> {shortId(conversation.id)}</span>
                  </td>
                  <td className="mono" title={conversation.agentId}>
                    {shortId(conversation.agentId)}
                  </td>
                  <td>
                    {conversation.running ? (
                      <Badge tone="accent">Running</Badge>
                    ) : (
                      <Badge tone="neutral">Idle</Badge>
                    )}
                  </td>
                  <td className="mono">{conversation.headRevision}</td>
                  <td>{formatDate(conversation.createdAt)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </Card>
  );
}
