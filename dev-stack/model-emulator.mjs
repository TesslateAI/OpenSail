#!/usr/bin/env node
/**
 * Deterministic OpenAI-compatible chat-completions emulator for the bounded
 * local dev stack only.
 *
 * It speaks exactly the wire shape crates/voie-cloud ModelRelay posts and
 * parses: POST {base}/chat/completions with bearer auth, and a response of
 * choices[0].message.{content, tool_calls[].{id,type,function.{name,
 * arguments}}} where arguments travel as their JSON text. Behavior is a
 * pure function of the request history:
 *
 *   - with the bash tool offered and no tool result in the history, it
 *     returns exactly one typed Bash tool call whose command echoes the
 *     marker named in the last user prompt (default voie-dev-ok);
 *   - any other request returns one non-empty final answer that embeds the
 *     tool output when a tool result exists.
 *
 * Identical histories therefore yield identical responses across restarts;
 * there is no session table. Ephemeral state — the API key file and a
 * size-capped JSONL trace — lives only under XDG_RUNTIME_DIR via --state-
 * root. Binds to loopback only; this is a development fixture, never a
 * product component.
 */
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import {
  appendFileSync,
  mkdirSync,
  readFileSync,
  statSync,
  truncateSync,
} from "node:fs";
import { join } from "node:path";

// Loopback host, constructed here rather than copied from any environment:
const HOST = [127, 0, 0, 1].join(".");

const MAX_REQUEST_BYTES = 32 * 1024; // mirrors MAX_REQUEST_BYTES in model/mod.rs
const MAX_TOKENS = 8192; // mirrors MAX_TOKENS in model/mod.rs
const DEFAULT_MARKER = "voie-dev-ok";
// Mirrors the fixed VOIE_MODEL_NAME the dev stack exports in fixture mode;
// CloudModelRelay readiness probes GET {base}/models and fails closed on any
// non-success, so a fixture without this route can never pass readyz.
const MODEL_ID = "voie-dev-model";
const TRACE_MAX_BYTES = 1024 * 1024;

function parseArgs(argv) {
  const args = { port: 18083, stateRoot: "", apiKeyFile: "", selfTest: false };
  for (let i = 0; i < argv.length; i += 2) {
    const name = argv[i];
    const value = argv[i + 1] ?? "";
    if (name === "--port") args.port = Number(value);
    else if (name === "--state-root") args.stateRoot = value;
    else if (name === "--api-key-file") args.apiKeyFile = value;
    else if (name === "--self-test") {
      args.selfTest = true;
      i -= 1; // flag takes no value
    } else throw new Error(`unknown argument: ${name}`);
  }
  return args;
}

/** The marker word a prompt asks the remote Bash to print, if any. */
function pickMarker(prompt) {
  const match = prompt.match(
    /(?:prints?|outputs?|echo(?:s|es)?)\s+([A-Za-z0-9][A-Za-z0-9._-]{2,63})/i,
  );
  return match === null ? DEFAULT_MARKER : match[1];
}

function lastOf(messages, role) {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    if (messages[i]?.role === role) return messages[i];
  }
  return null;
}

function hasToolResult(messages) {
  return messages.some((message) => message?.role === "tool");
}

function offersBash(tools) {
  return (
    Array.isArray(tools) &&
    tools.some((tool) => tool?.function?.name === "bash")
  );
}

/** Constant-time-enough bearer check for a dev fixture. */
function authorized(headerValue, apiKey) {
  return typeof headerValue === "string" && headerValue === `Bearer ${apiKey}`;
}

function errorBody(message, type, code) {
  return { error: { message, type, code } };
}

/**
 * The deterministic completion for one already-validated request body.
 * Pure: same input bytes, same output object.
 */
function respond(body) {
  const { messages, tools, model } = body;
  const userMessage = lastOf(messages, "user");
  const prompt =
    typeof userMessage?.content === "string" ? userMessage.content : "";
  const promptTokens = Math.max(
    1,
    Math.ceil(JSON.stringify(messages).length / 4),
  );

  if (offersBash(tools) && !hasToolResult(messages)) {
    // Phase one: exactly one typed Bash tool call carrying the marker echo.
    const args = JSON.stringify({ command: `echo ${pickMarker(prompt)}` });
    const digest = createHash("sha256").update(args).digest("hex");
    const id = `call_${digest.slice(0, 24)}`;
    const completionTokens = Math.max(1, Math.ceil(args.length / 4));
    return {
      id: `chatcmpl-dev-${digest.slice(24, 32)}`,
      object: "chat.completion",
      created: 0,
      model,
      choices: [
        {
          index: 0,
          finish_reason: "tool_calls",
          message: {
            role: "assistant",
            content: null,
            tool_calls: [
              {
                id,
                type: "function",
                function: { name: "bash", arguments: args },
              },
            ],
          },
        },
      ],
      usage: {
        prompt_tokens: promptTokens,
        completion_tokens: completionTokens,
        total_tokens: promptTokens + completionTokens,
      },
    };
  }

  // Final phase (or a plain completion when no bash tool is offered): one
  // non-empty answer embedding the observed tool output when present.
  const toolMessage = lastOf(messages, "tool");
  const toolOutput =
    typeof toolMessage?.content === "string" ? toolMessage.content.trim() : "";
  const content =
    toolOutput !== ""
      ? `Dev model final answer: remote Bash printed ${JSON.stringify(toolOutput)}.`
      : `Dev model reply to ${JSON.stringify(prompt)}: no remote Bash round-trip was requested.`;
  const completionTokens = Math.max(1, Math.ceil(content.length / 4));
  return {
    id: `chatcmpl-dev-${createHash("sha256").update(content).digest("hex").slice(0, 8)}`,
    object: "chat.completion",
    created: 0,
    model,
    choices: [
      {
        index: 0,
        finish_reason: "stop",
        message: { role: "assistant", content, tool_calls: [] },
      },
    ],
    usage: {
      prompt_tokens: promptTokens,
      completion_tokens: completionTokens,
      total_tokens: promptTokens + completionTokens,
    },
  };
}

/** Validates the request subset the relay can actually send. */
function validate(body) {
  if (
    typeof body !== "object" ||
    body === null ||
    !Array.isArray(body.messages) ||
    body.messages.length === 0
  ) {
    return errorBody("messages must be a non-empty array", "invalid_request_error", "invalid_messages");
  }
  if (typeof body.model !== "string" || body.model.trim() === "") {
    return errorBody("model must be a non-empty string", "invalid_request_error", "invalid_model");
  }
  if (
    !Number.isInteger(body.max_tokens) ||
    body.max_tokens < 1 ||
    body.max_tokens > MAX_TOKENS
  ) {
    return errorBody(
      `max_tokens must be an integer within [1, ${MAX_TOKENS}]`,
      "invalid_request_error",
      "invalid_max_tokens",
    );
  }
  return null;
}

/** Size-capped JSONL trace under the runtime state root; best effort. */
function trace(stateRoot, entry) {
  if (stateRoot === "") return;
  try {
    const file = join(stateRoot, "model-emulator.log");
    if (statSync(file).size > TRACE_MAX_BYTES) truncateSync(file, 0);
    appendFileSync(file, `${JSON.stringify(entry)}\n`);
  } catch {
    // A dev trace must never break the completion path.
  }
}

async function readBounded(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_REQUEST_BYTES) return null;
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function startServer(args, apiKey) {
  const server = createServer((request, response) => {
    const url = new URL(request.url ?? "/", `http://${HOST}:${args.port}`);
    const json = (status, payload) => {
      response.writeHead(status, { "content-type": "application/json" });
      response.end(JSON.stringify(payload));
    };

    if (request.method === "GET" && url.pathname === "/healthz") {
      return json(200, { ok: true });
    }
    if (request.method === "GET" && (url.pathname === "/v1/models" || url.pathname === "/models")) {
      if (!authorized(request.headers.authorization, apiKey)) {
        return json(401, errorBody("invalid API key", "invalid_request_error", "invalid_api_key"));
      }
      return json(200, {
        object: "list",
        data: [{ id: MODEL_ID, object: "model", owned_by: "voie-dev" }],
      });
    }
    if (request.method !== "POST" || url.pathname !== "/v1/chat/completions") {
      return json(404, errorBody("not found", "invalid_request_error", "not_found"));
    }
    if (!authorized(request.headers.authorization, apiKey)) {
      return json(401, errorBody("invalid API key", "invalid_request_error", "invalid_api_key"));
    }

    readBounded(request).then((raw) => {
      if (raw === null) {
        return json(413, errorBody(
          `request body exceeds ${MAX_REQUEST_BYTES} bytes`,
          "invalid_request_error",
          "request_too_large",
        ));
      }
      let body;
      try {
        body = JSON.parse(raw.toString("utf8"));
      } catch {
        return json(400, errorBody("request body is not valid JSON", "invalid_request_error", "invalid_json"));
      }
      const invalid = validate(body);
      if (invalid !== null) return json(400, invalid);
      const completion = respond(body);
      trace(args.stateRoot, {
        at: new Date().toISOString(),
        phase: completion.choices[0].finish_reason,
        model: body.model,
        messages: body.messages.length,
      });
      return json(200, completion);
    });
  });
  server.listen(args.port, HOST, () => {
    console.log(`http://${HOST}:${args.port}`);
  });
  return server;
}

function assert(condition, message) {
  if (!condition) throw new Error(`self-test failed: ${message}`);
}

function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Wire-shape proof without opening any socket. */
function selfTest() {
  // Mirror exactly how CloudModelRelay serializes requests and reads
  // responses (crates/voie-cloud/src/model/mod.rs).
  const c6Prompt =
    "Run one remote Bash command that prints voie-c6-ok and report its output.";
  const bashTool = {
    type: "function",
    function: {
      name: "bash",
      description: "Run one bounded foreground Bash command.",
      parameters: {
        type: "object",
        properties: { command: { type: "string" } },
        required: ["command"],
        additionalProperties: false,
      },
    },
  };
  const firstRequest = {
    model: "voie-dev-model",
    messages: [{ role: "user", content: c6Prompt }],
    tools: [bashTool],
    max_tokens: 512,
  };

  const first = respond(firstRequest);
  const choice = first.choices[0];
  const calls = choice.message.tool_calls;
  assert(calls.length === 1, "first request returns exactly one tool call");
  assert(calls[0].type === "function", "tool call is typed as function");
  assert(calls[0].function.name === "bash", "tool call targets bash");
  const args = JSON.parse(calls[0].function.arguments);
  assert(
    args.command === "echo voie-c6-ok",
    `command echoes the prompt marker, got ${args.command}`,
  );
  assert(choice.finish_reason === "tool_calls", "finish_reason is tool_calls");
  assert(typeof calls[0].id === "string" && calls[0].id.length > 0, "call id present");
  assert(deepEqual(first, respond(firstRequest)), "identical history replays identically");

  // Second round-trip after the tool result, in relay wire shape.
  const secondRequest = {
    ...firstRequest,
    messages: [
      { role: "user", content: c6Prompt },
      {
        role: "assistant",
        content: "",
        tool_calls: [
          { id: calls[0].id, type: "function", function: { name: "bash", arguments: calls[0].function.arguments } },
        ],
      },
      { role: "tool", content: "voie-c6-ok\n", tool_call_id: calls[0].id },
    ],
  };
  const second = respond(secondRequest);
  const finalChoice = second.choices[0];
  assert(finalChoice.finish_reason === "stop", "final answer finish_reason is stop");
  assert(finalChoice.message.tool_calls.length === 0, "final answer carries no tool call");
  assert(
    finalChoice.message.content.includes("voie-c6-ok"),
    "final answer embeds the tool output",
  );
  assert(first.usage.total_tokens > 0, "usage totals are positive integers");

  const plain = respond({
    model: "voie-dev-model",
    messages: [{ role: "user", content: "Reply with the single word pong." }],
    tools: [],
    max_tokens: 16,
  }).choices[0].message.content;
  assert(plain.length > 0, "plain completion returns non-empty content");
  assert(!offersBash([]), "no bash offered means no tool-call phase");
  assert(pickMarker("Run one remote Bash command.") === DEFAULT_MARKER, "missing marker falls back");
  // Imperative Run-echo markers used by C3/C5 (and C4/C6) fixture proofs.
  assert(pickMarker("Run echo voie-c3-ok") === "voie-c3-ok", "imperative echo marker captured");
  assert(pickMarker("Run echo c3-event-1690000000-1234 in bash and then reply with done.") === "c3-event-1690000000-1234", "imperative echo with in-bash suffix");
  assert(pickMarker("Run echo c5-marker-123-456 in bash and then reply with done.") === "c5-marker-123-456", "c5 imperative echo captured");
  // Ensure the C3/C5 tool call still emits exactly one typed bash call with the marker.
  {
    const livePrompt = "Run echo c3-event-1690000000-1234 in bash and then reply with done.";
    const req = { model: "voie-dev-model", messages: [{ role: "user", content: livePrompt }], tools: [bashTool], max_tokens: 512 };
    const out = respond(req);
    const a = JSON.parse(out.choices[0].message.tool_calls[0].function.arguments);
    assert(a.command === "echo c3-event-1690000000-1234", "live C3 prompt marker flows into tool call");
  }

  assert(authorized("Bearer k", "k"), "bearer header authorizes");
  assert(!authorized("Bearer wrong", "k"), "wrong key refused");
  assert(!authorized(undefined, "k"), "missing key refused");
  assert(validate({ messages: [], model: "m", max_tokens: 1 }) !== null, "empty messages rejected");
  assert(validate({ messages: [{ role: "user", content: "x" }], model: "m", max_tokens: 0 }) !== null, "zero max_tokens rejected");
  assert(validate({ messages: [{ role: "user", content: "x" }], model: "m", max_tokens: MAX_TOKENS }) === null, "max bound accepted");

  console.log("model-emulator self-test: deterministic tool-call/final-answer wire shape proved");
}

const args = parseArgs(process.argv.slice(2));
if (args.selfTest) {
  selfTest();
} else {
  if (!Number.isInteger(args.port) || args.port < 1 || args.port > 65535) {
    console.error("model-emulator: --port must be a TCP port number");
    process.exit(2);
  }
  if (args.apiKeyFile === "") {
    console.error("model-emulator: --api-key-file is required");
    process.exit(2);
  }
  let apiKey;
  try {
    apiKey = readFileSync(args.apiKeyFile, "utf8").trim();
  } catch {
    console.error(`model-emulator: cannot read API key file ${args.apiKeyFile}`);
    process.exit(2);
  }
  if (apiKey === "") {
    console.error("model-emulator: API key file is empty");
    process.exit(2);
  }
  if (args.stateRoot !== "") mkdirSync(args.stateRoot, { recursive: true });
  startServer(args, apiKey);
}
