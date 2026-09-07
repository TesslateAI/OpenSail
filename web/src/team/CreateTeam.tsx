/**
 * Create Team: the ordinary browser operation missing from first-run
 * Personal boot. Always POSTs `kind: "team"` through `createTeam`.
 */

import { useCallback, useState, type ChangeEvent, type FormEvent } from "react";
import { createTeam } from "../api/api.ts";
import type { ProjectSummaryDto } from "../api/dto.ts";

export type CreateTeamProps = {
  onCreated: (project: ProjectSummaryDto) => void;
};

export function CreateTeam({ onCreated }: CreateTeamProps) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const close = useCallback((): void => {
    if (submitting) return;
    setOpen(false);
    setName("");
    setSubmitError(null);
  }, [submitting]);

  const submit = useCallback(async (): Promise<void> => {
    const trimmed = name.trim();
    if (trimmed.length === 0 || submitting) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      const project = await createTeam(trimmed);
      setOpen(false);
      setName("");
      onCreated(project);
    } catch (reason: unknown) {
      setSubmitError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setSubmitting(false);
    }
  }, [name, onCreated, submitting]);

  if (!open) {
    return (
      <button type="button" className="btn" onClick={() => setOpen(true)}>
        Create team
      </button>
    );
  }

  return (
    <form
      className="row"
      onSubmit={(event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        void submit();
      }}
    >
      <input
        aria-label="Team name"
        placeholder="Team name"
        value={name}
        disabled={submitting}
        autoFocus
        onChange={(event: ChangeEvent<HTMLInputElement>) => setName(event.target.value)}
      />
      <button type="button" className="btn" onClick={close} disabled={submitting}>
        Cancel
      </button>
      <button
        type="submit"
        className={
          submitting || name.trim().length === 0 ? "btn btn-primary btn-disabled" : "btn btn-primary"
        }
        disabled={submitting || name.trim().length === 0}
      >
        {submitting ? "Creating…" : "Create team"}
      </button>
      {submitError !== null ? (
        <span role="alert" className="muted">
          {submitError}
        </span>
      ) : null}
    </form>
  );
}
