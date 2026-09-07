import { Context } from "@deepseek-ai/cordis";
import { CallId, LlmAdapter } from "@deepseek-ai/dsh-llm";
import type { GenerateOptions, StreamChunk } from "@deepseek-ai/dsh-llm";
import { ShellExecutor } from "@deepseek-ai/dsh-shell";
import type { ShellExecRequest, ShellExecSpec, ShellProcess, ShellRunResult } from "@deepseek-ai/dsh-shell";
import type { CallTracker, EventSource, ParentLink, WireMessage } from "./parent.js";

const PROVIDER = "voie-parent";
const MODEL = "voie-scripted";

export { PROVIDER as PARENT_PROVIDER, MODEL as PARENT_MODEL };

/** Model adapter that relays every request over the inherited parent connection. */
export interface BridgeDeps {
  parent: ParentLink;
  events: EventSource;
  calls: CallTracker;
}

export class ParentLlmAdapter extends LlmAdapter {
  private readonly parent: ParentLink;
  private readonly events: EventSource;
  private readonly calls: CallTracker;

  constructor(deps: BridgeDeps) {
    super();
    this.parent = deps.parent;
    this.events = deps.events;
    this.calls = deps.calls;
  }

  override resolveModel(provider: string, model: string) {
    return Promise.resolve({ provider, id: model, name: model });
  }

  override async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    options.signal?.throwIfAborted();
    const messages: WireMessage[] = [];
    for (const message of options.messages) {
      const texts: string[] = [];
      const toolCalls: NonNullable<WireMessage["tool_calls"]> = [];
      const toolResults: NonNullable<WireMessage["tool_results"]> = [];
      type Blockish = { type: string; text?: string; id?: string; name?: string; arguments?: string };
      type Resultish = { type: string; toolCallId?: string; isError?: boolean; content?: Blockish[] };
      for (const block of message.content as Array<Blockish & Resultish>) {
        if (typeof block.text === "string") texts.push(block.text);
        if (block.type === "tool-call") {
          // Resume may rehydrate arguments as an object; the parent wire
          // keeps one JSON text field.
          const rawArgs = (block as { arguments?: unknown }).arguments;
          const argumentsJson =
            typeof rawArgs === "string"
              ? (rawArgs.length > 0 ? rawArgs : "{}")
              : JSON.stringify(rawArgs ?? {});
          toolCalls.push({ id: block.id ?? "", name: block.name ?? "", arguments: argumentsJson });
        }
        if (block.type === "tool-result") {
          const inner = block.content ?? [];
          const innerText = inner.map((nested) => (typeof nested.text === "string" ? nested.text : "")).join("");
          texts.push(innerText);
          toolResults.push({
            call_id: block.toolCallId ?? "",
            text: innerText,
            is_error: block.isError === true,
          });
        }
      }
      const role = message.role === "assistant" ? "assistant" : message.role === "user" ? "user" : "tool";
      const base: WireMessage = { role, text: texts.join("") };
      messages.push(
        toolCalls.length > 0 || toolResults.length > 0
          ? {
              ...base,
              ...(toolCalls.length > 0 ? { tool_calls: toolCalls } : {}),
              ...(toolResults.length > 0 ? { tool_results: toolResults } : {}),
            }
          : base,
      );
    }
    const tools = (options.tools ?? []).map((tool) => ({ name: tool.name }));
    const reply = await this.parent.model({
      system: options.system,
      tools,
      messages,
      events: this.events.collect(),
    });
    // Bytes are durably appended; the next effect flushes only new events.
    this.events.advance();
    options.signal?.throwIfAborted();
    const usageChunk =
      reply.usage === undefined
        ? undefined
        : {
            type: "usage" as const,
            usage: { inputTokens: reply.usage.prompt_tokens, outputTokens: reply.usage.completion_tokens },
          };
    if (reply.kind === "text") {
      yield { type: "block-start", index: 0, blockType: "text" };
      yield { type: "text-delta", index: 0, text: reply.text };
      yield { type: "block-end", index: 0, block: { type: "text", text: reply.text } };
      if (usageChunk) yield usageChunk;
      yield { type: "finish", reason: { kind: "stop" } };
      return;
    }
    const callId = CallId(reply.call_id);
    // Typed tool-call data straight through: no content parsing anywhere.
    // Stable model-issued identity travels with the effect it authorizes.
    this.calls.register(reply.call_id);
    const argumentsJson = JSON.stringify(reply.arguments);
    yield { type: "block-start", index: 0, blockType: "tool-call" };
    yield { type: "tool-call-delta", index: 0, id: callId, name: reply.name, argumentsDelta: argumentsJson };
    yield {
      type: "block-end",
      index: 0,
      block: { type: "tool-call", id: callId, name: reply.name, arguments: argumentsJson },
    };
    if (usageChunk) yield usageChunk;
    yield { type: "finish", reason: { kind: "tool-calls" } };
  }
}

function collected(text: string) {
  return { text, truncated: false };
}

function rejectedBackground(spec: ShellExecSpec): ShellProcess {
  const proc: ShellProcess = {
    status: "killed",
    exitCode: null,
    signal: null,
    done: Promise.resolve(),
    readOutput() {
      return { delta: "background bash is not available on the activation bridge", lossy: false };
    },
    kill() {
      return false;
    },
  };
  void spec;
  return proc;
}

/** Bash executor that sends tool intent to the parent and never starts a local process. */
export class ParentBashExecutor extends ShellExecutor {
  private readonly parent: ParentLink;
  private readonly events: EventSource;
  private readonly calls: CallTracker;

  constructor(ctx: Context, deps: BridgeDeps) {
    super(ctx);
    this.parent = deps.parent;
    this.events = deps.events;
    this.calls = deps.calls;
  }

  resolve(request: ShellExecRequest): ShellExecSpec {
    return {
      command: request.command,
      workdir: request.workdir ?? "/tmp",
      timeoutMs: request.timeoutMs ?? 60_000,
      stdoutMaxBytes: request.stdoutMaxBytes ?? 64_000,
      ...(request.signal !== undefined ? { signal: request.signal } : {}),
      ...(request.stdin !== undefined ? { stdin: request.stdin } : {}),
      ...(request.env !== undefined ? { env: request.env } : {}),
      ...(request.dshEnv !== undefined ? { dshEnv: request.dshEnv } : {}),
      sandboxPolicy: request.sandboxPolicy,
    };
  }

  async run(spec: ShellExecSpec): Promise<ShellRunResult> {
    spec.signal?.throwIfAborted();
    const call_id = this.calls.take();
    if (call_id === undefined) {
      return {
        exitCode: null,
        signal: null,
        timedOut: false,
        aborted: false,
        timeoutMs: spec.timeoutMs,
        stdout: collected(""),
        stderr: collected("bash intent has no outstanding model call id\n"),
      };
    }
    const reply = await this.parent.bash({
      call_id,
      command: spec.command,
      description: "Relay bash intent to voie-cloud",
      workdir: spec.workdir,
      timeout_ms: spec.timeoutMs,
      events: this.events.collect(),
    });
    this.events.advance();
    // Outcome uncertainty stays explicit end to end.
    const exitCode = "completed" in reply.outcome ? reply.outcome.completed.exit_code : null;
    const stderrText = "unknown" in reply.outcome
      ? `${reply.stderr}bash outcome unknown: ${reply.outcome.unknown.reason}\n`
      : reply.stderr;
    return {
      exitCode,
      signal: null,
      timedOut: "timed_out" in reply.outcome,
      aborted: "aborted" in reply.outcome,
      timeoutMs: reply.timeout_ms,
      stdout: collected(reply.stdout),
      stderr: collected(stderrText),
    };
  }

  start(spec: ShellExecSpec): ShellProcess {
    return rejectedBackground(spec);
  }
}
