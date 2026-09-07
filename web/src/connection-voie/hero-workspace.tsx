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
import { getVoieDshHostContext } from "./host-context.ts";
import { lastWorkspace } from "./last-workspace.ts";

type WorkspaceItem = {
  workspaceId: string;
  title: string;
  state?: string;
  createdAt?: string;
};

type WorkspaceListView = {
  items: readonly WorkspaceItem[];
  phase: string;
  recentWorkspaceId?: string;
};

function newestReadyId(items: readonly WorkspaceItem[]): string | undefined {
  const ready = items.filter((item) => item.state === "ready" || item.state === undefined);
  const pool = ready.length > 0 ? ready : items;
  const sorted = [...pool].sort((left, right) =>
    (right.createdAt ?? "").localeCompare(left.createdAt ?? ""),
  );
  return sorted[0]?.workspaceId;
}

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
    const preferred =
      lastWorkspace(getVoieDshHostContext().projectId)
      || getVoieDshHostContext().workspaceId
      || "";
    if (preferred !== "") {
      // New Chat creates the Session on this Workspace. Do not connect an
      // older listed Workspace just to unlock the composer.
      tried.current = preferred;
      return;
    }
    const target = newestReadyId(view.items) ?? view.recentWorkspaceId;
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
    <div ref={menu} role="menu" data-voie-workspace-picker="" aria-label="Workspaces">
      {view.items.length === 0 ? (
        <p>{view.phase === "ready" ? "No workspaces yet." : "Loading workspaces…"}</p>
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
            >
              {labelOf(item)}
            </button>
          );
        })
      )}
    </div>
  );
}
