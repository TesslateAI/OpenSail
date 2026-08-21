import { useState, useEffect } from 'react';
import { useParams, useNavigate, Link } from 'react-router-dom';
import { useTeam } from '../contexts/TeamContext';
import {
  ArrowLeft,
  Check,
  Download,
  GitFork,
  GithubLogo,
  Globe,
  File,
  FileText,
  FilePlus,
  Terminal,
  ListChecks,
  Pencil,
  Package,
  ChatCircle,
  Trash,
  X,
  ShieldCheck,
  Users,
  Tag,
  ArrowRight,
  GitCommit,
  GitBranch,
  Kanban,
  Gauge,
  MagnifyingGlass,
  PaperPlaneTilt,
  BookOpen,
} from '@phosphor-icons/react';
import { LoadingSpinner } from '../components/PulsingGridSpinner';
import {
  StatsBar,
  type MarketplaceItem,
  parseGitHubRepo,
  AgentCard,
  ReviewCard,
  RatingPicker,
  type Review,
} from '../components/marketplace';
import { marketplaceApi } from '../lib/api';
import toast from 'react-hot-toast';
import {
  SEO,
  generateProductStructuredData,
  generateBreadcrumbStructuredData,
} from '../components/SEO';
import { useMarketplaceAuth } from '../contexts/MarketplaceAuthContext';

function formatRelativeDate(dateStr: string): string {
  const date = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffDays = Math.floor(diffMs / (1000 * 60 * 60 * 24));
  if (diffDays < 1) return 'today';
  if (diffDays === 1) return 'yesterday';
  if (diffDays < 30) return `${diffDays} days ago`;
  const diffMonths = Math.floor(diffDays / 30);
  if (diffMonths < 12) return `${diffMonths} month${diffMonths > 1 ? 's' : ''} ago`;
  const diffYears = Math.floor(diffDays / 365);
  return `${diffYears} year${diffYears > 1 ? 's' : ''} ago`;
}

// Tool icons mapping
const toolIcons: Record<string, { icon: React.ReactNode; label: string }> = {
  read_file: { icon: <File size={14} weight="fill" />, label: 'Read File' },
  write_file: { icon: <FilePlus size={14} weight="fill" />, label: 'Write File' },
  patch_file: { icon: <Pencil size={14} weight="fill" />, label: 'Patch File' },
  multi_edit: { icon: <FileText size={14} weight="fill" />, label: 'Multi-Edit' },
  bash_exec: { icon: <Terminal size={14} weight="fill" />, label: 'Bash' },
  shell_open: { icon: <Terminal size={14} weight="fill" />, label: 'Shell Open' },
  shell_exec: { icon: <Terminal size={14} weight="fill" />, label: 'Shell' },
  shell_close: { icon: <Terminal size={14} weight="fill" />, label: 'Shell Close' },
  get_project_info: { icon: <Package size={14} weight="fill" />, label: 'Project Info' },
  todo_read: { icon: <ListChecks size={14} weight="fill" />, label: 'Todo Read' },
  todo_write: { icon: <ListChecks size={14} weight="fill" />, label: 'Todo Write' },
  web_fetch: { icon: <Globe size={14} weight="fill" />, label: 'Web Fetch' },
  web_search: { icon: <MagnifyingGlass size={14} weight="fill" />, label: 'Web Search' },
  send_message: { icon: <PaperPlaneTilt size={14} weight="fill" />, label: 'Send Message' },
  kanban: { icon: <Kanban size={14} weight="fill" />, label: 'Kanban' },
  project_control: { icon: <Gauge size={14} weight="fill" />, label: 'Project Control' },
  load_skill: { icon: <BookOpen size={14} weight="fill" />, label: 'Load Skill' },
};

const ALL_TOOLS = Object.keys(toolIcons);

export default function MarketplaceDetail() {
  const { slug } = useParams<{ slug: string }>();
  const navigate = useNavigate();
  const { isAuthenticated } = useMarketplaceAuth();
  const { teamSwitchKey } = useTeam();
  const [item, setItem] = useState<MarketplaceItem | null>(null);
  const [relatedItems, setRelatedItems] = useState<MarketplaceItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [uninstalling, setUninstalling] = useState(false);
  const [forking, setForking] = useState(false);

  // Review state
  const [reviews, setReviews] = useState<Review[]>([]);
  const [loadingReviews, setLoadingReviews] = useState(false);
  const [showReviewForm, setShowReviewForm] = useState(false);
  const [reviewRating, setReviewRating] = useState(5);
  const [reviewComment, setReviewComment] = useState('');
  const [submittingReview, setSubmittingReview] = useState(false);
  const [editingReview, setEditingReview] = useState(false);

  // Version state for bases
  const [versions, setVersions] = useState<
    Array<{
      tag: string;
      sha: string;
      date: string | null;
      url: string;
    }>
  >([]);
  const [loadingVersions, setLoadingVersions] = useState(false);
  const [versionsDefaultBranch, setVersionsDefaultBranch] = useState<string | null>(null);

  useEffect(() => {
    if (slug) {
      setLoading(true);
      loadItemDetails();
    }
  }, [slug, teamSwitchKey]);

  // Reload agent details when returning to this page (e.g., after removing from Library)
  useEffect(() => {
    const handleFocus = () => {
      if (slug && item) {
        loadItemDetails();
      }
    };
    window.addEventListener('focus', handleFocus);
    return () => window.removeEventListener('focus', handleFocus);
  }, [slug, item?.id]);

  const loadItemDetails = async () => {
    try {
      // Try to get agent details first
      const data = await marketplaceApi.getAgentDetails(slug!);
      setItem({ ...data, item_type: 'agent' });

      // Load related items using recommendations API (co-install based)
      try {
        const related = await marketplaceApi.getRelatedAgents(slug!, 4);
        setRelatedItems(
          related.map((a: Record<string, unknown>) => ({ ...a, item_type: 'agent' }))
        );
      } catch {
        // Fallback to same category if recommendations fail
        const allAgents = await marketplaceApi.getAllAgents();
        const related = (allAgents.agents || [])
          .filter((a: Record<string, unknown>) => a.slug !== slug && a.category === data.category)
          .slice(0, 4)
          .map((a: Record<string, unknown>) => ({ ...a, item_type: 'agent' }));
        setRelatedItems(related);
      }
    } catch {
      // Try base if agent not found
      try {
        const baseDetail = await marketplaceApi.getBaseDetails(slug!);
        if (baseDetail) {
          setItem({ ...baseDetail, item_type: 'base' });
          // Load related bases from same category
          try {
            const relatedResult = await marketplaceApi.getAllBases({
              category: baseDetail.category || undefined,
              limit: 5,
            });
            const related = (relatedResult.bases || [])
              .filter((b: Record<string, unknown>) => b.slug !== slug)
              .slice(0, 4)
              .map((b: Record<string, unknown>) => ({ ...b, item_type: 'base' }));
            setRelatedItems(related);
          } catch {
            // Non-blocking: related items are optional
          }
        } else {
          toast.error('Extension not found');
          navigate('/marketplace');
        }
      } catch {
        // Try MCP server if base not found
        try {
          const mcpData = await marketplaceApi.getMcpServerDetails(slug!);
          if (mcpData) {
            setItem({ ...mcpData, item_type: 'mcp_server' });
            // Load related MCP servers from same category
            try {
              const relatedResult = await marketplaceApi.getAllMcpServers({
                category: mcpData.category || undefined,
                limit: 5,
              });
              const related = (relatedResult.mcp_servers || [])
                .filter((s: Record<string, unknown>) => s.slug !== slug)
                .slice(0, 4)
                .map((s: Record<string, unknown>) => ({ ...s, item_type: 'mcp_server' }));
              setRelatedItems(related);
            } catch {
              // Non-blocking: related items are optional
            }
          } else {
            toast.error('Extension not found');
            navigate('/marketplace');
          }
        } catch {
          toast.error('Failed to load extension');
          navigate('/marketplace');
        }
      }
    } finally {
      setLoading(false);
    }
  };

  const handleInstall = async () => {
    if (!item) return;

    // Redirect unauthenticated users to sign up
    if (!isAuthenticated) {
      navigate(`/register?redirect=${encodeURIComponent(`/marketplace/${item.slug}`)}`);
      return;
    }

    if (item.is_purchased) {
      toast.success(`${item.name} already in your library`);
      return;
    }

    if (!item.is_active) {
      // Button already shows "Coming Soon" - no toast needed
      return;
    }

    setInstalling(true);
    try {
      const data =
        item.item_type === 'base'
          ? await marketplaceApi.purchaseBase(item.id)
          : item.item_type === 'mcp_server'
            ? await marketplaceApi.installMcpServer(item.id)
            : await marketplaceApi.purchaseAgent(item.id);

      if (data.checkout_url) {
        window.location.href = data.checkout_url;
      } else {
        toast.success(`${item.name} added to your library!`);
        setItem({ ...item, is_purchased: true });
      }
    } catch (error) {
      console.error('Failed to install:', error);
      toast.error('Failed to add to library');
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = async () => {
    if (!item) return;

    setUninstalling(true);
    try {
      await marketplaceApi.removeFromLibrary(item.id);
      toast.success(`${item.name} removed from your library`);
      setItem({ ...item, is_purchased: false });
    } catch (error) {
      console.error('Failed to uninstall:', error);
      toast.error('Failed to remove from library');
    } finally {
      setUninstalling(false);
    }
  };

  const handleRelatedInstall = async (relatedItem: MarketplaceItem) => {
    // Redirect unauthenticated users to sign up
    if (!isAuthenticated) {
      navigate(`/register?redirect=${encodeURIComponent(`/marketplace/${relatedItem.slug}`)}`);
      return;
    }

    if (relatedItem.is_purchased) {
      toast.success(`${relatedItem.name} already in your library`);
      return;
    }

    try {
      const data =
        relatedItem.item_type === 'base'
          ? await marketplaceApi.purchaseBase(relatedItem.id)
          : relatedItem.item_type === 'mcp_server'
            ? await marketplaceApi.installMcpServer(relatedItem.id)
            : await marketplaceApi.purchaseAgent(relatedItem.id);

      if (data.checkout_url) {
        window.location.href = data.checkout_url;
      } else {
        toast.success(`${relatedItem.name} added to your library!`);
        setRelatedItems((prev) =>
          prev.map((i) => (i.id === relatedItem.id ? { ...i, is_purchased: true } : i))
        );
      }
    } catch {
      toast.error('Failed to add to library');
    }
  };

  const handleFork = async () => {
    if (!item || !isAuthenticated) return;

    setForking(true);
    try {
      await marketplaceApi.forkAgent(item.id);
      toast.success(`Forked "${item.name}" to your library! You can now customize it.`);
      navigate('/library?tab=agents');
    } catch (error: unknown) {
      console.error('Fork failed:', error);
      const err = error as { response?: { data?: { detail?: string } } };
      toast.error(err.response?.data?.detail || 'Failed to fork agent');
    } finally {
      setForking(false);
    }
  };

  // Load reviews for the agent
  const loadReviews = async () => {
    if (!item?.id || (item.item_type !== 'agent' && item.item_type !== 'base')) return;

    setLoadingReviews(true);
    try {
      const data =
        item.item_type === 'agent'
          ? await marketplaceApi.getAgentReviews(item.id)
          : await marketplaceApi.getBaseReviews(item.id);
      setReviews(data.reviews || []);

      // Check if user has already reviewed - pre-fill form if so
      const userReview = data.reviews?.find((r: Review) => r.is_own_review);
      if (userReview) {
        setReviewRating(userReview.rating);
        setReviewComment(userReview.comment || '');
        setEditingReview(true);
      }
    } catch (error) {
      console.error('Failed to load reviews:', error);
    } finally {
      setLoadingReviews(false);
    }
  };

  // Submit or update review
  const handleSubmitReview = async () => {
    if (!item?.id) return;

    setSubmittingReview(true);
    try {
      if (item.item_type === 'agent') {
        await marketplaceApi.createAgentReview(item.id, reviewRating, reviewComment || undefined);
      } else {
        await marketplaceApi.createBaseReview(item.id, reviewRating, reviewComment || undefined);
      }
      toast.success(editingReview ? 'Review updated!' : 'Review submitted!');
      setShowReviewForm(false);
      setEditingReview(true);
      loadReviews(); // Reload reviews
    } catch (error: unknown) {
      console.error('Failed to submit review:', error);
      const err = error as { response?: { data?: { detail?: string } } };
      toast.error(err.response?.data?.detail || 'Failed to submit review');
    } finally {
      setSubmittingReview(false);
    }
  };

  // Delete review
  const handleDeleteReview = async () => {
    if (!item?.id) return;

    if (!confirm('Are you sure you want to delete your review?')) return;

    try {
      if (item.item_type === 'agent') {
        await marketplaceApi.deleteAgentReview(item.id);
      } else {
        await marketplaceApi.deleteBaseReview(item.id);
      }
      toast.success('Review deleted');
      setReviewRating(5);
      setReviewComment('');
      setEditingReview(false);
      loadReviews(); // Reload reviews
    } catch (error: unknown) {
      console.error('Failed to delete review:', error);
      const err = error as { response?: { data?: { detail?: string } } };
      toast.error(err.response?.data?.detail || 'Failed to delete review');
    }
  };

  // Load reviews when item loads
  useEffect(() => {
    if (item?.id && (item.item_type === 'agent' || item.item_type === 'base')) {
      loadReviews();
    }
  }, [item?.id]);

  // Load versions for base items with a git repo
  useEffect(() => {
    if (item?.item_type === 'base' && item?.git_repo_url && slug) {
      setLoadingVersions(true);
      marketplaceApi
        .getBaseVersions(slug)
        .then((data) => {
          setVersions(data.versions || []);
          setVersionsDefaultBranch(data.default_branch || null);
        })
        .catch(() => {
          // Non-blocking: versions are optional
          setVersions([]);
        })
        .finally(() => setLoadingVersions(false));
    }
  }, [item?.id, item?.item_type]);

  // Build marketplace back URL preserving the tab the user came from
  const marketplaceBackUrl = item?.item_type
    ? `/marketplace?type=${item.item_type}`
    : '/marketplace';

  const creatorId = item?.forked_by_user_id || item?.created_by_user_id;

  if (loading) {
    return (
      <div className="h-screen flex items-center justify-center bg-[var(--bg)]">
        <LoadingSpinner message="Loading extension..." size={40} />
      </div>
    );
  }

  if (!item) {
    return null;
  }

  // Generate SEO structured data
  const baseUrl = typeof window !== 'undefined' ? window.location.origin : 'https://tesslate.com';
  const productStructuredData = generateProductStructuredData({
    name: item.name,
    description: item.description,
    slug: item.slug,
    price: item.price || 0,
    pricing_type: item.pricing_type,
    rating: item.rating,
    review_count: item.reviews_count,
    creator_name: item.creator_name ?? undefined,
    avatar_url: item.avatar_url ?? undefined,
    category: item.category,
  });

  const breadcrumbData = generateBreadcrumbStructuredData([
    { name: 'Marketplace', url: `${baseUrl}/marketplace` },
    {
      name: item.category || 'Agents',
      url: `${baseUrl}/marketplace/browse/agent?category=${item.category || 'builder'}`,
    },
    { name: item.name, url: `${baseUrl}/marketplace/${item.slug}` },
  ]);

  return (
    <>
      <SEO
        title={`${item.name} - AI ${item.item_type === 'base' ? 'Template' : 'Agent'}`}
        description={
          item.description ||
          `${item.name} is an AI-powered ${item.item_type === 'base' ? 'project template' : 'coding agent'} on Tesslate Marketplace.`
        }
        keywords={[
          item.name,
          item.category || '',
          item.item_type || 'agent',
          'AI agent',
          'Tesslate',
          ...(item.tags || []),
        ].filter(Boolean)}
        image={(item.avatar_url || item.preview_image) ?? undefined}
        url={`${baseUrl}/marketplace/${item.slug}`}
        type="product"
        author={item.creator_name ?? undefined}
        structuredData={{
          '@context': 'https://schema.org',
          '@graph': [productStructuredData, breadcrumbData],
        }}
      />
      <div
        key={teamSwitchKey}
        className="h-screen overflow-y-auto bg-[var(--bg)]"
        style={{ animation: 'fade-in 0.25s ease-out' }}
      >
        {/* Header */}
        <div className="border-b border-[var(--border)] flex-shrink-0">
          <div className="max-w-5xl mx-auto px-6 md:px-12">
            <div className="h-10 flex items-center gap-3">
              <button onClick={() => navigate(marketplaceBackUrl)} className="btn">
                <ArrowLeft size={15} />
                Marketplace
              </button>
            </div>
          </div>
        </div>

        {/* Hero Section */}
        <div className="max-w-5xl mx-auto px-6 md:px-12 py-6">
          <div className="flex flex-col md:flex-row gap-8">
            {/* Icon */}
            <div className="flex-shrink-0">
              <div className="w-16 h-16 rounded-[var(--radius)] bg-[var(--surface-hover)] border border-[var(--border)] flex items-center justify-center overflow-hidden">
                {item.avatar_url ? (
                  <img
                    src={item.avatar_url}
                    alt={item.name}
                    className="w-full h-full object-cover"
                  />
                ) : item.git_repo_url && parseGitHubRepo(item.git_repo_url) ? (
                  <GithubLogo size={48} weight="fill" className="text-[var(--text-muted)]" />
                ) : (
                  <img src="/favicon.svg" alt="Tesslate" className="w-10 h-10" />
                )}
              </div>
            </div>

            {/* Content */}
            <div className="flex-1">
              {/* Title Row */}
              <div className="flex flex-wrap items-start gap-2 sm:gap-3 mb-3">
                <h1 className="font-heading text-xl font-semibold text-[var(--text)]">
                  {item.name}
                </h1>
                <div className="flex items-center gap-2 flex-wrap">
                  {(item.source_type === 'open' ||
                    (item.source_type === 'git' && item.git_repo_url)) && (
                    <span className="flex items-center gap-1.5 bg-[var(--surface-hover)] border border-[var(--border)] text-[var(--text-muted)] rounded-[var(--radius-small)] text-[10px] px-1.5 py-0.5 font-medium whitespace-nowrap">
                      <GitFork size={14} weight="bold" />
                      Open Source
                    </span>
                  )}
                  {item.creator_type === 'community' && (
                    <span className="flex items-center gap-1.5 bg-[var(--surface-hover)] border border-[var(--border)] text-[var(--text-muted)] rounded-[var(--radius-small)] text-[10px] px-1.5 py-0.5 font-medium whitespace-nowrap">
                      <Users size={14} weight="bold" />
                      Community
                    </span>
                  )}
                  {item.creator_type === 'official' && (
                    <span className="flex items-center gap-1.5 bg-[var(--surface-hover)] border border-[var(--border)] text-[var(--text-muted)] rounded-[var(--radius-small)] text-[10px] px-1.5 py-0.5 font-medium whitespace-nowrap">
                      <ShieldCheck size={14} weight="bold" />
                      Official
                    </span>
                  )}
                </div>
              </div>

              {/* Description */}
              <p className="text-xs text-[var(--text-muted)] mb-4">{item.description}</p>

              {/* Author */}
              <div className="flex items-center gap-4 mb-6">
                {creatorId || item.creator_username ? (
                  <Link
                    to={
                      item.creator_username
                        ? `/@${item.creator_username}`
                        : `/marketplace/creator/${creatorId}`
                    }
                    className="flex items-center gap-2 text-[11px] text-[var(--text-muted)] hover:text-[var(--primary)] transition-colors"
                  >
                    <div className="w-6 h-6 rounded-full overflow-hidden flex-shrink-0 bg-[var(--surface-hover)]">
                      {item.creator_avatar_url ? (
                        <img
                          src={item.creator_avatar_url}
                          alt={item.creator_name || 'Creator'}
                          className="w-full h-full object-cover"
                        />
                      ) : (
                        <div className="w-full h-full flex items-center justify-center text-xs font-medium">
                          {item.creator_name?.charAt(0).toUpperCase() || 'T'}
                        </div>
                      )}
                    </div>
                    <span>
                      {item.creator_type === 'official'
                        ? 'Tesslate'
                        : item.creator_username
                          ? `@${item.creator_username}`
                          : item.creator_name || 'Unknown'}
                    </span>
                  </Link>
                ) : (
                  <span className="text-[11px] text-[var(--text-muted)]">By Tesslate</span>
                )}
              </div>

              {/* Install Button */}
              <div className="flex flex-wrap items-center gap-3 sm:gap-4">
                {item.is_purchased ? (
                  <div className="flex items-center gap-3">
                    <span className="btn flex items-center gap-2">
                      <Check size={18} weight="bold" />
                      Installed
                    </span>
                    {item.item_type === 'agent' && (
                      <button
                        onClick={handleUninstall}
                        disabled={uninstalling}
                        className="btn btn-danger"
                      >
                        {uninstalling ? (
                          <>
                            <div className="w-4 h-4 border-2 border-red-400/30 border-t-red-400 rounded-full animate-spin" />
                            Removing...
                          </>
                        ) : (
                          <>
                            <Trash size={18} weight="bold" />
                            Uninstall
                          </>
                        )}
                      </button>
                    )}
                  </div>
                ) : (
                  <button
                    onClick={handleInstall}
                    disabled={!item.is_active || installing}
                    className={
                      item.is_active ? 'btn btn-filled' : 'btn opacity-50 cursor-not-allowed'
                    }
                  >
                    {installing ? (
                      <>
                        <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                        Installing...
                      </>
                    ) : !isAuthenticated ? (
                      <>
                        <Download size={18} weight="bold" />
                        Sign Up to Install
                      </>
                    ) : item.is_active ? (
                      <>
                        <Download size={18} weight="bold" />
                        {item.pricing_type === 'free'
                          ? 'Install Extension'
                          : `Subscribe for $${item.price}/mo`}
                      </>
                    ) : (
                      'Coming Soon'
                    )}
                  </button>
                )}

                {/* Fork Button - for installed open-source agents */}
                {item.is_purchased &&
                  isAuthenticated &&
                  item.source_type === 'open' &&
                  item.is_forkable && (
                    <button onClick={handleFork} disabled={forking} className="btn">
                      {forking ? (
                        <>
                          <div className="w-4 h-4 border-2 border-purple-400/30 border-t-purple-400 rounded-full animate-spin" />
                          Forking...
                        </>
                      ) : (
                        <>
                          <GitFork size={18} weight="bold" />
                          Fork &amp; Customize
                        </>
                      )}
                    </button>
                  )}

                {/* Price Badge */}
                {item.pricing_type === 'free' && (
                  <span className="text-xs text-[var(--text-subtle)]">Free</span>
                )}
              </div>
            </div>
          </div>
        </div>

        {/* Stats Bar */}
        <div className="max-w-5xl mx-auto px-6 md:px-12 mb-8">
          <StatsBar usageCount={item.usage_count || 0} category={item.category} />
        </div>

        {/* Versions Section (bases with git repos only) */}
        {item.item_type === 'base' && item.git_repo_url && (
          <div className="max-w-5xl mx-auto px-6 md:px-12 mb-8">
            <h2 className="text-sm font-semibold text-[var(--text)] mb-4">Versions</h2>
            {loadingVersions ? (
              <div className="flex items-center gap-2 py-4 text-xs text-[var(--text-subtle)]">
                <div className="w-4 h-4 border-2 border-current border-t-transparent rounded-full animate-spin" />
                Loading versions...
              </div>
            ) : versions.length > 0 ? (
              <div className="space-y-2">
                {versions.map((version, idx) => (
                  <div
                    key={version.tag}
                    className="flex items-center justify-between gap-4 px-4 py-3 bg-[var(--surface-hover)] hover:bg-[var(--surface)] rounded-[var(--radius-small)] transition-colors"
                  >
                    <div className="flex items-center gap-3 min-w-0">
                      <Tag size={16} weight="fill" className="text-[var(--text-subtle)]" />
                      <span className="font-semibold text-sm text-[var(--text)]">
                        {version.tag}
                      </span>
                      {idx === 0 && (
                        <span className="px-2 py-0.5 bg-[var(--status-success)]/15 text-[var(--status-success)] text-xs rounded-[var(--radius-small)] font-medium">
                          latest
                        </span>
                      )}
                      <span className="flex items-center gap-1 text-xs text-[var(--text-subtle)]">
                        <GitCommit size={12} />
                        {version.sha}
                      </span>
                      {version.date && (
                        <span className="text-xs text-[var(--text-subtle)]">
                          {formatRelativeDate(version.date)}
                        </span>
                      )}
                    </div>
                    {isAuthenticated && (
                      <button
                        onClick={() => {
                          navigate(
                            `/dashboard?create=true&base_id=${item.id}&base_version=${encodeURIComponent(version.tag)}&base_name=${encodeURIComponent(item.name)}`
                          );
                        }}
                        className="btn btn-sm flex-shrink-0"
                      >
                        Use This Version
                        <ArrowRight size={12} weight="bold" />
                      </button>
                    )}
                  </div>
                ))}
              </div>
            ) : (
              <div className="flex items-center justify-between gap-4 px-4 py-3 bg-[var(--surface-hover)] hover:bg-[var(--surface)] rounded-[var(--radius-small)] transition-colors">
                <div className="flex items-center gap-3 min-w-0">
                  <GitBranch size={16} weight="fill" className="text-[var(--text-subtle)]" />
                  <span className="font-semibold text-sm text-[var(--text)]">
                    {versionsDefaultBranch || 'main'}
                  </span>
                  <span className="px-2 py-0.5 bg-[var(--surface-hover)] text-[var(--text-muted)] text-xs rounded-[var(--radius-small)] font-medium">
                    default branch
                  </span>
                </div>
                {isAuthenticated && (
                  <button
                    onClick={() => {
                      navigate(
                        `/dashboard?create=true&base_id=${item.id}&base_name=${encodeURIComponent(item.name)}`
                      );
                    }}
                    className="btn btn-sm flex-shrink-0"
                  >
                    Use This Version
                    <ArrowRight size={12} weight="bold" />
                  </button>
                )}
              </div>
            )}
          </div>
        )}

        {/* Content Sections */}
        <div className="max-w-5xl mx-auto px-6 md:px-12 pb-16">
          {/* Long Description */}
          {item.long_description && (
            <section className="mb-8">
              <h2 className="text-sm font-semibold text-[var(--text)] mb-4">About</h2>
              <div>
                <p className="text-xs text-[var(--text-muted)] leading-relaxed">
                  {item.long_description}
                </p>
              </div>
            </section>
          )}

          {/* Features */}
          {item.features && item.features.length > 0 && (
            <section className="mb-8">
              <h2 className="text-sm font-semibold text-[var(--text)] mb-4">Features</h2>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                {item.features.map((feature, idx) => (
                  <div key={idx} className="flex items-center gap-3">
                    <Check
                      size={18}
                      className="text-[var(--status-success)] flex-shrink-0"
                      weight="bold"
                    />
                    <span className="text-xs text-[var(--text-muted)]">{feature}</span>
                  </div>
                ))}
              </div>
            </section>
          )}

          {/* Tools (for agents) */}
          {item.item_type === 'agent' && (
            <section className="mb-8">
              <h2 className="text-sm font-semibold text-[var(--text)] mb-4">Available Tools</h2>
              <div className="flex flex-wrap items-start gap-2">
                {(item.tools && item.tools.length > 0 ? item.tools : ALL_TOOLS).map(
                  (toolName, idx) => {
                    const tool = toolIcons[toolName];
                    if (!tool) return null;
                    return (
                      <div
                        key={idx}
                        className="flex flex-shrink-0 items-center gap-2 px-3 py-2 bg-[var(--surface-hover)] border border-[var(--border)] text-[var(--text-muted)] rounded-[var(--radius-small)] text-[11px] whitespace-nowrap"
                      >
                        {tool.icon}
                        <span className="font-medium">{tool.label}</span>
                      </div>
                    );
                  }
                )}
              </div>
            </section>
          )}

          {/* Tags */}
          {item.tags && item.tags.length > 0 && (
            <section className="mb-8">
              <h2 className="text-sm font-semibold text-[var(--text)] mb-4">Tags</h2>
              <div className="flex flex-wrap gap-2">
                {item.tags.map((tag, idx) => (
                  <button
                    key={idx}
                    onClick={() => navigate(`/marketplace?search=${encodeURIComponent(tag)}`)}
                    className="btn btn-sm"
                  >
                    {tag}
                  </button>
                ))}
              </div>
            </section>
          )}

          {/* Reviews Section (Agents and Bases) */}
          {(item.item_type === 'agent' || item.item_type === 'base') && (
            <section className="mb-8">
              <div className="flex items-center justify-between mb-6">
                <h2 className="text-sm font-semibold text-[var(--text)]">Reviews</h2>
                {item.is_purchased && !showReviewForm && (
                  <button onClick={() => setShowReviewForm(true)} className="btn">
                    <ChatCircle size={16} weight="fill" />
                    {editingReview ? 'Edit Review' : 'Write a Review'}
                  </button>
                )}
              </div>

              {/* Review Form */}
              {showReviewForm && (
                <div className="p-6 rounded-[var(--radius)] bg-[var(--surface)] mb-6">
                  <div className="flex items-center justify-between mb-4">
                    <h3 className="font-semibold text-[var(--text)]">
                      {editingReview ? 'Edit Your Review' : 'Write a Review'}
                    </h3>
                    <button onClick={() => setShowReviewForm(false)} className="btn">
                      <X size={18} className="text-[var(--text-subtle)]" />
                    </button>
                  </div>

                  <div className="mb-4">
                    <label className="block text-xs font-medium mb-2 text-[var(--text-muted)]">
                      Rating
                    </label>
                    <RatingPicker value={reviewRating} onChange={setReviewRating} />
                  </div>

                  <div className="mb-4">
                    <label className="block text-xs font-medium mb-2 text-[var(--text-muted)]">
                      Comment (optional)
                    </label>
                    <textarea
                      value={reviewComment}
                      onChange={(e) => setReviewComment(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' && !e.shiftKey) {
                          e.preventDefault();
                          handleSubmitReview();
                        }
                      }}
                      placeholder="Share your experience with this extension... (Press Enter to submit, Shift+Enter for new line)"
                      rows={4}
                      className="w-full px-4 py-3 rounded-[var(--radius-small)] text-sm bg-[var(--surface-hover)] border border-[var(--border)] text-[var(--text)] placeholder:text-[var(--text-subtle)] focus:outline-none focus:ring-2 focus:ring-[var(--primary)]/50 resize-none"
                    />
                  </div>

                  <div className="flex items-center gap-3">
                    <button
                      onClick={handleSubmitReview}
                      disabled={submittingReview}
                      className="btn btn-filled disabled:opacity-50"
                    >
                      {submittingReview ? (
                        <>
                          <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                          Submitting...
                        </>
                      ) : (
                        <>
                          <Check size={16} weight="bold" />
                          {editingReview ? 'Update Review' : 'Submit Review'}
                        </>
                      )}
                    </button>
                    {editingReview && (
                      <button onClick={handleDeleteReview} className="btn btn-danger">
                        <Trash size={16} weight="bold" />
                        Delete
                      </button>
                    )}
                  </div>
                </div>
              )}

              {/* Reviews List */}
              {loadingReviews ? (
                <div className="text-center py-8 text-[var(--text-subtle)]">
                  <div className="w-6 h-6 border-2 border-current border-t-transparent rounded-full animate-spin mx-auto mb-2" />
                  Loading reviews...
                </div>
              ) : reviews.length > 0 ? (
                <div className="space-y-4">
                  {reviews.map((review) => (
                    <ReviewCard
                      key={review.id}
                      review={review}
                      onEdit={review.is_own_review ? () => setShowReviewForm(true) : undefined}
                      onDelete={review.is_own_review ? handleDeleteReview : undefined}
                    />
                  ))}
                </div>
              ) : (
                <div className="text-center py-12 rounded-[var(--radius)] bg-[var(--surface)]">
                  <ChatCircle
                    size={40}
                    weight="duotone"
                    className="mx-auto mb-3 text-[var(--text-subtle)]"
                  />
                  <p className="text-xs text-[var(--text-subtle)]">
                    No reviews yet. {item.is_purchased && 'Be the first to review!'}
                  </p>
                </div>
              )}
            </section>
          )}

          {/* Related Items */}
          {relatedItems.length > 0 && (
            <section>
              <h2 className="text-sm font-semibold text-[var(--text)] mb-6">People also like</h2>
              <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5">
                {relatedItems.map((relatedItem) => (
                  <AgentCard
                    key={relatedItem.id}
                    item={relatedItem}
                    onInstall={handleRelatedInstall}
                    isAuthenticated={isAuthenticated}
                  />
                ))}
              </div>
            </section>
          )}
        </div>
      </div>
    </>
  );
}
