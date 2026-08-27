/**
 * Canonical event projection: raw persisted session events -> console blocks.
 *
 * The source vocabulary is the pinned session log (`user/message`,
 * `assistant/message` with nested `text`/`tool-call` content, `tool/call`,
 * `tool/result`, plus `turn/start`/`turn/end` markers). The parsing rules
 * mirror the backend's `project_event` byte-for-byte; anything outside the
 * vocabulary projects nothing. Unknown events are never fabricated into UI.
 */

import type { RawEventDto } from "../api/dto.ts";
import { asStr, isRecord, recordAt, strAt } from "../api/validate.ts";

// --- projected blocks --------------------------------------------------------

export type BashCallBlock = {
  kind: "bash-call";
  callId: string;
  command: string;
  cwd: string;
};

export type BashResultBlock = {
  kind: "bash-result";
  callId: string;
  output: string;
  exitCode: number;
};

export type ConsoleBlock =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "turn-start"; turnId: string | null }
  | BashCallBlock
  | BashResultBlock;

// --- raw event field access (mirrors the backend projection) -----------------

/** `data.message.content[]`: the nested message carrier both messages use. */
function contentBlocks(data: unknown): unknown[] {
  const message = recordAt(data, "message");
  const content: unknown = message === null ? undefined : message.content;
  return Array.isArray(content) ? content : [];
}

function textOfBlocks(blocks: readonly unknown[]): string {
  let text = "";
  for (const block of blocks) {
    const record = isRecord(block) ? block : {};
    if (strAt(record, "type") !== "text") continue;
    text += asStr(record.text) ?? "";
  }
  return text;
}

/** One Bash tool invocation card payload; arguments travel as a JSON string. */
function bashCallFrom(callIdRaw: unknown, argumentsRaw: unknown): BashCallBlock {
  const callId = typeof callIdRaw === "string" ? callIdRaw : "";
  const argumentsJson = typeof argumentsRaw === "string" ? argumentsRaw : "";
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(argumentsJson);
  } catch {
    parsed = null;
  }
  const args = isRecord(parsed) ? parsed : {};
  const commandField: unknown = args.command;
  const cwdField: unknown = args.workdir;
  return {
    kind: "bash-call",
    callId,
    command: typeof commandField === "string" ? commandField : argumentsJson,
    cwd: typeof cwdField === "string" ? cwdField : "",
  };
}

// --- one raw event -> zero or more blocks -------------------------------------

export function projectRawEvent(event: RawEventDto): ConsoleBlock[] {
  switch (event.type) {
    case "user/message":
      return contentBlocks(event.data)
        .map((block) => (isRecord(block) ? block : {}))
        .filter((record) => strAt(record, "type") === "text")
        .map((record) => ({ kind: "user" as const, text: strAt(record, "text") ?? "" }));
    case "assistant/message":
      return contentBlocks(event.data).flatMap((raw) => {
        const block = isRecord(raw) ? raw : {};
        switch (strAt(block, "type")) {
          case "text":
            return [{ kind: "assistant", text: strAt(block, "text") ?? "" }] as ConsoleBlock[];
          case "tool-call":
            return [bashCallFrom(block.id, block.arguments)] as ConsoleBlock[];
          default:
            return [] as ConsoleBlock[];
        }
      });
    case "tool/call": {
      const data = isRecord(event.data) ? event.data : {};
      return [bashCallFrom(data.callId, data.arguments)];
    }
    case "tool/result": {
      // The result rides `data.message.content[0]` as a `tool-result` block;
      // anything else projects nothing, exactly like the backend.
      const first = contentBlocks(event.data)[0];
      const result = isRecord(first) && strAt(first, "type") === "tool-result" ? first : undefined;
      if (result === undefined) return [];
      const outputBlocks: unknown[] = Array.isArray(result.content) ? result.content : [];
      return [
        {
          kind: "bash-result" as const,
          callId: strAt(result, "toolCallId") ?? "",
          output: textOfBlocks(outputBlocks),
          exitCode: result.isError === true ? 1 : 0,
        },
      ];
    }
    case "turn/start": {
      const data = isRecord(event.data) ? event.data : {};
      return [{ kind: "turn-start" as const, turnId: strAt(data, "turnId") }];
    }
    case "turn/end":
      // Recognized structural close; projects no visible block.
      return [];
    default:
      // Unknown vocabulary: omit entirely. No generic event framework.
      return [];
  }
}

export function projectEvents(events: readonly RawEventDto[]): ConsoleBlock[] {
  return events.flatMap(projectRawEvent);
}

// --- pairing pass: calls meet their results ---------------------------------

export type BashCardStatus = "ok" | "failure" | "unknown" | "running";

/**
 * One renderable surface item. A Bash card pairs one call with its result:
 * `ok` is a completed run (exit 0), `failure` a program failure (non-zero
 * exit), `running` a call whose current turn is still active (no result yet,
 * the session is live), and `unknown` an outcome the settled log does not
 * determine (no result ever recorded, or a result with no visible call).
 * Running is deliberately distinct from settled unknown: the same card
 * re-projects from `running` to `unknown` when the turn settles.
 */
export type SurfaceItem =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "turn-start"; turnId: string | null; index: number }
  | {
      kind: "bash";
      call: BashCallBlock | null;
      result: BashResultBlock | null;
      status: BashCardStatus;
    };

/**
 * Pairs projected blocks into surface items. `running` marks only unmatched
 * calls in the latest user/turn span as in-progress; older unmatched calls
 * stay settled-unknown even while a later turn is live.
 */
export function pairSurfaceItems(
  blocks: readonly ConsoleBlock[],
  running = false,
): SurfaceItem[] {
  const items: SurfaceItem[] = [];
  const openCalls = new Map<string, Extract<SurfaceItem, { kind: "bash" }>>();
  let turnIndex = 0;
  let latestTurnBoundary = -1;
  for (const [index, block] of blocks.entries()) {
    if (block.kind === "user" || block.kind === "turn-start") latestTurnBoundary = index;
  }

  for (const [blockIndex, block] of blocks.entries()) {
    switch (block.kind) {
      case "user":
        items.push({ kind: "user", text: block.text });
        break;
      case "assistant":
        items.push({ kind: "assistant", text: block.text });
        break;
      case "turn-start": {
        turnIndex += 1;
        items.push({ kind: "turn-start", turnId: block.turnId, index: turnIndex });
        break;
      }
      case "bash-call": {
        const item: Extract<SurfaceItem, { kind: "bash" }> = {
          kind: "bash",
          call: block,
          result: null,
          status: running && blockIndex > latestTurnBoundary ? "running" : "unknown",
        };
        items.push(item);
        openCalls.set(block.callId, item);
        break;
      }
      case "bash-result": {
        const status: BashCardStatus = block.exitCode === 0 ? "ok" : "failure";
        const pending = openCalls.get(block.callId);
        if (pending === undefined || pending.result !== null) {
          // No visible call owns this result; keep it as an unknown outcome.
          items.push({ kind: "bash", call: null, result: block, status: "unknown" });
          break;
        }
        pending.result = block;
        pending.status = status;
        openCalls.delete(block.callId);
        break;
      }
    }
  }
  return items;
}

/** The plain text of one canonical `user/message` event. */
export function userMessageTextOf(event: RawEventDto): string | null {
  if (event.type !== "user/message") return null;
  const text = textOfBlocks(contentBlocks(event.data)).trim();
  return text.length === 0 ? null : text;
}
