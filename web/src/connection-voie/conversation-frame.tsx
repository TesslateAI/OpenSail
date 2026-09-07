/**
 * VOIE conversation pane: the main seat is one column that fills the portal
 * main area. DSH AppFrame is not composed — it always reserved an empty
 * sidebar track beside the product nav. Details is a trailing flex column
 * and occupies width only while open.
 *
 * ConversationRoot pins `--dsh-chat-content-width: 748px` and measures the
 * composer with a ResizeObserver. Percentage widths re-enter that observer.
 * This pane assigns pixel widths from its own box instead.
 */
import { useLayoutEffect, useRef, type ReactNode } from "react";
import type { VoieLayoutState } from "./layout.ts";

type SessionList = {
  current?: string;
  byId: Record<string, { blank?: boolean } | undefined>;
};

export type VoieConversationPaneProps = {
  useStore: <S>(select: (state: VoieLayoutState) => S) => S;
  useSessions: <S>(select: (state: SessionList) => S) => S;
  actions: {
    closeDetails: () => void;
  };
  renderSlot: (name: string, owner: object) => ReactNode;
};

function assignPaneGeometry(pane: HTMLElement): void {
  const width = pane.clientWidth;
  if (width <= 0) return;
  const contentPx = Math.max(0, width - 48);
  const composerPx = Math.max(0, width - 24);
  const root = pane.querySelector<HTMLElement>("[data-phase]");
  if (root === null) return;
  const previous = Number.parseInt(root.style.getPropertyValue("--dsh-chat-content-width"), 10);
  if (Number.isFinite(previous) && Math.abs(previous - contentPx) < 8) return;
  root.style.setProperty("--dsh-chat-content-width", `${String(contentPx)}px`);
  root.style.setProperty("--dsh-composer-card-max-width", `${String(composerPx)}px`);
}

export function VoieConversationPane({
  useStore,
  useSessions,
  actions,
  renderSlot,
}: VoieConversationPaneProps): ReactNode {
  const details = useStore((state) => state.details);
  const detailsSession = useSessions((state) => {
    const current = state.current;
    return current !== undefined && state.byId[current]?.blank === false ? current : undefined;
  });
  const open = details > 0 && detailsSession !== undefined;
  const lastSession = useRef(detailsSession);
  const paneRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (detailsSession === undefined) return;
    if (lastSession.current !== undefined && lastSession.current !== detailsSession) {
      actions.closeDetails();
    }
    lastSession.current = detailsSession;
  }, [actions, detailsSession]);

  useLayoutEffect(() => {
    const pane = paneRef.current;
    const stage = stageRef.current;
    if (pane === null) return;
    let raf = 0;
    const schedule = (): void => {
      if (raf !== 0) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        assignPaneGeometry(pane);
      });
    };
    schedule();
    const resize = new ResizeObserver(schedule);
    resize.observe(pane);
    const mount = new MutationObserver(schedule);
    mount.observe(stage ?? pane, { childList: true });
    return () => {
      if (raf !== 0) cancelAnimationFrame(raf);
      resize.disconnect();
      mount.disconnect();
    };
  }, []);

  return (
    <div
      ref={paneRef}
      className="voie-conversation-pane"
      data-details-open={open ? "true" : undefined}
    >
      <div ref={stageRef} className="voie-conversation-pane__stage">
        {renderSlot("conversation", {})}
      </div>
      <div className="voie-conversation-pane__details">{renderSlot("details", {})}</div>
      <div className="voie-conversation-pane__overlay" data-shell-overlay="">
        {renderSlot("shell.overlay", {})}
      </div>
    </div>
  );
}
