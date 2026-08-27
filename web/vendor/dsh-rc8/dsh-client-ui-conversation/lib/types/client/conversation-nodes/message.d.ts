import type { Context } from '@deepseek-ai/cordis';
import type { ContextMessageNode, ConversationNodeDefinition, SteeringMessageNode, UserMessageNode } from '@deepseek-ai/dsh-client-runtime/client';
interface ReferencedUserMessageNode extends UserMessageNode {
    /** Labels cited by the immediately following session-reference context. */
    readonly referenceLabels?: readonly string[];
}
interface ReferencedSteeringMessageNode extends SteeringMessageNode {
    /** Labels cited by the immediately following session-reference context. */
    readonly referenceLabels?: readonly string[];
}
type MessageNode = ReferencedUserMessageNode | ReferencedSteeringMessageNode | ContextMessageNode;
declare module '@deepseek-ai/dsh-client-ui-conversation/client' {
    interface ChatNodeDataMap {
        /** Ordinary turn-opening user message. */
        user: ReferencedUserMessageNode;
        /** User message admitted into an active turn. */
        steering: ReferencedSteeringMessageNode;
        /** Non-user context injected into model history. */
        context: ContextMessageNode;
    }
}
/** User, steering, and injected-context message classification Definition. */
export declare const messageDefinition: ConversationNodeDefinition<MessageNode>;
/**
 * Register the user, steering, and injected-context message contribution.
 * @param ctx - owning UI Conversation context.
 */
export declare function registerMessageConversationNode(ctx: Context): void;
export {};
//# sourceMappingURL=message.d.ts.map