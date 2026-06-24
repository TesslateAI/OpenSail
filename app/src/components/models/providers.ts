/**
 * Provider visual metadata — brand colors and icon masks for the Models tab.
 * Mirrors `app/src/components/channels/platforms.ts` so provider tiles read
 * the same way channel tiles do (brand-tinted chip + mask-image silhouette).
 *
 * `iconUrl` is consumed via `mask-image: url(...)`, so any single-color SVG
 * works. Most resolve to simpleicons.org; the rest ship as inline data URIs
 * for providers simpleicons doesn't catalog.
 */

export interface ProviderMeta {
  /** Slug matching the orchestrator's provider id (e.g. 'openai', 'anthropic'). */
  key: string;
  /** Display name. */
  name: string;
  /** Hex color used at `${brandColor}1a` for chip background and at full strength for the mask fill. */
  brandColor: string;
  /** URL or data URI of a single-color silhouette SVG used as a CSS mask. */
  iconUrl: string;
  /** Optional public website for the provider header link. */
  website?: string;
}

const simpleicons = (slug: string) => `https://cdn.simpleicons.org/${slug}`;

/** Inline SVG → data URI helper. Use minimal monochrome glyphs (paths only). */
const dataUri = (svg: string) =>
  `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;

const TESSLATE_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M3 4h18v3H14.5v13h-5V7H3z"/>' +
    '</svg>'
);

const GROQ_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M12 2a10 10 0 1 0 10 10A10 10 0 0 0 12 2zm0 4.2a3.6 3.6 0 0 1 3.6 3.6v3.6h-2.4V9.8a1.2 1.2 0 1 0-2.4 0v6h-2.4v-6A3.6 3.6 0 0 1 12 6.2zm3.6 11.4h-2.4v-2.4h2.4z"/>' +
    '</svg>'
);

const TOGETHER_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<circle cx="6" cy="6" r="3"/>' +
    '<circle cx="18" cy="6" r="3"/>' +
    '<circle cx="6" cy="18" r="3"/>' +
    '<circle cx="18" cy="18" r="3"/>' +
    '<circle cx="12" cy="12" r="3"/>' +
    '</svg>'
);

const DEEPSEEK_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M22.94 6.41a1.34 1.34 0 0 0-2.16-.36 5.41 5.41 0 0 1-2.21 1.36c-1-2.6-3.78-4.4-7-4.4a8 8 0 0 0-2.08.27.4.4 0 0 0-.16.69 4.41 4.41 0 0 1 1.6 2.18.5.5 0 0 1-.61.6A6.86 6.86 0 0 0 5.6 7.9a6.55 6.55 0 0 0-3.2 5.6 6.84 6.84 0 0 0 7 6.84c4.66 0 7.6-3.18 7.6-6.94a8.14 8.14 0 0 0-.42-2.6 7.62 7.62 0 0 0 5.27-2.45 1.34 1.34 0 0 0 .09-1.94zM10.4 15.5a1.5 1.5 0 1 1 1.5-1.5 1.5 1.5 0 0 1-1.5 1.5z"/>' +
    '</svg>'
);

const FIREWORKS_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M12 2 9.5 9.5 2 12l7.5 2.5L12 22l2.5-7.5L22 12l-7.5-2.5z"/>' +
    '</svg>'
);

const OPENROUTER_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M3 12h6.5a3 3 0 0 1 3-3h2a3 3 0 0 0 3-3h3.5v2H17a1 1 0 0 0-1 1 5 5 0 0 1-5 5H9.5v3l-5-4zm5 5v-3l-5 4 5 4v-3h2.5a5 5 0 0 0 5-5 1 1 0 0 1 1-1H21v-2h-3.5a3 3 0 0 0-3 3 3 3 0 0 1-3 3z"/>' +
    '</svg>'
);

const REQUESTY_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M4 4h9a5 5 0 0 1 2 9.58L19 20h-3l-3.5-6H8v6H4zm4 3v4h5a2 2 0 0 0 0-4z"/>' +
    '</svg>'
);

const NANOGPT_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M5 4h3l8 11V4h3v16h-3L8 9v11H5z"/>' +
    '</svg>'
);

const ZAI_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M5 4h14v3l-8 10h8v3H5v-3l8-10H5z"/>' +
    '</svg>'
);

const CUSTOM_ICON = dataUri(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="black">' +
    '<path d="M19 11h-3V8a4 4 0 0 0-8 0v3H5a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7a2 2 0 0 0-2-2zm-9-3a2 2 0 0 1 4 0v3h-4z"/>' +
    '</svg>'
);

export const PROVIDER_META: ProviderMeta[] = [
  {
    key: 'internal',
    name: 'Tesslate',
    brandColor: '#f89521',
    iconUrl: TESSLATE_ICON,
    website: 'https://tesslate.com',
  },
  {
    key: 'openai',
    name: 'OpenAI',
    brandColor: '#10a37f',
    iconUrl: simpleicons('openai'),
    website: 'https://platform.openai.com/api-keys',
  },
  {
    key: 'anthropic',
    name: 'Anthropic',
    brandColor: '#d97757',
    iconUrl: simpleicons('anthropic'),
    website: 'https://console.anthropic.com/settings/keys',
  },
  {
    key: 'openrouter',
    name: 'OpenRouter',
    brandColor: '#6467f2',
    iconUrl: OPENROUTER_ICON,
    website: 'https://openrouter.ai/keys',
  },
  {
    key: 'requesty',
    name: 'Requesty',
    brandColor: '#6366f1',
    iconUrl: REQUESTY_ICON,
    website: 'https://app.requesty.ai/api-keys',
  },
  {
    key: 'groq',
    name: 'Groq',
    brandColor: '#f55036',
    iconUrl: GROQ_ICON,
    website: 'https://console.groq.com/keys',
  },
  {
    key: 'together',
    name: 'Together AI',
    brandColor: '#0f6fff',
    iconUrl: TOGETHER_ICON,
    website: 'https://api.together.xyz/settings/api-keys',
  },
  {
    key: 'deepseek',
    name: 'DeepSeek',
    brandColor: '#4d6bfe',
    iconUrl: DEEPSEEK_ICON,
    website: 'https://platform.deepseek.com/api_keys',
  },
  {
    key: 'fireworks',
    name: 'Fireworks AI',
    brandColor: '#7c3aed',
    iconUrl: FIREWORKS_ICON,
    website: 'https://fireworks.ai/api-keys',
  },
  {
    key: 'nano-gpt',
    name: 'NanoGPT',
    brandColor: '#22c55e',
    iconUrl: NANOGPT_ICON,
    website: 'https://nano-gpt.com',
  },
  {
    key: 'z-ai',
    name: 'Z.AI',
    brandColor: '#0ea5e9',
    iconUrl: ZAI_ICON,
    website: 'https://z.ai',
  },
];

const META_BY_KEY = new Map(PROVIDER_META.map((p) => [p.key, p]));

export function getProviderMeta(key: string): ProviderMeta | undefined {
  return META_BY_KEY.get(key.toLowerCase());
}

/**
 * Resolve a meta entry, falling back to a generic one for unknown providers
 * (e.g. user-created custom providers). The fallback uses a neutral lock icon
 * and the muted-text token color so unknown providers still render coherently.
 */
export function resolveProviderMeta(
  key: string,
  displayName?: string
): ProviderMeta {
  return (
    getProviderMeta(key) || {
      key,
      name: displayName || key.charAt(0).toUpperCase() + key.slice(1),
      brandColor: '#6b6f76',
      iconUrl: CUSTOM_ICON,
    }
  );
}
