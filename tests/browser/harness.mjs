// VOIE browser smoke harness — zero-dependency headless Chromium driver.
//
// Rationale: neither playwright nor puppeteer exists anywhere in the pnpm
// workspace (web/package.json has zero dependencies), so installing a heavy
// framework would violate the "lightest option" constraint. Node >= 22 in
// flake.nix ships the WHATWG WebSocket global, which is exactly enough to
// speak Chrome DevTools Protocol directly against a system chromium via
// --remote-debugging-port=0.
//
// Everything here is library surface consumed by steps.mjs. Run modes live in
// steps.mjs so this file stays free of product-flow knowledge.

import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { statSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// ---------------------------------------------------------------------------
// Environment / CLI plumbing

export class PreflightError extends Error {}

export const CHROMIUM_CANDIDATES = [
  '/run/current-system/sw/bin/chromium',
  '/run/current-system/sw/bin/chromium-browser',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
  '/usr/bin/google-chrome',
  '/usr/bin/google-chrome-stable',
];

export function resolveExecutable(override) {
  const candidates = [];
  if (override) candidates.push(override);
  candidates.push(...CHROMIUM_CANDIDATES);
  const found = candidates.find((p) => {
    try {
      return Boolean(statSyncExecutable(p));
    } catch {
      return false;
    }
  });
  if (!found) {
    throw new PreflightError(
      `no chromium executable found; tried:\n  ${candidates.join('\n  ')}\n` +
        'set VOIE_SMOKE_EXECUTABLE=/path/to/chromium to point at your own build.',
    );
  }
  return found;
}

function statSyncExecutable(p) {
  void statSync(p);
  return true;
}

// NOTE: replaced below with a sync check that does not depend on experimental APIs.

export function parseArgs(argv) {
  const out = {
    dryRun: false,
    baseUrl: null,
    headful: false,
    executable: null,
    timeoutMs: 60_000,
    help: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case '--dry-run':
        out.dryRun = true;
        break;
      case '--base-url':
        out.baseUrl = argv[++i];
        break;
      case '--headful':
        out.headful = true;
        break;
      case '--executable':
        out.executable = argv[++i];
        break;
      case '--timeout-ms':
        out.timeoutMs = Number(argv[++i]);
        if (!Number.isFinite(out.timeoutMs) || out.timeoutMs <= 0) {
          throw new PreflightError(`invalid value for --timeout-ms: ${argv[i]}`);
        }
        break;
      case '--help':
      case '-h':
        out.help = true;
        break;
      default:
        throw new PreflightError(`unknown argument: ${a}`);
    }
  }
  return out;
}

export function requiredEnv() {
  const origin = normalizeOrigin(
    process.env.VOIE_SMOKE_ORIGIN ?? '',
  );
  const user = process.env.VOIE_SMOKE_USER ?? '';
  const passwordFile = process.env.VOIE_SMOKE_PASSWORD_FILE ?? '';
  const missing = [];
  if (!origin) missing.push('VOIE_SMOKE_ORIGIN');
  if (!user) missing.push('VOIE_SMOKE_USER');
  if (!passwordFile) missing.push('VOIE_SMOKE_PASSWORD_FILE');
  if (missing.length > 0) {
    throw new PreflightError(
      `real run requires env vars (missing: ${missing.join(', ')})`,
    );
  }
  return { origin, user, passwordFile };
}

export function normalizeOrigin(raw) {
  let s = String(raw).trim().replace(/\/+$/, '');
  if (s && !/^https?:\/\//.test(s)) s = `http://${s}`;
  return s;
}

// Reads the password file after verifying 0600 permissions. NEVER log contents.
export async function readPassword(passwordFile) {
  let st;
  try {
    st = await stat(passwordFile);
  } catch {
    throw new PreflightError(`password file not readable: ${passwordFile}`);
  }
  const mode = st.mode & 0o777;
  if (mode !== 0o600) {
    throw new PreflightError(
      `password file ${passwordFile} must have mode 0600 (found ${mode.toString(8)})`,
    );
  }
  return (await readFile(passwordFile, 'utf8')).replace(/\r?\n$/, '');
}

// ---------------------------------------------------------------------------
// Minimal DevTools protocol client

export class Cdp {
  /** @param {WebSocket} ws */
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    /** @type {Map<number, {resolve: Function, reject: Function}>} */
    this.pending = new Map();
    /** @type {{method: string, params: any, sessionId?: string}[]} */
    this.events = [];
    this.closed = null;
    ws.addEventListener('message', (ev) => this.#onMessage(ev.data));
    ws.addEventListener('error', (ev) => {
      const err = new Error(`cdp websocket error: ${ev.message ?? 'unknown'}`);
      this.closed = err;
      for (const p of this.pending.values()) p.reject(err);
    });
    ws.addEventListener('close', () => {
      if (!this.closed) this.closed = new Error('cdp websocket closed');
      for (const p of this.pending.values()) {
        p.reject(this.closed);
      }
    });
  }

  static async connect(url, timeoutMs = 10_000) {
    const ws = new WebSocket(url);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`timed out connecting cdp websocket ${url}`)),
        timeoutMs,
      );
      ws.addEventListener('open', () => {
        clearTimeout(timer);
        resolve();
      }, { once: true });
      ws.addEventListener('error', (e) => {
        clearTimeout(timer);
        reject(new Error(`cdp websocket failed: ${e.message ?? 'error'}`));
      }, { once: true });
    });
    return new Cdp(ws);
  }

  #onMessage(data) {
    let msg;
    try {
      msg = JSON.parse(typeof data === 'string' ? data : String(data));
    } catch {
      return; // binary frames are not part of our usage
    }
    if (msg.id !== undefined) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) {
        p.reject(new Error(`cdp ${msg.error.message ?? 'error'} ${JSON.stringify(msg.error.data ?? '')}`));
      } else {
        p.resolve(msg.result ?? {});
      }
      return;
    }
    if (msg.method) {
      this.events.push({ method: msg.method, params: msg.params, sessionId: msg.sessionId });
      if (this.events.length > 5000) this.events.splice(0, 1000);
    }
  }

  send(method, params = {}, sessionId) {
    if (this.closed) return Promise.reject(this.closed);
    const id = this.nextId++;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify(payload));
    });
  }

  eventsWhere(pred) {
    return this.events.filter(pred);
  }

  close() {
    try {
      this.ws.close();
    } catch {
      /* already gone */
    }
  }
}

// ---------------------------------------------------------------------------
// Browser lifecycle

export async function launchBrowser({
  executable,
  headless = true,
} = {}) {
  const exe = resolveExecutable(executable ?? process.env.VOIE_SMOKE_EXECUTABLE);
  const profileDir = await mkdtemp(path.join(os.tmpdir(), 'voie-smoke-profile-'));
  const args = [
    headless ? '--headless=new' : '--start-maximized',
    '--remote-debugging-port=0',
    `--user-data-dir=${profileDir}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--disable-crash-reporter',
    '--window-size=1440,900',
    '--remote-allow-origins=*',
    // Local stacks terminate TLS with the dev-stack self-signed cert;
    // opt into trusting it rather than weakening any server-side cookie
    // flags.
    ...(process.env.VOIE_SMOKE_TRUST_LOCAL_TLS === '1'
      ? ['--ignore-certificate-errors']
      : []),
    'about:blank',
  ];
  const child = spawn(exe, args, { stdio: ['ignore', 'ignore', 'pipe'] });
  const wsUrl = await waitForDevToolsEndpoint(child);

  const cdp = await Cdp.connect(wsUrl);
  const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await cdp.send('Target.attachToTarget', {
    targetId,
    flatten: true,
  });
  await cdp.send('Page.enable', {}, sessionId);
  await cdp.send('Runtime.enable', {}, sessionId);
  await cdp.send('Network.enable', {}, sessionId);

  return new Browser(cdp, child, sessionId, profileDir, targetId);
}

function waitForDevToolsEndpoint(child, timeoutMs = 20_000) {
  return new Promise((resolve, reject) => {
    let buf = '';
    const onData = (chunk) => {
      buf += chunk.toString('utf8');
      const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        cleanup();
        resolve(m[1]);
      }
    };
    const onError = (err) => {
      cleanup();
      reject(new Error(`chromium exited early: ${err.message}`));
    };
    const onExit = (code) => {
      cleanup();
      reject(new Error(
        `chromium exited before exposing CDP (code=${code}). stderr:\n${buf}`,
      ));
    };
    const cleanup = () => {
      clearTimeout(timer);
      child.stderr.off('data', onData);
      child.off('error', onError);
      child.off('exit', onExit);
    };
    const timer = setTimeout(() => {
      cleanup();
      child.kill('SIGKILL');
      reject(new Error(`timed out waiting for DevTools endpoint. stderr:\n${buf}`));
    }, timeoutMs);
    child.stderr.on('data', onData);
    child.once('error', onError);
    child.once('exit', onExit);
  });
}

// ---------------------------------------------------------------------------
// Page handle: all browser interaction used by steps

export class Browser {
  constructor(cdp, child, sessionId, profileDir, targetId) {
    this.cdp = cdp;
    this.child = child;
    this.sessionId = sessionId;
    this.profileDir = profileDir;
    this.targetId = targetId;
    this.consoleTail = [];
    this.artifactsDir = path.join(
      path.dirname(fileURLToPath(import.meta.url)),
      'artifacts',
    );
  }

  async goto(url, timeoutMs = 30_000) {
    const started = Date.now();
    await this.cdp.send('Page.navigate', { url }, this.sessionId).catch((err) => {
      // ERR_ABORTED fires when navigation redirects immediately; harmless.
      if (!String(err.message).includes('ERR_ABORTED')) throw err;
    });
    await this.waitForFunction(
      "document.readyState === 'complete' || document.readyState === 'interactive'",
      { timeoutMs },
    );
    return Date.now() - started;
  }

  async reload(timeoutMs = 30_000) {
    await this.cdp.send('Page.reload', {}, this.sessionId);
    await this.waitForFunction(
      "document.readyState === 'complete' || document.readyState === 'interactive'",
      { timeoutMs },
    );
  }

  /**
   * Evaluate an async JS snippet in the page. Provide source text so we never
   * rely on serialization tricks; helpers below build snippets for you.
   */
  async evalJs(expression) {
    const res = await this.cdp.send(
      'Runtime.evaluate',
      {
        expression,
        awaitPromise: true,
        returnByValue: true,
        userGesture: true,
      },
      this.sessionId,
    );
    if (res.exceptionDetails) {
      const d = res.exceptionDetails;
      const text =
        d.exception?.description ?? d.text ?? 'unknown page exception';
      throw new Error(`page evaluation failed: ${text.split('\n')[0]}`);
    }
    return res.result?.value;
  }

  /** Poll a JS predicate (source text) until truthy or timeout. */
  async waitForFunction(expression, { timeoutMs = 15_000, intervalMs = 250 } = {}) {
    const deadline = Date.now() + timeoutMs;
    let lastErr = null;
    for (;;) {
      try {
        const v = await this.evalJs(`Boolean((${expression}))`);
        if (v) return true;
      } catch (err) {
        lastErr = err; // transient (navigation teardown) – keep polling
      }
      if (Date.now() > deadline) {
        const suffix = lastErr ? ` (last error: ${lastErr.message})` : '';
        throw new Error(
          `waitForFunction timed out after ${timeoutMs}ms: ${expression.slice(0, 120)}…${suffix}`,
        );
      }
      await sleep(intervalMs);
    }
  }

  async currentUrl() {
    return this.evalJs('location.href');
  }

  async screenshot(name) {
    await mkdir(this.artifactsDir, { recursive: true });
    const file = path.join(this.artifactsDir, `${sanitize(name)}.png`);
    const { data } = await this.cdp.send(
      'Page.captureScreenshot',
      { format: 'png' },
      this.sessionId,
    );
    await writeFile(file, Buffer.from(data, 'base64'));
    return file;
  }

  async dumpHtml(name) {
    await mkdir(this.artifactsDir, { recursive: true });
    const file = path.join(this.artifactsDir, `${sanitize(name)}.html`);
    const html = await this.evalJs('document.documentElement.outerHTML');
    // Credentials must never persist in artifacts: scrub every form field
    // value/attribute before writing, whatever the failure path was.
    const clean = typeof html === 'string'
      ? html
        .replace(/(\\svalue=")[^"]*(")/gi, '$1$2')
        .replace(/(\\svalue=)(?!["'])([^\\s>]+)/gi, '$1""')
      : '';
    await writeFile(file, clean);
    return file;
  }

  async cookies(urls) {
    const res = await this.cdp.send('Network.getCookies', { urls }, this.sessionId);
    return res.cookies ?? [];
  }

  networkResponses({ methodRe, urlRe, statusMax }) {
    // Pair requestWillBeSent (gives method) with responseReceived (status) by requestId.
    const reqMethod = new Map();
    const out = [];
    for (const ev of this.cdp.eventsWhere((e) => e.sessionId === this.sessionId)) {
      if (ev.method === 'Network.requestWillBeSent') {
        reqMethod.set(ev.params.requestId, ev.params.request.method);
      } else if (ev.method === 'Network.responseReceived') {
        const method = reqMethod.get(ev.params.requestId) ?? '?';
        const { url } = ev.params.response;
        const status = ev.params.response.status;
        if (
          (!methodRe || methodRe.test(method)) &&
          (!urlRe || urlRe.test(url)) &&
          (statusMax === undefined || status <= statusMax)
        ) {
          out.push({ method, url, status });
        }
      }
    }
    return out;
  }

  noteConsole() {
    for (const ev of this.cdp.eventsWhere((e) => e.sessionId === this.sessionId)) {
      if (ev.method === 'Runtime.consoleAPICalled') {
        const parts = (ev.params.args ?? []).map((a) => a.value ?? a.description ?? '').join(' ');
        this.consoleTail.push(`[${ev.params.type}] ${parts}`.slice(0, 300));
      }
    }
    while (this.consoleTail.length > 50) this.consoleTail.shift();
    return this.consoleTail;
  }

  async close() {
    this.cdp.close();
    if (this.child.exitCode === null && this.child.signalCode === null) {
      this.child.kill('SIGTERM');
      const gone = await Promise.race([
        new Promise((res) => this.child.once('exit', res)),
        sleep(3000).then(() => false),
      ]);
      if (!gone) this.child.kill('SIGKILL');
    }
    await rm(this.profileDir, { recursive: true, force: true }).catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// Snippet builders (kept as functions returning strings so pages get fresh JS)

/** Fill a (possibly React-controlled) input firing the native input event. */
export function fillSnippet(selector, value) {
  return `(async () => {
  const sel = ${JSON.stringify(selector)};
  const els = Array.from(document.querySelectorAll(sel)).filter(isVisible);
  if (els.length === 0) throw new Error('no visible element for selector ' + sel);
  const el = els[0];
  const editable = el.isContentEditable || el.getAttribute('contenteditable') === 'true';
  const proto = el.tagName === 'TEXTAREA'
    ? HTMLTextAreaElement.prototype
    : el instanceof HTMLInputElement
      ? HTMLInputElement.prototype
      : null;
  if (editable) {
    el.textContent = ${JSON.stringify(value)};
    el.value = ${JSON.stringify(value)};
  } else if (proto && Object.getOwnPropertyDescriptor(proto, 'value')?.set) {
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(value)});
  } else {
    el.value = ${JSON.stringify(value)};
  }
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  el.dispatchEvent(new KeyboardEvent('keyup', { bubbles: true, key: 'Unidentified' }));
  el.blur();
  return true;
  function isVisible(e) {
    const r = e.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const st = getComputedStyle(e);
    return st.visibility !== 'hidden' && st.display !== 'none';
  }
})()`;
}

/** Click the first visible element matching candidate selectors (array or css). */
export function clickSnippet(selectors, { textRe } = {}) {
  return `(async () => {
  const sels = ${JSON.stringify(Array.isArray(selectors) ? selectors : [selectors])};
  const textReSrc = ${textRe ? JSON.stringify(textRe.source) : 'null'};
  const textFlags = ${textRe ? JSON.stringify(textRe.flags) : 'null'};
  const textRe = textReSrc === null ? null : new RegExp(textReSrc, textFlags ?? '');
  for (const sel of sels) {
    for (const el of Array.from(document.querySelectorAll(sel))) {
      if (!isVisible(el)) continue;
      if (textRe) {
        const t = (el.getAttribute('aria-label') || el.textContent || '').trim();
        if (!textRe.test(t)) continue;
      }
      el.scrollIntoView({ block: 'center' });
      el.click();
      return true;
    }
  }
  throw new Error('clickSnippet: no visible clickable match for ' + JSON.stringify(sels));
  function isVisible(e) {
    const r = e.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const st = getComputedStyle(e);
    return st.visibility !== 'hidden' && st.display !== 'none' && r.width > 1;
  }
})()`;
}

/**
 * Search page text/DOM for the first visible element among candidate
 * selectors. Returns its trimmed text (null when absent).
 */
export function queryTextSnippet(selectors) {
  return `(() => {
  const sels = ${JSON.stringify(Array.isArray(selectors) ? selectors : [selectors])};
  for (const sel of sels) {
    for (const el of Array.from(document.querySelectorAll(sel))) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) continue;
      return (el.textContent || '').trim();
    }
  }
  return null;
})()`;
}

// ---------------------------------------------------------------------------
// Step runner shared by dry-run and live execution

export function uniqueSuffix(runTag) {
  return runTag ?? `${Date.now().toString(36)}${process.pid.toString(36).slice(-3)}`;
}

export function makeRunId() {
  return new Date()
    .toISOString()
    .replace(/[-:T]/g, '')
    .replace(/\..*/, '')
    .concat(`-${process.pid}`);
}

export class AssertionError extends Error {
  constructor(stepName, message) {
    super(`[${stepName}] ${message}`);
    this.stepName = stepName;
  }
}

export function assert(cond, stepName, message) {
  if (!cond) throw new AssertionError(stepName, message);
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function sanitize(s) {
  return String(s).replace(/[^a-zA-Z0-9._-]+/g, '-').slice(0, 80);
}

export function isMainScript(metaUrl) {
  if (!process.argv[1]) return false;
  try {
    return import.meta.url === pathToFileURL(process.argv[1]).href ||
      metaUrl === pathToFileURL(process.argv[1]).href;
  } catch {
    return false;
  }
}

export function helpText() {
  return [
    'voie browser smoke — headless acceptance flow driver',
    '',
    'USAGE',
    '  node tests/browser/steps.mjs [--dry-run] [--base-url URL] [--headful]',
    '                              [--executable PATH] [--timeout-ms N]',
    '',
    'ENV (required for real runs)',
    '  VOIE_SMOKE_ORIGIN          base URL, e.g. http://127.0.0.1:8080',
    '  VOIE_SMOKE_USER            account username for the portal',
    '  VOIE_SMOKE_PASSWORD_FILE   path to 0600 file holding the password',
    '                             (contents are never printed)',
    '',
    'OPTIONAL ENV',
    '  VOIE_SMOKE_EXECUTABLE       chromium binary override',
    '  VOIE_SMOKE_SESSION_COOKIE   expected cookie name set by POST /login',
    '  VOIE_SMOKE_RUN_TAG          stable unique suffix for created names',
    '',
    'MODES',
    '  --dry-run  print the scripted acceptance steps and exit 0 without',
    '             requiring VOIE_SMOKE_ORIGIN or credentials.',
  ].join('\n');
}
