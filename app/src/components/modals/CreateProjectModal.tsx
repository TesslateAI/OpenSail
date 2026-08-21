import { useState, useEffect, useMemo, useRef, type KeyboardEvent, type MouseEvent } from 'react';
import { X, Folder, CaretRight, FileDashed } from '@phosphor-icons/react';
import { marketplaceApi, projectsApi } from '../../lib/api';
import { useTeam } from '../../contexts/TeamContext';

interface MarketplaceBase {
  id: string;
  name: string;
  slug: string;
  description?: string;
  icon_url?: string;
  default_port?: number;
}

interface SiblingFolder {
  id: string;
  name: string;
  slug: string;
  updatedAt: string;
}

interface CreateProjectModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Called with the chosen name. ``baseId === ''`` (empty string) signals
   * an empty-workspace creation; the caller should branch and call
   * ``projectsApi.create(name, '', 'empty')``. ``undefined`` falls back to
   * the user's selected template. */
  onConfirm: (projectName: string, baseId?: string, baseVersion?: string) => void;
  isLoading?: boolean;
  initialBaseId?: string;
  baseVersion?: string;
  /** Pre-toggle the modal into "empty workspace" mode. Used by the
   * dashboard's "New empty workspace" button. */
  initialEmptyMode?: boolean;
}

const FEATURED_SLUGS = ['nextjs-16', 'vite-react-fastapi', 'vite-react-go', 'expo-default'];

// Synthetic "empty" tile that sits alongside the featured templates. Selected
// by reference equality (id === EMPTY_TILE.id), so the rest of the picker
// stays uniform.
const EMPTY_TILE: MarketplaceBase = {
  id: '__empty__',
  name: 'Empty',
  slug: '__empty__',
  description: 'No template — start from scratch',
};

function formatRelative(iso: string): string {
  if (!iso) return '';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return '';
  const abs = Math.abs(Math.round((t - Date.now()) / 1000));
  if (abs < 60) return 'now';
  if (abs < 3600) return `${Math.round(abs / 60)}m ago`;
  if (abs < 86400) return `${Math.round(abs / 3600)}h ago`;
  if (abs < 86400 * 30) return `${Math.round(abs / 86400)}d ago`;
  return `${Math.round(abs / (86400 * 30))}mo ago`;
}

export function CreateProjectModal({
  isOpen,
  onClose,
  onConfirm,
  isLoading = false,
  initialBaseId,
  baseVersion,
  initialEmptyMode = false,
}: CreateProjectModalProps) {
  const { activeTeam } = useTeam();

  const [projectName, setProjectName] = useState('');
  const [selectedBase, setSelectedBase] = useState<MarketplaceBase | null>(null);
  const [allBases, setAllBases] = useState<MarketplaceBase[]>([]);
  const [userBases, setUserBases] = useState<MarketplaceBase[]>([]);
  const [siblings, setSiblings] = useState<SiblingFolder[]>([]);
  const [hasUserEdits, setHasUserEdits] = useState(false);

  const inputRef = useRef<HTMLInputElement>(null);

  // Reset selection on open. If parent asked for empty mode, pin EMPTY_TILE.
  // Otherwise leave selectedBase null until the bases finish loading.
  useEffect(() => {
    if (!isOpen) return;
    setSelectedBase(initialEmptyMode ? EMPTY_TILE : null);
    setProjectName('');
    setHasUserEdits(false);
  }, [isOpen, initialEmptyMode]);

  // Load templates
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    Promise.all([
      marketplaceApi.getAllBases({ limit: 50 }),
      marketplaceApi.getUserBases().catch(() => ({ bases: [] })),
    ])
      .then(([allBasesRes, userBasesRes]) => {
        if (cancelled) return;
        const bases = (allBasesRes.bases || allBasesRes || []) as MarketplaceBase[];
        const userBasesData = (userBasesRes.bases || userBasesRes || []) as MarketplaceBase[];
        setAllBases(bases);
        setUserBases(userBasesData);
        setSelectedBase((current) => {
          if (current) return current;
          if (initialBaseId) {
            const preselected = bases.find((b) => b.id === initialBaseId);
            if (preselected) return preselected;
          }
          const defaultBase = FEATURED_SLUGS.map((slug) => bases.find((b) => b.slug === slug)).find(
            Boolean
          );
          return defaultBase || EMPTY_TILE;
        });
      })
      .catch((error) => console.error('Failed to load bases:', error));
    return () => {
      cancelled = true;
    };
  }, [isOpen, initialBaseId]);

  // Load existing sibling folders (used in the breadcrumb subtitle).
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    projectsApi
      .getAll(activeTeam?.slug)
      .then((data: unknown) => {
        if (cancelled) return;
        const list = (Array.isArray(data) ? data : []) as Array<Record<string, unknown>>;
        const mapped: SiblingFolder[] = list
          .map((p) => ({
            id: (p.id as string) || '',
            name: (p.name as string) || 'Untitled',
            slug: (p.slug as string) || '',
            updatedAt:
              (p.updated_at as string) || (p.created_at as string) || new Date(0).toISOString(),
          }))
          .filter((p) => p.slug)
          .sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime());
        setSiblings(mapped);
      })
      .catch(() => {
        if (!cancelled) setSiblings([]);
      });
    return () => {
      cancelled = true;
    };
  }, [isOpen, activeTeam?.slug]);

  useEffect(() => {
    if (isOpen) {
      const t = setTimeout(() => inputRef.current?.focus(), 40);
      return () => clearTimeout(t);
    }
  }, [isOpen]);

  // Tiles in render order: Empty first, then the 4 featured templates we have
  // resolved IDs for, then any extras from the user's library.
  const tiles = useMemo<MarketplaceBase[]>(() => {
    const featured = FEATURED_SLUGS.map((slug) => allBases.find((b) => b.slug === slug)).filter(
      Boolean
    ) as MarketplaceBase[];
    const featuredIds = new Set(featured.map((b) => b.id));
    const userOnly = userBases.filter((b) => !featuredIds.has(b.id));
    return [EMPTY_TILE, ...featured, ...userOnly];
  }, [allBases, userBases]);

  const isInLibrary = (baseId: string) => userBases.some((b) => b.id === baseId);

  const handleTileClick = async (base: MarketplaceBase) => {
    setHasUserEdits(true);
    if (base.id === EMPTY_TILE.id) {
      setSelectedBase(EMPTY_TILE);
      inputRef.current?.focus();
      return;
    }
    if (isInLibrary(base.id)) {
      setSelectedBase(base);
      inputRef.current?.focus();
      return;
    }
    try {
      await marketplaceApi.purchaseBase(base.id);
      const userBasesRes = await marketplaceApi.getUserBases();
      setUserBases((userBasesRes.bases || userBasesRes || []) as MarketplaceBase[]);
      setSelectedBase(base);
      inputRef.current?.focus();
    } catch (error) {
      console.error('Failed to add to library:', error);
    }
  };

  const resetAndClose = () => {
    if (isLoading) return;
    setProjectName('');
    setSelectedBase(null);
    setHasUserEdits(false);
    onClose();
  };

  const handleBackdropClick = (e: MouseEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget) return;
    if (hasUserEdits) return;
    resetAndClose();
  };

  const isEmptyMode = selectedBase?.id === EMPTY_TILE.id;
  const trimmedName = projectName.trim();
  const canSubmit = !isLoading && !!trimmedName && !!selectedBase;

  const disabledReason = !trimmedName
    ? 'Enter a workspace name'
    : !selectedBase
      ? 'Pick a template'
      : '';

  const handleConfirm = () => {
    if (!canSubmit) return;
    if (isEmptyMode) {
      onConfirm(trimmedName, '', undefined);
      return;
    }
    onConfirm(trimmedName, selectedBase!.id, baseVersion || undefined);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Enter' && canSubmit) {
      e.preventDefault();
      handleConfirm();
    } else if (e.key === 'Escape' && !isLoading) {
      e.preventDefault();
      resetAndClose();
    }
  };

  const stopPropagation = (e: MouseEvent<HTMLDivElement>) => e.stopPropagation();

  if (!isOpen) return null;

  const teamSlug = activeTeam?.slug || 'team';
  const recent = siblings.slice(0, 3);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-2 backdrop-blur-sm sm:p-4"
      onClick={handleBackdropClick}
      onKeyDown={handleKeyDown}
      role="dialog"
      aria-modal="true"
      aria-labelledby="create-workspace-title"
    >
      <div
        onClick={stopPropagation}
        className="flex max-h-[calc(100dvh-1rem)] w-full max-w-[520px] flex-col overflow-hidden rounded-[var(--radius)] border border-[var(--border)] bg-[var(--surface)] shadow-[var(--shadow-large)] sm:max-h-[calc(100dvh-2rem)]"
      >
        {/* Header */}
        <div className="flex items-start justify-between gap-3 border-b border-[var(--border)] bg-[var(--sidebar-bg)] px-4 py-3">
          <div className="min-w-0">
            <h2
              id="create-workspace-title"
              className="text-[14px] font-semibold text-[var(--text)]"
            >
              New workspace
            </h2>
            <div className="mt-0.5 flex min-w-0 items-center gap-1 text-[11px] text-[var(--text-muted)]">
              <Folder size={11} weight="fill" className="flex-shrink-0 text-[var(--primary)]" />
              <span className="truncate">{teamSlug}</span>
              <CaretRight size={9} className="flex-shrink-0 text-[var(--text-subtle)]" />
              <span className="truncate text-[var(--text-muted)]">
                {recent.length > 0
                  ? `${siblings.length} workspace${siblings.length === 1 ? '' : 's'}`
                  : 'No workspaces yet'}
              </span>
            </div>
          </div>
          <button
            type="button"
            onClick={resetAndClose}
            disabled={isLoading}
            aria-label="Close"
            className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-[var(--radius-small)] text-[var(--text-muted)] hover:bg-[var(--surface-hover)] hover:text-[var(--text)] disabled:opacity-50 motion-safe:transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto bg-[var(--bg)] px-4 py-4">
          {/* Name field */}
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <label
                htmlFor="cw-name"
                className="text-[11px] font-medium uppercase tracking-wide text-[var(--text-muted)]"
              >
                Workspace name
              </label>
              <span
                className={`text-[10px] tabular-nums ${
                  projectName.length > 90
                    ? 'text-[var(--status-warning)]'
                    : 'text-[var(--text-subtle)]'
                }`}
              >
                {projectName.length}/100
              </span>
            </div>
            <input
              ref={inputRef}
              id="cw-name"
              type="text"
              value={projectName}
              onChange={(e) => {
                setProjectName(e.target.value);
                setHasUserEdits(true);
              }}
              placeholder="my-awesome-app"
              disabled={isLoading}
              maxLength={100}
              autoComplete="off"
              spellCheck={false}
              aria-label="Workspace name"
              className="w-full rounded-[var(--radius-small)] border border-[var(--border)] bg-[var(--surface)] px-3 py-2 text-[13px] text-[var(--text)] placeholder:text-[var(--text-subtle)] focus:border-[var(--primary)] focus:outline-none focus:ring-1 focus:ring-[var(--primary)]/30 disabled:opacity-50"
            />
          </div>

          {/* Template grid */}
          <div className="mt-4">
            <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              Starting point
            </label>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
              {tiles.map((base) => {
                const isSelected = selectedBase?.id === base.id;
                const isEmpty = base.id === EMPTY_TILE.id;
                const inLib = isEmpty || isInLibrary(base.id);
                return (
                  <button
                    key={base.id}
                    type="button"
                    onClick={() => handleTileClick(base)}
                    disabled={isLoading}
                    aria-pressed={isSelected}
                    className={[
                      'group flex h-[78px] flex-col items-start gap-1 rounded-[var(--radius-small)] border px-3 py-2 text-left motion-safe:transition-all',
                      isSelected
                        ? 'border-[var(--primary)] bg-[rgba(var(--primary-rgb),0.10)] ring-1 ring-[var(--primary)]/30'
                        : 'border-[var(--border)] bg-[var(--surface)] hover:border-[var(--border-hover)] hover:bg-[var(--surface-hover)]',
                      isLoading ? 'cursor-not-allowed opacity-50' : '',
                    ].join(' ')}
                  >
                    <div className="flex w-full items-center gap-1.5">
                      {isEmpty ? (
                        <FileDashed
                          size={13}
                          weight={isSelected ? 'fill' : 'regular'}
                          className={
                            isSelected ? 'text-[var(--primary)]' : 'text-[var(--text-muted)]'
                          }
                        />
                      ) : (
                        <Folder
                          size={13}
                          weight={isSelected ? 'fill' : 'duotone'}
                          className={
                            isSelected ? 'text-[var(--primary)]' : 'text-[var(--text-muted)]'
                          }
                        />
                      )}
                      <span className="min-w-0 flex-1 truncate text-[12px] font-medium text-[var(--text)]">
                        {base.name}
                      </span>
                      {!inLib && (
                        <span className="flex-shrink-0 text-[9px] uppercase tracking-wide text-[var(--text-subtle)]">
                          add
                        </span>
                      )}
                    </div>
                    <p className="line-clamp-2 text-[10px] leading-snug text-[var(--text-subtle)]">
                      {base.description || (isEmpty ? 'Blank repo, no scaffolding.' : base.slug)}
                    </p>
                  </button>
                );
              })}
            </div>
          </div>

          {recent.length > 0 && (
            <div className="mt-4">
              <div className="mb-1 text-[10px] uppercase tracking-wide text-[var(--text-subtle)]">
                Recent in {teamSlug}
              </div>
              <ul className="space-y-0.5">
                {recent.map((s) => (
                  <li
                    key={s.id}
                    className="flex items-center gap-2 rounded-[var(--radius-small)] px-2 py-1 text-[11px] text-[var(--text-muted)]"
                  >
                    <Folder
                      size={11}
                      weight="duotone"
                      className="flex-shrink-0 text-[var(--text-subtle)]"
                    />
                    <span className="min-w-0 flex-1 truncate">{s.name}</span>
                    <span className="flex-shrink-0 text-[10px] text-[var(--text-subtle)]">
                      {formatRelative(s.updatedAt)}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between gap-2 border-t border-[var(--border)] bg-[var(--sidebar-bg)] px-4 py-2.5">
          <span className="hidden text-[10px] text-[var(--text-subtle)] sm:inline">
            <kbd className="font-mono">esc</kbd> cancel · <kbd className="font-mono">↵</kbd> create
          </span>
          <div className="ml-auto flex items-center gap-2">
            {!canSubmit && disabledReason && (
              <span className="text-[10px] text-[var(--text-subtle)]">{disabledReason}</span>
            )}
            <button
              type="button"
              onClick={resetAndClose}
              disabled={isLoading}
              className="btn btn-sm btn-ghost"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={handleConfirm}
              disabled={!canSubmit}
              aria-disabled={!canSubmit}
              title={!canSubmit ? disabledReason : undefined}
              className="btn btn-sm btn-filled"
            >
              {isLoading ? 'Creating…' : 'Create workspace'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
