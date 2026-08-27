/**
 * Avatar + AvatarStack (overlapping faces, Drive-style).
 * Source: mock app.css § .avatar ; mock surfaces.css § .avatars/.avatar.more
 */
import type { ReactNode } from "react";
import { cx } from "../cx";

export interface AvatarProps {
  initials: string;
  /** Larger 38px avatar (profile foot). */
  lg?: boolean;
}

export function Avatar({ initials, lg }: AvatarProps): ReactNode {
  return <span className={cx("kds-avatar", lg && "kds-lg")}>{initials}</span>;
}

export interface AvatarStackProps {
  items: ReadonlyArray<{ id: string; initials: string }>;
  /** "more" capsule text, e.g. "+3"; rendered when there are extras. */
  more?: string;
}

export function AvatarStack({ items, more }: AvatarStackProps): ReactNode {
  return (
    <span className="kds-avatars">
      {items.map((p) => <Avatar key={p.id} initials={p.initials} />)}
      {more !== undefined ? <span className="kds-avatar-more">{more}</span> : null}
    </span>
  );
}
