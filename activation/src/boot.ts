import { Context } from "@deepseek-ai/cordis";
import AgentRegistry from "@deepseek-ai/dsh-agent";
import AgentLoop from "@deepseek-ai/dsh-agent-loop";
import LlmRuntime, { createUserMessage } from "@deepseek-ai/dsh-llm";
import SessionStore, {
  KNOWN_SESSION_EVENT_TYPES,
  SESSION_FORMAT_VERSION,
  SessionId,
} from "@deepseek-ai/dsh-session";
import type { SessionEvent } from "@deepseek-ai/dsh-session";
import * as checkpointPolicy from "@deepseek-ai/dsh-session-checkpoint-policy";
import * as shellEnv from "@deepseek-ai/dsh-shell-env";
import SystemPrompt from "@deepseek-ai/dsh-system-prompt";
import * as toolBash from "@deepseek-ai/dsh-tool-bash";
import ToolRuntime from "@deepseek-ai/dsh-tools";
import type { Bootstrap, CallTracker, EventSource, ParentLink } from "./parent.js";
import { MemorySessionPersistence } from "./persist.js";
import { PARENT_MODEL, PARENT_PROVIDER, ParentBashExecutor, ParentLlmAdapter } from "./plugins.js";

function finalAssistantText(events: ReadonlyArray<{ type: string; data: unknown }>): string {
  const texts: string[] = [];
  for (const event of events) {
    if (event.type !== "assistant/message") continue;
    const data = event.data as { message?: { content?: Array<{ type?: string; text?: string }> } };
    for (const block of data.message?.content ?? []) {
      if (block.type === "text" && typeof block.text === "string") texts.push(block.text);
    }
  }
  return texts.at(-1) ?? "";
}

/**
 * Split the parent's canonical history into the session log it encodes.
 * Each received chunk is one persisted append batch: newline-joined
 * serialized events, exactly the bytes checkpoint-before-effect flushed.
 * Lines are parsed individually and kept in durable order; structural fields
 * are checked here, and an unrecognized required event type is refused
 * exactly like dsh-session would refuse it on load, so a transcript this
 * runtime cannot faithfully replay never reaches the agent loop.
 */
export function parseHistoryEvents(history: ReadonlyArray<Uint8Array>): SessionEvent[] {
  const decoder = new TextDecoder();
  const events: SessionEvent[] = [];
  let previousSeq = -1;
  for (const [batchPosition, raw] of history.entries()) {
    // Each received chunk is one persisted append batch: newline-joined
    // serialized events, exactly the bytes checkpoint-before-effect flushed.
    const lines = decoder.decode(raw).split("\n");
    for (const [linePosition, line] of lines.entries()) {
      if (line.length === 0) continue;
      const label = `history event ${batchPosition}:${linePosition}`;
      let parsed: unknown;
      try {
        parsed = JSON.parse(line);
      } catch {
        throw new Error(`${label} is not valid json`);
      }
      if (typeof parsed !== "object" || parsed === null) {
        throw new Error(`${label} is not an object`);
      }
      if (!("type" in parsed) || typeof parsed.type !== "string") {
        throw new Error(`${label} lacks a string type`);
      }
      const eventType = parsed.type;
      if (!("seq" in parsed)) {
        throw new Error(`${label} lacks a seq`);
      }
      const seq = parsed.seq;
      if (typeof seq !== "number" || !Number.isInteger(seq) || seq < 0) {
        throw new Error(`${label} seq is not a natural number`);
      }
      if (seq <= previousSeq) {
        throw new Error(`${label} seq does not advance the log`);
      }
      if (!("time" in parsed) || typeof parsed.time !== "number" || !Number.isInteger(parsed.time)) {
        throw new Error(`${label} time is not an epoch milliseconds integer`);
      }
      if (!("data" in parsed) || typeof parsed.data !== "object" || parsed.data === null) {
        throw new Error(`${label} lacks an event data object`);
      }
      if (!KNOWN_SESSION_EVENT_TYPES.has(eventType) && (!("ignorable" in parsed) || parsed.ignorable !== true)) {
        throw new Error(`${label} carries an unrecognized required event type`);
      }
      previousSeq = seq;
      // Structural envelope validated above; the per-type data payload stays
      // opaque to the bridge and is interpreted by dsh-session on load.
      events.push(parsed as SessionEvent);
    }
  }
  return events;
}

export async function runActivation(parent: ParentLink, bootstrap: Bootstrap): Promise<void> {
  const ctx = new Context();
  await ctx.plugin(LlmRuntime);
  await ctx.plugin(SessionStore);
  await ctx.plugin(SystemPrompt, {
    persona: "You are a voie-cloud activation. Use bash when a command is required.",
    includeHarnessIdentity: false,
    includeRuntimeContext: false,
  });
  await ctx.plugin(ToolRuntime);
  await ctx.plugin(AgentRegistry);
  await ctx.plugin(MemorySessionPersistence);
  await ctx.plugin(AgentLoop, { agents: [], maxParallelToolCalls: 1 });
  await ctx.plugin(shellEnv, { dshHome: "/tmp" });
  // The collector starts inert so a pre-agent effect cannot flush anything;
  // it binds to the live session the moment the agent exists. The tracker
  // holds at most one outstanding model call (maxParallelToolCalls is 1).
  let outstandingCall: string | undefined;
  const events: EventSource & { advance(): void } = { collect: () => "", advance: () => {} };
  const calls: CallTracker = {
    register: (id: string) => {
      outstandingCall = id;
    },
    take: () => {
      const id = outstandingCall;
      outstandingCall = undefined;
      return id;
    },
  };
  await ctx.plugin(ParentBashExecutor, { parent, events, calls });
  await ctx.plugin(toolBash, { enableRunInBackground: false });
  await ctx.plugin(checkpointPolicy);
  ctx.llm.registerAdapter([PARENT_PROVIDER], new ParentLlmAdapter({ parent, events, calls }));

  const sessionId = SessionId(bootstrap.session_id);

  const persistence = ctx.sessionPersistence;
  if (!(persistence instanceof MemorySessionPersistence)) {
    throw new Error("memory session persistence is not installed");
  }
  const history = parseHistoryEvents(await parent.receiveHistory());
  if (bootstrap.mode === "resume") {
    // The disposable child replays exactly the durable transcript the parent
    // handed over; its own log exists only inside this process.
    persistence.seedHistory(
      { version: SESSION_FORMAT_VERSION, id: sessionId, createdAt: Date.now() },
      history,
    );
  } else if (history.length > 0) {
    throw new Error("create-mode session received existing history");
  }

  const options = { provider: PARENT_PROVIDER, model: PARENT_MODEL };
  const agent = bootstrap.mode === "resume"
    ? (await ctx.agentLoop.resume(ctx, { resumeSessionId: sessionId, agentOptions: options })).agent
    : ctx.agentLoop.create(sessionId, options);

  let flushed = 0;
  const pendingEvents = (): string =>
    agent.session.events.slice(flushed).map((event) => JSON.stringify(event)).join("\n");
  events.collect = pendingEvents;
  events.advance = () => {
    flushed = agent.session.events.length;
  };

  agent.followup(createUserMessage({
    content: [{ type: "text", text: bootstrap.prompt }],
    source: { kind: "user" },
  }));
  await agent.whenIdle();
  const text = finalAssistantText(agent.session.events);
  // Final flush carries every remaining byte; the parent appends them before
  // acknowledging the finished response.
  await parent.finish(text, pendingEvents());
  await ctx.fiber.dispose();
}
