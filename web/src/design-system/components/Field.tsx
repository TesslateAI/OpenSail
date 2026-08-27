/**
 * Segmented control — one active option on a muted pill track.
 * Field — labelled input wrapper.
 * Source: mock app.css § .segmented / .field* ; mock ui.js § Segmented/Field
 */
import type { ReactNode } from "react";
import { cx } from "../cx";

export interface SegmentedOption<T extends string> {
  value: T;
  label?: ReactNode;
  icon?: ReactNode;
  title?: string;
}

export interface SegmentedProps<T extends string> {
  options: ReadonlyArray<SegmentedOption<T>>;
  value: T;
  onChange: (value: T) => void;
  className?: string;
}

export function Segmented<T extends string>({ options, value, onChange, className }: SegmentedProps<T>): ReactNode {
  return (
    <div className={cx("kds-segmented", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          className={value === o.value ? "kds-active" : undefined}
          onClick={() => onChange(o.value)}
          title={o.title ?? undefined}
        >
          {o.icon ?? o.label}
        </button>
      ))}
    </div>
  );
}

export interface FieldProps {
  label?: ReactNode;
  hint?: ReactNode;
  children: ReactNode;
  className?: string;
}

export function Field({ label, hint, children, className }: FieldProps): ReactNode {
  return (
    <div className={cx("kds-field", className)}>
      {label !== undefined ? <label className="kds-field-label">{label}</label> : null}
      {children}
      {hint !== undefined ? <p className="kds-field-hint">{hint}</p> : null}
    </div>
  );
}
