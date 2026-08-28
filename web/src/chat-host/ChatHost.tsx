import { useEffect, useRef, useState } from "react";
import { mountDshApp, unmountDshApp } from "../dsh-lifecycle.ts";
import type {
  ChatAccountContext,
  ChatAgentContext,
  ChatHostErrorHandler,
  ChatHostPhase,
  ChatHostProps,
  ChatScopeContext,
  ChatWorkspaceContext,
} from "./types.ts";
import "./styles.css";

const DSH_ROOT_ID = "voie-dsh-root";

function nonEmpty(value: string | undefined, fallback: string): string {
  const trimmed = value?.trim();
  return trimmed === undefined || trimmed === "" ? fallback : trimmed;
}

function reasonMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim() !== "") return reason.message;
  if (typeof reason === "string" && reason.trim() !== "") return reason;
  return "The conversation surface could not be loaded.";
}

function contextLabel(label: string | undefined, id: string, fallback: string): string {
  return nonEmpty(label, nonEmpty(id, fallback));
}

/**
 * Wait for a vendored-graph run to settle before disposing it. This ordering
 * keeps a fast route change from starting a second graph while the previous
 * graph is still loading or tearing down.
 */
async function disposeDsh(run: Promise<void> | undefined): Promise<void> {
  if (run !== undefined) {
    try {
      await run;
    } catch {
      // The graph is still disposed below; the mount error is reported by the
      // caller and cleanup failures must not become unhandled rejections.
    }
  }
  try {
    await unmountDshApp();
  } catch {
    // React cleanup cannot surface an error to a removed tree.
  }
}

function useDshLifecycle(
  scopeId: string,
  onError: ChatHostErrorHandler | undefined,
): { phase: ChatHostPhase; error: string | null; retry: () => void } {
  const [phase, setPhase] = useState<ChatHostPhase>("mounting");
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const onErrorRef = useRef<ChatHostErrorHandler | undefined>(onError);
  const pendingUnmount = useRef<Promise<void> | undefined>(undefined);
  onErrorRef.current = onError;

  useEffect(() => {
    let active = true;
    let mountRun: Promise<void> | undefined;
    const previousUnmount = pendingUnmount.current;

    setPhase("mounting");
    setError(null);

    const start = async (): Promise<void> => {
      if (previousUnmount !== undefined) await previousUnmount;
      if (!active) return;

      // Promise.resolve().then() also captures a synchronous missing-root
      // throw from mountDshApp in the same error path as an async rejection.
      mountRun = Promise.resolve().then(() => mountDshApp());
      try {
        await mountRun;
        if (active) setPhase("ready");
      } catch (reason: unknown) {
        if (!active) return;
        const errorObject = reason instanceof Error ? reason : new Error(reasonMessage(reason));
        await disposeDsh(mountRun);
        if (!active) return;
        setError(reasonMessage(errorObject));
        setPhase("error");
        try {
          onErrorRef.current?.(errorObject);
        } catch {
          // Error callbacks are notifications; they must not hide host state.
        }
      }
    };

    void start();

    return () => {
      active = false;
      // Capture the predecessor before publishing this cleanup promise so
      // retries and route changes serialize dispose operations.
      const cleanup = (async (): Promise<void> => {
        if (previousUnmount !== undefined) await previousUnmount;
        await disposeDsh(mountRun);
      })();
      pendingUnmount.current = cleanup;
    };
    // Conversation and workspace ids are data-attribute identity for the
    // already-booted graph. Remounting on those fields destroys the live
    // seat when the URL is promoted after the first message. Scope changes
    // still remount because the carrier binds scope at boot.
  }, [attempt, scopeId]);

  return {
    phase,
    error,
    retry: () => setAttempt((value) => value + 1),
  };
}

function ScopeChrome({
  scope,
  account,
}: {
  scope: ChatScopeContext;
  account: ChatAccountContext | undefined;
}) {
  const scopeName = contextLabel(scope.name, scope.id, "Unknown scope");
  const accountName = nonEmpty(
    account?.displayName,
    nonEmpty(account?.id, "Signed-in account"),
  );

  return (
    <header className="chat-host__chrome" aria-label="VOIE account and scope">
      <div className="chat-host__identity">
        <span className="chat-host__mark" aria-hidden="true" />
        <span className="chat-host__wordmark">VOIE</span>
        <span className="chat-host__product">Chat</span>
      </div>
      <div className="chat-host__scope">
        <span className="chat-host__eyebrow">Scope</span>
        <strong title={scope.id}>{scopeName}</strong>
        <span className="chat-host__secondary">{scope.kind}</span>
      </div>
      <div className="chat-host__account">
        <span className="chat-host__eyebrow">Account</span>
        <strong title={account?.id}>{accountName}</strong>
      </div>
    </header>
  );
}


function LifecycleNotice({
  phase,
  error,
  onRetry,
}: {
  phase: ChatHostPhase;
  error: string | null;
  onRetry: () => void;
}) {
  if (phase === "mounting") {
    return (
      <p className="chat-host__notice" role="status" aria-live="polite">
        Preparing the conversation surface…
      </p>
    );
  }
  if (phase !== "error") return null;
  return (
    <div className="chat-host__notice chat-host__notice--error" role="alert">
      <strong>Conversation unavailable</strong>
      <span>{error ?? "The conversation surface could not be loaded."}</span>
      <button type="button" className="chat-host__retry" onClick={onRetry}>
        Retry
      </button>
    </div>
  );
}

/**
 * Native VOIE chrome and lifecycle seat for the vendored conversation app.
 *
 * This component deliberately does not render conversation messages, prompt
 * controls, tools, or management actions. `mountDshApp` owns that surface.
 * `unmountDshApp` restores the module-loader queue so a later seat can boot
 * again; conversation URL promotion does not remount the graph.
 */
export function ChatHost({
  scope,
  workspace,
  agent,
  conversationId,
  account,
  onError,
  className,
}: ChatHostProps) {
  const lifecycle = useDshLifecycle(scope.id, onError);
  const hostClassName = ["chat-host", className].filter(Boolean).join(" ");

  return (
    <section
      className={hostClassName}
      aria-busy={lifecycle.phase === "mounting"}
    >
      <ScopeChrome scope={scope} account={account} />
      <div className="chat-host__surface">
        <LifecycleNotice
          phase={lifecycle.phase}
          error={lifecycle.error}
          onRetry={lifecycle.retry}
        />
        <div
          id={DSH_ROOT_ID}
          className="chat-host__dsh-root"
          data-voie-scope-id={scope.id}
          data-voie-workspace-id={workspace.id}
          {...(agent === undefined ? {} : { "data-voie-agent-id": agent.id })}
          {...(conversationId === undefined ? {} : { "data-voie-conversation-id": conversationId })}
        />
      </div>
    </section>
  );
}
