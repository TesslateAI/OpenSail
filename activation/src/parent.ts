import { Socket } from "node:net";

const PARENT_FD = 3;
const MAX_FRAME_BYTES = 1_048_576;
/** Per-event decoded bound; the parent refuses larger events at framing time. */
const MAX_HISTORY_EVENT_BYTES = 64 * 1024;
/** Total reassembled history bound; the parent enforces the same budget. */
const MAX_HISTORY_TOTAL_BYTES = 4 * 1024 * 1024;
/** Canonical base64 alphabet with padding; anything else is refused. */
const BASE64_PATTERN = /^[A-Za-z0-9+/]*={0,2}$/;

/** Kernel-observed child boundary facts reported inside `hello`. */
export interface Attestation {
  fds: number[];
  env_keys: string[];
}

/**
 * Tracks model-issued tool-call ids so each bash intent carries its stable
 * call identity; the parent keys no-replay decisions on it.
 */
export interface CallTracker {
  register(id: string): void;
  /** Returns the sole outstanding call id, if any, and consumes it. */
  take(): string | undefined;
}

export type ActivationMode = "create" | "resume";

export interface Bootstrap {
  mode: ActivationMode;
  session_id: string;
  prompt: string;
}

/**
 * Source of serialized session events accumulated since the previous flush.
 * The bridge persists these actual bytes before any effect runs.
 */
export interface EventSource {
  /** Serialized session events accumulated since the previous advance. */
  collect(): string;
  /** Marks the last collected bytes as durably appended by the parent. */
  advance(): void;
}
export interface WireMessage {
  role: "user" | "assistant" | "tool";
  /** Visible text of the turn; tool results concatenate their text blocks. */
  text: string;
  /** Assistant-issued tool invocations, carried as typed data. */
  tool_calls?: Array<{ id: string; name: string; arguments: string }>;
  /** Tool outcomes returned to the model, carried as typed data. */
  tool_results?: Array<{ call_id: string; text: string; is_error: boolean }>;
}

export interface WireTool {
  name: string;
}

export type ModelReply =
  | { kind: "text"; text: string }
  | { kind: "tool_call"; call_id: string; name: string; arguments: Record<string, unknown> };

/** Outcome union authored by the parent; unknown stays explicitly unknown. */
export type BashOutcome =
  | { completed: { exit_code: number } }
  | { timed_out: true }
  | { aborted: true }
  | { unknown: { reason: string } };

export interface BashReply {
  call_id: string;
  outcome: BashOutcome;
  stdout: string;
  stderr: string;
  timeout_ms: number;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

interface IncomingFrame {
  id?: unknown;
  op?: unknown;
  ok?: unknown;
  error?: unknown;
  index?: unknown;
  total?: unknown;
  done?: unknown;
  items?: unknown;
}

function naturalNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

/**
 * Decode one strictly canonical base64 payload. Lenient decoders silently
 * accept whitespace and mixed alphabets; a boundary-carrying channel must not,
 * so the round trip has to reproduce the input byte for byte.
 */
function decodeCanonicalBase64(value: string): Uint8Array {
  if (value.length % 4 !== 0 || !BASE64_PATTERN.test(value)) {
    throw new Error("history event bytes are not canonical base64");
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.toString("base64") !== value) {
    throw new Error("history event bytes are not canonical base64");
  }
  return new Uint8Array(decoded);
}

/** Bounded newline-delimited JSON over the inherited parent socket. */
export class ParentLink {
  private readonly pending = new Map<string, Pending>();
  /** Op each outstanding child request was issued under, for reply routing. */
  private readonly pendingOps = new Map<string, string>();
  /** Set synchronously when the hello reply is matched, before any await. */
  private helloServed = false;
  private readonly historyPending: Array<PromiseWithResolvers<ReadonlyArray<Uint8Array>>> = [];
  private historyEvents: Uint8Array[] = [];
  private historyBytesTotal = 0;
  private historyNextIndex = 0;
  private historyTotal: number | undefined;
  private historyComplete = false;
  private nextId = 1;
  private buffer = "";
  private closed = false;

  private constructor(private readonly socket: Socket) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      this.buffer += chunk;
      for (;;) {
        const nl = this.buffer.indexOf("\n");
        if (nl < 0) {
          if (this.buffer.length > MAX_FRAME_BYTES) {
            this.fail(new Error("parent frame exceeded bound"));
          }
          return;
        }
        const line = this.buffer.slice(0, nl);
        this.buffer = this.buffer.slice(nl + 1);
        if (line.length > MAX_FRAME_BYTES) {
          this.fail(new Error("parent frame exceeded bound"));
          return;
        }
        if (line.length === 0) continue;
        this.onLine(line);
      }
    });
    socket.on("error", (error: Error) => {
      this.fail(error);
    });
    socket.on("close", () => {
      this.fail(new Error("parent connection closed"));
    });
  }

  static open(): ParentLink {
    const socket = new Socket({ fd: PARENT_FD });
    return new ParentLink(socket);
  }

  private fail(error: Error): void {
    if (this.closed) return;
    this.closed = true;
    for (const waiter of this.pending.values()) waiter.reject(error);
    this.pending.clear();
    for (const waiter of this.historyPending.splice(0)) waiter.reject(error);
    // A failed bridge must not keep the process alive waiting on a socket the
    // parent no longer serves; destroy so exits surface promptly.
    this.socket.destroy();
  }

  private onLine(line: string): void {
    let frame: IncomingFrame;
    try {
      frame = JSON.parse(line) as IncomingFrame;
    } catch {
      this.fail(new Error("parent sent invalid json"));
      return;
    }
    if (typeof frame.id !== "string") return;
    if (typeof frame.op === "string") {
      // Parent-initiated request. Only bounded history streaming between the
      // hello reply and the first child effect exists in the protocol; every
      // other operation, or any history after completion, fails the bridge.
      if (frame.op !== "history" || this.historyComplete) {
        this.fail(
          new Error(frame.op === "history" ? "parent sent late history" : "parent sent an unexpected operation"),
        );
        return;
      }
      try {
        this.acceptHistoryFrame(frame);
      } catch (error) {
        this.fail(error instanceof Error ? error : new Error(String(error)));
      }
      return;
    }
    const waiter = this.pending.get(frame.id);
    if (waiter === undefined) return;
    this.pending.delete(frame.id);
    if (this.pendingOps.get(frame.id) === "hello") {
      this.helloServed = true;
    }
    this.pendingOps.delete(frame.id);
    if (frame.ok === true) {
      waiter.resolve(frame);
      return;
    }
    const message = typeof frame.error === "string" ? frame.error : "parent request failed";
    waiter.reject(new Error(message));
  }

  /** Validate and absorb one `op:"history"` frame; throws on any violation. */
  private acceptHistoryFrame(frame: IncomingFrame): void {
    if (!this.helloServed) {
      throw new Error("history arrived before the hello reply");
    }
    const index = frame.index;
    if (!naturalNumber(index)) {
      throw new Error("history chunk index is not a natural number");
    }
    const total = frame.total;
    if (!naturalNumber(total)) {
      throw new Error("history chunk total is not a natural number");
    }
    if (frame.id !== `history-${index}`) {
      throw new Error("history chunk identity does not match its index");
    }
    if (this.historyTotal === undefined) {
      this.historyTotal = total;
    } else if (this.historyTotal !== total) {
      throw new Error("history chunk total changed mid-stream");
    }
    if (index !== this.historyNextIndex) {
      throw new Error("history chunk arrived out of order");
    }
    if (total > 0 && index >= total) {
      throw new Error("history chunks exceeded the declared total");
    }
    const items = frame.items;
    if (!Array.isArray(items)) {
      throw new Error("history chunk lacks its event items");
    }
    for (const item of items) {
      if (typeof item !== "object" || item === null) {
        throw new Error("history event entry is not an object");
      }
      if (!("bytes" in item) || typeof item.bytes !== "string") {
        throw new Error("history event entry lacks canonical base64 bytes");
      }
      const decoded = decodeCanonicalBase64(item.bytes);
      if (decoded.byteLength > MAX_HISTORY_EVENT_BYTES) {
        throw new Error("history event exceeded the per-event bound");
      }
      this.historyBytesTotal += decoded.byteLength;
      if (this.historyBytesTotal > MAX_HISTORY_TOTAL_BYTES) {
        throw new Error("history exceeded the activation bound");
      }
      this.historyEvents.push(decoded);
    }
    const done = frame.done;
    if (done !== undefined && typeof done !== "boolean") {
      throw new Error("history done marker is not a boolean");
    }
    if (done === true) {
      // The terminal frame is index total-1, or the lone empty marker when the
      // durable session had nothing to stream.
      if (index !== Math.max(total - 1, 0)) {
        throw new Error("history completion arrived early");
      }
      this.historyComplete = true;
      const snapshot = [...this.historyEvents];
      for (const waiter of this.historyPending.splice(0)) waiter.resolve(snapshot);
      return;
    }
    this.historyNextIndex += 1;
  }

  /**
   * Await the complete canonical history the parent streams right after the
   * hello reply. Frames absorbed earlier are included; late frames fail the
   * connection regardless of whether anyone awaited them.
   */
  receiveHistory(): Promise<ReadonlyArray<Uint8Array>> {
    if (this.closed) return Promise.reject(new Error("parent connection closed"));
    if (this.historyComplete) return Promise.resolve([...this.historyEvents]);
    const waiter = Promise.withResolvers<ReadonlyArray<Uint8Array>>();
    this.historyPending.push(waiter);
    return waiter.promise;
  }

  private call(op: string, extra: Record<string, unknown>): Promise<Record<string, unknown>> {
    if (this.closed) return Promise.reject(new Error("parent connection closed"));
    const id = String(this.nextId++);
    const frame = { id, op, ...extra };
    const encoded = `${JSON.stringify(frame)}\n`;
    if (encoded.length > MAX_FRAME_BYTES) {
      return Promise.reject(new Error("child frame exceeded bound"));
    }
    return new Promise((resolve, reject) => {
      this.pendingOps.set(id, op);
      this.pending.set(id, {
        resolve: (value) => resolve(value as Record<string, unknown>),
        reject,
      });
      this.socket.write(encoded, (error) => {
        if (error) {
          this.pending.delete(id);
          this.pendingOps.delete(id);
          reject(error);
        }
      });
    });
  }

  async hello(attestation: Attestation): Promise<Bootstrap> {
    const reply = await this.call("hello", { attest: attestation });
    const bootstrap = reply.bootstrap;
    if (bootstrap === null || typeof bootstrap !== "object") {
      throw new Error("parent hello lacked bootstrap");
    }
    const body = bootstrap as Record<string, unknown>;
    if (body.mode !== "create" && body.mode !== "resume") {
      throw new Error("parent bootstrap mode is not create or resume");
    }
    if (typeof body.session_id !== "string" || body.session_id.length === 0) {
      throw new Error("parent bootstrap lacked session_id");
    }
    if (typeof body.prompt !== "string") {
      throw new Error("parent bootstrap lacked prompt");
    }
    return {
      mode: body.mode,
      session_id: body.session_id,
      prompt: body.prompt,
    };
  }

  async model(params: {
    system: string | undefined;
    tools: WireTool[];
    messages: WireMessage[];
    events: string;
  }): Promise<ModelReply> {
    const reply = await this.call("model", params);
    const model = reply.model;
    if (model === null || typeof model !== "object") {
      throw new Error("parent model reply missing");
    }
    const body = model as Record<string, unknown>;
    if (body.kind === "text") {
      if (typeof body.text !== "string") throw new Error("parent text reply missing");
      return { kind: "text", text: body.text };
    }
    if (body.kind === "tool_call") {
      if (
        typeof body.call_id !== "string" ||
        typeof body.name !== "string" ||
        body.arguments === null ||
        typeof body.arguments !== "object"
      ) {
        throw new Error("parent tool-call reply missing fields");
      }
      return {
        kind: "tool_call",
        call_id: body.call_id,
        name: body.name,
        arguments: body.arguments as Record<string, unknown>,
      };
    }
    throw new Error("parent model reply kind is unsupported");
  }

  async bash(params: {
    call_id: string;
    command: string;
    description: string;
    workdir: string | undefined;
    timeout_ms: number | undefined;
    events: string;
  }): Promise<BashReply> {
    const reply = await this.call("bash", params);
    const bash = reply.bash;
    if (bash === null || typeof bash !== "object") {
      throw new Error("parent bash reply missing");
    }
    const body = bash as Record<string, unknown>;
    const outcome = body.outcome as Record<string, unknown> | undefined;
    if (outcome === null || typeof outcome !== "object" || Object.keys(outcome).length !== 1) {
      throw new Error("parent bash reply missing outcome");
    }
    const [kind] = Object.keys(outcome);
    let parsed: BashOutcome;
    if (kind === "completed") {
      const known = outcome.completed as Record<string, unknown>;
      if (typeof known?.exit_code !== "number") throw new Error("parent bash completion missing exit_code");
      parsed = { completed: { exit_code: known.exit_code } };
    } else if (kind === "timed_out") {
      parsed = { timed_out: true };
    } else if (kind === "aborted") {
      parsed = { aborted: true };
    } else if (kind === "unknown") {
      const detail = outcome.unknown as Record<string, unknown>;
      if (typeof detail?.reason !== "string") throw new Error("parent unknown outcome missing reason");
      parsed = { unknown: { reason: detail.reason } };
    } else {
      throw new Error("parent bash outcome kind is unsupported");
    }
    return {
      call_id: params.call_id,
      outcome: parsed,
      stdout: typeof body.stdout === "string" ? body.stdout : "",
      stderr: typeof body.stderr === "string" ? body.stderr : "",
      timeout_ms: typeof body.timeout_ms === "number" ? body.timeout_ms : 60_000,
    };
  }

  async finish(text: string, events: string): Promise<void> {
    await this.call("finish", { text, events });
    this.closed = true;
    this.socket.end();
  }
}
