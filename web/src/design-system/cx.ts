/**
 * Classname joiner matching the mock's filter(Boolean).join(" ") idiom
 * (mock app.css/ui.js). Falsy entries are dropped.
 */
export type ClassValue = string | false | null | undefined;

export function cx(...parts: ClassValue[]): string {
  let out = "";
  for (const p of parts) {
    if (p) out += (out ? " " : "") + p;
  }
  return out;
}
