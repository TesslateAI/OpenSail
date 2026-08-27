/**
 * Display context for the native VOIE chrome around the vendored conversation
 * surface. The graph remains the owner of conversation rendering and
 * receives no provider, credential, or management data from this contract.
 */

export type ChatScopeContext = Readonly<{
  /** Stable scope identity used by the server authorization boundary. */
  id: string;
  /** Product-visible scope name. */
  name: string;
  /** Server-declared collaboration kind. */
  kind: "personal" | "team";
}>;

export type ChatWorkspaceContext = Readonly<{
  /** Stable workspace identity. */
  id: string;
  /** Optional product-visible workspace name. */
  name?: string | undefined;
}>;

export type ChatAgentContext = Readonly<{
  /** Stable agent identity. */
  id: string;
  /** Product-visible agent name. */
  name: string;
}>;

export type ChatAccountContext = Readonly<{
  /** Stable user identity shown in the account chrome. */
  id: string;
  /** Product-visible account display name. */
  displayName: string;
}>;

export type ChatHostErrorHandler = (error: Error) => void;

export type ChatHostProps = Readonly<{
  scope: ChatScopeContext;
  workspace: ChatWorkspaceContext;
  /** The conversation surface can resolve the agent itself while loading. */
  agent?: ChatAgentContext | undefined;
  /** The host displays this identity; the carrier remains the authority. */
  conversationId?: string | undefined;
  /** Account is optional so the host remains usable during bootstrap. */
  account?: ChatAccountContext | undefined;
  /** Receives a normalized mount failure without owning surface recovery. */
  onError?: ChatHostErrorHandler | undefined;
  /** Additional class name for the outer native host element. */
  className?: string | undefined;
}>;

export type ChatHostPhase = "mounting" | "ready" | "error";
