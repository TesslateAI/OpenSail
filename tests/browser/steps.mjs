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
    note: 'scope control exposes a "personal" entry marked selected/active after /api/scopes resolves',
  },
  {
    id: 'open-workspaces-page',
    title: 'open the workspaces page',
    note: 'portal nav entry for Workspaces is present and its list surface mounts',
  },
  {
    id: 'create-or-select-workspace',
    title: 'create a workspace (or select existing)',
    note: 'creates `Smoke <run-tag>` when absent, else reuses the first existing smoke workspace — idempotent; created workspaces are cleanup-registered for API delete',
  },
  {
    id: 'open-new-chat',
    title: 'start a New chat',
    note: 'composer (textarea/contenteditable) becomes visible inside the workspace',
  },
  {
    id: 'type-first-prompt',
    title: 'type the first prompt',
    note: 'prompt text lands in the composer via native event dispatch',
  },
  {
    id: 'send-and-conversation-id',
    title: 'send -> conversationId appears in URL/state',
    note: 'conversationId captured from URL param or canonical state. NO server session-count assertion here (operator procedure in README §6)',
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
    note: 'second prompt accepted; UI surfaces a queued/pending state before processing',
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
  if (location.pathname.includes('workspace')) return true;
  const sels = ['[data-workspaces]', '[data-testid="workspace-list"]', '[role="list"]'];
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.getBoundingClientRect().width > 0) return true;
    }
  }
  return false;
})()`;

const COMPOSER_READY = `(() => {
  const nodes = Array.from(document.querySelectorAll(
    'textarea, [contenteditable="true"]'));
  return nodes.some((e) => e.getBoundingClientRect().width > 0);
})()`;

const COMPOSER_SEL = 'textarea, [contenteditable="true"]';

const ASSISTANT_EVIDENCE = `(() => {
  const sels = [
    '[data-role="assistant"]',
    '[data-message-role="assistant"]',
    '[data-testid*="assistant" i]',
    '[class*="assistant" i]',
    '[data-tool]',
  ];
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.getBoundingClientRect().width === 0) continue;
      if ((el.textContent || '').trim().length > 0) return true;
    }
  }
  return false;
})()`;

const TOOL_CARD = `(() => {
  const sels = [
    '[data-tool-card]',
    '[data-tool]',
    '[data-testid*="tool" i]',
    '[class*="tool-card" i]',
  ];
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.getBoundingClientRect().width > 0) return true;
    }
  }
  return false;
})()`;

const STREAMING_MARKER = `(() => !!document.querySelector(
  '[data-streaming="true"], [aria-busy="true"], [data-status="streaming"], .streaming, [data-testid*="streaming" i]',
))()`;

const COMPOSER_ENABLED = `(() => {
  const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
    .find((e) => e.getBoundingClientRect().width > 0);
  if (!el) return false;
  return !el.disabled && !(el.closest('[data-testid*="composer" i]')?.hasAttribute('disabled'));
})()`;

const QUEUED_INDICATOR = `(() => {
  const sels = [
    '[data-queued]',
    '[data-status="queued"]',
    '[data-state="pending"]',
    '[data-testid*="queued" i]',
    '[aria-live="polite"]',
    '[aria-live="assertive"]',
  ];
  const re = /queued|pending|waiting|in progress/i;
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.getBoundingClientRect().width === 0) continue;
      const hay = [
        el.textContent || '',
        el.getAttribute('data-status') || '',
        el.getAttribute('data-state') || '',
      ].join(' ');
      if (re.test(hay)) return true;
    }
  }
  return false;
})()`;

const CONVO_ID_PROBE = `(() => {
  const u = new URL(location.href);
  for (const k of ['conversation', 'thread', 'c', 'id']) {
    const v = u.searchParams.get(k);
    if (v && v.length > 0) return v;
  }
  const m = location.pathname.match(/\/(?:conversations|threads?|c)\/([A-Za-z0-9_-]+)/);
  if (m) return m[1];
  const els = document.querySelectorAll('[data-conversation-id], [data-conversation], [data-thread-id]');
  for (const el of els) {
    const v = el.getAttribute('data-conversation-id')
      || el.getAttribute('data-conversation')
      || el.getAttribute('data-thread-id');
    if (v && v.length > 0) return v;
  }
  return null;
})()`;

const SEND_NOW = `(() => {
  const sels = [
    'button[type="submit"]',
    '[data-testid="send"]',
    '[aria-label*="send" i]',
    'button',
    '[role="button"]',
  ];
  const near = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
    .find((e) => e.getBoundingClientRect().width > 0);
  for (const s of sels) {
    for (const el of document.querySelectorAll(s)) {
      if (el.getBoundingClientRect().width === 0) continue;
      const t = ((el.textContent || '') + ' ' + (el.getAttribute('aria-label') || '')).trim().toLowerCase();
      if (t.includes('send') || t === '→' || t === 'submit') {
        el.click();
        return 'click';
      }
    }
  }
  if (near) {
    near.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Enter', code: 'Enter', bubbles: true, cancelable: true,
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
  for (let i = 0; i < 40; i++) {
    const v = await page.evalJs(`(() => { const r = (${CONVO_ID_PROBE}); return r; })()`);
    if (v) return v;
    await sleep(250);
  }
  return null;
}

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
}

async function stepOpenWorkspaces(ctx) {
  const clicked = await ctx.page.evalJs(clickTextSnippet(
    ['a[href="/workspaces"]', '[data-testid="nav-workspaces"]', 'button', '[role="button"]'],
    /^workspaces$/i,
  ));
  assert(clicked, ctx.step.id, 'no clickable Workspaces nav entry found');
  await ctx.page.waitForFunction(WS_VISIBLE, { timeoutMs: 20_000 });
}

async function stepCreateOrSelectWorkspace(ctx) {
  const target = `Smoke ${ctx.state.runTag}`;
  const existing = await ctx.page.evalJs(
    `(() => {
      const sels = ['[data-workspace]', '[data-testid="workspace"]', '[role="listitem"]', 'article', 'td.mono'];
      for (const s of sels) {
        for (const el of document.querySelectorAll(s)) {
          if (el.getBoundingClientRect().width === 0) continue;
          const t = (el.textContent || '').trim();
          if (t.startsWith('Smoke ')) return t;
        }
      }
      return null;
    })()`,
  );
  if (existing) {
    // Workspaces render as table rows; no row click is part of the
    // product flow. The New-chat context selector picks the surface.
    ctx.state.workspaceName = existing;
    ctx.state.createdWorkspace = false;
  } else {
    // Product shape: an inline aria-labeled name input and a "New
    // workspace" button that stays disabled until the name fills.
    // Fill first; React commits the name asynchronously, so wait for the
    // create button to become enabled before clicking it.
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
    // Fabric provisioning is realistic; wait until the row settles out of
    // "Creating…" (bounded to 60s) instead of racing the request.
    await ctx.page.waitForFunction(
      `(() => {
        const rows = Array.from(document.querySelectorAll('td.mono'));
        const cell = rows.find((c) => (c.textContent || '').includes(${JSON.stringify(target)}));
        if (!cell) return false;
        return true;
      })()`,
      { timeoutMs: 60_000 },
    );
    // The row appears as soon as the create response lands; provisioning
    // may still be async, so also allow a bounded settle for "creating".
    await sleep(1500);
    ctx.state.workspaceName = target;
    ctx.state.createdWorkspace = true;
  }

  const wid = await ctx.page.evalJs(
    `(() => {
      const el = document.querySelector('[data-workspace-id], [data-workspace]');
      if (el) {
        const v = el.getAttribute('data-workspace-id') || el.getAttribute('data-workspace');
        if (v && v.length > 0) return v;
      }
      const m = location.href.match(/\/(?:workspaces?)\/([0-9a-fA-F-]{8,})/);
      return m ? m[1] : null;
    })()`,
  );
  ctx.state.workspaceId = wid;
}

async function stepOpenNewChat(ctx) {
  ctx.probe('before-open-new-chat');
  const ok = await ctx.page.evalJs(clickTextSnippet(
    ['[data-testid="new-chat"]', '[data-testid="new-conversation"]', 'button', '[role="button"]'],
    /^new$|new chat|new conversation/i,
  ));
  assert(ok, ctx.step.id, 'no New chat control found');
  await ctx.page.waitForFunction(COMPOSER_READY, { timeoutMs: 10_000 });
  ctx.probe('after-open-new-chat');
}

async function stepTypeFirstPrompt(ctx) {
  const prompt = `First prompt ${ctx.state.runTag}`;
  await ctx.page.evalJs(fillSnippet(COMPOSER_SEL, prompt));
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

async function stepSendAndCid(ctx) {
  const sent = await ctx.page.evalJs(`(${SEND_NOW})`);
  assert(sent !== null, ctx.step.id, 'no send mechanism (button or Enter) found');
  const cid = await grabConversationId(ctx.page);
  assert(
    cid !== null,
    ctx.step.id,
    'conversationId never appeared in URL params, /conversations|threads|c/ path, or [data-conversation-*] state within 10s',
  );
  ctx.state.conversationId = cid;
  ctx.probe('after-send-and-conversation-id');
}

async function stepPollAssistant(ctx) {
  await ctx.page.waitForFunction(ASSISTANT_EVIDENCE, {
    timeoutMs: ctx.cfg.timeoutMs ?? 60_000,
    intervalMs: 500,
  });
}

async function stepToolCard(ctx) {
  await ctx.page.waitForFunction(TOOL_CARD, { timeoutMs: 15_000 });
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

async function stepSendFollowupQueued(ctx) {
  await ctx.page.evalJs(fillSnippet(COMPOSER_SEL, `Follow-up ${ctx.state.runTag}`));
  const sent = await ctx.page.evalJs(`(${SEND_NOW})`);
  assert(sent !== null, ctx.step.id, 'no send mechanism for follow-up found');
  const appeared = await ctx.page.waitForFunction(QUEUED_INDICATOR, { timeoutMs: 20_000 });
  assert(appeared, ctx.step.id, 'queued/pending indicator never appeared after follow-up send');
}

async function stepReloadReconstructs(ctx) {
  const before = ctx.state.conversationId;
  const promptHead = ctx.state.prompt ? ctx.state.prompt.slice(0, 40) : null;
  await ctx.page.reload(30_000);
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
      { timeoutMs: 15_000 },
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
  'create-or-select-workspace': stepCreateOrSelectWorkspace,
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
    // Zero-session-on-open proof (operator-side, probe hook only):
    // opening New chat must not create a sessions row (delta 0), and the
    // first send must create exactly one (delta 1).
    if (probeResults.length > 0) {
      const byLabel = Object.fromEntries(probeResults.map((r) => r.split(':')));
      const beforeOpen = Number(byLabel['before-open-new-chat']);
      const afterOpen = Number(byLabel['after-open-new-chat']);
      const afterSend = Number(byLabel['after-send-and-conversation-id']);
      process.stdout.write(`\nzero-session probe (sessions rows):\n  ${probeResults.join('\n  ')}\n`);
      if (![beforeOpen, afterOpen, afterSend].every(Number.isFinite)) {
        process.stderr.write('zero-session probe: missing checkpoint counts\n');
        return 1;
      }
      if (afterOpen !== beforeOpen) {
        process.stderr.write(
          `zero-session FAIL: opening New chat created ${afterOpen - beforeOpen} sessions row(s)\n`,
        );
        return 1;
      }
      if (afterSend !== afterOpen + 1) {
        process.stderr.write(
          `zero-session FAIL: first send created ${afterSend - afterOpen} sessions row(s), expected 1\n`,
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