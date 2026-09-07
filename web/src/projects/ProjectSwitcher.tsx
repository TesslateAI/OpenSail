/**
 * Project switcher: the personal/team dropdown over the console's project list.
 * The control plane owns project validity — this component only renders what
 * the project data model resolves and reports the chosen project id upward, so
 * the shell can swap its selection and persistence.
 */

import { useMemo, type ChangeEvent } from "react";
import type { ProjectSummaryDto } from "../api/dto.ts";
import {
  createProjectSwitcherModel,
  PROJECT_KIND_LABELS,
} from "./model.ts";

export type ProjectSwitcherProps = {
  projects: readonly ProjectSummaryDto[];
  value: string | null;
  onChange: (projectId: string) => void;
  disabled?: boolean | undefined;
};

export function ProjectSwitcher({
  projects,
  value,
  onChange,
  disabled = false,
}: ProjectSwitcherProps) {
  const model = useMemo(
    () => createProjectSwitcherModel(projects, value),
    [projects, value],
  );

  const handleChange = (event: ChangeEvent<HTMLSelectElement>): void => {
    const next = event.target.value;
    if (next !== "") onChange(next);
  };

  const personal = model.personal;
  const team = model.team;

  return (
    <select
      className="select scope-switcher"
      aria-label="Project"
      value={model.selected?.project.id ?? ""}
      disabled={disabled || projects.length === 0}
      onChange={handleChange}
    >
      {projects.length === 0 ? <option value="">No projects available</option> : null}
      {projects.length > 0 && model.selected === null ? (
        <option value="" disabled>
          Select a project…
        </option>
      ) : null}
      {personal.length > 0 ? (
        <optgroup label={PROJECT_KIND_LABELS.personal}>
          {personal.map((option) => (
            <option key={option.project.id} value={option.project.id}>
              {option.label}
            </option>
          ))}
        </optgroup>
      ) : null}
      {team.length > 0 ? (
        <optgroup label={PROJECT_KIND_LABELS.team}>
          {team.map((option) => (
            <option key={option.project.id} value={option.project.id}>
              {option.label}
            </option>
          ))}
        </optgroup>
      ) : null}
    </select>
  );
}
