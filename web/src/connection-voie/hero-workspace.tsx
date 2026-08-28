/**
 * VOIE fill for DSH's empty `conversation.hero.workspace` hole.
 *
 * Stock conversation chrome declares the picker slot but ships no occupant
 * (the dropped sidebar package is not in the VOIE graph). Without this
 * contribution the hero stays inert: runtime `startInitialSelection` can
 * open a provisional session, but ConversationRoot only unlocks the
 * composer after `onPick` sets the pending workspace chip.
 */
import { useEffect, useRef, type MouseEvent, type ReactElement } from "react";

type WorkspaceItem = {
  workspaceId: string;
  title: string;
};

type WorkspaceListView = {
  items: readonly WorkspaceItem[];
  phase: string;
  recentWorkspaceId?: string;
};

type UseWorkspaces = <S>(select: (state: WorkspaceListView) => S) => S;

export type VoieHeroWorkspaceProps = {
  open: boolean;
  selectedId?: string | undefined;
  onPick: (workspaceId: string) => void;
  onClose: () => void;
  useWorkspaces: UseWorkspaces;
};

function labelOf(item: WorkspaceItem): string {
  const title = item.title.trim();
  return title === "" ? item.workspaceId.slice(0, 8) : title;
}

export function VoieHeroWorkspace({
  open,
  selectedId,
  onPick,
  onClose,
  useWorkspaces,
}: VoieHeroWorkspaceProps): ReactElement | null {
  const view = useWorkspaces((state) => ({
    items: state.items,
    phase: state.phase,
    recentWorkspaceId: state.recentWorkspaceId,
  }));
  const tried = useRef<string | undefined>(undefined);
  const menu = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (selectedId !== undefined && selectedId !== "") {
      tried.current = selectedId;
      return;
    }
    if (view.phase !== "ready") return;
    const target = view.recentWorkspaceId ?? view.items[0]?.workspaceId;
    if (target === undefined || tried.current === target) return;
    tried.current = target;
    onPick(target);
  }, [onPick, selectedId, view.items, view.phase, view.recentWorkspaceId]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (event: Event): void => {
      const node = event.target;
      if (!(node instanceof Node)) return;
      if (menu.current?.contains(node)) return;
      onClose();
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open, onClose]);

  if (!open) return null;

  const pick = (workspaceId: string) => (event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    event.stopPropagation();
    onPick(workspaceId);
  };

  return (
    <div
      ref={menu}
      role="menu"
      data-voie-workspace-picker=""
      aria-label="Workspaces"
      style={{
        position: "absolute",
        zIndex: 20,
        marginTop: 8,
        minWidth: 240,
        maxHeight: 280,
        overflow: "auto",
        padding: 6,
        borderRadius: 12,
        border: "1px solid var(--dsw-alias-border-l2-darkmode-thin, #d7dbe0)",
        background: "var(--dsw-specific-input-major, #fff)",
        boxShadow: "0 8px 24px rgba(15, 23, 42, 0.12)",
      }}
    >
      {view.items.length === 0 ? (
        <p style={{ margin: 8, color: "var(--kds-muted-foreground, #64748b)", fontSize: 13 }}>
          {view.phase === "ready" ? "No workspaces yet." : "Loading workspaces…"}
        </p>
      ) : (
        view.items.map((item) => {
          const selected = item.workspaceId === selectedId;
          return (
            <button
              key={item.workspaceId}
              type="button"
              role="menuitem"
              data-workspace-id={item.workspaceId}
              aria-current={selected ? "true" : undefined}
              onClick={pick(item.workspaceId)}
              style={{
                display: "block",
                width: "100%",
                textAlign: "left",
                padding: "8px 10px",
                border: 0,
                borderRadius: 8,
                background: selected ? "var(--kds-accent-soft, #e8f1ff)" : "transparent",
                cursor: "pointer",
                font: "inherit",
              }}
            >
              {labelOf(item)}
            </button>
          );
        })
      )}
    </div>
  );
}
