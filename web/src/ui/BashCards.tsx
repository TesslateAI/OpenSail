/** Bash tool cards with visibly distinct terminal outcomes.
 *
 * `ok` is a completed run (exit 0), `failure` a program failure (non-zero
 * exit), `running` an in-flight call in a live turn, and `unknown` an outcome
 * the settled log does not determine (missing call or missing result).
 * Running is a live state, never a settled one: the same card re-projects
 * from `running` to `unknown` when the turn settles. Only known
 * command/cwd/output is rendered; raw unknown event data never reaches the
 * screen.
 */

import type { BashCallBlock, BashResultBlock, BashCardStatus } from "../events/project.ts";
import { Badge, Card } from "./primitives.tsx";

type BadgeTone = "neutral" | "ok" | "warn" | "fail" | "accent";

export type BashCardProps = {
  call: BashCallBlock | null;
  result: BashResultBlock | null;
  status: BashCardStatus;
};

export function BashCard({ call, result, status }: BashCardProps) {
  const variant =
    status === "ok" ? "terminal" : status === "failure" ? "failure" : status === "running" ? "default" : "unknown";
  const statusTone: BadgeTone =
    status === "ok" ? "ok" : status === "failure" ? "fail" : status === "running" ? "accent" : "warn";
  const statusLabel =
    status === "ok"
      ? "completed · exit 0"
      : status === "failure"
        ? `failed · exit ${result?.exitCode ?? "?"}`
        : status === "running"
          ? "running…"
          : "outcome unknown";

  return (
    <Card
      title="bash"
      variant={variant}
      actions={
        <Badge tone={statusTone}>
          {status === "running" ? (
            <span className="row row-tight">
              <span className="spinner spinner-inline" aria-hidden="true" />
              {statusLabel}
            </span>
          ) : (
            statusLabel
          )}
        </Badge>
      }
    >
      <div className={`bash-card bash-${status}`}>
        {call !== null ? (
          <>
            {call.cwd.length > 0 ? (
              <div className="bash-head mono muted">{call.cwd}</div>
            ) : null}
            {call.command.length > 0 ? (
              <pre className="bash-command mono">{call.command}</pre>
            ) : (
              <div className="bash-head muted">Command unavailable.</div>
            )}
          </>
        ) : (
          <div className="bash-head muted">No matching invocation in the visible log.</div>
        )}
        {result !== null ? (
          <pre className="bash-output mono">{result.output}</pre>
        ) : status === "running" ? (
          <div className="muted row row-tight bash-waiting">
            <span className="spinner spinner-inline" aria-hidden="true" />
            <span>Running — the result lands when the turn settles.</span>
          </div>
        ) : (
          <div className="muted">No result was recorded; the outcome is unknown.</div>
        )}
      </div>
    </Card>
  );
}
