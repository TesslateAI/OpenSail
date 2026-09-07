import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const WEB_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (relativePath) => readFileSync(join(WEB_ROOT, relativePath), "utf8");

function bracedBlock(source, openingIndex, label) {
  assert.notEqual(openingIndex, -1, `${label} must exist`);
  let depth = 0;
  let quote = null;
  let escaped = false;
  let lineComment = false;
  let blockComment = false;
  for (let index = openingIndex; index < source.length; index += 1) {
    const character = source[index];
    const next = source[index + 1];
    if (lineComment) {
      if (character === "\n") lineComment = false;
      continue;
    }
    if (blockComment) {
      if (character === "*" && next === "/") {
        blockComment = false;
        index += 1;
      }
      continue;
    }
    if (quote !== null) {
      if (escaped) {
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === quote) {
        quote = null;
      }
      continue;
    }
    if (character === "/" && next === "/") {
      lineComment = true;
      index += 1;
      continue;
    }
    if (character === "/" && next === "*") {
      blockComment = true;
      index += 1;
      continue;
    }
    if (character === '"' || character === "'" || character === "`") {
      quote = character;
      continue;
    }
    if (character === "{") depth += 1;
    if (character === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(openingIndex, index + 1);
    }
  }
  assert.fail(`${label} has an unterminated block`);
}

function blockAfter(source, marker, label = marker) {
  const markerIndex = source.indexOf(marker);
  assert.notEqual(markerIndex, -1, `${label} must exist`);
  return bracedBlock(source, source.indexOf("{", markerIndex), label);
}

function escapedText(value) {
  return [...value]
    .map((character) => "\\^$.*+?()[]{}|".includes(character) ? `\\${character}` : character)
    .join("");
}

test("CarrierEvent keeps the store identity and producer fields explicit", () => {
  const types = read("src/carrier/types.ts");
  const declaration = types.match(/export\s+(?:type|interface)\s+(?:CanonicalEvent|CarrierEvent)\b/);
  assert.ok(declaration, "carrier types must export CanonicalEvent or CarrierEvent");
  const eventBody = bracedBlock(types, types.indexOf("{", declaration.index), "CarrierEvent type");

  for (const field of [
    "sessionId",
    "globalSeq",
    "revision",
    "eventIndex",
    "appendId",
    "objectKey",
    "contentHash",
    "byteLength",
    "seq",
    "time",
    "data",
  ]) {
    assert.match(eventBody, new RegExp(`\\b${field}\\s*:`), `CarrierEvent must expose ${field}`);
  }
  assert.match(eventBody, /globalSeq\s*:\s*number/);
  assert.match(eventBody, /revision\s*:\s*number/);
  assert.match(eventBody, /eventIndex\s*:\s*number/);
  assert.match(eventBody, /appendId\s*:\s*string\s*\|\s*null/);
  assert.match(eventBody, /seq\s*:\s*number\s*\|\s*null/);
  assert.match(eventBody, /time\s*:\s*number\s*\|\s*null/);
});

test("the carrier maps canonical identity verbatim and does not synthesize event order", () => {
  const carrier = read("src/carrier/voie.ts");
  const eventMapping = blockAfter(carrier, "function canonicalEventsOf(", "canonicalEventsOf");
  const itemMapping = blockAfter(carrier, "function canonicalItemsOf(", "canonicalItemsOf");

  assert.match(eventMapping, /globalSeq\s*:\s*item\.globalSeq/);
  assert.match(eventMapping, /revision\s*:\s*item\.revision/);
  assert.match(eventMapping, /eventIndex\s*,?/);
  assert.match(eventMapping, /appendId\s*:\s*item\.appendId/);
  assert.match(eventMapping, /objectKey\s*:\s*item\.objectKey/);
  assert.match(eventMapping, /contentHash\s*:\s*item\.contentHash/);
  assert.match(eventMapping, /byteLength\s*:\s*item\.byteLength/);
  assert.match(eventMapping, /seq\s*:\s*asNum\(record\["seq"\]\)/);
  assert.match(eventMapping, /time\s*:\s*asNum\(record\["time"\]\)/);
  assert.doesNotMatch(eventMapping, /seq\s*:\s*(?:eventIndex|globalSeq|revision)/);
  assert.doesNotMatch(eventMapping, /time\s*:\s*(?:Date\.|eventIndex|globalSeq|revision)/);

  // Missing append identity is rejected rather than replaced with a made-up
  // zero/empty value before the canonical event is exposed.
  assert.doesNotMatch(itemMapping, /globalSeq\s*:\s*[^,\n]*(?:\?\?|\|\|)\s*0\b/);
  assert.doesNotMatch(itemMapping, /revision\s*:\s*[^,\n]*(?:\?\?|\|\|)\s*0\b/);
  assert.doesNotMatch(itemMapping, /appendId\s*:\s*[^,\n]*(?:\?\?|\|\|)\s*["'](?:["']|none|null)/i);

  for (const lifecycleType of ["turn/start", "turn/end", "session/start", "session/end"]) {
    assert.doesNotMatch(carrier, new RegExp(`type\\s*:\s*["']${escapedText(lifecycleType)}["']`));
  }
});

test("poll reports stale cursors without inventing a replacement cursor", () => {
  const carrier = read("src/carrier/voie.ts");
  const poll = blockAfter(carrier, "async poll(", "VoieCarrier.poll");
  assert.match(poll, /error\.status\s*===\s*409/);
  assert.match(poll, /return\s*\{\s*kind:\s*["']stale["']\s*\}/);
  assert.match(poll, /serverCursor\s*<\s*after/);
});

test("one intent has one in-flight mutation and stale recovery does not resubmit it", () => {
  const carrier = read("src/carrier/voie.ts");
  const mutate = blockAfter(carrier, "async mutate(", "VoieCarrier.mutate");
  assert.match(mutate, /const key\s*=\s*mutation\.intentId/);
  assert.match(mutate, /this\.inflight\.has\(key\)/);
  assert.match(mutate, /duplicate in-flight mutation/);
  assert.match(mutate, /this\.inflight\.add\(key\)/);
  assert.match(mutate, /finally\s*\{[\s\S]*this\.inflight\.delete\(key\)/);
  assert.doesNotMatch(mutate, /\b(?:for|while)\s*\(/, "mutations must not retry in a loop");

  for (const operation of [
    "conversation.create",
    "conversation.message",
    "conversation.cancel",
  ]) {
    const start = mutate.indexOf(`case "${operation}"`);
    assert.notEqual(start, -1, `${operation} must be implemented`);
  }
  const cancelCase = mutate.slice(mutate.indexOf('case "conversation.cancel"'));
  assert.match(
    cancelCase,
    /\/api\/conversations\/\$\{encodeURIComponent\(mutation\.conversationId\)\}\/cancel/,
  );
  assert.doesNotMatch(cancelCase, /\/runs/);

  const apiPath = join(WEB_ROOT, "src", "connection-voie", "api.ts");
  assert.ok(existsSync(apiPath), "connection-voie/api.ts must provide the stale recovery face");
  const api = read("src/connection-voie/api.ts");
  const startLoop = blockAfter(api, "start(sinks: ConnectionSinks)", "connection start");
  const staleStart = startLoop.indexOf('if (result.kind === "stale")');
  const staleEnd = startLoop.indexOf("continue;", staleStart);
  assert.notEqual(staleStart, -1, "connection loop must handle stale cursors");
  assert.notEqual(staleEnd, -1, "stale handling must resume the poll loop");
  const staleBranch = startLoop.slice(staleStart, staleEnd);
  assert.match(staleBranch, /built\.refreshBaseline\(\)/);
  assert.match(staleBranch, /cursor\s*=\s*fresh\.cursor/);
  assert.doesNotMatch(staleBranch, /\.mutate\s*\(/);

  assert.doesNotMatch(api, /pendingConversations/);
  assert.doesNotMatch(api, /\bpromoting\b/);
  const promptAt = api.indexOf("promptTextOf(payload)");
  assert.notEqual(promptAt, -1, "sessions.prompt must exist");
  const prompt = api.slice(promptAt, promptAt + 2500);
  assert.ok((prompt.match(/carrier\.mutate\s*\(/g) ?? []).length >= 1, "prompt still uses the mutation seam");
  assert.doesNotMatch(prompt, /\bwhile\s*\(/, "a prompt mutation remains single-attempt");
  assert.match(prompt, /conversation\.message/);
  assert.match(api, /const pending = inflightPrompt\.get\(sessionId\);/);
  assert.match(api, /async function cancelExactRun\(/);
  const cancelAt = api.indexOf("op: \"conversation.cancel\"");
  assert.notEqual(cancelAt, -1, "sessions.cancel must use conversation.cancel");
  const cancelWindow = api.slice(Math.max(0, cancelAt - 200), cancelAt + 400);
  assert.match(cancelWindow, /carrier\.mutate\s*\(/);
  assert.match(api, /\/api\/runs\/\$\{encodeURIComponent\(runId\)\}\/cancel/);
});

test("empty poll batches still reconcile durable run seats", () => {
  const api = read("src/connection-voie/api.ts");
  assert.doesNotMatch(
    api,
    /if \(touched\.size > 0\) void built\.reconcileQueues/,
    "empty poll batches must still reconcile live seats",
  );
  assert.match(api, /void built\.reconcileQueues\(touched, ac\.signal\)/);
});

test("DSH event envelopes preserve producer seq/time and reject absent identity", () => {
  const api = read("src/connection-voie/api.ts");
  const envelope = blockAfter(api, "function eventEnvelopeOf(", "eventEnvelopeOf");
  assert.match(envelope, /event\.seq\s*===\s*null\s*\|\|\s*event\.time\s*===\s*null/);
  assert.match(envelope, /return\s+null/);
  assert.match(envelope, /seq\s*:\s*event\.seq/);
  assert.match(envelope, /time\s*:\s*event\.time/);
  assert.doesNotMatch(envelope, /seq\s*:\s*(?:event\.globalSeq|event\.eventIndex|Date\.)/);
  assert.doesNotMatch(envelope, /time\s*:\s*(?:event\.globalSeq|event\.eventIndex|Date\.)/);
});

test("New Chat creates a durable Session without connectWorkspace reuse", () => {
  const plugin = read("src/connection-voie/plugin-fn.ts");
  assert.match(plugin, /create\(\{\s*workspaceId:\s*target\s*\}\)/);
  assert.doesNotMatch(plugin, /workspaces\?\.startSession/);
  const hero = read("src/connection-voie/hero-workspace.tsx");
  assert.match(hero, /lastWorkspace\(/);
  assert.match(hero, /Do not connect an/);
});
