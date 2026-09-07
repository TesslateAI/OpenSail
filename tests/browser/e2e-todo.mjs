// Product todo E2E: login → fresh Workspace → durable Chat → agent builds a
// usable todo app on the real stack. 429 on Workspace create is a failure.
//
//   just e2e-todo
//
// Uses the same VOIE_SMOKE_* environment as browser-smoke.

import { pathToFileURL } from 'node:url';

import {
  assert,
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
import { BODIES, STEPS } from './steps.mjs';

const TODO_PROMPT =
  'Build a small usable todo list application. Implement it, run it, and expose the dev preview.';
const FOLLOWUP_PROMPT =
  'Make completed todos visually distinct and add a clear-completed action.';

const PREFIX_IDS = [
  'open-login',
  'react-login-form-renders',
  'fill-credentials',
  'submit-login',
  'account-label-visible',
  'personal-scope-selected',
  'open-workspaces-page',
  'create-workspace',
  'open-new-chat',
];

const COMPOSER_SEL =
  'textarea[placeholder], textarea, [contenteditable="true"], [role="textbox"]';

function routeKey(url) {
  try {
    const path = new URL(url, 'http://voie.local').pathname;
    return path.replace(/[0-9a-fA-F-]{8,}/g, ':id');
  } catch {
    return String(url);
  }
}

function countByRoute(page, extra = {}) {
  const hits = page.networkResponses({
    urlRe: /\/api\//,
    ...extra,
  });
  const counts = new Map();
  for (const hit of hits) {
    const key = `${hit.method} ${routeKey(hit.url)}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return { hits, counts };
}

function printCounts(label, snapshot) {
  const rows = [...snapshot.counts.entries()].sort((a, b) => b[1] - a[1]);
  process.stdout.write(`\n${label}: ${snapshot.hits.length} /api/ requests\n`);
  for (const [key, n] of rows.slice(0, 24)) {
    process.stdout.write(`  ${String(n).padStart(4)}  ${key}\n`);
  }
}

const COMPOSER_ENABLED = `(() => {
  const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
    .find((e) => e.getBoundingClientRect().width > 0);
  if (!el) return false;
  const phase = el.getAttribute('data-phase') || '';
  return !el.disabled && phase !== 'submitting' && phase !== 'adjudicating';
})()`;

const BUSY_STOP = `(() => {
  for (const label of ['Stop generating', 'Stop']) {
    const btn = document.querySelector('button[aria-label="' + label + '"]');
    if (btn && btn.getBoundingClientRect().width > 0) return true;
  }
  return false;
})()`;

function acceptedMessagePosts(page) {
  return page.networkResponses({
    methodRe: /^POST$/,
    urlRe: /\/api\/conversations\/[0-9A-Fa-f-]{36}\/messages/,
  }).filter((hit) => hit.status < 400);
}

async function focusComposer(page, stepId) {
  await page.waitForFunction(COMPOSER_ENABLED, { timeoutMs: 30_000 });
  const focused = await page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    el.focus();
    if (typeof el.select === 'function') el.select();
    return true;
  })()`);
  assert(focused, stepId, 'no visible prompt composer');
}

async function fillComposer(page, text, stepId) {
  await focusComposer(page, stepId);
  await page.insertText(text);
  const holds = await page.evalJs(`(() => {
    const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
      .find((e) => e.getBoundingClientRect().width > 0);
    if (!el) return false;
    const proto = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value');
    if (proto && proto.set && 'value' in el) proto.set.call(el, ${JSON.stringify(text)});
    el.dispatchEvent(new InputEvent('input', {
      bubbles: true, data: ${JSON.stringify(text)}, inputType: 'insertFromPaste',
    }));
    return (el.value ?? el.textContent ?? '').includes(${JSON.stringify(text)});
  })()`);
  assert(holds, stepId, 'prompt text did not land in the composer');
}

async function typeComposer(page, text) {
  await fillComposer(page, text, 'send-prompt');
  await page.waitForFunction(
    `(() => {
      const btn = document.querySelector('button[aria-label="Send message"]');
      return Boolean(btn) && btn.disabled === false;
    })()`,
    { timeoutMs: 10_000 },
  );
  const clicked = await page.evalJs(`(() => {
    const btn = document.querySelector('button[aria-label="Send message"]');
    if (!btn || btn.disabled) return false;
    btn.click();
    return true;
  })()`);
  assert(clicked, 'send-prompt', 'Send message was not clicked');
}

async function fireComposerEnter(page) {
  return page.evalJs(`(() => {
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

async function sendFollowup(page, text) {
  const before = acceptedMessagePosts(page).length;
  await fillComposer(page, text, 'follow-up');
  const busy = await page.evalJs(BUSY_STOP);
  process.stdout.write(`follow-up: Stop generating visible=${Boolean(busy)}\n`);
  if (busy) {
    // Primary control is Stop while a Run is live. Queue the follow-up
    // with busy-Enter, the same path C6 smoke uses.
    await page.pressEnter();
  } else {
    const clicked = await page.evalJs(`(() => {
      const btn = document.querySelector('button[aria-label="Send message"]');
      if (!btn || btn.disabled) return false;
      btn.click();
      return true;
    })()`);
    assert(clicked, 'follow-up', 'Send message was not clicked and Stop generating was absent');
  }
  let accepted = before;
  let enterRetried = false;
  for (let i = 0; i < 80; i++) {
    accepted = acceptedMessagePosts(page).length;
    if (accepted > before) break;
    if (!enterRetried && busy && i === 2) {
      const stillDraft = await page.evalJs(`(() => {
        const el = Array.from(document.querySelectorAll(${JSON.stringify(COMPOSER_SEL)}))
          .find((e) => e.getBoundingClientRect().width > 0);
        if (!el) return false;
        const textNow = el.value ?? el.textContent ?? '';
        const phase = el.getAttribute('data-phase') || '';
        return textNow.includes(${JSON.stringify(text)})
          && phase !== 'submitting'
          && phase !== 'adjudicating';
      })()`);
      if (stillDraft) await fireComposerEnter(page);
      enterRetried = true;
    }
    await sleep(250);
  }
  assert(
    accepted > before,
    'follow-up',
    `second Run was not admitted (accepted ${accepted} after ${before}; busy=${Boolean(busy)})`,
  );
}

async function personalProjectId(page) {
  const projects = await pageJson(page, '/api/projects');
  const personal = (projects.body?.items ?? []).find((item) => item.kind === 'personal')
    ?? (projects.body?.items ?? [])[0];
  assert(personal?.id, 'personal-project', `no personal project: ${JSON.stringify(projects.body)?.slice(0, 200)}`);
  return personal.id;
}

async function mutateDelete(page, path) {
  return page.evalJs(`(async () => {
    const res = await fetch(${JSON.stringify(path)}, {
      method: 'DELETE',
      credentials: 'same-origin',
      headers: { 'x-voie-intent': 'mutate' },
    });
    const text = await res.text();
    let body = null;
    try { body = JSON.parse(text); } catch { body = text; }
    return { status: res.status, body };
  })()`, 120_000);
}

async function cleanupCreatedIds(page, state) {
  const notes = [];
  if (state.projectId && state.applicationId) {
    const del = await mutateDelete(
      page,
      `/api/projects/${state.projectId}/applications/${state.applicationId}`,
    );
    notes.push(`application ${state.applicationId} HTTP ${del.status}`);
    if (del.status >= 400 && del.status !== 404) {
      process.stderr.write(
        `e2e cleanup: application ${state.applicationId} failed: ${JSON.stringify(del.body)?.slice(0, 200)}\n`,
      );
    }
  }
  if (state.projectId && state.workspaceId) {
    const del = await mutateDelete(
      page,
      `/api/projects/${state.projectId}/workspaces/${state.workspaceId}`,
    );
    notes.push(`workspace ${state.workspaceId} HTTP ${del.status}`);
    if (del.status >= 400 && del.status !== 404 && del.status !== 409) {
      process.stderr.write(
        `e2e cleanup: workspace ${state.workspaceId} failed: ${JSON.stringify(del.body)?.slice(0, 200)}\n`,
      );
    }
  }
  if (notes.length > 0) {
    process.stdout.write(`e2e cleanup (created IDs only): ${notes.join('; ')}\n`);
  }
}

async function pageJson(page, path) {
  return page.evalJs(`(async () => {
    const res = await fetch(${JSON.stringify(path)}, {
      credentials: 'same-origin',
      headers: { accept: 'application/json' },
    });
    const text = await res.text();
    let body = null;
    try { body = JSON.parse(text); } catch { body = text; }
    return { status: res.status, body };
  })()`, 30_000);
}

function isLiveDeployment(row) {
  const state = String(row?.state || '').toLowerCase();
  return state === 'active';
}

async function waitForProduct(page, projectId, workspaceId, timeoutMs, opts = {}) {
  const afterReleaseId = opts.afterReleaseId;
  const afterDeploymentId = opts.afterDeploymentId;
  const deadline = Date.now() + timeoutMs;
  let last = null;
  if (!workspaceId) {
    throw new Error('todo app wait requires the created Workspace id');
  }
  while (Date.now() < deadline) {
    const apps = await pageJson(page, `/api/projects/${projectId}/applications`);
    const items = Array.isArray(apps.body?.items) ? apps.body.items : [];
    const app = items.find((item) => item.workspaceId === workspaceId);
    const snapshot = { workspaceId, app: app ?? null, releases: [], environments: [], deployments: [] };
    if (app?.id) {
      const releases = await pageJson(page, `/api/applications/${app.id}/releases`);
      const environments = await pageJson(page, `/api/applications/${app.id}/environments`);
      const releaseItems = Array.isArray(releases.body?.items) ? releases.body.items : [];
      const envItems = Array.isArray(environments.body?.items) ? environments.body.items : [];
      snapshot.releases = releaseItems.map((row) => ({ id: row.id, state: row.state }));
      snapshot.environments = envItems.map((row) => ({
        id: row.id, kind: row.kind, hostname: row.hostname, state: row.state,
      }));
      const readyRelease = afterReleaseId
        ? releaseItems.find((row) => row.state === 'ready' && row.id !== afterReleaseId)
        : releaseItems.find((row) => row.state === 'ready');
      const env = envItems.find((row) => String(row.kind || row.name || '').toLowerCase().includes('dev'))
        ?? envItems[0];
      let healthy = null;
      if (env?.id) {
        const deployments = await pageJson(page, `/api/environments/${env.id}/deployments`);
        const depItems = Array.isArray(deployments.body?.items) ? deployments.body.items : [];
        snapshot.deployments = depItems.map((row) => ({
          id: row.id, state: row.state, desiredState: row.desiredState, lastErrorCode: row.lastErrorCode,
        }));
        healthy = depItems.find((row) =>
          isLiveDeployment(row) && (!afterDeploymentId || row.id !== afterDeploymentId),
        ) ?? null;
      }
      const hostname = env?.hostname || app.hostname;
      last = snapshot;
      if (readyRelease && hostname && healthy && env?.id) {
        return {
          app,
          readyRelease,
          healthy,
          hostname,
          applicationId: app.id,
          environmentId: env.id,
        };
      }
    } else {
      last = snapshot;
    }
    await sleep(5000);
  }
  throw new Error(`todo app did not become ready: ${JSON.stringify(last)?.slice(0, 1200)}`);
}

async function openPrivatePreview(page, applicationId, environmentId) {
  const login = await pageJson(
    page,
    `/api/preview/login?applicationId=${encodeURIComponent(applicationId)}&environmentId=${encodeURIComponent(environmentId)}`,
  );
  const redirect = login.body?.redirect;
  assert(
    login.status < 400 && typeof redirect === 'string' && redirect.startsWith('https://'),
    'preview-login',
    `preview login failed HTTP ${login.status}: ${JSON.stringify(login.body)?.slice(0, 300)}`,
  );
  await page.goto(redirect, 60_000);
  // The callback is a 302 with an empty body. Wait until the browser has
  // followed it onto the Application document (or a typed preview error).
  await page.waitForFunction(
    `(() => {
      if (location.pathname.indexOf('/.voie/auth/') === 0) return false;
      const html = document.documentElement ? document.documentElement.outerHTML : '';
      const text = (document.body && document.body.textContent || '').trim();
      if (html.includes('preview authorization failed') || text === 'not found') return true;
      return Boolean(document.querySelector('input[type="text"], input:not([type]), textarea, [contenteditable="true"]'));
    })()`,
    { timeoutMs: 45_000 },
  );
  const html = await page.evalJs('document.documentElement.outerHTML');
  assert(
    typeof html === 'string' && !html.includes('preview authorization failed'),
    'preview-login',
    `preview still unauthorized after handshake at ${redirect}`,
  );
  const bodyText = await page.evalJs(`(() => (document.body && document.body.textContent || '').trim())()`);
  assert(
    bodyText !== 'not found',
    'todo-preview',
    `preview origin returned gateway catch-all at ${redirect}`,
  );
  return page.currentUrl();
}

async function interactTodo(page) {
  const url = await page.currentUrl();
  const marker = `e2e-todo-${Date.now()}`;
  const filled = await page.evalJs(`(() => {
    const input = document.querySelector('input[type="text"], input:not([type]), textarea, [contenteditable="true"]');
    if (!input) return false;
    input.focus();
    if ('value' in input) input.value = ${JSON.stringify(marker)};
    else input.textContent = ${JSON.stringify(marker)};
    input.dispatchEvent(new InputEvent('input', {
      bubbles: true, data: ${JSON.stringify(marker)}, inputType: 'insertText',
    }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  })()`);
  assert(filled, 'todo-interact', `no input on preview ${url}`);
  const submitted = await page.evalJs(`(() => {
    const btn = Array.from(document.querySelectorAll('button')).find((b) => {
      const t = ((b.textContent || '') + ' ' + (b.getAttribute('aria-label') || '')).toLowerCase();
      return b.type === 'submit' || t.includes('add') || t.includes('create');
    });
    const form = document.querySelector('form');
    if (btn) btn.click();
    else if (form && typeof form.requestSubmit === 'function') form.requestSubmit();
    else if (form) form.submit();
    return Boolean(btn || form);
  })()`);
  assert(submitted, 'todo-interact', `no submit control on preview ${url}`);
  await sleep(2500);
  const shown = await page.evalJs(
    `(() => (document.body.textContent || '').includes(${JSON.stringify(marker)}))()`,
  );
  assert(shown, 'todo-interact', `typed todo ${marker} did not render at ${url}`);
  return { url, marker };
}

export async function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    process.stdout.write('just e2e-todo — real product flow against VOIE_SMOKE_ORIGIN\n');
    return 0;
  }
  if (args.dryRun) {
    process.stdout.write('e2e-todo: login, fresh workspace, durable chat, todo build, preview, follow-up, reload\n');
    return 0;
  }

  const env = requiredEnv();
  if (args.baseUrl) env.origin = normalizeOrigin(args.baseUrl);
  const password = await readPassword(env.passwordFile);
  const runId = makeRunId();
  const runTag = process.env.VOIE_SMOKE_RUN_TAG ?? uniqueSuffix();
  const browser = await launchBrowser({
    executable: args.executable,
    headless: !args.headful,
  });
  const ctx = {
    cfg: { origin: env.origin, user: env.user, password, timeoutMs: args.timeoutMs },
    state: { runId, runTag },
    browser,
    step: null,
    page: browser,
    probe: () => {},
  };

  const prefix = STEPS.filter((step) => PREFIX_IDS.includes(step.id));
  try {
    for (let i = 0; i < prefix.length; i++) {
      const st = prefix[i];
      ctx.step = st;
      const fn = BODIES[st.id];
      if (!fn) throw new Error(`step ${st.id} has no body`);
      if (st.id === 'create-workspace') {
        ctx.state.projectId = await personalProjectId(ctx.page);
      }
      await fn(ctx);
      process.stdout.write(`PASS [${i + 1}/${prefix.length}] ${st.id}\n`);
    }

    const portal = countByRoute(ctx.page);
    printCounts('initial portal + workspace + new chat', portal);
    const blobGets = [...portal.counts.entries()].filter(([key]) => /blob|objects\//i.test(key));
    const eventsGets = portal.hits.filter((hit) => hit.method === 'GET' && /\/api\/events(\?|$)/.test(hit.url));
    const waitPolls = eventsGets.filter((hit) => /[?&]wait=/.test(hit.url));
    const headReads = eventsGets.filter((hit) => /[?&]head=/.test(hit.url));
    const otherEvents = eventsGets.filter(
      (hit) => !/[?&]wait=/.test(hit.url) && !/[?&]head=/.test(hit.url),
    );
    assert(blobGets.length === 0, 'net', 'browser fetched Blob history objects');
    assert(
      otherEvents.length === 0,
      'net',
      `pseudo-long-poll /api/events without wait/head: ${otherEvents.map((h) => h.url).join(', ')}`,
    );
    // wait= is one held long-poll (~20s). Prefix includes Workspace Ready, so
    // a handful of reconnects is expected. 1 Hz /api/events is `otherEvents`.
    assert(
      waitPolls.length <= 40,
      'net',
      `too many held /api/events wait polls during mount: ${waitPolls.length}`,
    );
    process.stdout.write(
      `events: wait=${waitPolls.length} head=${headReads.length} other=${otherEvents.length}\n`,
    );

    const projectId = await ctx.page.evalJs(`(() => {
      const m = location.href.match(/project[=/]([0-9a-fA-F-]{36})/);
      return m ? m[1] : null;
    })()`);
    const projects = await pageJson(ctx.page, '/api/projects');
    const personal = (projects.body?.items ?? []).find((item) => item.kind === 'personal')
      ?? (projects.body?.items ?? [])[0];
    const scopeId = projectId || personal?.id;
    assert(scopeId, 'todo', `no personal project: ${JSON.stringify(projects.body)?.slice(0, 200)}`);
    ctx.state.projectId = scopeId;

    await typeComposer(ctx.page, TODO_PROMPT);
    const accepted = ctx.page.networkResponses({
      methodRe: /^POST$/,
      urlRe: /\/api\/conversations\/[0-9A-Fa-f-]{36}\/messages/,
    });
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline && accepted.filter((p) => p.status < 400).length === 0) {
      await sleep(250);
    }
    const messagePosts = ctx.page.networkResponses({
      methodRe: /^POST$/,
      urlRe: /\/api\/conversations\/[0-9A-Fa-f-]{36}\/messages/,
    });
    assert(
      messagePosts.some((p) => p.status < 400),
      'todo-prompt',
      `first Run was not accepted: ${JSON.stringify(messagePosts.map((p) => p.status))}`,
    );

    process.stdout.write('waiting for Application/Release/dev Deployment...\n');
    const product = await waitForProduct(
      ctx.page,
      ctx.state.projectId,
      ctx.state.workspaceId,
      20 * 60 * 1000,
    );
    assert(product.hostname, 'todo-preview', 'no dev hostname on Application status');
    assert(product.healthy, 'todo-preview', 'no healthy/active dev Deployment');
    ctx.state.applicationId = product.applicationId;
    process.stdout.write(`dev hostname: ${product.hostname}\n`);
    const previewUrl = await openPrivatePreview(
      ctx.page,
      product.applicationId,
      product.environmentId,
    );
    const preview = await interactTodo(ctx.page);
    preview.url = preview.url || previewUrl;
    process.stdout.write(`todo rendered at ${preview.url} (${preview.marker})\n`);
    const previewShot = await ctx.page.screenshot(`pass-${runId}-todo-preview`).catch(() => null);
    if (previewShot) process.stdout.write(`preview screenshot: ${previewShot}\n`);

    await ctx.page.goto(`${env.origin}/chat/${ctx.state.conversationId}`, 30_000);
    await ctx.page.waitForFunction(COMPOSER_ENABLED, { timeoutMs: 30_000 });
    const historyVisible = await ctx.page.evalJs(
      `(() => (document.body.textContent || '').includes('todo list'))()`,
    );
    assert(historyVisible, 'chat-return', 'prior todo prompt not visible after returning to chat');
    await sendFollowup(ctx.page, FOLLOWUP_PROMPT);
    const followProduct = await waitForProduct(
      ctx.page,
      ctx.state.projectId,
      ctx.state.workspaceId,
      20 * 60 * 1000,
      {
        afterReleaseId: product.readyRelease.id,
        afterDeploymentId: product.healthy.id,
      },
    );
    await openPrivatePreview(
      ctx.page,
      followProduct.applicationId,
      followProduct.environmentId,
    );
    const changed = await ctx.page.evalJs(`(() => {
      const html = (document.documentElement.outerHTML || '').toLowerCase();
      const text = (document.body.textContent || '').toLowerCase();
      const struck = document.querySelector('[style*="line-through"], .completed, .done, s, del');
      return text.includes('clear') || text.includes('completed')
        || html.includes('clear') || html.includes('line-through')
        || Boolean(struck);
    })()`);
    assert(changed, 'follow-up-preview', 'follow-up did not change the preview');
    const followShot = await ctx.page.screenshot(`pass-${runId}-todo-followup`).catch(() => null);
    if (followShot) process.stdout.write(`follow-up screenshot: ${followShot}\n`);

    const beforeReload = countByRoute(ctx.page);
    await ctx.page.goto(`${env.origin}/chat/${ctx.state.conversationId}`, 30_000);
    await sleep(3000);
    const afterReload = countByRoute(ctx.page);
    const reloadHistory = afterReload.hits.filter((hit) => hit.url.includes('/history')).length
      - beforeReload.hits.filter((hit) => hit.url.includes('/history')).length;
    assert(reloadHistory <= 2, 'reload-history', `history fan-out on reload: ${reloadHistory}`);
    const reloadRuns = afterReload.hits.filter((hit) => /\/runs(\?|$)/.test(hit.url)).length
      - beforeReload.hits.filter((hit) => /\/runs(\?|$)/.test(hit.url)).length;
    assert(reloadRuns <= 4, 'reload-runs', `per-run GET explosion: ${reloadRuns}`);
    printCounts('after reload', afterReload);

    process.stdout.write(`\ne2e-todo passed. conversation=${ctx.state.conversationId} workspace=${ctx.state.workspaceId} application=${ctx.state.applicationId} preview=${preview.url}\n`);
    return 0;
  } catch (err) {
    const shot = await ctx.browser.screenshot(`fail-${runId}-e2e-todo`).catch(() => null);
    const html = await ctx.browser.dumpHtml(`fail-${runId}-e2e-todo`).catch(() => null);
    process.stderr.write(
      `FAIL e2e-todo\n  error: ${err.message}\n  artifacts: ${[shot, html].filter(Boolean).join(', ') || 'none'}\n`,
    );
    return 1;
  } finally {
    await cleanupCreatedIds(ctx.page, ctx.state).catch((err) => {
      process.stderr.write(`e2e cleanup: ${err.message}\n`);
    });
    // Leave the browser running only when headful; headless still closes.
    if (!args.headful) await browser.close().catch(() => {});
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((code) => {
    process.exitCode = code;
  });
}
