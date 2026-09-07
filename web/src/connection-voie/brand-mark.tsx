/**
 * VOIE fill for DSH's `conversation.hero.brand.mark` hole.
 *
 * Stock conversation chrome falls back to FishLogo when the slot is empty.
 * The vendored graph is byte-preserved; this occupant is the product mark.
 */
import type { CSSProperties, ReactElement } from "react";

export type VoieBrandMarkProps = {
  size: number;
  className?: string | undefined;
};

const markStyle = (size: number): CSSProperties => ({
  background: "var(--kds-primary, #2563eb)",
  borderRadius: Math.max(3, Math.round(size * 0.12)),
  display: "block",
  flex: "0 0 auto",
  height: size,
  width: size,
});

export function VoieBrandMark({ size }: VoieBrandMarkProps): ReactElement {
  return <span aria-hidden="true" data-voie-brand-mark="" style={markStyle(size)} />;
}
