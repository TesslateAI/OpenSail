/** First-run project creation. A fresh account owns no project and no other
 * console surface can act without one, so the boot screen offers the real
 * POST /api/projects flow instead of a dead end. */

import { useCallback, useState, type ChangeEvent, type FormEvent } from "react";
import { createProject } from "../api/api.ts";
import { newIntentId } from "../api/http.ts";
import { Card } from "./primitives.tsx";

export function CreateProjectForm({ onCreated }: { onCreated: () => void }) {
  const [name, setName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const submit = useCallback(async (): Promise<void> => {
    const trimmed = name.trim();
    if (trimmed.length === 0 || submitting) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      await createProject({ id: newIntentId(), name: trimmed });
      onCreated();
    } catch (reason: unknown) {
      setSubmitError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setSubmitting(false);
    }
  }, [name, onCreated, submitting]);

  const handleNameChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => setName(event.target.value),
    [],
  );

  const handleSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      void submit();
    },
    [submit],
  );

  return (
    <div className="boot-card">
      <Card title="Create your first project">
        <form className="stack" onSubmit={handleSubmit}>
          <p className="muted">
            A project groups agents, sessions, and workspaces. Your account becomes its owner.
          </p>
          <div className="field">
            <label htmlFor="create-project-name">Project name</label>
            <input
              id="create-project-name"
              value={name}
              onChange={handleNameChange}
              placeholder="e.g. Field trials"
              disabled={submitting}
              autoFocus
            />
          </div>
          {submitError !== null ? (
            <p role="alert" className="muted">
              Creating the project failed: {submitError}. Nothing was created; you can retry.
            </p>
          ) : null}
          <div className="actions">
            <button
              type="submit"
              className={
                submitting || name.trim().length === 0
                  ? "btn btn-primary btn-disabled"
                  : "btn btn-primary"
              }
              disabled={submitting || name.trim().length === 0}
            >
              {submitting ? "Creating…" : "Create project"}
            </button>
          </div>
        </form>
      </Card>
    </div>
  );
}
