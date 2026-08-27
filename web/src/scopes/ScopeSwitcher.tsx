/**
 * Scope switcher: the personal/team dropdown over the console's scope list.
 * The control plane owns scope validity — this component only renders what
 * the scope data model resolves and reports the chosen scope id upward, so
 * the shell can swap its selection and persistence.
 */

import { useMemo, type ChangeEvent } from "react";
import type { ScopeSummaryDto } from "../api/dto.ts";
import {
  createScopeSwitcherModel,
  SCOPE_KIND_LABELS,
} from "./model.ts";

export type ScopeSwitcherProps = {
  scopes: readonly ScopeSummaryDto[];
  value: string | null;
  onChange: (scopeId: string) => void;
  disabled?: boolean | undefined;
};

export function ScopeSwitcher({ scopes, value, onChange, disabled = false }: ScopeSwitcherProps) {
  const model = useMemo(
    () => createScopeSwitcherModel(scopes, value),
    [scopes, value],
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
      aria-label="Scope"
      value={model.selected?.scope.id ?? ""}
      disabled={disabled || scopes.length === 0}
      onChange={handleChange}
    >
      {scopes.length === 0 ? <option value="">No scopes available</option> : null}
      {scopes.length > 0 && model.selected === null ? (
        <option value="" disabled>
          Select a scope…
        </option>
      ) : null}
      {personal.length > 0 ? (
        <optgroup label={SCOPE_KIND_LABELS.personal}>
          {personal.map((option) => (
            <option key={option.scope.id} value={option.scope.id}>
              {option.label}
            </option>
          ))}
        </optgroup>
      ) : null}
      {team.length > 0 ? (
        <optgroup label={SCOPE_KIND_LABELS.team}>
          {team.map((option) => (
            <option key={option.scope.id} value={option.scope.id}>
              {option.label}
            </option>
          ))}
        </optgroup>
      ) : null}
    </select>
  );
}
