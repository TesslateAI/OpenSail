/** Session route: live agent surface, bounded event polling, and run lifecycle.
 *
 * Events are kept as a local ordered RawEventDto[] merged by global sequence;
 * the poll cursor advances only to the highest sequence actually received, so
 * a truncated page never skips its remainder. Run state polls every 2s while
 * accepted/dispatched and stops on terminal/unknown, refreshing events once.
 *
 * Chat-first behavior: the composer draft is persistent per session (survives
 * navigation and refresh), Enter sends while Shift+Enter breaks lines, and
 * the primary action flips to Stop while a run is active. The submitted
 * prompt renders immediately as an optimistic user bubble keyed by the
 * client-minted run/intent identity; the canonical `user/message` event
 * reconciles it, and refresh re-adopts the run without resubmitting it.
 * A first message handed off by the hero composer is submitted exactly once.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  cancelRun,
  decodeEventItems,
  getRun,
  getSession,
  listSessionEvents,
  listRuns,
} from "../api/api.ts";
import {
  clearPendingChatPrompt,
  newChatIntent,
  readPendingChatPrompt,
  submitChatPrompt,
  takeFirstPrompt,
  writePendingChatPrompt,
  type ChatIntent,
  type PendingChatPrompt,
} from "../api/chat.ts";
import type {
  RawEventDto,
  RunDto,
  RunState,
  SessionEventsPageDto,
  SessionSummaryDto,
  Uuid,
} from "../api/dto.ts";
import { userMessageTextOf } from "../events/project.ts";
import { useConsole } from "../console.tsx";
import { useBoundedPoll, usePersistentDraft, useResource } from "../hooks.ts";
import { appHref, Link, useRouter } from "../router.tsx";
import { AgentSurface, type PendingUserMessage } from "../ui/AgentSurface.tsx";
import { Composer } from "../ui/Composer.tsx";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

const POLL_MS = 2000;

function isActiveState(state: RunState): boolean {
  return state === "accepted" || state === "dispatched";
}

function errorOf(reason: unknown): Error {
  return reason instanceof Error ? reason : new Error("request failed");
}

function formatWhen(iso: string | null): string | null {
  if (iso === null || iso.trim().length === 0) return null;
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString();
}

function eventKey(event: RawEventDto): string {
  // Canonical appends always carry the sole global sequence.
  return `${event.globalSeq}:${event.eventIndex ?? 0}`;
}

function pendingMatchesEvent(pending: PendingChatPrompt, event: RawEventDto): boolean {
  // The event's sequence must be newer than the cursor observed before this
  // intent. This prevents an older identical prompt from retiring a new
  // optimistic bubble.
  return event.globalSeq > pending.afterSeq && userMessageTextOf(event) === pending.prompt;
}

export function Session() {
  const { location } = useRouter();
  const sessionId = location.route.name === "session" ? location.route.sessionId : null;
  const { projectId, canOperate: projectCanOperate } = useConsole();

  // --- session resource ------------------------------------------------------

  const loadSession = useCallback(
    async (signal: AbortSignal): Promise<SessionSummaryDto> => {
      if (sessionId === null) throw new Error("no session id in route");
      return getSession(sessionId, signal);
    },
    [sessionId],
  );
  const sessionResource = useResource(loadSession, [sessionId]);
  const session = sessionResource.data;
  const reloadSession = sessionResource.reload;

  // Prompting is governed solely by this session's own server-emitted
  // capability set — never AND-ed with the currently selected project,
  // which may differ from the session's project. The selected project's
  // set is only a stand-in while the detail resource omits capabilities.
  const canOperate =
    session !== null && session.capabilities !== null
      ? session.capabilities.operateSessions
      : projectCanOperate;

  // --- local event log: ordered, deduped by global append sequence ----------

  const [events, setEvents] = useState<RawEventDto[]>([]);
  const [headRevision, setHeadRevision] = useState(0);
  const headRef = useRef(0);
  const [eventsLoading, setEventsLoading] = useState(true);
  const [eventsError, setEventsError] = useState<Error | null>(null);
  const [eventsReady, setEventsReady] = useState(false);

  const mergePage = useCallback((page: SessionEventsPageDto) => {
    const pageEvents = decodeEventItems(page.items);
    const receivedCursor = Math.max(
      page.cursor ?? 0,
      ...pageEvents.map((event) => event.globalSeq),
    );
    if (receivedCursor > headRef.current) headRef.current = receivedCursor;
    const receivedRevision = pageEvents.reduce(
      (maximum, event) => Math.max(maximum, event.revision),
      0,
    );
    if (receivedRevision > 0) {
      setHeadRevision((current) => Math.max(current, receivedRevision));
    }
    setEvents((previous) => {
      const byKey = new Map<string, RawEventDto>();
      for (const event of previous) byKey.set(eventKey(event), event);
      for (const event of pageEvents) {
        if (!byKey.has(eventKey(event))) byKey.set(eventKey(event), event);
      }
      if (byKey.size === previous.length) return previous;
      return [...byKey.values()].sort(
        (a, b) => a.globalSeq - b.globalSeq,
      );
    });
  }, []);

  const loadInitialEvents = useCallback(
    async (signal: AbortSignal): Promise<void> => {
      if (sessionId === null) throw new Error("no session id in route");
      mergePage(await listSessionEvents(sessionId, 0, signal));
    },
    [mergePage, sessionId],
  );

  useEffect(() => {
    setEvents([]);
    setHeadRevision(0);
    headRef.current = 0;
    setEventsLoading(true);
    setEventsError(null);
    setEventsReady(false);
    if (sessionId === null) {
      setEventsLoading(false);
      return undefined;
    }
    const controller = new AbortController();
    let disposed = false;
    void loadInitialEvents(controller.signal)
      .then(() => {
        if (!disposed) setEventsReady(true);
      })
      .catch((reason: unknown) => {
        if (!disposed && !controller.signal.aborted) setEventsError(errorOf(reason));
      })
      .finally(() => {
        if (!disposed) setEventsLoading(false);
      });
    return () => {
      disposed = true;
      controller.abort();
    };
  }, [loadInitialEvents, sessionId]);

  const retryEvents = useCallback(() => {
    if (sessionId === null) return;
    const controller = new AbortController();
    setEventsLoading(true);
    setEventsError(null);
    void loadInitialEvents(controller.signal)
      .then(() => setEventsReady(true))
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) setEventsError(errorOf(reason));
      })
      .finally(() => setEventsLoading(false));
  }, [loadInitialEvents, sessionId]);

  const eventsTick = useCallback(
    async (signal: AbortSignal): Promise<void> => {
      if (sessionId !== null) {
        mergePage(await listSessionEvents(sessionId, headRef.current, signal));
      }
    },
    [mergePage, sessionId],
  );
  // Bounded incremental polling while mounted: one single-flight page per
  // tick, paused on hidden tabs, aborted on unmount.
  useBoundedPoll(eventsTick, POLL_MS, eventsReady);

  // --- run lifecycle ----------------------------------------------------------

  const [run, setRun] = useState<RunDto | null>(null);
  const { draft, setDraft, clearDraft } = usePersistentDraft(sessionId);
  const [submitting, setSubmitting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [actionError, setActionError] = useState<Error | null>(null);
  const runIdRef = useRef<Uuid | null>(null);
  const [cancelRequested, setCancelRequested] = useState(false);
  const [pendingPrompt, setPendingPrompt] = useState<PendingChatPrompt | null>(() =>
    sessionId === null ? null : readPendingChatPrompt(sessionId),
  );
  const [runsLoaded, setRunsLoaded] = useState(false);

  useEffect(() => {
    setRun(null);
    runIdRef.current = null;
    setCancelRequested(false);
    setPendingPrompt(sessionId === null ? null : readPendingChatPrompt(sessionId));
    setRunsLoaded(false);
    setSubmitting(false);
    setCancelling(false);
    setActionError(null);
  }, [sessionId]);

  const runActive = run !== null && isActiveState(run.state);
  const serverRunning = session?.running === true;
  const runLive = runActive || cancelRequested;

  const adoptRun = useCallback((fresh: RunDto) => {
    runIdRef.current = fresh.id;
    setRun(fresh);
  }, []);

  const observeSettledRun = useCallback(() => {
    reloadSession();
    setCancelRequested(false);
    if (sessionId !== null) {
      void listSessionEvents(sessionId, headRef.current)
        .then(mergePage)
        .catch(() => undefined);
      // A settled run no longer needs an optimistic overlay. The canonical
      // event refresh above remains authoritative and may render the same
      // user message in its ordered position.
      setPendingPrompt(null);
      clearPendingChatPrompt(sessionId);
    }
  }, [mergePage, reloadSession, sessionId]);

  const runTick = useCallback(
    async (signal: AbortSignal): Promise<void> => {
      const runId = runIdRef.current;
      if (runId === null) return;
      const fresh = await getRun(runId, signal);
      adoptRun(fresh);
      // A cancel request may temporarily normalize to `unknown`; keep
      // polling while that transient request is visible, but preserve the
      // original terminal/unknown stop behavior for ordinary runs.
      if (!isActiveState(fresh.state) && !(cancelRequested && fresh.state === "unknown")) {
        observeSettledRun();
      }
    },
    [adoptRun, cancelRequested, observeSettledRun],
  );
  // Poll the active run every 2s; stops itself once terminal/unknown.
  useBoundedPoll(runTick, POLL_MS, runLive);

  // A refresh loses the locally started run. Re-adopt any active run for this
  // session from the authoritative runs listing so polling and cancel resume.
  // The listing is ordered by accepted time, so the last match is latest.
  useEffect(() => {
    if (sessionId === null || runIdRef.current !== null) return undefined;
    const controller = new AbortController();
    let disposed = false;
    void listRuns(controller.signal)
      .then((runs) => {
        if (disposed) return;
        const mine = runs.filter((candidate) => candidate.sessionId === sessionId);
        const latest = mine.at(-1);
        if (latest !== undefined && isActiveState(latest.state)) adoptRun(latest);
        setRunsLoaded(true);
      })
      .catch(() => {
        if (!disposed) setRunsLoaded(true);
      });
    return () => {
      disposed = true;
      controller.abort();
    };
  }, [adoptRun, sessionId]);

  const submitPrompt = useCallback(
    async (prompt: string): Promise<void> => {
      if (sessionId === null || !canOperate || submitting) return;
      const intent: ChatIntent = newChatIntent();
      const optimistic: PendingChatPrompt = {
        ...intent,
        prompt,
        afterSeq: headRef.current,
      };
      // Render and persist the identity before the request resolves. A
      // refresh can restore this overlay, but it never replays the request.
      setPendingPrompt(optimistic);
      writePendingChatPrompt(sessionId, optimistic);
      setSubmitting(true);
      setActionError(null);
      try {
        const receipt = await submitChatPrompt(sessionId, prompt, intent);
        if (!receipt.accepted) {
          setPendingPrompt(null);
          clearPendingChatPrompt(sessionId);
          setDraft(prompt);
          setActionError(new Error(receipt.reason ?? "the run was not accepted"));
          return;
        }
        const committed: PendingChatPrompt = {
          ...optimistic,
          runId: receipt.runId,
          intentId: receipt.intentId,
        };
        setPendingPrompt(committed);
        writePendingChatPrompt(sessionId, committed);
        // The answer is an acceptance receipt, not a run resource; poll
        // `/api/runs/:id` for timestamps and the retained result.
        adoptRun({
          id: receipt.runId,
          intentId: receipt.intentId,
          sessionId,
          state: receipt.state,
          result: null,
          acceptedAt: null,
          dispatchedAt: null,
          terminalAt: null,
          cancelledAt: null,
        });
        clearDraft();
      } catch (reason: unknown) {
        setPendingPrompt(null);
        clearPendingChatPrompt(sessionId);
        setDraft(prompt);
        setActionError(errorOf(reason));
      } finally {
        setSubmitting(false);
      }
    },
    [adoptRun, canOperate, clearDraft, sessionId, setDraft, submitting],
  );

  // Chat-first handoff: the hero composer created this session and stashed
  // the first message. Submit it exactly once — the stash is consumed
  // atomically, so a refresh can never resubmit it.
  useEffect(() => {
    if (
      sessionId === null ||
      !canOperate ||
      !eventsReady ||
      !runsLoaded ||
      runLive ||
      pendingPrompt !== null
    ) {
      return undefined;
    }
    const firstPrompt = takeFirstPrompt(sessionId);
    if (firstPrompt === null) return undefined;
    // If a prior attempt already made it into the canonical stream, consume
    // the handoff without issuing a duplicate run.
    if (events.some((event) => userMessageTextOf(event) === firstPrompt)) {
      return undefined;
    }
    void submitPrompt(firstPrompt);
    return undefined;
  }, [canOperate, events, eventsReady, pendingPrompt, runLive, runsLoaded, sessionId, submitPrompt]);

  const cancelActiveRun = useCallback(async (): Promise<void> => {
    const runId = runIdRef.current;
    if (runId === null || !canOperate || cancelling) return;
    setCancelling(true);
    setActionError(null);
    try {
      const receipt = await cancelRun(runId);
      if (receipt.accepted || receipt.stateLabel === "cancel-requested") {
        setCancelRequested(true);
      }
      const fresh = await getRun(runId);
      adoptRun(fresh);
      if (!isActiveState(fresh.state) && fresh.state !== "unknown") {
        observeSettledRun();
      }
    } catch (reason: unknown) {
      setActionError(errorOf(reason));
    } finally {
      setCancelling(false);
    }
  }, [adoptRun, canOperate, cancelling, observeSettledRun]);

  // Reconciliation: the canonical log is the source of truth for the user's
  // own message. A newer `user/message` event with the same text supersedes
  // the optimistic row; the sequence guard prevents an older identical
  // prompt from retiring a later intent.
  useEffect(() => {
    if (pendingPrompt === null || sessionId === null) return;
    const matched = events.some((event) => pendingMatchesEvent(pendingPrompt, event));
    if (!matched) return;
    setPendingPrompt(null);
    clearPendingChatPrompt(sessionId);
  }, [events, pendingPrompt, sessionId]);

  // A pending overlay restored after a refresh belongs only to a live run.
  // Once the initial events and run listings are both authoritative, discard
  // a stale overlay rather than presenting it as a new message or resending it.
  useEffect(() => {
    if (pendingPrompt === null || !eventsReady || !runsLoaded || submitting) return;
    const belongsToActiveRun =
      run !== null && run.id === pendingPrompt.runId && isActiveState(run.state);
    if (belongsToActiveRun) return;
    setPendingPrompt(null);
    if (sessionId !== null) clearPendingChatPrompt(sessionId);
  }, [eventsReady, pendingPrompt, run, runsLoaded, sessionId, submitting]);

  // --- render ------------------------------------------------------------------

  if (sessionId === null) {
    return (
      <StateView
        state="error"
        title="Invalid session link"
        detail="This URL does not identify a session."
      />
    );
  }

  if (sessionResource.error !== null) {
    return (
      <StateView
        state="error"
        title="Session unavailable"
        detail={sessionResource.error.message}
        onRetry={reloadSession}
      />
    );
  }

  if (session === null) {
    return <StateView state="loading" title="Loading session…" />;
  }

  const created = formatWhen(session.createdAt);
  const detailParts: string[] = [
    `agent ${session.agentId.slice(0, 8)}`,
    `workspace ${session.workspaceId.slice(0, 8)}`,
  ];
  if (created !== null) detailParts.push(`created ${created}`);

  const composerDisabled =
    !canOperate || submitting || (serverRunning && !runActive);
  const composerLockNote =
    !canOperate || (!runActive && !serverRunning)
      ? null
      : submitting
        ? "Starting the run…"
        : cancelRequested
          ? "Stopping the run…"
          : serverRunning && !runActive
            ? "A run is active on this session. Prompting stays unavailable until it settles."
            : null;

  const pendingMessages: PendingUserMessage[] =
    pendingPrompt === null
      ? []
      : [
          {
            key: pendingPrompt.intentId,
            text: pendingPrompt.prompt,
            pending: runLive,
          },
        ];

  const runWhenParts: string[] = [];
  // Real durable timestamps: accepted, dispatched, then terminal or
  // cancelled as the end of the attempt.
  const acceptedAt = formatWhen(run?.acceptedAt ?? null);
  const dispatchedAt = formatWhen(run?.dispatchedAt ?? null);
  const endedAt = formatWhen(run?.terminalAt ?? run?.cancelledAt ?? null);
  if (dispatchedAt !== null) {
    runWhenParts.push(`dispatched ${dispatchedAt}`);
    if (endedAt !== null) runWhenParts.push(`ended ${endedAt}`);
  } else if (acceptedAt !== null) {
    runWhenParts.push(`accepted ${acceptedAt}`);
  }

  return (
    <div className="stack">
      <PageHeader
        title={session.title ?? "Untitled session"}
        subtitle={detailParts.join(" · ")}
        actions={
          <>
            {!canOperate ? <Badge tone="warn">viewer</Badge> : null}
            {serverRunning || runLive ? (
              <Badge tone="accent">running</Badge>
            ) : (
              <Badge tone="neutral">idle</Badge>
            )}
            <Link className="btn" to={appHref("/sessions", projectId)}>
              All sessions
            </Link>
          </>
        }
      />

      <Card
        title="Activity"
        actions={<span className="mono muted">at rev {headRevision}</span>}
      >
        {eventsError !== null ? (
          <StateView
            state="error"
            title="Could not load events"
            detail={eventsError.message}
            onRetry={retryEvents}
          />
        ) : eventsLoading ? (
          <StateView state="loading" title="Loading events…" />
        ) : (
          <AgentSurface
            events={events}
            pending={pendingMessages}
            running={serverRunning || runLive}
            cancelRequested={cancelRequested}
          />
        )}
      </Card>

      <Card
        title="Prompt"
        actions={
          run !== null ? (
            <span className="row">
              <Badge
                tone={
                  cancelRequested
                    ? "warn"
                    : run.state === "terminal"
                      ? "ok"
                      : run.state === "unknown"
                        ? "warn"
                        : run.state === "dispatched"
                          ? "accent"
                          : "neutral"
                }
              >
                {cancelRequested ? "run cancel-requested" : `run ${run.state}`}
              </Badge>
              {runWhenParts.length > 0 ? (
                <span className="mono muted">{runWhenParts.join(" · ")}</span>
              ) : null}
            </span>
          ) : serverRunning ? (
            <Badge tone="warn">run active</Badge>
          ) : (
            <span className="muted">idle</span>
          )
        }
      >
        <div className="stack">
          {actionError !== null ? (
            <div className="row" role="alert">
              <Badge tone="fail">error</Badge>
              <span>{actionError.message}</span>
            </div>
          ) : null}
          {run !== null && run.result !== null && run.result.trim() !== "" ? (
            <div
              className={
                run.state === "unknown" || run.state === "cancelled"
                  ? "card card-unknown"
                  : run.state === "terminal"
                    ? "card card-terminal"
                    : "card"
              }
            >
              <div className="card-head">
                <span className="card-title">Run result · {run.state}</span>
                <Badge
                  tone={
                    run.state === "unknown" ? "warn" : run.state === "cancelled" ? "warn" : "ok"
                  }
                >
                  {run.state}
                </Badge>
              </div>
              <div className="card-body">
                <pre className="bash-output mono" style={{ borderTop: "none", maxHeight: "240px" }}>
                  {run.result}
                </pre>
              </div>
            </div>
          ) : null}
          <Composer
            value={draft}
            onValueChange={setDraft}
            onSubmit={(prompt) => void submitPrompt(prompt)}
            disabled={composerDisabled}
            submitting={submitting}
            lockNote={composerLockNote}
            running={runLive}
            cancelling={cancelling}
            onCancel={() => void cancelActiveRun()}
          />
          {serverRunning && !runActive && canOperate ? (
            <p className="muted">
              A run is active on this session. Prompting stays unavailable until it settles.
            </p>
          ) : null}
        </div>
      </Card>
    </div>
  );
}
