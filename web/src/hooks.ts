/** Shared resource loading and visibility-aware bounded polling hooks. */

import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, redirectToLogin } from "./api/http.ts";
import type { Uuid } from "./api/dto.ts";

export type ResourceState<T> = {
  data: T | null;
  error: Error | null;
  loading: boolean;
  reload: () => void;
};

/** Loads one resource and aborts it when the owning view unmounts. */
export function useResource<T>(
  load: (signal: AbortSignal) => Promise<T>,
  dependencies: readonly unknown[] = [],
): ResourceState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const [reloadToken, setReloadToken] = useState(0);
  const reload = useCallback(() => setReloadToken((token) => token + 1), []);

  useEffect(() => {
    const controller = new AbortController();
    let mounted = true;
    setLoading(true);
    setError(null);
    void load(controller.signal)
      .then((value) => {
        if (!mounted) return;
        setData(value);
        setLoading(false);
      })
      .catch((reason: unknown) => {
        if (!mounted || controller.signal.aborted) return;
        if (reason instanceof ApiError && reason.status === 401) redirectToLogin();
        const nextError = reason instanceof Error ? reason : new Error("request failed");
        setError(nextError);
        setLoading(false);
      });
    return () => {
      mounted = false;
      controller.abort();
    };
    // Callers provide a memoized loader and explicit resource dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [load, reloadToken, ...dependencies]);

  return { data, error, loading, reload };
}

/**
 * Runs one single-flight request at a bounded interval while visible. Hidden
 * tabs pause without accumulating work; unmount aborts the in-flight request.
 */
export function useBoundedPoll(
  tick: (signal: AbortSignal) => Promise<void>,
  intervalMs: number,
  active: boolean,
): void {
  useEffect(() => {
    if (!active) return undefined;
    let timer: number | undefined;
    let disposed = false;
    let running = false;
    let controller: AbortController | null = null;

    const schedule = (delay: number): void => {
      if (disposed) return;
      timer = window.setTimeout(() => {
        timer = undefined;
        if (disposed) return;
        if (document.hidden) {
          schedule(intervalMs);
          return;
        }
        if (running) {
          schedule(intervalMs);
          return;
        }
        running = true;
        controller = new AbortController();
        void tick(controller.signal)
          .catch(() => undefined)
          .finally(() => {
            running = false;
            controller = null;
            schedule(intervalMs);
          });
      }, delay);
    };

    const onVisibility = (): void => {
      if (!document.hidden && timer === undefined && !running) schedule(0);
    };
    document.addEventListener("visibilitychange", onVisibility);
    schedule(0);
    return () => {
      disposed = true;
      document.removeEventListener("visibilitychange", onVisibility);
      if (timer !== undefined) window.clearTimeout(timer);
      controller?.abort();
    };
  }, [active, intervalMs, tick]);
}

const DRAFT_STORAGE_PREFIX = "voie:draft:";

function draftStorageKey(sessionId: Uuid): string {
  return `${DRAFT_STORAGE_PREFIX}${sessionId}`;
}

/**
 * One persistent draft per session, keyed by the session's own identity.
 * The draft survives navigation, refresh, and tab close; it is cleared only
 * when the caller commits the prompt to a run. Storage failures degrade to
 * an in-memory draft instead of breaking the composer.
 */
export function usePersistentDraft(sessionId: Uuid | null): {
  draft: string;
  setDraft: (next: string) => void;
  clearDraft: () => void;
} {
  const [draft, setDraftState] = useState("");
  const sessionRef = useRef<Uuid | null>(null);

  // Switching sessions swaps the draft wholesale; the previous session's
  // draft is already persisted, so nothing is lost.
  useEffect(() => {
    sessionRef.current = sessionId;
    if (sessionId === null) {
      setDraftState("");
      return;
    }
    let stored = "";
    try {
      stored = window.localStorage.getItem(draftStorageKey(sessionId)) ?? "";
    } catch {
      stored = "";
    }
    setDraftState(stored);
  }, [sessionId]);

  const setDraft = useCallback((next: string): void => {
    setDraftState(next);
    const sessionIdNow = sessionRef.current;
    if (sessionIdNow === null) return;
    try {
      if (next.length === 0) window.localStorage.removeItem(draftStorageKey(sessionIdNow));
      else window.localStorage.setItem(draftStorageKey(sessionIdNow), next);
    } catch {
      // Storage may be unavailable; the in-memory draft still works.
    }
  }, []);

  const clearDraft = useCallback((): void => {
    setDraftState("");
    const sessionIdNow = sessionRef.current;
    if (sessionIdNow === null) return;
    try {
      window.localStorage.removeItem(draftStorageKey(sessionIdNow));
    } catch {
      // Nothing to clean up when storage is unavailable.
    }
  }, []);

  return { draft, setDraft, clearDraft };
}
