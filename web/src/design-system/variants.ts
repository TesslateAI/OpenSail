/**
 * Shared variant unions and state->tone mappers for the parity system.
 * Sources:
 *   - mock app.css § .btn* / .badge-*  (sizes, tones)
 *   - mock ui.js § toneForRunState / toneForHealth
 *   - mock surfaces.css § .stage*      (stage states)
 */

/** Tone classes are `<prefix>-<tone>` in primitives.css, shared by badges,
 * feed dots, notif icons, progress fills, meter fills and stat trends. */
export type Tone =
  | "neutral"
  | "ok"
  | "warn"
  | "fail"
  | "info"
  | "pending"
  | "accent";

export type Size = "sm" | "md" | "lg";

/** Button visual variants (`.kds-btn-<variant>`). */
export type ButtonVariant = "default" | "primary" | "ghost" | "danger";

/** Stage rail node states (`.kds-stage.kds-<state>`). */
export type StageState = "done" | "current" | "waiting" | "upcoming";

/**
 * Run/execution states -> tone, ported from mock ui.js `toneForRunState`.
 * An unknown state maps to warn; cancelled reads neutral rather than failing.
 */
export type RunState =
  | "terminal"
  | "unknown"
  | "cancelled"
  | "dispatched"
  | "running"
  | "pending";

export function toneForRunState(state: RunState): Tone {
  switch (state) {
    case "terminal": return "ok";
    case "unknown": return "warn";
    case "cancelled": return "neutral";
    case "dispatched": return "info";
    case "running":
    case "pending": return "pending";
  }
}

/** Health string -> tone, ported from mock ui.js `toneForHealth`. */
export function toneForHealth(h: string): Tone {
  return h === "ok" ? "ok" : h === "warn" ? "warn" : "fail";
}
