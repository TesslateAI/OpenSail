/**
 * Canonical conversation surface: projects raw session events and renders them.
 *
 * Projection is exclusively `projectEvents` + `pairSurfaceItems`; unknown
 * vocabularies vanish there and are never fabricated into rows or cards.
 * Final answers are assistant blocks; tool calls/results are Bash cards with
 * ok/failure/running/unknown outcomes. While a run is live, in-flight tools
 * and the active turn are visibly distinct from settled unknown outcomes.
 * Optimistic user bubbles are keyed by the submitted intent and disappear
 * when Session reconciliation sees the canonical user/message event.
 */

import { useMemo } from "react";
import type { RawEventDto } from "../api/dto.ts";
import { pairSurfaceItems, projectEvents } from "../events/project.ts";
import { Badge, StateView } from "./primitives.tsx";
import { BashCard } from "./BashCards.tsx";

export type PendingUserMessage = {
  /** Existing run/intent identity used as the React key. */
  key: string;
  text: string;
  /** True while the canonical user/message is still pending. */
  pending?: boolean | undefined;
};

export type AgentSurfaceProps = {
  events: readonly RawEventDto[];
  /** Optimistic user bubbles awaiting canonical reconciliation. */
  pending?: readonly PendingUserMessage[] | undefined;
  /** True while the current session turn is live. */
  running?: boolean | undefined;
  /** True after the user has requested cancellation but before settlement. */
  cancelRequested?: boolean | undefined;
};

function TurnDivider({
  index,
  turnId,
  running,
}: {
  index: number;
  turnId: string | null;
  running: boolean;
}) {
  return (
    <div
      className={running ? "turn-divider turn-divider-running mono muted" : "turn-divider mono muted"}
      role="separator"
      aria-label={`turn ${index}${running ? " · running" : ""}`}
    >
      turn {index}
      {turnId !== null ? ` · ${turnId}` : ""}
      {running ? <Badge tone="accent">running</Badge> : null}
    </div>
  );
}

function MessageRow({
  kind,
  text,
  isFinal,
  streaming,
  pending,
}: {
  kind: "user" | "assistant";
  text: string;
  isFinal?: boolean | undefined;
  streaming?: boolean | undefined;
  pending?: boolean | undefined;
}) {
  const className =
    kind === "user"
      ? pending
        ? "message message-user message-optimistic"
        : "message message-user"
      : streaming
        ? "message message-assistant message-streaming"
        : isFinal
          ? "message message-assistant message-final"
          : "message message-assistant";
  return (
    <article className={className}>
      <span className="row">
        <Badge tone={kind === "user" ? "accent" : "neutral"}>{kind}</Badge>
        {pending ? <Badge tone="accent">pending</Badge> : null}
        {streaming ? <Badge tone="accent">streaming</Badge> : null}
        {isFinal && kind === "assistant" ? <Badge tone="ok">final answer</Badge> : null}
      </span>
      <div className="message-text">
        {text.length > 0 ? text : <span className="muted">(empty message)</span>}
      </div>
    </article>
  );
}

export function AgentSurface({
  events,
  pending = [],
  running = false,
  cancelRequested = false,
}: AgentSurfaceProps) {
  const items = useMemo(() => pairSurfaceItems(projectEvents(events), running), [events, running]);
  const projectedEvents = useMemo(() => projectEvents(events), [events]);
  const unknownCount = events.length - projectedEvents.length;

  if (items.length === 0 && pending.length === 0) {
    if (running) {
      return (
        <div className="agent-surface stack">
          <div className="row muted">
            <Badge tone={cancelRequested ? "warn" : "accent"}>
              {cancelRequested ? "cancel requested" : "run live"}
            </Badge>
            <span className="mono">Waiting for the active turn to emit canonical activity.</span>
          </div>
          <StateView state="loading" title="Waiting for live activity" />
        </div>
      );
    }
    if (events.length > 0 && unknownCount > 0) {
      return (
        <div className="stack">
          <StateView
            state="empty"
            title="No projected activity"
            detail={`${events.length} raw event(s) matched no console vocabulary and were omitted. Raw unknown events do not become UI.`}
          />
          <p className="muted">
            Showing 0 of {events.length} raw events — {unknownCount} unknown vocabulary event(s) omitted.
          </p>
        </div>
      );
    }
    return (
      <StateView
        state="empty"
        title="No console activity"
        detail="Projected session events will appear here."
      />
    );
  }

  let lastAssistantIndex: number | null = null;
  let lastTurnStartIndex: number | null = null;
  let lastUserIndex: number | null = null;
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i];
    if (item === undefined) continue;
    if (lastAssistantIndex === null && item.kind === "assistant") lastAssistantIndex = i;
    if (lastTurnStartIndex === null && item.kind === "turn-start") lastTurnStartIndex = i;
    if (lastUserIndex === null && item.kind === "user") lastUserIndex = i;
    if (lastAssistantIndex !== null && lastTurnStartIndex !== null && lastUserIndex !== null) break;
  }
  const liveBoundary = Math.max(lastTurnStartIndex ?? -1, lastUserIndex ?? -1);
  const streamingAssistantIndex =
    running && lastAssistantIndex !== null && lastAssistantIndex > liveBoundary
      ? lastAssistantIndex
      : null;

  return (
    <div className="agent-surface stack">
      {running ? (
        <div className="row muted" role="status" aria-live="polite">
          <Badge tone={cancelRequested ? "warn" : "accent"}>
            {cancelRequested ? "cancel requested" : "run live"}
          </Badge>
          <span className="mono">
            {cancelRequested
              ? "The active turn is stopping; waiting for durable settlement."
              : "In-flight tools and assistant text are still streaming."}
          </span>
        </div>
      ) : null}
      {unknownCount > 0 ? (
        <div className="row muted">
          <Badge tone="warn">omitted {unknownCount} unknown event(s)</Badge>
          <span className="mono">Unknown vocabulary is never rendered.</span>
        </div>
      ) : null}
      {items.map((item, index) => {
        switch (item.kind) {
          case "turn-start":
            return (
              <TurnDivider
                key={`turn-${index}`}
                index={item.index}
                turnId={item.turnId}
                running={running && index === lastTurnStartIndex}
              />
            );
          case "user":
            return <MessageRow key={`message-${index}`} kind="user" text={item.text} />;
          case "assistant":
            return (
              <MessageRow
                key={`message-${index}`}
                kind="assistant"
                text={item.text}
                isFinal={index === lastAssistantIndex && !running}
                streaming={index === streamingAssistantIndex}
              />
            );
          case "bash":
            return (
              <BashCard
                key={`bash-${index}`}
                call={item.call}
                result={item.result}
                status={item.status}
              />
            );
        }
      })}
      {pending.map((message) => (
        <MessageRow
          key={message.key}
          kind="user"
          text={message.text}
          pending={message.pending !== false}
        />
      ))}
      {running && lastTurnStartIndex === null ? (
        <div className="turn-status row muted" role="status" aria-live="polite">
          <Badge tone={cancelRequested ? "warn" : "accent"}>
            {cancelRequested ? "cancel requested" : "turn in progress"}
          </Badge>
        </div>
      ) : null}
    </div>
  );
}
