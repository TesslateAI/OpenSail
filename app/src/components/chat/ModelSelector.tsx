import { useState, useRef, useEffect, useMemo, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { Cpu, MagnifyingGlass, CaretDown, Check, Lightning, X } from '@phosphor-icons/react';
import { marketplaceApi } from '../../lib/api';
import { type ChatAgent } from '../../types/chat';

interface ModelInfo {
  id: string;
  name?: string;
  source?: string;
  provider?: string;
  provider_name?: string;
  pricing: { input: number; output: number } | null;
  health?: 'healthy' | 'unhealthy' | 'timeout' | null;
}

interface ModelSelectorProps {
  /** Provide either currentAgent OR value for standalone usage */
  currentAgent?: ChatAgent;
  /** Direct model ID — alternative to currentAgent for standalone usage */
  value?: string;
  onModelChange: (model: string) => void;
  compact?: boolean;
  /** When true (default), dropdown opens upward; when false, opens downward */
  dropUp?: boolean;
}

/** Extract the raw model name from a full ID (e.g. "openai/gpt-4o" → "gpt-4o") */
function rawModelName(id: string): string {
  const parts = id.split('/');
  return parts[parts.length - 1];
}

/** Compact display for the trigger button */
function formatButtonLabel(model: ModelInfo): string {
  return model.name ? rawModelName(model.name) : rawModelName(model.id);
}

/** Convert USD per 1M tokens to credits (1 credit = $0.01) */
function formatCredits(usdPer1M: number): string {
  const credits = usdPer1M * 100;
  if (credits === 0) return '0';
  if (Number.isInteger(credits)) return credits.toLocaleString();
  return credits.toFixed(1);
}

/** Get a friendly provider label */
function getProviderLabel(provider: string, providerName?: string): string {
  if (providerName) return providerName;
  const labels: Record<string, string> = {
    internal: 'Tesslate',
    openai: 'OpenAI',
    anthropic: 'Anthropic',
    groq: 'Groq',
    together: 'Together AI',
    deepseek: 'DeepSeek',
    fireworks: 'Fireworks',
    openrouter: 'OpenRouter',
    requesty: 'Requesty',
    'nano-gpt': 'NanoGPT',
  };
  return labels[provider] || provider.charAt(0).toUpperCase() + provider.slice(1);
}

/** Provider sort order — system first, then alphabetical */
function providerOrder(provider: string): number {
  const order: Record<string, number> = { internal: 0, openai: 1, anthropic: 2 };
  return order[provider] ?? 10;
}

export function ModelSelector({
  currentAgent,
  value,
  onModelChange,
  compact = false,
  dropUp = true,
}: ModelSelectorProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [fetchError, setFetchError] = useState(false);
  const [hasFetched, setHasFetched] = useState(false);
  const lastFetchedAt = useRef<number>(0);
  const [search, setSearch] = useState('');
  const [activeTab, setActiveTab] = useState<string | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const activeModel = value || currentAgent?.selectedModel || currentAgent?.model || '';
  const isReadOnly = currentAgent
    ? currentAgent.sourceType === 'closed' && !currentAgent.isCustom
    : false;

  // Position state for portal-based dropdown
  const [dropdownPos, setDropdownPos] = useState<{
    top: number;
    left: number;
    width: number;
  } | null>(null);

  // Recalculate position when open
  useEffect(() => {
    if (!isOpen || !triggerRef.current) return;

    const updatePos = () => {
      const rect = triggerRef.current!.getBoundingClientRect();
      // Clamp horizontal position so dropdown doesn't overflow viewport
      const dropdownWidth = Math.min(480, window.innerWidth - 16);
      let left = rect.left;
      if (left + dropdownWidth > window.innerWidth - 8) {
        left = window.innerWidth - 8 - dropdownWidth;
      }
      if (left < 8) left = 8;

      if (dropUp) {
        setDropdownPos({ top: rect.top - 8, left, width: dropdownWidth });
      } else {
        setDropdownPos({ top: rect.bottom + 8, left, width: dropdownWidth });
      }
    };

    updatePos();

    // Reposition on scroll/resize (any ancestor could scroll)
    window.addEventListener('scroll', updatePos, true);
    window.addEventListener('resize', updatePos);
    return () => {
      window.removeEventListener('scroll', updatePos, true);
      window.removeEventListener('resize', updatePos);
    };
  }, [isOpen, dropUp]);

  // Close dropdown on click outside or window losing focus
  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      const target = event.target as Node;
      if (
        triggerRef.current &&
        !triggerRef.current.contains(target) &&
        dropdownRef.current &&
        !dropdownRef.current.contains(target)
      ) {
        setIsOpen(false);
      }
    };
    const handleBlur = () => setIsOpen(false);

    document.addEventListener('mousedown', handleClickOutside);
    window.addEventListener('blur', handleBlur);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      window.removeEventListener('blur', handleBlur);
    };
  }, [isOpen]);

  // Focus search on open
  useEffect(() => {
    if (isOpen && searchRef.current) {
      setTimeout(() => searchRef.current?.focus(), 50);
    }
    if (!isOpen) {
      setSearch('');
      setActiveTab(null);
    }
  }, [isOpen]);

  // Fetch models on first open
  const handleToggle = useCallback(async () => {
    if (isReadOnly) return;

    if (isOpen) {
      setIsOpen(false);
      return;
    }

    setIsOpen(true);

    // Refetch if never fetched or if data is stale (>5 min)
    const STALE_MS = 5 * 60 * 1000;
    const isStale = Date.now() - lastFetchedAt.current > STALE_MS;

    if (!hasFetched || isStale) {
      if (!hasFetched) setIsLoading(true);
      setFetchError(false);
      try {
        const data = await marketplaceApi.getAvailableModels();
        const raw: unknown[] = Array.isArray(data) ? data : data.models || [];
        const modelList = raw
          .map((m) => {
            if (typeof m === 'string') return { id: m, pricing: null };
            const obj = m as Record<string, unknown>;
            const id = obj.id as string;
            const pricing = (obj.pricing as { input: number; output: number }) || null;
            return id
              ? {
                  id,
                  name: (obj.name as string) ?? undefined,
                  source: (obj.source as string) ?? undefined,
                  provider: (obj.provider as string) ?? undefined,
                  provider_name: (obj.provider_name as string) ?? undefined,
                  pricing,
                  health: (obj.health as ModelInfo['health']) ?? undefined,
                }
              : null;
          })
          .filter((m) => m !== null) as ModelInfo[];
        setModels(modelList);
        setHasFetched(true);
        lastFetchedAt.current = Date.now();
      } catch (error) {
        console.error('Failed to fetch models:', error);
        setFetchError(true);
      } finally {
        setIsLoading(false);
      }
    }
  }, [isOpen, isReadOnly, hasFetched]);

  const handleSelect = (model: string) => {
    onModelChange(model);
    setIsOpen(false);
  };

  // Build display list: filter disabled, ensure current model is always visible
  const allModels = useMemo(() => {
    if (!hasFetched) return [];
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const list = models.filter((m: any) => !m.disabled);
    if (!activeModel) return list;
    const active = list.find((m) => m.id === activeModel) ?? { id: activeModel, pricing: null, provider: 'internal' };
    const rest = list.filter((m) => m.id !== activeModel);
    return [active, ...rest];
  }, [hasFetched, models, activeModel]);

  // Get unique providers for tabs
  const providers = useMemo(() => {
    const seen = new Map<string, string>();
    for (const m of allModels) {
      const p = m.provider || 'internal';
      if (!seen.has(p)) {
        seen.set(p, getProviderLabel(p, m.provider_name));
      }
    }
    return Array.from(seen.entries())
      .map(([id, label]) => ({ id, label }))
      .sort((a, b) => providerOrder(a.id) - providerOrder(b.id));
  }, [allModels]);

  // Filter models by search + active tab
  const filteredModels = useMemo(() => {
    let filtered = allModels;

    if (activeTab) {
      filtered = filtered.filter((m) => (m.provider || 'internal') === activeTab);
    }

    if (search.trim()) {
      const q = search.toLowerCase();
      filtered = filtered.filter(
        (m) =>
          m.id.toLowerCase().includes(q) ||
          (m.name && m.name.toLowerCase().includes(q)) ||
          (m.provider_name && m.provider_name.toLowerCase().includes(q))
      );
    }

    return filtered;
  }, [allModels, activeTab, search]);

  // Group filtered models by provider for display
  const groupedModels = useMemo(() => {
    if (activeTab) return null; // Don't group when a specific tab is selected
    const groups = new Map<string, { label: string; models: ModelInfo[] }>();
    for (const m of filteredModels) {
      const p = m.provider || 'internal';
      if (!groups.has(p)) {
        groups.set(p, { label: getProviderLabel(p, m.provider_name), models: [] });
      }
      groups.get(p)!.models.push(m);
    }
    // Sort groups by provider order
    return Array.from(groups.entries())
      .sort(([a], [b]) => providerOrder(a) - providerOrder(b))
      .map(([id, g]) => {
        const sorted = activeModel
          ? [...g.models].sort((a, b) =>
              a.id === activeModel ? -1 : b.id === activeModel ? 1 : 0
            )
          : g.models;
        return { id, ...g, models: sorted };
      });
  }, [filteredModels, activeTab, activeModel]);

  // Find the active model's info for button label
  const activeModelInfo = useMemo(() => {
    if (allModels.length > 0) {
      const found = allModels.find((m) => m.id === activeModel);
      if (found) return found;
    }
    let fallbackProvider: string;
    if (activeModel.startsWith('custom/')) {
      const rest = activeModel.slice('custom/'.length);
      const slashIdx = rest.indexOf('/');
      fallbackProvider = slashIdx > 0 ? rest.substring(0, slashIdx) : rest;
    } else {
      const slashIdx = activeModel.indexOf('/');
      fallbackProvider = slashIdx > 0 ? activeModel.substring(0, slashIdx) : 'internal';
    }
    const normalizedProvider = fallbackProvider === 'builtin' ? 'internal' : fallbackProvider;
    return { id: activeModel, pricing: null, provider: normalizedProvider } as ModelInfo;
  }, [allModels, activeModel]);

  // Count models per provider for tab badges
  const providerCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const m of allModels) {
      const p = m.provider || 'internal';
      counts.set(p, (counts.get(p) || 0) + 1);
    }
    return counts;
  }, [allModels]);

  // No model info at all — hide the selector
  if (!activeModel) return null;

  // Dropdown rendered via portal to escape overflow:auto ancestors
  const dropdownContent =
    isOpen && !isReadOnly && dropdownPos
      ? createPortal(
          <div
            ref={dropdownRef}
            style={{
              position: 'fixed',
              top: dropUp ? undefined : dropdownPos.top,
              bottom: dropUp ? window.innerHeight - dropdownPos.top : undefined,
              left: dropdownPos.left,
              width: dropdownPos.width,
            }}
            className="bg-[var(--surface)] border border-[var(--border-hover)] rounded-[var(--radius)] max-h-[420px] z-[10000] flex flex-col overflow-hidden"
          >
            {/* Search + provider filter pills */}
            <div className="px-3 pt-3 pb-2 flex-shrink-0 space-y-2">
              <div className="relative">
                <MagnifyingGlass
                  size={14}
                  className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[var(--text-subtle)]"
                />
                <input
                  ref={searchRef}
                  type="text"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  placeholder="Search models..."
                  className="w-full pl-8 pr-8 py-1.5 bg-[var(--bg)] border border-[var(--border)] rounded-[var(--radius-small)] text-xs text-[var(--text)] placeholder-[var(--text-subtle)] focus:outline-none focus:border-[var(--border-hover)] transition-colors"
                  onKeyDown={(e) => {
                    if (e.key === 'Escape') setIsOpen(false);
                  }}
                />
                {search && (
                  <button
                    type="button"
                    onClick={() => setSearch('')}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--text-subtle)] hover:text-[var(--text-muted)] transition-colors"
                  >
                    <X size={12} />
                  </button>
                )}
              </div>

              {/* Provider filter pills — horizontal scrollable */}
              {providers.length > 1 && (
                <div
                  className="flex items-center gap-1 overflow-x-auto scrollbar-none"
                  style={{
                    maskImage: 'linear-gradient(to right, black calc(100% - 12px), transparent)',
                    WebkitMaskImage:
                      'linear-gradient(to right, black calc(100% - 12px), transparent)',
                  }}
                >
                  <button
                    type="button"
                    onClick={() => setActiveTab(null)}
                    className={`btn btn-sm shrink-0 ${activeTab === null ? 'btn-tab-active' : 'btn-tab'}`}
                  >
                    All
                    <span className="text-[10px] opacity-50 ml-0.5">{allModels.length}</span>
                  </button>
                  {providers.map((p) => (
                    <button
                      type="button"
                      key={p.id}
                      onClick={() => setActiveTab(activeTab === p.id ? null : p.id)}
                      className={`btn btn-sm shrink-0 ${activeTab === p.id ? 'btn-tab-active' : 'btn-tab'}`}
                    >
                      {p.label}
                      <span className="text-[10px] opacity-50 ml-0.5">
                        {providerCounts.get(p.id) || 0}
                      </span>
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Divider */}
            <div className="h-px bg-[var(--border)] flex-shrink-0" />

            {/* Model list */}
            <div className="flex-1 overflow-y-auto overscroll-contain min-h-0">
              {isLoading ? (
                <div className="px-4 py-8 text-center">
                  <div className="inline-block w-4 h-4 border-2 border-[var(--border)] border-t-[var(--primary)] rounded-full animate-spin mb-2" />
                  <div className="text-[11px] text-[var(--text-subtle)]">Loading models...</div>
                </div>
              ) : fetchError ? (
                <div className="px-4 py-8 text-center">
                  <div className="text-xs text-[var(--text-muted)] mb-2">Failed to load models</div>
                  <button
                    type="button"
                    onClick={() => {
                      setHasFetched(false);
                      handleToggle();
                    }}
                    className="text-xs text-[var(--primary)] hover:underline"
                  >
                    Retry
                  </button>
                </div>
              ) : filteredModels.length === 0 ? (
                <div className="px-4 py-8 text-center">
                  <MagnifyingGlass size={20} className="mx-auto mb-2 text-[var(--text-subtle)]" />
                  <div className="text-xs text-[var(--text-muted)]">
                    {search.trim() ? 'No models match your search' : 'No models available'}
                  </div>
                </div>
              ) : groupedModels && !search.trim() ? (
                /* Grouped by provider when showing all */
                <div className="py-1">
                  {groupedModels.map((group) => (
                    <div key={group.id}>
                      <div className="px-3 pt-2.5 pb-1 text-[10px] font-semibold text-[var(--text-subtle)] uppercase tracking-wider">
                        {group.label}
                      </div>
                      {group.models.map((model) => (
                        <ModelRow
                          key={model.id}
                          model={model}
                          isActive={model.id === activeModel}
                          onSelect={handleSelect}
                        />
                      ))}
                    </div>
                  ))}
                </div>
              ) : (
                /* Flat list when filtered by tab or search */
                <div className="py-1">
                  {filteredModels.map((model) => (
                    <ModelRow
                      key={model.id}
                      model={model}
                      isActive={model.id === activeModel}
                      onSelect={handleSelect}
                      showProvider={!activeTab}
                    />
                  ))}
                </div>
              )}
            </div>

            {/* Footer — active model indicator */}
            {activeModel && (
              <>
                <div className="h-px bg-[var(--border)] flex-shrink-0" />
                <div className="px-3 py-2 flex items-center gap-2 flex-shrink-0">
                  <span className="w-1.5 h-1.5 rounded-full bg-[var(--primary)] flex-shrink-0" />
                  <span className="text-[10px] text-[var(--text-muted)] truncate">
                    Active: {formatButtonLabel(activeModelInfo)}
                  </span>
                  {activeModelInfo.provider && (
                    <span className="text-[10px] text-[var(--text-subtle)] ml-auto flex-shrink-0">
                      {getProviderLabel(activeModelInfo.provider, activeModelInfo.provider_name)}
                    </span>
                  )}
                </div>
              </>
            )}
          </div>,
          document.body
        )
      : null;

  return (
    <div className="relative" onFocus={(e) => e.stopPropagation()}>
      <button
        ref={triggerRef}
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          handleToggle();
        }}
        disabled={isReadOnly}
        className={`
          flex items-center gap-1.5
          transition-all
          text-xs font-medium
          h-7
          rounded-full
          border border-[var(--border)]
          overflow-hidden
          max-w-[220px]
          ${compact ? 'px-2' : 'px-3'}
          ${
            isReadOnly
              ? 'text-[var(--text-subtle)] cursor-default bg-[var(--surface)]'
              : isOpen
                ? 'text-[var(--text)] bg-[var(--surface-hover)] border-[var(--border-hover)]'
                : 'text-[var(--text-muted)] bg-[var(--surface)] hover:bg-[var(--surface-hover)] hover:text-[var(--text)]'
          }
        `}
        title={isReadOnly ? `Model: ${activeModel} (not changeable)` : `Model: ${activeModel}`}
      >
        <Cpu size={14} weight="bold" className="flex-shrink-0" />
        {!compact && (
          <span className="truncate max-w-[180px]">{formatButtonLabel(activeModelInfo)}</span>
        )}
        {!compact && !isReadOnly && (
          <CaretDown
            size={12}
            weight="bold"
            className={`flex-shrink-0 transition-transform ${isOpen ? 'rotate-180' : ''}`}
          />
        )}
      </button>

      {dropdownContent}
    </div>
  );
}

function ModelRow({
  model,
  isActive,
  onSelect,
  showProvider = false,
}: {
  model: ModelInfo;
  isActive: boolean;
  onSelect: (id: string) => void;
  showProvider?: boolean;
}) {
  const isFree = model.pricing != null && model.pricing.input === 0 && model.pricing.output === 0;
  const modelName = model.name ? rawModelName(model.name) : rawModelName(model.id);
  const providerLabel = getProviderLabel(model.provider || 'internal', model.provider_name);

  return (
    <button
      type="button"
      onClick={() => onSelect(model.id)}
      className={`
        w-full px-3 py-2 flex items-center gap-2.5
        text-xs transition-colors group rounded-[var(--radius-small)] mx-0
        ${isActive ? 'bg-[var(--surface-hover)] text-[var(--text)]' : 'text-[var(--text-muted)] hover:bg-[var(--surface-hover)] hover:text-[var(--text)]'}
      `}
    >
      {/* Health / active dot */}
      <span
        className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${
          model.health === 'unhealthy' || model.health === 'timeout'
            ? 'bg-[var(--status-error)]'
            : model.health === 'healthy'
              ? 'bg-[var(--status-success)]'
              : isActive
                ? 'bg-[var(--primary)]'
                : 'bg-[var(--text-subtle)] group-hover:bg-[var(--text-muted)]'
        }`}
      />

      <div className="flex-1 text-left min-w-0">
        <div className="truncate text-xs leading-tight">
          {showProvider && <span className="text-[var(--text-subtle)]">{providerLabel} / </span>}
          {modelName}
        </div>
        {model.pricing != null && (
          <div className="text-[10px] mt-0.5 leading-tight">
            {isFree ? (
              <span className="text-[var(--status-success)] inline-flex items-center gap-0.5">
                <Lightning size={9} weight="fill" />
                Free
              </span>
            ) : (
              <span className="text-[var(--text-subtle)]">
                {formatCredits(model.pricing.input)} / {formatCredits(model.pricing.output)} credits
                per 1M
              </span>
            )}
          </div>
        )}
      </div>

      {isActive && (
        <Check size={14} weight="bold" className="text-[var(--primary)] flex-shrink-0" />
      )}
    </button>
  );
}
