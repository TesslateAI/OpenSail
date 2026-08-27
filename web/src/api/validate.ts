/**
 * Canonical runtime shape guards for network-decoded JSON. This is the
 * package's single type-guard module: never redefine these at call sites.
 *
 * Guards prove object-ness only; every field read stays `unknown` until an
 * accessor below validates it, so no unchecked shape is ever trusted.
 */

import type { JsonValue } from "./dto.ts";

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Reads `value[key]` as a record when it is one, else `null`. */
export function recordAt(value: unknown, key: string): Record<string, unknown> | null {
  if (!isRecord(value)) return null;
  const field: unknown = value[key];
  return isRecord(field) ? field : null;
}

/** Reads `value[key]` as an array when it is one, else `[]`. */
export function arrayAt(value: Record<string, unknown>, key: string): unknown[] {
  const field: unknown = value[key];
  return Array.isArray(field) ? field : [];
}

/** String field accessor: non-strings degrade to `null`. */
export function asStr(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/** Reads `value[key]` as a string when it is one, else `null`. */
export function strAt(value: Record<string, unknown>, key: string): string | null {
  return asStr(value[key]);
}

/** Numeric field accessor: non-finite numbers degrade to `null`. */
export function asNum(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Boolean field accessor with an explicit fallback for absent/invalid values. */
export function asBoolOr(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/**
 * JSON value accessor: validates and detaches a wire value as plain JSON.
 * Objects and arrays are deep-copied so no foreign or mutable reference is
 * retained; non-JSON leaves (`undefined`, functions, symbols, bigints,
 * non-finite numbers) degrade to `null`, and a circular structure degrades
 * the whole value to `null` instead of recursing forever. Never throws.
 */
export function asJson(value: unknown): JsonValue | null {
  try {
    return jsonOf(value, new Set());
  } catch {
    // Only circularity reaches here; it has no JSON representation.
    return null;
  }
}

function jsonOf(value: unknown, seen: Set<object>): JsonValue {
  if (typeof value === "string") return value;
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value === "boolean" || value === null) return value;
  if (typeof value !== "object") return null;
  if (seen.has(value)) throw new Error("circular JSON structure");
  seen.add(value);
  if (Array.isArray(value)) {
    const items: JsonValue[] = [];
    for (const item of value as unknown[]) items.push(jsonOf(item, seen));
    return items;
  }
  const record: { [key: string]: JsonValue } = {};
  for (const [key, item] of Object.entries(value)) {
    record[key] = jsonOf(item, seen);
  }
  return record;
}
