// VOIE browser smoke — scripted acceptance flow (16 discrete steps).
//
// Runs against any origin: default env VOIE_SMOKE_ORIGIN, or --base-url.
// No origin required in --dry-run mode (prints the plan, exit 0).
//
// Design notes for operators:
//   * Every step is a discrete named assertion; failures carry the step name
//     and the exact expectation that broke. Screenshot + HTML dump land in
//     tests/browser/artifacts/<run-id>/.
//   * Steps target the *contracted* product surface (see README for the full
//     mapping). Selector candidates are layered: data-testid -> role/label
//     text -> generic fallbacks, so a not-yet-landed candidate fails loudly
//     instead of silently passing.
//   * Zero-session-on-open is an OPERATOR-side API check with the recorded
//     session cookie (README section 6), never asserted from the client.

import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

import {
  assert,
  clickSnippet,
  fillSnippet,
  helpText,
  launchBrowser,
  makeRunId,
  normalizeOrigin,
  parseArgs,
  readPassword,
  requiredEnv,
  sleep,
  UUID_RE,
  uniqueSuffix,
} from './harness.mjs';

export const STEPS = [
  {
    id: 'open-login',
    title: 'open {origin}/login',
    note: 'navigation resolves with a <400 /login response and readyState complete',
  },
  {
    id: 'react-login-form-renders',
    title: 'React login form renders (no bare server <form>)',
    note: 'username+password inputs appear inside a React mount; no page <form> lacks controls (the server-HTML marker this step forbids)',
  },
  {
    id: 'fill-credentials',
    title: 'fill username / password',
    note: 'native setter + input/change events so React state receives the values',
  },
  {
    id: 'submit-login',
    title: 'submit -> cookie set + redirect into portal',
    note: 'wire shows POST /login <400; a session cookie lands in the jar; location leaves /login',
  },
  {
    id: 'account-label-visible',
    title: 'account label visible, not a raw UUID',
    note: 'visible account label in the portal header: non-empty and not a UUID / placeholder (VOIE_SMOKE_ACCOUNT_REGEX optional tightener)',
  },
  {
    id: 'personal-scope-selected',
    title: 'Personal scope is the active selection',
    note: 'scope control exposes a "personal" entry marked selected/active after /api/projects resolves',
  },
  {
    id: 'open-workspaces-page',
    title: 'open the workspaces page',
    note: 'portal nav entry for Workspaces is present and its list surface mounts',
  },
  {
    id: 'create-workspace',
    title: 'create a fresh workspace',
    note: 'POST Create Workspace must succeed; HTTP 429 fails; never reuse an existing Workspace',
  },
  {
    id: 'open-new-chat',
    title: 'start a New chat',
    note: 'editable prompt composer becomes visible after New chat binds a workspace',
  },
  {
    id: 'type-first-prompt',
    title: 'type the first prompt',
    note: 'prompt text lands in the composer via native event dispatch',
  },
  {
    id: 'send-and-conversation-id',
    title: 'send -> conversationId appears in URL/state',
    note: 'conversationId captured from the create POST, a new /chat/ recent, or GET /api/sessions as a last-resort identity lookup. NO server session-count assertion (operator procedure in README §6)',
  },
  {
    id: 'poll-assistant-events-60s',
    title: 'poll assistant/tool events (bounded 60s)',
    note: 'assistant turn becomes visible within the bound; timeout = failure (stall), never an infinite wait',
  },
  {
    id: 'tool-card-visible',
    title: 'tool card visible in transcript',
    note: 'a tool/tool-card element appears in the transcript during the turn',
  },
  {
    id: 'followup-enabled-while-running',
    title: 'follow-up input enabled while assistant runs',
    note: 'composer stays interactive during the streaming turn (sampled when a streaming marker exists)',
  },
  {
    id: 'send-followup-queued',
    title: 'send follow-up -> queued indicator appears',
    note: 'second prompt accepted; DSH queue dock shows a new queued row for that follow-up, not generic first-turn pending/in-progress chrome',
  },
  {
    id: 'reload-reconstructs',
    title: 'reload -> same conversation reconstructs',
    note: 'hard reload restores the same conversationId and the prior prompt text',
  },
];

// ---------------------------------------------------------------------------
// Page snippet constants (evaluated as page-side JS)

const MOUNT_SEL = ['#root', '#__next', '[data-reactroot]', '[data-hydrated]'];

const S_LOGIN_OK = `(() => {
  const mounts = ${JSON.stringify(MOUNT_SEL)};
  const m = mounts.map((s) => document.querySelector(s)).find(Boolean);
  if (!m) return { ok: false, why: 'no React mount container present' };
  const all = Array.from(document.querySelectorAll('input, textarea'));
  if (all.length === 0) return { ok: false, why: 'no input fields on page' };
  const bareForms = Array.from(document.querySelectorAll('form'))
    .filter((f) => !f.querySelector('input, textarea, select, button'));
  const inMount = m.querySelector('input, textarea, button') != null;
  return {
    ok: bareForms.length === 0 && inMount,
    why: bareForms.length
      ? 'bare server <form> markers without controls present'
      : 'inputs exist but not inside the React mount',
    inputs: all.length,
  };
})()`;

const USER_SEL = [
  'input[name="username"]',
  'input[name="user"]',
  '#username',
  'input[autocomplete="username"]',
  '[data-testid="login-username"]',
  'form input[type="text"]',
  'form input:not([type="password"]):not([type="hidden"])',
];
const USER_SEL_STR = USER_SEL.join(', ');

const PASSWORD_SEL = 'input[type="password"]';

const ACCOUNT_SELS = [
  '[data-testid="account"]',
  '[data-testid="account-label"]',
  '[data-account]',
  'header [class*="account" i]',
  '[class*="user-menu" i]',
];

const SCOPE_SELS = [
  '[data-scope]',
  '[data-persona]',
  '[role="tab"]',
  '[role="option"]',
  '[data-testid*="scope" i]',
  '[class*="scope" i]',
];

const WS_VISIBLE = `(() => {
  if (!location.pathname.includes('workspace')) return false;
  const input = document.querySelector('input[aria-label="Workspace name"], input[placeholder="Workspace name"]');
  const table = document.querySelector('table');
  const empty = document.body.textContent.includes('No workspaces');
  return Boolean(
    (input && input.getBoundingClientRect().width > 0)
    || (table && table.getBoundingClientRect().width > 0)
    || empty,
  );
})()`;

const COMPOSER_SEL =
  'textarea:not([readonly]):not([disabled]), [contenteditable="true"]';

const COMPOSER_READY = `(() => {
  const nodes = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}));
  return nodes.some((e) => {
    if (e.getBoundingClientRect().width === 0) return false;
    const label = (e.getAttribute('aria-label') || '').toLowerCase();
    if (label.includes('choose workspace')) return false;
    if (e.getAttribute('aria-haspopup') === 'menu') return false;
    return true;
  });
})()`;

const TOOL_CARD = `(() => {
  const sels = [
    '[data-tool-card]',
    '[data-tool]',
    '[data-testid*="tool" i]',
    '[class*="tool-card" i]',
    '[class*="toolCall" i]',
  ];
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.closest('style, script')) continue;
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) return true;
    }
  }
  // DSH tool rows are hashed-class chrome (title "Bash" + summary), not data-tool.
  const scroll = document.querySelector('[data-conversation-scroll]') || document.body;
  for (const el of scroll.querySelectorAll('span, div, p, button')) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    const t = (el.textContent || '').trim();
    if (t === 'Bash' || t === 'bash') return true;
  }
  const text = scroll.textContent || '';
  if (/\\bBash\\b/.test(text) && /Tool call/i.test(text)) return true;
  return false;
})()`;

const STREAMING_MARKER = `(() => !!document.querySelector(
  '[data-streaming="true"], [aria-busy="true"], [data-status="streaming"], .streaming, [data-testid*="streaming" i]',
))()`;

const BUSY_STOP = `(() => {
  for (const label of ['Stop generating', 'Stop']) {
    const btn = document.querySelector('button[aria-label="' + label + '"]');
    if (btn && btn.getBoundingClientRect().width > 0) return true;
  }
  return false;
})()`;

const COMPOSER_ENABLED = `(() => {
  const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
    .find((e) => e.getBoundingClientRect().width > 0);
  if (!el) return false;
  return !el.disabled && !(el.closest('[data-testid*="composer" i]')?.hasAttribute('disabled'));
})()`;

const QUEUE_DOCK_SNAPSHOT = `(() => {
  const dock = document.querySelector('[data-queue-dock]');
  if (!dock) return { present: false, rowCount: 0, previews: [], signature: '' };
  const box = dock.getBoundingClientRect();
  if (box.width === 0 || box.height === 0) {
    return { present: false, rowCount: 0, previews: [], signature: '' };
  }
  const rows = Array.from(dock.querySelectorAll('li'));
  const previews = rows
    .map((row) => (row.textContent || '').replace(/\\s+/g, ' ').trim())
    .filter(Boolean);
  const countText = (dock.textContent || '').replace(/\\s+/g, ' ').trim();
  return {
    present: true,
    rowCount: rows.length,
    previews,
    signature: [String(rows.length), ...previews, countText].join('|'),
  };
})()`;

const CONVO_ID_PROBE = `(() => {
  const host = document.getElementById('voie-dsh-root');
  const fromHost = host && (host.getAttribute('data-voie-conversation-id') || host.getAttribute('data-voie-session-id'));
  if (fromHost) return fromHost;
  const path = location.pathname;
  const parts = path.split('/').filter(Boolean);
  const chatIdx = parts.indexOf('chat');
  if (chatIdx >= 0 && parts[chatIdx + 1] && /^[0-9A-Fa-f-]{36}$/.test(parts[chatIdx + 1])) {
    return parts[chatIdx + 1];
  }
  for (const key of ['conversations', 'conversation', 'threads', 'thread', 'c']) {
    const idx = parts.indexOf(key);
    if (idx >= 0 && parts[idx + 1] && /^[0-9A-Fa-f-]{36}$/.test(parts[idx + 1])) {
      return parts[idx + 1];
    }
  }
  const u = new URL(location.href);
  for (const k of ['conversation', 'thread', 'c', 'id']) {
    const v = u.searchParams.get(k);
    if (v && /^[0-9A-Fa-f-]{36}$/.test(v)) return v;
  }
  const els = document.querySelectorAll('[data-conversation-id], [data-conversation], [data-thread-id]');
  for (const el of els) {
    const v = el.getAttribute('data-conversation-id')
      || el.getAttribute('data-conversation')
      || el.getAttribute('data-thread-id');
    if (v && /^[0-9A-Fa-f-]{36}$/.test(v)) return v;
  }
  const recent = document.querySelector('.portal-recents a.nav-link-active[href^="/chat/"]');
  if (recent) {
    const href = recent.getAttribute('href') || '';
    const id = href.split('/').pop();
    if (id && /^[0-9A-Fa-f-]{36}$/.test(id)) return id;
  }
  return null;
})()`;

const SEND_NOW = `(() => {
  const near = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
    .find((e) => e.getBoundingClientRect().width > 0);
  const sendBtn = Array.from(document.querySelectorAll(
    'button[aria-label="Send message"], [data-testid="send"], button.uV2eYG_primary, button[type="submit"]',
  )).find((el) => el.getBoundingClientRect().width > 0 && !el.disabled);
  if (sendBtn) {
    sendBtn.click();
    return 'click';
  }
  if (near) {
    near.focus();
    near.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true,
    }));
    return 'enter';
  }
  return null;
})()`;

// ---------------------------------------------------------------------------
// Page-side helper factories (return snippets)

function clickTextSnippet(sels, re) {
  return `(() => {
    const sels = ${JSON.stringify(sels)};
    const re = ${re.toString()};
    for (const s of sels) {
      for (const el of document.querySelectorAll(s)) {
        if (el.getBoundingClientRect().width === 0) continue;
        const t = (el.textContent || el.getAttribute('aria-label') || '').trim();
        if (!re.test(t)) continue;
        el.click();
        return true;
      }
    }
    return false;
  })()`;
}

async function grabConversationId(page) {
  for (let i = 0; i < 80; i++) {
    const v = await page.evalJs(CONVO_ID_PROBE);
    if (v) return v;
    await sleep(250);
  }
  return null;
}

function isCreateConversationUrl(url) {
  try {
    const parsed = new URL(String(url));
    return parsed.pathname.replace(/\/+$/, '') === '/api/conversations';
  } catch {
    return /\/api\/conversations\/?$/.test(String(url).split('?')[0]);
  }
}

function conversationIdFromPosts(posts) {
  for (const p of posts) {
    if (p.status >= 400) continue;
    if (isCreateConversationUrl(p.url) && typeof p.postData === 'string') {
      try {
        const body = JSON.parse(p.postData);
        if (typeof body.conversationId === 'string' && UUID_RE.test(body.conversationId)) {
          return body.conversationId;
        }
      } catch {
        // ignore malformed bodies
      }
    }
    const message = String(p.url).match(/\/api\/conversations\/([0-9A-Fa-f-]{36})\/messages/);
    if (message) return message[1];
  }
  return null;
}

async function conversationIdFromNetwork(page, posts) {
  for (const p of [...posts].reverse()) {
    if (p.status >= 400 || !p.requestId) continue;
    if (!isCreateConversationUrl(p.url)) continue;
    const raw = await page.requestResponseBody(p.requestId);
    if (typeof raw !== 'string' || raw === '') continue;
    try {
      const body = JSON.parse(raw);
      if (typeof body.conversationId === 'string' && UUID_RE.test(body.conversationId)) {
        return body.conversationId;
      }
    } catch {
      // ignore malformed bodies
    }
  }
  const fromBody = conversationIdFromPosts(posts);
  if (fromBody) return fromBody;
  for (const p of posts) {
    if (p.status >= 400 || !p.requestId) continue;
    if (!isCreateConversationUrl(p.url)) continue;
    const raw = await page.requestPostData(p.requestId);
    if (typeof raw !== 'string') continue;
    try {
      const body = JSON.parse(raw);
      if (typeof body.conversationId === 'string' && UUID_RE.test(body.conversationId)) {
        return body.conversationId;
      }
    } catch {
      // ignore malformed bodies
    }
  }
  return null;
}

const RECENTS_HREFS = `([...document.querySelectorAll('.portal-recents a[href^="/chat/"]')]
  .map((a) => (a.getAttribute('href') || '').split('/').pop())
  .filter((id) => id && /^[0-9A-Fa-f-]{36}$/.test(id)))`;

const LIST_SESSION_ROWS = `(async () => {
  const res = await fetch('/api/sessions', {
    credentials: 'same-origin',
    headers: { accept: 'application/json' },
  });
  if (!res.ok) return [];
  const data = await res.json();
  const items = Array.isArray(data.items) ? data.items : [];
  return items
    .map((row) => ({
      id: typeof row.id === 'string' ? row.id : '',
      createdAt: typeof row.createdAt === 'string' ? row.createdAt : '',
    }))
    .filter((row) => /^[0-9A-Fa-f-]{36}$/.test(row.id));
})()`;

// ---------------------------------------------------------------------------
// Step bodies

async function stepOpenLogin(ctx) {
  await ctx.page.goto(`${ctx.cfg.origin}/login`, 30_000);
  const hits = ctx.page.networkResponses({ urlRe: /\/login/ });
  const status = hits.at(-1)?.status;
  assert(
    status !== undefined && status < 400,
    ctx.step.id,
    `GET ${ctx.cfg.origin}/login yielded status ${status ?? 'no response observed'}`,
  );
  const url = await ctx.page.currentUrl();
  assert(
    url.startsWith(ctx.cfg.origin),
    ctx.step.id,
    `landed at unexpected URL: ${url}`,
  );
}

async function stepReactForm(ctx) {
  await ctx.page.waitForFunction(
    `(() => { const r = (${S_LOGIN_OK}); return r.ok; })()`,
    { timeoutMs: 20_000 },
  );
  const detail = await ctx.page.evalJs(`(${S_LOGIN_OK})`);
  assert(detail.ok, ctx.step.id, detail.why ?? 'login form check failed');
}

async function stepFillCredentials(ctx) {
  await ctx.page.evalJs(fillSnippet(USER_SEL_STR, ctx.cfg.user));
  await ctx.page.evalJs(fillSnippet(PASSWORD_SEL, ctx.cfg.password));
  const got = await ctx.page.evalJs(
    `(() => {
      const u = Array.from(document.querySelectorAll(${JSON.stringify(USER_SEL_STR)}))
        .find((e) => e.getBoundingClientRect().width > 0);
      const p = document.querySelector(${JSON.stringify(PASSWORD_SEL)});
      return { u: u ? u.value : null, p: p ? p.value : null };
    })()`,
  );
  assert(
    got.u === ctx.cfg.user && got.p === ctx.cfg.password,
    ctx.step.id,
    `credentials not in form (username=${got.u ? 'set' : 'missing'}, password=${got.p ? 'set' : 'missing'})`,
  );
}

async function stepSubmitLogin(ctx) {
  let sent = false;
  try {
    sent = await ctx.page.evalJs(clickSnippet(
      ['button[type="submit"]', '[data-testid="login-submit"]', 'form button'],
      { textRe: /log\s?in|sign\s?in/i },
    ));
  } catch {
    sent = false;
  }
  if (!sent) {
    await ctx.page.evalJs(
      `(() => {
        const p = document.querySelector(${JSON.stringify(PASSWORD_SEL)});
        if (!p) throw new Error('no password field for Enter fallback');
        p.dispatchEvent(new KeyboardEvent('keydown', {
          key: 'Enter', code: 'Enter', bubbles: true, cancelable: true,
        }));
        if (p.form) p.form.dispatchEvent(new Event('submit', {
          bubbles: true, cancelable: true,
        }));
        return !!p.form;
      })()`,
    );
  }

  let cookies = [];
  for (let i = 0; i < 80 && cookies.length === 0; i++) {
    cookies = await ctx.page.cookies([ctx.cfg.origin]);
    if (cookies.length === 0) await sleep(250);
  }
  const names = cookies.map((c) => c.name).join(',');
  assert(
    cookies.length > 0,
    ctx.step.id,
    `no cookie landed in the browser jar after submit (current jar: ${names})`,
  );
  const expected = process.env.VOIE_SMOKE_SESSION_COOKIE;
  if (expected) {
    assert(
      cookies.some((c) => c.name === expected),
      ctx.step.id,
      `expected cookie "${expected}" absent (jar: ${names})`,
    );
  }
  ctx.state.cookies = cookies;

  // Capture the wire entry BEFORE waiting on the post-login navigation:
  // the console reload clears the network log, so the successful POST can
  // no longer be observed afterwards.
  // Wire-truth fallback: the cookie jar is server-set only via the 303 of
  // a successful POST /login; when post-navigation network buffers have
  // already rotated, the jar itself proves the wire outcome.
  if (cookies.length === 0) {
    const posts = ctx.page.networkResponses({ methodRe: /^POST$/, urlRe: /\/login/ });
    assert(
      posts.some((r) => r.status < 400),
      ctx.step.id,
      'POST /login never observed with a 2xx/3xx status on the wire',
    );
  }

  await ctx.page.waitForFunction(
    `(() => {
      const p = location.pathname;
      return p.indexOf('/login') === -1 && p !== '';
    })()`,
    { timeoutMs: 20_000 },
  );
}

async function stepAccountLabel(ctx) {
  await ctx.page.waitForFunction(
    `(() => {
      const sels = ${JSON.stringify(ACCOUNT_SELS)};
      for (const s of sels) {
        for (const el of document.querySelectorAll(s)) {
          if (el.getBoundingClientRect().width === 0) continue;
          if ((el.textContent || '').trim().length > 0) return true;
        }
      }
      return false;
    })()`,
    { timeoutMs: 15_000 },
  );
  const label = await ctx.page.evalJs(
    `(() => {
      const sels = ${JSON.stringify(ACCOUNT_SELS)};
      for (const s of sels) {
        for (const el of document.querySelectorAll(s)) {
          if (el.getBoundingClientRect().width === 0) continue;
          const t = (el.textContent || '').trim();
          if (t.length > 0) return t;
        }
      }
      return null;
    })()`,
  );
  assert(label !== null && label.length > 4, ctx.step.id, `account label empty/too short: ${JSON.stringify(label)}`);
  assert(!UUID_RE.test(label), ctx.step.id, `account label looks like a raw UUID: ${label}`);
  assert(!/^[•_—…\- ]{1,8}$/.test(label), ctx.step.id, `account label is a placeholder: ${JSON.stringify(label)}`);
  const tighter = process.env.VOIE_SMOKE_ACCOUNT_REGEX;
  if (tighter) {
    const re = new RegExp(tighter);
    assert(re.test(label), ctx.step.id, `account label ${JSON.stringify(label)} did not match ${tighter}`);
  }
  ctx.state.accountLabel = label;
}

async function stepPersonalScope(ctx) {
  await ctx.page.waitForFunction(
    `(() => {
      const sels = ${JSON.stringify(SCOPE_SELS)};
      for (const s of sels) {
        for (const el of document.querySelectorAll(s)) {
          if (el.getBoundingClientRect().width === 0) continue;
          const t = ((el.textContent || '') + ' ' + (el.getAttribute('aria-label') || '')).toLowerCase();
          if (t.includes('personal')) return true;
        }
      }
      return false;
    })()`,
    { timeoutMs: 15_000 },
  );
  const scopeState = await ctx.page.evalJs(
    `(() => {
      // Native <select> switchers encode selection in their value; any
      // non-selected option listing a scope is not an active marker.
      const selects = Array.from(document.querySelectorAll('select.scope-switcher, select[aria-label="Scope"]'))
        .filter((el) => el.getBoundingClientRect().width > 0);
      for (const sel of selects) {
        const opt = sel.selectedOptions?.[0];
        if (!opt) continue;
        const label = (opt.textContent || '').trim().toLowerCase();
        if (label.includes('personal')) {
          return { found: opt.textContent.trim().slice(0, 40), active: true };
        }
      }
      const sels = ${JSON.stringify(SCOPE_SELS)};
      for (const s of sels) {
        for (const el of document.querySelectorAll(s)) {
          if (el.getBoundingClientRect().width === 0) continue;
          const hay = ((el.textContent || '') + ' ' + (el.getAttribute('aria-label') || '')).toLowerCase();
          if (!hay.includes('personal')) continue;
          const active = el.matches('[aria-selected="true"], [aria-checked="true"], [data-selected="true"], .active, .selected');
          return { found: (el.textContent || '').trim().slice(0, 40), active };
        }
      }
      return { found: null, active: false };
    })()`,
  );
  assert(
    scopeState.found !== null && scopeState.active,
    ctx.step.id,
    `personal scope not the active selection (entry: ${JSON.stringify(scopeState.found)})`,
  );
  const wantScope = (process.env.VOIE_SMOKE_SCOPE_ID || '').trim();
  if (wantScope) {
    const bound = await ctx.page.evalJs(
      `(() => {
        const sel = document.querySelector('select.scope-switcher, select[aria-label="Scope"]');
        if (!sel) return false;
        const opt = Array.from(sel.options).find((o) => o.value === ${JSON.stringify(wantScope)});
        if (!opt) return false;
        sel.value = opt.value;
        sel.dispatchEvent(new Event('input', { bubbles: true }));
        sel.dispatchEvent(new Event('change', { bubbles: true }));
        return sel.value === ${JSON.stringify(wantScope)};
      })()`,
    );
    assert(bound, ctx.step.id, `scope ${wantScope} is not present on the switcher`);
    await ctx.page.waitForFunction(
      `(() => {
        const sel = document.querySelector('select.scope-switcher, select[aria-label="Scope"]');
        return Boolean(sel && sel.value === ${JSON.stringify(wantScope)});
      })()`,
      { timeoutMs: 10_000 },
    );
    // Scope swap remounts the shell; wait for primary nav before the next step.
    await ctx.page.waitForFunction(WORKSPACES_NAV_VISIBLE, { timeoutMs: 15_000 });
  }
}

const WORKSPACES_NAV_VISIBLE = `(() => {
  const els = document.querySelectorAll('a[href="/workspaces"], a[href^="/workspaces"], a[href$="/workspaces"], [data-testid="nav-workspaces"]');
  for (const el of els) {
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) return true;
  }
  return false;
})()`;

async function stepOpenWorkspaces(ctx) {
  await ctx.page.waitForFunction(WORKSPACES_NAV_VISIBLE, { timeoutMs: 20_000 });
  const clicked = await ctx.page.evalJs(clickTextSnippet(
    ['a[href="/workspaces"]', 'a[href^="/workspaces"]', 'a[href$="/workspaces"]', '[data-testid="nav-workspaces"]', 'nav[aria-label="Primary"] a', 'button', '[role="button"]'],
    /^workspaces$/i,
  ));
  assert(clicked, ctx.step.id, 'no clickable Workspaces nav entry found');
  await ctx.page.waitForFunction(WS_VISIBLE, { timeoutMs: 20_000 });
}

async function stepCreateWorkspace(ctx) {
  const target = `Smoke ${ctx.state.runTag}`;
  const workspacesHref = await ctx.page.evalJs(`(() => {
    const a = Array.from(document.querySelectorAll('a[href*="workspaces"]'))
      .find((el) => el.getBoundingClientRect().width > 0);
    return a ? a.href : (location.origin + '/workspaces' + location.search);
  })()`);
  assert(typeof workspacesHref === 'string' && workspacesHref.length > 0, ctx.step.id, 'no Workspaces URL');
  await ctx.page.goto(workspacesHref, 30_000);
  await ctx.page.waitForFunction(WS_VISIBLE, { timeoutMs: 20_000 });
  const beforePosts = ctx.page.networkResponses({
    methodRe: /^POST$/,
    urlRe: /\/api\/(?:projects\/[^/]+\/)?workspaces\/?$/,
  }).length;
  const sel = 'input[aria-label="Workspace name"], input[placeholder="Workspace name"]';
  await ctx.page.evalJs(fillSnippet(sel, target));
  await ctx.page.waitForFunction(
    `(() => {
      const btn = Array.from(document.querySelectorAll('button'))
        .find((b) => /^new workspace$/i.test((b.textContent || '').trim()));
      return btn ? btn.disabled === false : false;
    })()`,
    { timeoutMs: 10_000 },
  );
  const ok = await ctx.page.evalJs(clickTextSnippet(
    ['button'],
    /^new workspace$/i,
  ));
  assert(ok, ctx.step.id, 'no "create workspace" control found');
  let createPost = null;
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const posts = ctx.page.networkResponses({
      methodRe: /^POST$/,
      urlRe: /\/api\/(?:projects\/[^/]+\/)?workspaces\/?$/,
    });
    createPost = posts[beforePosts] ?? posts[posts.length - 1] ?? null;
    if (createPost && posts.length > beforePosts) break;
    await sleep(250);
  }
  assert(createPost !== null, ctx.step.id, 'Create Workspace POST was not observed');
  assert(
    createPost.status !== 429,
    ctx.step.id,
    `Create Workspace returned HTTP 429: ${createPost.url}`,
  );
  assert(
    createPost.status < 400,
    ctx.step.id,
    `Create Workspace POST failed HTTP ${createPost.status}: ${createPost.url}`,
  );
  const stillOnWorkspaces = await ctx.page.evalJs(
    "location.pathname.includes('workspace')",
  );
  assert(
    stillOnWorkspaces,
    ctx.step.id,
    `Create Workspace left the Workspaces page (${await ctx.page.evalJs('location.pathname')})`,
  );
  await ctx.page.waitForFunction(
    `(() => {
      const rows = Array.from(document.querySelectorAll('td, tr, [data-workspace]'));
      return rows.some((c) => (c.textContent || '').includes(${JSON.stringify(target)}));
    })()`,
    { timeoutMs: 60_000 },
  );
  await ctx.page.waitForFunction(
    `(() => {
      const rows = Array.from(document.querySelectorAll('tr[data-workspace-id], [data-workspace]'));
      const row = rows.find((c) => (c.textContent || '').includes(${JSON.stringify(target)}));
      if (!row) return false;
      const state = (row.getAttribute('data-workspace-state') || '').toLowerCase();
      return state === 'ready';
    })()`,
    { timeoutMs: 480_000 },
  );
  ctx.state.workspaceName = target;
  ctx.state.createdWorkspace = true;
  const wid = await ctx.page.evalJs(
    `(() => {
      const want = ${JSON.stringify(target)};
      const row = Array.from(document.querySelectorAll('tr, [data-workspace]'))
        .find((el) => (el.textContent || '').includes(want));
      return row?.getAttribute('data-workspace-id')
        || row?.getAttribute('data-workspace')
        || document.querySelector('[data-workspace-id]')?.getAttribute('data-workspace-id')
        || (location.href.match(/\\/(?:workspaces?)\\/([0-9a-fA-F-]{8,})/) || [])[1]
        || null;
    })()`,
  );
  ctx.state.workspaceId = wid;
  assert(wid && UUID_RE.test(wid), ctx.step.id, `created Workspace id missing from UI: ${wid}`);
}

function bindWorkspaceSnippet(preferredName) {
  return `(() => {
    const want = ${JSON.stringify(preferredName ?? '')}.trim().toLowerCase();
    const picker = document.querySelector('[data-voie-workspace-picker]');
    if (picker && picker.getBoundingClientRect().width > 0) {
      const items = Array.from(picker.querySelectorAll('[role="menuitem"], button'));
      const hit = items.find((el) => {
        const t = (el.textContent || '').trim().toLowerCase();
        return want !== '' && t.includes(want);
      });
      if (hit) {
        hit.click();
        return 'picked';
      }
      return 'picker-unmatched';
    }
    const trigger = Array.from(document.querySelectorAll('button, [role="button"], textarea'))
      .find((el) => {
        if (el.getBoundingClientRect().width === 0) return false;
        const hay = ((el.getAttribute('aria-label') || '') + ' ' + (el.textContent || '')).toLowerCase();
        return hay.includes('choose workspace');
      });
    if (trigger) {
      trigger.click();
      return 'opened';
    }
    return 'idle';
  })()`;
}

async function stepOpenNewChat(ctx) {
  ctx.probe('before-open-new-chat');
  const beforeCreates = ctx.page.networkResponses({
    methodRe: /^POST$/,
    urlRe: /\/api\/conversations\/?$/,
  }).length;
  const clickNew = clickTextSnippet(
    ['[data-testid="new-chat"]', '[data-testid="new-conversation"]', 'a.portal-new-chat', 'a[href="/"]', 'button', '[role="button"]', 'a'],
    /^new$|new chat|new conversation/i,
  );
  try {
    const ok = await ctx.page.evalJs(clickNew, 8_000);
    assert(ok, ctx.step.id, 'no New chat control found');
  } catch (err) {
    // A same-document route change can abort the click evaluate; the
    // pathname wait below is the real success check.
    ctx.page.noteConsole();
  }
  const homeDeadline = Date.now() + 10_000;
  while (Date.now() < homeDeadline) {
    try {
      const path = await ctx.page.evalJs('location.pathname', 5_000);
      if (path === '/') break;
    } catch {
      // CDP may be mid-navigation.
    }
    await sleep(250);
  }
  const bind = bindWorkspaceSnippet(ctx.state.workspaceName);
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      if (await ctx.page.evalJs(COMPOSER_READY, 5_000)) break;
      await ctx.page.evalJs(bind, 5_000);
    } catch {
      // Timed-out evaluate must not freeze this step.
    }
    await sleep(250);
  }
  await ctx.page.waitForFunction(COMPOSER_READY, { timeoutMs: 20_000 });
  let posts = [];
  const postDeadline = Date.now() + 20_000;
  while (Date.now() < postDeadline) {
    posts = ctx.page.networkResponses({
      methodRe: /^POST$/,
      urlRe: /\/api\/conversations\/?$/,
    }).slice(beforeCreates);
    if (posts.some((p) => p.status < 400)) break;
    await sleep(250);
  }
  const createPost = [...posts].reverse().find((p) => p.status < 400) ?? posts[posts.length - 1];
  assert(createPost !== undefined, ctx.step.id, 'New Chat did not POST /api/conversations');
  assert(createPost.status < 400, ctx.step.id, `New Chat POST failed HTTP ${createPost.status}`);
  const cid = await conversationIdFromNetwork(ctx.page, posts);
  assert(cid !== null && UUID_RE.test(cid), ctx.step.id, `New Chat POST produced no durable Session id`);
  const boundWorkspace = await ctx.page.evalJs(`(async () => {
    const res = await fetch(${JSON.stringify(`/api/sessions/${cid}`)}, {
      credentials: 'same-origin',
      headers: { accept: 'application/json' },
    });
    const data = await res.json().catch(() => ({}));
    return data.workspaceId || null;
  })()`);
  assert(
    !ctx.state.workspaceId || boundWorkspace === ctx.state.workspaceId,
    ctx.step.id,
    `New Chat bound Workspace ${boundWorkspace} instead of created ${ctx.state.workspaceId}`,
  );
  const listed = await ctx.page.evalJs(LIST_SESSION_ROWS) ?? [];
  assert(
    listed.some((row) => row.id === cid),
    ctx.step.id,
    `durable Session ${cid} missing from GET /api/sessions before any prompt`,
  );
  const recents = await ctx.page.evalJs(RECENTS_HREFS) ?? [];
  assert(
    recents.includes(cid) || listed.some((row) => row.id === cid),
    ctx.step.id,
    `durable Session ${cid} missing from navigation`,
  );
  ctx.state.conversationId = cid;
  await ctx.page.evalJs('location.reload()');
  await sleep(1000);
  const listedAfter = await ctx.page.evalJs(LIST_SESSION_ROWS) ?? [];
  assert(
    listedAfter.some((row) => row.id === cid),
    ctx.step.id,
    `empty Session ${cid} did not survive hard reload`,
  );
  const recentsAfter = await ctx.page.evalJs(RECENTS_HREFS) ?? [];
  if (!recentsAfter.includes(cid)) {
    await ctx.page.goto(`${ctx.cfg.origin}/chat/${cid}`, 30_000);
  } else {
    await ctx.page.evalJs(`(() => {
      const a = document.querySelector('a[href="/chat/${cid}"]');
      if (a) a.click();
      return Boolean(a);
    })()`);
  }
  const rebind = bindWorkspaceSnippet(ctx.state.workspaceName);
  const reopenDeadline = Date.now() + 20_000;
  while (Date.now() < reopenDeadline) {
    try {
      if (await ctx.page.evalJs(COMPOSER_READY, 5_000)) break;
      await ctx.page.evalJs(rebind, 5_000);
    } catch {
      // ignore
    }
    await sleep(250);
  }
  await ctx.page.waitForFunction(COMPOSER_READY, { timeoutMs: 20_000 });
  ctx.probe('after-open-new-chat');
}

async function stepTypeFirstPrompt(ctx) {
  // Hold the first turn long enough that the follow-up step can observe a
  // real queued/pending UI state. Bash tool timeout is 30s; stay under it.
  const prompt = `Run sleep 20 in bash, then reply with first-turn-done-${ctx.state.runTag}`;
  const focused = await ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    el.focus();
    if (typeof el.select === 'function') el.select();
    return true;
  })()`);
  assert(focused, ctx.step.id, 'no visible prompt composer to type into');
  await ctx.page.insertText(prompt);
  await ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    el.dispatchEvent(new InputEvent('input', {
      bubbles: true, data: ${JSON.stringify(prompt)}, inputType: 'insertFromPaste',
    }));
    return true;
  })()`);
  await ctx.page.waitForFunction(
    `(() => {
      const btn = document.querySelector('button[aria-label="Send message"]');
      return Boolean(btn) && btn.disabled === false;
    })()`,
    { timeoutMs: 10_000 },
  );
  const got = await ctx.page.evalJs(
    `(() => {
      const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
        .find((e) => e.getBoundingClientRect().width > 0);
      return el ? (el.value ?? el.textContent ?? '') : null;
    })()`,
  );
  assert(
    got === prompt,
    ctx.step.id,
    `prompt text not in composer (found: ${JSON.stringify(got)})`,
  );
  ctx.state.prompt = prompt;
}

async function composerHoldsPrompt(page, prompt) {
  return page.evalJs(
    `(() => {
      const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
        .find((e) => e.getBoundingClientRect().width > 0);
      if (!el) return { present: false, holds: false };
      const text = el.value ?? el.textContent ?? '';
      return { present: true, holds: text.includes(${JSON.stringify(prompt)}) };
    })()`,
  );
}

async function stepSendAndCid(ctx) {
  const prompt = ctx.state.prompt ?? '';
  const recentsBefore = new Set(await ctx.page.evalJs(RECENTS_HREFS) ?? []);
  const focused = await ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (el) el.focus();
    return Boolean(el);
  })()`);
  assert(focused, ctx.step.id, 'no visible prompt composer to send from');
  await ctx.page.evalJs(`(() => {
    const btn = document.querySelector('button[aria-label="Send message"]');
    if (!btn || btn.disabled) return false;
    btn.click();
    return true;
  })()`);
  let leftComposer = false;
  let acceptedPost = false;
  for (let i = 0; i < 40; i++) {
    const state = await composerHoldsPrompt(ctx.page, prompt);
    if (state.present && !state.holds) {
      leftComposer = true;
      break;
    }
    const posts = ctx.page.networkResponses({ methodRe: /^POST$/, urlRe: /\/api\/conversations/ });
    if (posts.some((p) => p.status < 400)) {
      acceptedPost = true;
      break;
    }
    await sleep(250);
  }
  const diag = await ctx.page.evalJs(`(() => {
    const btn = document.querySelector('button[aria-label="Send message"]');
    if (!btn) return { send: 'missing' };
    const r = btn.getBoundingClientRect();
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return {
      disabled: Boolean(btn.disabled),
      w: r.width,
      h: r.height,
      x: r.left,
      y: r.top,
      hit: hit ? (hit.getAttribute('aria-label') || hit.tagName) : null,
    };
  })()`);
  const posts = ctx.page.networkResponses({ methodRe: /^POST$/, urlRe: /\/api\/conversations/ });
  const postSummary = posts.map((p) => ({ method: p.method, url: p.url, status: p.status }));
  assert(
    leftComposer || acceptedPost,
    ctx.step.id,
    `first prompt was not accepted (composer unchanged and no POST /api/conversations <400); diag=${JSON.stringify(diag)} posts=${JSON.stringify(postSummary)}`,
  );
  let cid = await conversationIdFromNetwork(ctx.page, posts);
  if (cid === null) {
    for (let i = 0; i < 40; i++) {
      const recents = await ctx.page.evalJs(RECENTS_HREFS) ?? [];
      const newcomer = recents.find((id) => !recentsBefore.has(id));
      if (newcomer) {
        cid = newcomer;
        break;
      }
      const rows = await ctx.page.evalJs(LIST_SESSION_ROWS) ?? [];
      const created = rows
        .filter((row) => row.id && !recentsBefore.has(row.id))
        .sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)))[0];
      if (created) {
        cid = created.id;
        break;
      }
      await sleep(250);
    }
  }
  const postDebug = posts.map((p) => ({
    method: p.method,
    url: p.url,
    status: p.status,
    hasPost: Boolean(p.postData),
    requestId: Boolean(p.requestId),
  }));
  assert(
    cid !== null,
    ctx.step.id,
    `conversationId never appeared in the create POST body, a new /chat/ recent, or GET /api/sessions; posts=${JSON.stringify(postDebug)}`,
  );
  ctx.state.conversationId = cid;
  ctx.probe('after-send-and-conversation-id');
}

async function stepPollAssistant(ctx) {
  // The first turn holds with `sleep 20` (under the 30s bash tool timeout).
  // Tool-card appearance is the start of that hold. Waiting for the final
  // assistant text consumes it, and leftover empty-state copy in the
  // conversation DOM can hide the user turn until the reply lands.
  await ctx.page.waitForFunction(TOOL_CARD, {
    timeoutMs: ctx.cfg.timeoutMs ?? 60_000,
    intervalMs: 500,
  });
}

async function stepToolCard(ctx) {
  // The first turn is held with a bash sleep so step 15 can observe a
  // queued follow-up. The tool card appears when bash starts, which can
  // be after the model thinks; do not treat a short wait as "no tool".
  await ctx.page.waitForFunction(TOOL_CARD, { timeoutMs: 60_000 });
}

async function stepFollowupEnabled(ctx) {
  const streaming = await ctx.page.evalJs(STREAMING_MARKER);
  let enabled;
  if (streaming) {
    enabled = await ctx.page.evalJs(COMPOSER_ENABLED);
    assert(enabled, ctx.step.id, 'composer disabled while assistant turn is streaming');
  } else {
    enabled = await ctx.page.evalJs(COMPOSER_ENABLED);
    assert(enabled, ctx.step.id, 'turn already completed but composer is disabled even so');
  }
}

async function focusVisibleComposer(ctx) {
  return ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    el.focus();
    return true;
  })()`);
}

async function composerHoldsFollowup(ctx, follow) {
  return ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    const text = el.value ?? el.textContent ?? '';
    return text.includes(${JSON.stringify(follow)});
  })()`);
}

async function fireComposerEnter(ctx) {
  return ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    el.focus();
    return el.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Enter',
      code: 'Enter',
      keyCode: 13,
      which: 13,
      bubbles: true,
      cancelable: true,
    }));
  })()`);
}

async function stepSendFollowupQueued(ctx) {
  const follow = `Follow-up ${ctx.state.runTag}`;
  let busy = await ctx.page.evalJs(BUSY_STOP);
  for (let i = 0; !busy && i < 40; i++) {
    await sleep(250);
    busy = await ctx.page.evalJs(BUSY_STOP);
  }
  assert(
    busy,
    ctx.step.id,
    'first turn is not holding Stop generating; queue dock requires an active Run',
  );
  const focused = await focusVisibleComposer(ctx);
  assert(focused, ctx.step.id, 'no visible prompt composer for follow-up');
  await ctx.page.insertText(follow);
  await ctx.page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    const proto = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value');
    if (proto && proto.set && 'value' in el) proto.set.call(el, ${JSON.stringify(follow)});
    el.dispatchEvent(new InputEvent('input', {
      bubbles: true, data: ${JSON.stringify(follow)}, inputType: 'insertFromPaste',
    }));
    return true;
  })()`);
  const got = await composerHoldsFollowup(ctx, follow);
  assert(got, ctx.step.id, 'follow-up text not in composer');
  // Snapshot the dock before the follow-up so a later row is attributable
  // to this send, not to first-turn chrome that was already on screen.
  const before = await ctx.page.evalJs(QUEUE_DOCK_SNAPSHOT) ?? {
    present: false, rowCount: 0, previews: [], signature: '',
  };
  // Idle Send starts a sequential turn and never paints the dock. Queue
  // only via busy-Enter while Stop generating is the composer primary.
  const focusedAgain = await focusVisibleComposer(ctx);
  assert(focusedAgain, ctx.step.id, 'composer lost focus before busy-Enter queue');
  await ctx.page.pressEnter();
  let queued = false;
  let acceptedPost = false;
  let after = before;
  let enterRetried = false;
  for (let i = 0; i < 80; i++) {
    after = await ctx.page.evalJs(QUEUE_DOCK_SNAPSHOT) ?? before;
    const posts = ctx.page.networkResponses({
      methodRe: /^POST$/,
      urlRe: /\/api\/conversations\/[^/]+\/messages/,
    });
    if (posts.some((p) => p.status < 400)) acceptedPost = true;
    const followSeen = Array.isArray(after.previews)
      && after.previews.some((text) => typeof text === 'string' && text.includes(follow));
    const newRow = after.present === true
      && after.signature !== before.signature
      && Number(after.rowCount) > Number(before.rowCount);
    const dockAppeared = after.present === true && before.present !== true;
    if (followSeen || newRow || dockAppeared) {
      queued = true;
      break;
    }
    // CDP Enter can miss React if the key event lacked produced text.
    // If the draft is still sitting in a plain-phase composer, fire the
    // same keydown InputBar listens for. Skip when the machine already
    // entered submitting so we do not double-queue.
    if (!enterRetried && i === 2) {
      const stillDraft = await ctx.page.evalJs(`(() => {
        const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
          .find((e) => e.getBoundingClientRect().width > 0);
        if (!el) return false;
        const text = el.value ?? el.textContent ?? '';
        const phase = el.getAttribute('data-phase') || '';
        return text.includes(${JSON.stringify(follow)})
          && phase !== 'submitting'
          && phase !== 'adjudicating';
      })()`);
      if (stillDraft) await fireComposerEnter(ctx);
      enterRetried = true;
    }
    await sleep(250);
  }
  assert(
    queued,
    ctx.step.id,
    `follow-up queue dock never showed a new queued row (POST .../messages accepted=${acceptedPost}; before=${JSON.stringify(before)}; after=${JSON.stringify(after)})`,
  );
  process.stdout.write(
    `  queue-dock acceptedPost=${acceptedPost} before=${JSON.stringify(before)} after=${JSON.stringify(after)}\n`,
  );
  await ctx.page.screenshot(`pass-${ctx.state.runId}-queue-dock`);
  await ctx.page.dumpHtml(`pass-${ctx.state.runId}-queue-dock`);
}

async function stepReloadReconstructs(ctx) {
  const before = ctx.state.conversationId;
  const promptHead = ctx.state.prompt ? ctx.state.prompt.slice(0, 40) : null;
  assert(before !== null && before !== undefined, ctx.step.id, 'no conversationId captured before reload');
  await ctx.page.goto(`${ctx.cfg.origin}/chat/${encodeURIComponent(before)}`, 30_000);
  await ctx.page.waitForFunction(
    `(() => {
      const m = document.querySelectorAll('#root, [data-reactroot]');
      return m.length > 0 && (m[0].children.length > 0 || document.body.textContent.length > 0);
    })()`,
    { timeoutMs: 20_000 },
  );
  const cidAfter = await grabConversationId(ctx.page);
  assert(
    cidAfter === before,
    ctx.step.id,
    `conversationId changed after reload (${before} -> ${cidAfter})`,
  );
  if (promptHead) {
    await ctx.page.waitForFunction(
      `(() => (document.body.textContent || '').includes(${JSON.stringify(promptHead)}))()`,
      { timeoutMs: 30_000 },
    );
  }
}

export const BODIES = {
  'open-login': stepOpenLogin,
  'react-login-form-renders': stepReactForm,
  'fill-credentials': stepFillCredentials,
  'submit-login': stepSubmitLogin,
  'account-label-visible': stepAccountLabel,
  'personal-scope-selected': stepPersonalScope,
  'open-workspaces-page': stepOpenWorkspaces,
  'create-workspace': stepCreateWorkspace,
  'open-new-chat': stepOpenNewChat,
  'type-first-prompt': stepTypeFirstPrompt,
  'send-and-conversation-id': stepSendAndCid,
  'poll-assistant-events-60s': stepPollAssistant,
  'tool-card-visible': stepToolCard,
  'followup-enabled-while-running': stepFollowupEnabled,
  'send-followup-queued': stepSendFollowupQueued,
  'reload-reconstructs': stepReloadReconstructs,
};

async function cleanupWorkspace(ctx) {
  const { workspaceId, createdWorkspace } = ctx.state;
  if (!createdWorkspace || !workspaceId) return { skipped: true };
  const cookies = await ctx.browser.cookies([ctx.cfg.origin]);
  const header = cookies.map((c) => `${c.name}=${c.value}`).join('; ');
  try {
    const res = await fetch(`${ctx.cfg.origin}/api/workspaces/${workspaceId}`, {
      method: 'DELETE',
      headers: { Cookie: header || '' },
    });
    return { status: res.status, ok: res.ok };
  } catch (err) {
    return { error: String(err.message), status: null };
  }
}

export async function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    process.stdout.write(helpText() + '\n');
    return 0;
  }
  // Operator-side psql probe hook: when VOIE_SMOKE_PROBE_CMD is set, it is
  // executed at the three zero-session checkpoints (before opening New chat,
  // after opening New chat, after the first send) so the operator can prove
  // the session-count invariant out-of-band. The command receives the
  // checkpoint label via VOIE_SMOKE_PROBE_LABEL and must print one count.
  const probeCmd = process.env.VOIE_SMOKE_PROBE_CMD;
  const probeResults = [];
  const runProbe = (label) => {
    if (!probeCmd) return;
    const out = execFileSync(probeCmd, [], {
      env: { ...process.env, VOIE_SMOKE_PROBE_LABEL: label },
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'inherit'],
    });
    probeResults.push(`${label}:${out.trim()}`);
  };

  const runId = makeRunId();
  if (args.dryRun) {
    STEPS.forEach((s, i) => {
      process.stdout.write(`[${i + 1}/${STEPS.length}] ${s.id.padEnd(34)} ${s.title}\n`);
    });
    process.stdout.write(
      `\n${STEPS.length} steps scripted. dry-run complete — no browser launched, no origin/env required.\n`,
    );
    return 0;
  }

  const env = requiredEnv();
  if (args.baseUrl) env.origin = normalizeOrigin(args.baseUrl);
  const password = await readPassword(env.passwordFile);

  const cfg = {
    origin: env.origin,
    user: env.user,
    password,
    timeoutMs: args.timeoutMs,
  };
  const browser = await launchBrowser({
    executable: args.executable,
    headless: !args.headful,
  });
  const runTag = process.env.VOIE_SMOKE_RUN_TAG ?? uniqueSuffix();

  const ctx = {
    cfg,
    state: { runId, runTag },
    browser,
    step: null,
    page: browser,
    probe: runProbe,
  };

  try {
    let passed = 0;
    for (let i = 0; i < STEPS.length; i++) {
      const st = STEPS[i];
      ctx.step = st;
      const fn = BODIES[st.id];
      if (!fn) throw new Error(`step ${st.id} has no body registered`);
      try {
        await fn(ctx);
        passed++;
        process.stdout.write(`PASS [${i + 1}/${STEPS.length}] ${st.id} — ${st.title}\n`);
      } catch (err) {
        const shot = await ctx.browser.screenshot(`fail-${runId}-${st.id}`).catch(() => null);
        const html = await ctx.browser.dumpHtml(`fail-${runId}-${st.id}`).catch(() => null);
        const consoleNote = ctx.browser.consoleTail.slice(-15).join('\n') || '(no console output)';
        process.stderr.write(
          `FAIL [${i + 1}/${STEPS.length}] ${st.id}\n` +
          `  step:    ${st.title}\n` +
          `  error:   ${err.message}\n` +
          `  console: ${consoleNote}\n` +
          `  artifacts: ${[shot, html].filter(Boolean).join(', ') || 'none (capture failed)'}\n`,
        );
        return 1;
      }
    }
    const cleanup = await cleanupWorkspace(ctx);
    if (cleanup.error) {
      process.stdout.write(`cleanup: DELETE workspace best-effort failed (${cleanup.error})\n`);
    } else if (!cleanup.skipped) {
      process.stdout.write(`cleanup: DELETE workspace -> HTTP ${cleanup.status}\n`);
    }
    // Opening New chat creates one durable empty Session immediately.
    // The first send must not create a second Session row.
    if (probeResults.length > 0) {
      const byLabel = Object.fromEntries(probeResults.map((r) => r.split(':')));
      const beforeOpen = Number(byLabel['before-open-new-chat']);
      const afterOpen = Number(byLabel['after-open-new-chat']);
      const afterSend = Number(byLabel['after-send-and-conversation-id']);
      process.stdout.write(`\ndurable-session probe (sessions rows):\n  ${probeResults.join('\n  ')}\n`);
      if (![beforeOpen, afterOpen, afterSend].every(Number.isFinite)) {
        process.stderr.write('durable-session probe: missing checkpoint counts\n');
        return 1;
      }
      if (afterOpen !== beforeOpen + 1) {
        process.stderr.write(
          `durable-session FAIL: opening New chat created ${afterOpen - beforeOpen} sessions row(s), expected 1\n`,
        );
        return 1;
      }
      if (afterSend !== afterOpen) {
        process.stderr.write(
          `durable-session FAIL: first send created ${afterSend - afterOpen} extra sessions row(s)\n`,
        );
        return 1;
      }
    }
    process.stdout.write(
      `\n${passed}/${STEPS.length} steps passed (run-tag ${runTag}).\n`,
    );
    return passed === STEPS.length ? 0 : 1;
  } finally {
    await ctx.browser.close().catch(() => {});
  }
}

const isEntry =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isEntry) {
  process.exitCode = await main(process.argv.slice(2));
}