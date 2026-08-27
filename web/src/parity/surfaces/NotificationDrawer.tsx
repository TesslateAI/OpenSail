/**
 * NotificationDrawer — full-height queue drawer (approvals are a queue you
 * work through, not a glance). Controlled + effectless; wire `open`,
 * `onOpenChange` and Escape via `useEscapeToClose` in the shell.
 * Sources:
 *   - mock review.css § .notif-* + .bell-dot
 *   - mock review-ui.js § NotificationBell
 */
import type { ReactNode } from "react";
import { Badge } from "../../design-system/components/Badge";
import { IconButton } from "../../design-system/components/Button";
import { cx } from "../../design-system/cx";
import type { NotificationModel } from "../presentation/models";
import {
  groupNotifications,
  toneForNotificationKind,
} from "../presentation/models";

export interface NotificationIconSlot {
  (kind: NotificationModel["kind"]): ReactNode;
}

export interface NotificationDrawerProps {
  open: boolean;
  items: ReadonlyArray<NotificationModel>;
  onOpenChange: (open: boolean) => void;
  onItemClick: (item: NotificationModel) => void;
  onMarkAllRead?: () => void;
  /** Maps a notification kind to its icon glyph. */
  iconFor: NotificationIconSlot;
  closeIcon: ReactNode;
  emptyIcon: ReactNode;
  /** Footer affordance, e.g. "View all notifications" link. */
  footer?: ReactNode;
}

export function NotificationDrawer({
  open,
  items,
  onOpenChange,
  onItemClick,
  onMarkAllRead,
  iconFor,
  closeIcon,
  emptyIcon,
  footer,
}: NotificationDrawerProps): ReactNode {
  if (!open) return null;
  const unread = items.filter((n) => !n.read).length;
  const groups = groupNotifications(items);

  return (
    <>
      <div className="kds-notif-scrim" onClick={() => onOpenChange(false)} />
      <aside className="kds-notif-panel" role="dialog" aria-label="Notifications">
        <div className="kds-notif-head">
          <span className="kds-notif-title">Notifications</span>
          {unread > 0 ? <Badge tone="info">{unread} new</Badge> : null}
          <div className="kds-spacer" />
          {unread > 0 && onMarkAllRead !== undefined ? (
            <button type="button" className="kds-link-btn" onClick={onMarkAllRead}>
              Mark all read
            </button>
          ) : null}
          <IconButton icon={closeIcon} ariaLabel="Close" onClick={() => onOpenChange(false)} />
        </div>

        {items.length === 0 ? (
          <div className="kds-notif-empty">
            {emptyIcon}
            <p style={{ fontSize: 13 }}>Nothing new.</p>
            <p style={{ fontSize: 11.8 }}>Approvals and reviewer comments will show up here.</p>
          </div>
        ) : (
          <div className="kds-notif-list">
            {groups.map((g) => (
              <div key={g.group}>
                <div className="kds-notif-group">{g.group}</div>
                {g.items.map((n) => (
                  <button
                    key={n.id}
                    type="button"
                    className={cx("kds-notif-row", !n.read && "kds-unread")}
                    onClick={() => onItemClick(n)}
                  >
                    <div className={cx("kds-notif-icon", `kds-${toneForNotificationKind(n.kind)}`)}>
                      {iconFor(n.kind)}
                    </div>
                    <span className="kds-notif-body">
                      <span className="kds-notif-row-title">{n.title}</span>
                      <span className="kds-notif-text">{n.body}</span>
                      <span className="kds-notif-foot">
                        <span className="kds-notif-time">{n.at}</span>
                        {n.targetId !== undefined ? (
                          <>
                            <span className="kds-dotsep">·</span>
                            <span className="kds-notif-link">Open the app</span>
                          </>
                        ) : null}
                      </span>
                    </span>
                    {!n.read ? <span className="kds-notif-pip" /> : null}
                  </button>
                ))}
              </div>
            ))}
          </div>
        )}

        {footer !== undefined ? <div className="kds-notif-foot">{footer}</div> : null}
      </aside>
    </>
  );
}
