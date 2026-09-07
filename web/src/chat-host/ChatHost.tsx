import { useEffect, useRef, useState } from "react";
import {
  mountDshApp,
  setVoieDshHostContext,
  unmountDshApp,
} from "../connection-voie/adapter.ts";
import type {
  ChatHostErrorHandler,
  ChatHostPhase,
  ChatHostProps,
} from "./types.ts";
import "./styles.css";

const DSH_ROOT_ID = "voie-dsh-root";

function reasonMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim() !== "") return reason.message;
  if (typeof reason === "string" && reason.trim() !== "") return reason;
  return "The conversation surface could not be loaded.";
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
 * Native VOIE lifecycle seat for the vendored conversation app.
 *
 * Product identity chrome (scope, account, navigation) lives on PortalShell.
 * This component mounts the conversation graph into the main pane and does
 * not render a second header or a DSH application frame.
 */
export function ChatHost({
  scope,
  workspace,
  agent,
  conversationId,
  onError,
  className,
}: ChatHostProps) {
  const lifecycle = useDshLifecycle(scope.id, onError);
  const hostClassName = ["chat-host", className].filter(Boolean).join(" ");
  setVoieDshHostContext({
    projectId: scope.id,
    ...(workspace.id === "" ? {} : { workspaceId: workspace.id }),
    ...(conversationId === undefined ? {} : { conversationId }),
  });

  return (
    <section
      className={hostClassName}
      aria-busy={lifecycle.phase === "mounting"}
    >
      <div className="chat-host__surface">
        <LifecycleNotice
          phase={lifecycle.phase}
          error={lifecycle.error}
          onRetry={lifecycle.retry}
        />
        <div
          id={DSH_ROOT_ID}
          className="chat-host__dsh-root"
          data-voie-project-id={scope.id}
          data-voie-workspace-id={workspace.id}
          {...(agent === undefined ? {} : { "data-voie-agent-id": agent.id })}
          {...(conversationId === undefined ? {} : { "data-voie-conversation-id": conversationId })}
        />
      </div>
    </section>
  );
}
