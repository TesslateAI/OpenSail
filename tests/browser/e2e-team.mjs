// Browser Team lifecycle: create team, add member, refuse Owner promotion,
// then revoke access. Product-visible behavior plus API state.
//
//   just e2e-team
//
// User A is VOIE_SMOKE_USER. User B is VOIE_SMOKE_USER_B plus
// VOIE_SMOKE_PASSWORD_FILE_B, or is created through platform-admin recovery
// when A is a platform admin.

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
  uniqueSuffix,
} from './harness.mjs';
import { BODIES, STEPS } from './steps.mjs';

const LOGIN_PREFIX = [
  'open-login',
  'react-login-form-renders',
  'fill-credentials',
  'submit-login',
  'account-label-visible',
];

async function loginAs(ctx, user, password, expectPersonal = false) {
  ctx.cfg.user = user;
  ctx.cfg.password = password;
  await ctx.page.goto(`${ctx.cfg.origin}/login`, 30_000);
  const steps = expectPersonal ? [...LOGIN_PREFIX, 'personal-scope-selected'] : LOGIN_PREFIX;
  for (const id of steps) {
    ctx.step = STEPS.find((step) => step.id === id);
    await BODIES[id](ctx);
  }
}

async function pageJson(page, path, init = {}) {
  const method = init.method || 'GET';
  const hasBody = Object.prototype.hasOwnProperty.call(init, 'body');
  const payload = init.body ?? null;
  return page.evalJs(`(async () => {
    const headers = { accept: 'application/json' };
    const opts = { method: ${JSON.stringify(method)}, credentials: 'same-origin', headers };
    if (${JSON.stringify(method)} !== 'GET') {
      headers['content-type'] = 'application/json';
      headers['x-voie-intent'] = 'mutate';
    }
    if (${JSON.stringify(hasBody)}) opts.body = JSON.stringify(${JSON.stringify(payload)});
    const res = await fetch(${JSON.stringify(path)}, opts);
    const text = await res.text();
    let parsed = null;
    try { parsed = JSON.parse(text); } catch { parsed = text; }
    return { status: res.status, body: parsed };
  })()`, 30_000);
}

async function signOut(page) {
  const clicked = await page.evalJs(`(() => {
    const btn = Array.from(document.querySelectorAll('button'))
      .find((el) => (el.textContent || '').trim() === 'Sign out');
    if (!btn) return false;
    btn.click();
    return true;
  })()`);
  assert(clicked, 'sign-out', 'Sign out was not clicked');
  await page.waitForFunction(
    `location.pathname === '/login' || document.querySelector('input[type="password"]')`,
    { timeoutMs: 20_000 },
  );
}

async function ensureUserB(page, runTag) {
  const envUser = process.env.VOIE_SMOKE_USER_B ?? '';
  const envFile = process.env.VOIE_SMOKE_PASSWORD_FILE_B ?? '';
  if (envUser && envFile) {
    return { username: envUser, password: await readPassword(envFile), created: false };
  }
  const me = await pageJson(page, '/api/me');
  assert(me.status === 200, 'user-b', `GET /api/me failed: ${me.status}`);
  if (me.body?.platformRole !== 'admin') {
    throw new Error(
      'User B is required: set VOIE_SMOKE_USER_B and VOIE_SMOKE_PASSWORD_FILE_B, or run as a platform admin so the test can create User B',
    );
  }
  const username = `teamb${runTag.replace(/[^a-zA-Z0-9]/g, '').slice(0, 12)}`;
  const password = `Tb${runTag}Aa1!`;
  const created = await pageJson(page, '/api/admin/users', {
    method: 'POST',
    body: {
      username,
      displayName: 'Team User B',
      password,
      platformRole: 'user',
    },
  });
  assert(
    created.status === 201 || created.status === 200,
    'user-b',
    `platform admin could not create User B: HTTP ${created.status} ${JSON.stringify(created.body)?.slice(0, 200)}`,
  );
  return { username, password, created: true };
}

export async function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    process.stdout.write('just e2e-team — Team create, add member, refuse Owner, revoke\n');
    return 0;
  }
  if (args.dryRun) {
    process.stdout.write(
      'e2e-team: login A, create Engineering, add B as Member, login B, A promotes B, B cannot become Owner, A removes B\n',
    );
    return 0;
  }

  const env = requiredEnv();
  if (args.baseUrl) env.origin = normalizeOrigin(args.baseUrl);
  const passwordA = await readPassword(env.passwordFile);
  const runId = makeRunId();
  const runTag = process.env.VOIE_SMOKE_RUN_TAG ?? uniqueSuffix();
  const browser = await launchBrowser({
    executable: args.executable,
    headless: !args.headful,
  });
  const ctx = {
    cfg: { origin: env.origin, user: env.user, password: passwordA, timeoutMs: args.timeoutMs },
    state: { runId, runTag },
    browser,
    step: null,
    page: browser,
    probe: () => {},
  };

  try {
    await loginAs(ctx, env.user, passwordA, true);

    const before = await pageJson(ctx.page, '/api/projects');
    assert(before.status === 200, 'personal-selected', `project list failed: ${before.status}`);
    const personal = (before.body?.items ?? []).find((item) => item.kind === 'personal');
    assert(personal?.id, 'personal-selected', 'Personal project was not selected after login');

    const opened = await ctx.page.evalJs(`(() => {
      const btn = Array.from(document.querySelectorAll('button'))
        .find((el) => (el.textContent || '').trim() === 'Create team');
      if (!btn) return false;
      btn.click();
      return true;
    })()`);
    assert(opened, 'create-team', 'Create team control was not visible beside the project switcher');
    await ctx.page.waitForFunction(
      `Boolean(document.querySelector('input[aria-label="Team name"]'))`,
      { timeoutMs: 10_000 },
    );
    const teamName = `Engineering ${runTag}`;
    await ctx.page.evalJs(`(() => {
      const input = document.querySelector('input[aria-label="Team name"]');
      const proto = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value');
      proto.set.call(input, ${JSON.stringify(teamName)});
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.dispatchEvent(new Event('change', { bubbles: true }));
      return true;
    })()`);
    const submitted = await ctx.page.evalJs(`(() => {
      const btn = Array.from(document.querySelectorAll('button[type="submit"]'))
        .find((el) => (el.textContent || '').trim() === 'Create team');
      if (!btn || btn.disabled) return false;
      btn.click();
      return true;
    })()`);
    assert(submitted, 'create-team', 'Create team submit was not clicked');
    await ctx.page.waitForFunction(
      `location.pathname === '/team' && /project=/.test(location.search) && (document.body.innerText || '').includes(${JSON.stringify(teamName)})`,
      { timeoutMs: 20_000 },
    );
    const teamPath = await ctx.page.evalJs(`location.pathname`);
    const teamSearch = await ctx.page.evalJs(`location.search`);
    const teamNamed = await ctx.page.evalJs(
      `(document.body.innerText || '').includes(${JSON.stringify(teamName)})`,
    );
    assert(teamPath === '/team', 'team-page', `expected /team, got ${teamPath}`);
    assert(
      typeof teamSearch === 'string' && /project=/.test(teamSearch),
      'team-page',
      `expected project query, got ${teamSearch}`,
    );
    assert(teamNamed === true, 'team-page', `Team page did not render ${teamName}`);

    const projectsAfter = await pageJson(ctx.page, '/api/projects');
    const teamIdFromUrl = String(teamSearch || '').match(/project=([^&]+)/)?.[1] ?? '';
    const team = (projectsAfter.body?.items ?? []).find((item) => item.id === decodeURIComponent(teamIdFromUrl))
      ?? (projectsAfter.body?.items ?? []).find((item) => item.kind === 'team' && item.name === teamName);
    assert(team?.id, 'create-team', `team missing: ${JSON.stringify(projectsAfter.body)?.slice(0, 300)}`);
    ctx.state.teamId = team.id;
    assert(team.role === 'owner', 'create-team', `creator role is ${team.role}, not owner`);

    const userB = await ensureUserB(ctx.page, runTag);
    ctx.state.userB = userB.username;

    await ctx.page.evalJs(`(() => {
      const input = document.querySelector('input[aria-label="Search users"]');
      if (!input) return false;
      const proto = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value');
      proto.set.call(input, ${JSON.stringify(userB.username)});
      input.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    })()`);
    const searched = await ctx.page.evalJs(`(() => {
      const btn = Array.from(document.querySelectorAll('button'))
        .find((el) => (el.textContent || '').trim() === 'Search users');
      if (!btn || btn.disabled) return false;
      btn.click();
      return true;
    })()`);
    assert(searched, 'add-member', 'Search users was not clicked');
    await ctx.page.waitForFunction(
      `Array.from(document.querySelectorAll('button')).some((el) => (el.textContent || '').trim() === 'Add')`,
      { timeoutMs: 15_000 },
    );
    const added = await ctx.page.evalJs(`(() => {
      const btn = Array.from(document.querySelectorAll('button'))
        .find((el) => (el.textContent || '').trim() === 'Add');
      if (!btn || btn.disabled) return false;
      btn.click();
      return true;
    })()`);
    assert(added, 'add-member', 'Add was not clicked on the candidate row');
    let bRow = null;
    const addDeadline = Date.now() + 15_000;
    while (Date.now() < addDeadline) {
      const members = await pageJson(ctx.page, `/api/projects/${team.id}/members`);
      bRow = (members.body?.items ?? []).find((item) => item.username === userB.username) ?? null;
      if (bRow?.role === 'member') break;
      await sleep(250);
    }
    assert(
      bRow?.role === 'member',
      'add-member',
      `B was not added as member: ${bRow ? JSON.stringify(bRow) : 'missing'}`,
    );

    await signOut(ctx.page);
    await loginAs(ctx, userB.username, userB.password);
    const bProjects = await pageJson(ctx.page, '/api/projects');
    const bTeam = (bProjects.body?.items ?? []).find((item) => item.id === team.id);
    assert(bTeam, 'b-access', 'B cannot select Engineering');
    assert(bTeam.role === 'member', 'b-access', `B role is ${bTeam.role}`);
    assert(bTeam.capabilities?.operateSessions === true, 'b-access', 'Member cannot operate');
    assert(bTeam.capabilities?.manageMembers === false, 'b-access', 'Member can manage members');
    const bDetail = await pageJson(ctx.page, `/api/projects/${team.id}`);
    assert(bDetail.status === 200, 'b-access', `B cannot read Engineering: ${bDetail.status}`);

    await signOut(ctx.page);
    await loginAs(ctx, env.user, passwordA);
    await ctx.page.goto(`${env.origin}/team?project=${team.id}`, 30_000);
    await ctx.page.waitForFunction(
      `Array.from(document.querySelectorAll('table tbody tr')).some((row) => (row.textContent || '').includes(${JSON.stringify(userB.username)}))`,
      { timeoutMs: 20_000 },
    );
    const promoted = await ctx.page.evalJs(`(() => {
      const row = Array.from(document.querySelectorAll('table tbody tr'))
        .find((el) => (el.textContent || '').includes(${JSON.stringify(userB.username)}));
      const select = row && row.querySelector('select');
      if (!select) return false;
      select.value = 'admin';
      select.dispatchEvent(new Event('change', { bubbles: true }));
      return true;
    })()`);
    assert(promoted, 'promote-admin', 'Role dropdown for B was not changed to Admin');
    await sleep(1000);
    const afterPromote = await pageJson(ctx.page, `/api/projects/${team.id}/members`);
    const promotedRow = (afterPromote.body?.items ?? []).find((item) => item.username === userB.username);
    assert(promotedRow?.role === 'admin', 'promote-admin', `B is ${promotedRow?.role}, not admin`);

    await signOut(ctx.page);
    await loginAs(ctx, userB.username, userB.password);
    const steal = await pageJson(ctx.page, `/api/projects/${team.id}/members/${promotedRow.userId}`, {
      method: 'PATCH',
      body: { role: 'owner' },
    });
    assert(steal.status === 400, 'refuse-owner', `Admin->Owner was not refused: HTTP ${steal.status}`);
    const still = await pageJson(ctx.page, `/api/projects/${team.id}/members`);
    const stillRow = (still.body?.items ?? []).find((item) => item.username === userB.username);
    assert(stillRow?.role === 'admin', 'refuse-owner', `B became ${stillRow?.role} after Owner attempt`);

    await signOut(ctx.page);
    await loginAs(ctx, env.user, passwordA);
    const removed = await pageJson(
      ctx.page,
      `/api/projects/${team.id}/members/${stillRow.userId}`,
      { method: 'DELETE' },
    );
    assert(removed.status === 200, 'remove-b', `remove failed: HTTP ${removed.status}`);

    await signOut(ctx.page);
    await loginAs(ctx, userB.username, userB.password);
    const lost = await pageJson(ctx.page, '/api/projects');
    const stillListed = (lost.body?.items ?? []).some((item) => item.id === team.id);
    assert(!stillListed, 'b-revoked', 'B still sees Engineering after removal');
    const denied = await pageJson(ctx.page, `/api/projects/${team.id}`);
    assert(denied.status === 403 || denied.status === 404, 'b-revoked', `B still reads Engineering: ${denied.status}`);

    process.stdout.write(`\ne2e-team passed. team=${team.id} userB=${userB.username}\n`);
    return 0;
  } catch (err) {
    const shot = await ctx.browser.screenshot(`fail-${runId}-e2e-team`).catch(() => null);
    const html = await ctx.browser.dumpHtml(`fail-${runId}-e2e-team`).catch(() => null);
    process.stderr.write(
      `FAIL e2e-team\n  error: ${err.message}\n  artifacts: ${[shot, html].filter(Boolean).join(', ') || 'none'}\n`,
    );
    return 1;
  } finally {
    if (!args.headful) await browser.close().catch(() => {});
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((code) => {
    process.exitCode = code;
  });
}
