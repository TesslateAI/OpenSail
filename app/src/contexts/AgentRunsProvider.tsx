import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { chatApi } from '../lib/api';
import { notifyFileMutationsFromAgentEvent } from '../utils/agentFileEvents';
import {
  AgentRunsContext,
  type AgentRunsContextValue,
  type AgentRunState,
  type AgentRunStatus,
} from './agentRunsContext';

/**
 * Live run-state for every active agent task in a project.
 *
 * Keeps a background SSE subscription alive for each running chat so that:
 *   - switching between chats is instant (stream already warm)
 *   - the sidebar can show pulsing dots for all running agents
 *   - "+"-spawned parallel agents remain visible even when unfocused
 *
 * Bounded to a max concurrent-subscription count to respect the browser's
 * ~6-per-origin SSE cap; LRU eviction drops the oldest unfocused stream.
 * (Focused chat is always kept subscribed.)
 */

const MAX_CONCURRENT_STREAMS = 5;
/**
 * When the number of parallel runs would exceed this, switch the whole
 * project to the multiplexed SSE endpoint — one connection, all tasks fan
 * in server-side. Avoids hitting the browser's ~6-per-origin SSE cap.
 */
const MUX_THRESHOLD = MAX_CONCURRENT_STREAMS;

interface AgentRunsProviderProps {
  projectId: string | null;
  children: ReactNode;
  /** How often to re-sync cold state with the server (ms). */
  pollIntervalMs?: number;
}

export function AgentRunsProvider({
  projectId,
  children,
  pollIntervalMs = 15_000,
}: AgentRunsProviderProps) {
  const [runs, setRuns] = useState<Map<string, AgentRunState>>(() => new Map());
  const streamsRef = useRef<Map<string, EventSource>>(new Map());
  const muxStreamRef = useRef<EventSource | null>(null);
  const muxModeRef = useRef<boolean>(false);
  const focusedRef = useRef<Set<string>>(new Set());
  const lruRef = useRef<string[]>([]);

  const setRun = useCallback((chatId: string, patch: Partial<AgentRunState>) => {
    setRuns((prev) => {
      const next = new Map(prev);
      const existing = next.get(chatId);
      if (!existing && !patch.taskId) {
        return prev; // ignore patches for unknown chats without taskId
      }
      next.set(chatId, {
        taskId: existing?.taskId ?? patch.taskId!,
        chatId,
        status: patch.status ?? existing?.status ?? 'running',
        startedAt: existing?.startedAt ?? patch.startedAt ?? Date.now(),
        latestEvent: patch.latestEvent ?? existing?.latestEvent,
        lastEventId: patch.lastEventId ?? existing?.lastEventId,
      });
      return next;
    });
  }, []);

  const closeStream = useCallback((chatId: string) => {
    const es = streamsRef.current.get(chatId);
    if (es) {
      es.close();
      streamsRef.current.delete(chatId);
    }
  }, []);

  const applyEventToChat = useCallback(
    (chatId: string, taskId: string, payload: unknown, lastEventId?: string) => {
      setRun(chatId, {
        taskId,
        latestEvent: payload,
        lastEventId: lastEventId || undefined,
      });
      notifyFileMutationsFromAgentEvent(payload);
      const type = (payload && (payload as { type?: string }).type) || '';
      if (type === 'complete' || type === 'error') {
        const reason =
          (payload as { data?: { completion_reason?: string } }).data?.completion_reason || type;
        const status: AgentRunStatus =
          type === 'error' ? 'error' : reason === 'superseded' ? 'superseded' : 'completed';
        setRun(chatId, { status });
        closeStream(chatId);
      }
    },
    [closeStream, setRun]
  );

  const closeMux = useCallback(() => {
    if (muxStreamRef.current) {
      muxStreamRef.current.close();
      muxStreamRef.current = null;
    }
    muxModeRef.current = false;
  }, []);

  const openMux = useCallback(() => {
    if (!projectId || muxStreamRef.current) return;
    // Close all per-task streams — the mux covers everything now.
    for (const [, es] of streamsRef.current) es.close();
    streamsRef.current.clear();
    const es = chatApi.subscribeToProject(projectId);
    muxStreamRef.current = es;
    muxModeRef.current = true;
    es.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data) as {
          chat_id?: string;
          task_id?: string;
          event?: unknown;
          type?: string;
        };
        if (msg.type === 'ready') return;
        if (!msg.chat_id || !msg.task_id || !msg.event) return;
        applyEventToChat(msg.chat_id, msg.task_id, msg.event, e.lastEventId);
      } catch {
        // heartbeat / keepalive — ignore
      }
    };
    es.onerror = () => {
      // Browser auto-reconnects; nothing to do.
    };
  }, [projectId, applyEventToChat]);

  const openStream = useCallback(
    (chatId: string, taskId: string) => {
      // If mux is already running, no per-task stream needed.
      if (muxModeRef.current) {
        setRun(chatId, { taskId, status: 'running' });
        return;
      }
      // If adding this stream would push us past the mux threshold, upgrade.
      if (streamsRef.current.size + 1 > MUX_THRESHOLD) {
        setRun(chatId, { taskId, status: 'running' });
        openMux();
        return;
      }
      // If we already have a stream for this chat, and it's on the same task,
      // leave it alone. Otherwise close & re-open.
      const existing = streamsRef.current.get(chatId);
      if (existing) {
        closeStream(chatId);
      }

      // LRU eviction if we're over capacity.
      lruRef.current = lruRef.current.filter((c) => c !== chatId);
      lruRef.current.push(chatId);
      while (streamsRef.current.size >= MAX_CONCURRENT_STREAMS) {
        const evictable = lruRef.current.find((c) => c !== chatId && !focusedRef.current.has(c));
        if (!evictable) break;
        closeStream(evictable);
        lruRef.current = lruRef.current.filter((c) => c !== evictable);
      }

      const es = chatApi.subscribeToTask(taskId);
      streamsRef.current.set(chatId, es);

      es.onmessage = (e) => {
        try {
          const payload = JSON.parse(e.data);
          applyEventToChat(chatId, taskId, payload, e.lastEventId);
        } catch {
          // ignore parse errors; the stream may send heartbeat pings
        }
      };

      es.onerror = () => {
        // The browser will attempt to reconnect automatically until close().
        // If the stream has ended cleanly (task complete), onmessage handled it.
      };
    },
    [closeStream, setRun, openMux, applyEventToChat]
  );

  const register = useCallback(
    (chatId: string, taskId: string) => {
      setRun(chatId, { taskId, status: 'running', startedAt: Date.now() });
      openStream(chatId, taskId);
    },
    [openStream, setRun]
  );

  const markTerminal = useCallback(
    (chatId: string, status: AgentRunStatus) => {
      closeStream(chatId);
      setRun(chatId, { status });
    },
    [closeStream, setRun]
  );

  const isRunning = useCallback((chatId: string) => runs.get(chatId)?.status === 'running', [runs]);

  const focus = useCallback((chatId: string) => {
    focusedRef.current.add(chatId);
  }, []);

  const unfocus = useCallback((chatId: string) => {
    focusedRef.current.delete(chatId);
  }, []);

  const refresh = useCallback(async () => {
    if (!projectId) return;
    try {
      const { tasks } = await chatApi.getActiveTasksInProject(projectId);
      const live = new Set<string>(tasks.map((t) => t.chat_id));

      // If the server reports more live tasks than our per-task cap, upgrade
      // to the multiplexed stream. Otherwise open per-task streams as normal.
      if (tasks.length > MUX_THRESHOLD && !muxModeRef.current) {
        // Seed state for all known tasks before opening mux.
        for (const t of tasks) {
          setRun(t.chat_id, { taskId: t.task_id, status: 'running' });
        }
        openMux();
      } else if (!muxModeRef.current) {
        for (const t of tasks) {
          if (!streamsRef.current.has(t.chat_id)) {
            register(t.chat_id, t.task_id);
          }
        }
      } else {
        // Already in mux mode — just ensure the runs map reflects any
        // newly-reported tasks (events will arrive via the mux stream).
        for (const t of tasks) {
          setRun(t.chat_id, { taskId: t.task_id, status: 'running' });
        }
      }

      // Mark previously-running chats that are no longer active as completed.
      setRuns((prev) => {
        const next = new Map(prev);
        for (const [chatId, state] of prev) {
          if (state.status === 'running' && !live.has(chatId)) {
            next.set(chatId, { ...state, status: 'completed' });
            if (!muxModeRef.current) closeStream(chatId);
          }
        }
        return next;
      });
    } catch (e) {
      // Non-fatal — the next poll will try again.
      console.debug('[AGENT-RUNS] refresh failed', e);
    }
  }, [projectId, register, closeStream, openMux, setRun]);

  // Cold-start sync + periodic re-sync as a backstop for missed events.
  useEffect(() => {
    void refresh();
    if (!projectId) return;
    const id = setInterval(() => void refresh(), pollIntervalMs);
    return () => clearInterval(id);
  }, [projectId, refresh, pollIntervalMs]);

  // Close every stream on project change / unmount.
  useEffect(() => {
    const streams = streamsRef.current;
    const focused = focusedRef.current;
    return () => {
      for (const [, es] of streams) es.close();
      streams.clear();
      focused.clear();
      lruRef.current = [];
      closeMux();
    };
  }, [projectId, closeMux]);

  const value = useMemo<AgentRunsContextValue>(
    () => ({ runs, isRunning, focus, unfocus, register, markTerminal, refresh }),
    [runs, isRunning, focus, unfocus, register, markTerminal, refresh]
  );

  return <AgentRunsContext.Provider value={value}>{children}</AgentRunsContext.Provider>;
}
