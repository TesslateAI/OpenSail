# VOIE browser smoke harness

Headless-browser acceptance flow for the VOIE control plane, driven over
Chrome DevTools Protocol (CDP). Zero runtime dependencies: it speaks CDP over
the `WebSocket` global that ships with Node >= 22, and drives the *system*
chromium — no playwright, no puppeteer, no `npm install`.

```
tests/browser/
  harness.mjs   zero-dependency CDP driver (launcher, waits, asserts, artifacts)
  steps.mjs     the scripted acceptance flow (16 steps) + CLI entrypoint
  README.md     this file
  artifacts/    per-failure screenshots + HTML dumps (gitignored)
```

## Quick start

```sh
# print the scripted plan — no browser, no origin, no credentials needed
just browser-smoke --dry-run

# real run (headless, against any origin serving the dev stack)
export VOIE_SMOKE_ORIGIN=http://[IP]:8080
export VOIE_SMOKE_USER=alice
install -m 0600 /dev/null ~/.voie-smoke-pw   # password file
chmod 600 ~/.voie-smoke-pw                  # MUST be 0600
printf '%s' 'correct-horse' > ~/.voie-smoke-pw
export VOIE_SMOKE_PASSWORD_FILE=~/.voie-smoke-pw
just browser-smoke
```

Equivalent direct invocation (inside the flake shell):

```sh
nix develop -c node tests/browser/steps.mjs              # real run
nix develop -c node tests/browser/steps.mjs --dry-run   # plan only
node tests/browser/steps.mjs --help                      # flag reference
```

`just browser-smoke` forwards extra arguments, e.g.
`just browser-smoke -- --base-url https://stage.example.com --headful`.

## Environment

| Variable | Required | Purpose |
| --- | --- | --- |
| `VOIE_SMOKE_ORIGIN` | real runs | Base URL of the portal, e.g. `http://[IP]:8080`. Trailing slashes are stripped; a missing scheme defaults to `http://`. |
| `VOIE_SMOKE_USER` | real runs | Portal account username. |
| `VOIE_SMOKE_PASSWORD_FILE` | real runs | Path to a `0600` file containing the password. Mode is enforced — other modes fail fast. Contents are read once and **never printed or echoed**. |
| `VOIE_SMOKE_EXECUTABLE` | optional | Chromium binary override. `just browser-smoke` runs `tests/browser/ensure-chromium.sh` first: Chrome 148+ is skipped because CDP `Page.navigate` never commits with `--remote-debugging-port` on those builds, and a pinned Chrome-for-Testing `chrome-headless-shell` 131 is used instead. Set `VOIE_SMOKE_ALLOW_SYSTEM_CHROME=1` to keep a chosen 148+ binary. |
| `VOIE_SMOKE_SESSION_COOKIE` | optional | When set, the harness additionally asserts the login set a cookie of exactly this name. Without it, the harness only asserts *some* cookie landed in the jar. |
| `VOIE_SMOKE_ACCOUNT_REGEX` | optional | When set, the account-label step requires the label to match this regex (e.g. the display-name shape your build emits). |
| `VOIE_SMOKE_RUN_TAG` | optional | Unique-suffix override for created names (usernames are provided by the operator, so the suffix covers workspace names `Smoke <run-tag>`). |

The `--base-url` CLI flag overrides `VOIE_SMOKE_ORIGIN`.

## The 16 scripted steps and how they map to the acceptance checklist

Each step is a discrete assertion; a failure names the step, the expectation,
the page console tail, and writes a screenshot + HTML dump to
`tests/browser/artifacts/`.

| # | Step id | Assertion | Acceptance-checklist intent |
| --- | --- | --- | --- |
| 1 | `open-login` | `GET {origin}/login` responds `<400`; URL stays on origin | Login app is served |
| 2 | `react-login-form-renders` | username+password inputs exist **inside a React mount** and no `<form>` on the page lacks controls | The login page is a real client app, not a bare server `<form>` (the “bare server HTML marker” the task forbids) |
| 3 | `fill-credentials` | username and password values land in the fields (verified by re-read after fill) | Credential capture path works |
| 4 | `submit-login` | a cookie lands in the browser jar; `location` leaves `/login`; `POST /login` observed `< 400` | Native login end-to-end: `POST /login`, session cookie, redirect into portal |
| 5 | `account-label-visible` | a non-empty account label is visible and does **not** match the raw-UUID shape (nor a bare placeholder); optional `VOIE_SMOKE_ACCOUNT_REGEX` tightens this | Profile identity is displayed, not a raw UUID |
| 6 | `personal-scope-selected` | a scope control entry reading “personal” is marked active/selected in the DOM | Personal scope is chosen by default after `/api/projects` |
| 7 | `open-workspaces-page` | a clickable Workspaces nav entry exists; workspace surface mounts | Workspace navigation |
| 8 | `create-or-select-workspace` | a workspace named `Smoke <run-tag>` is **created when absent**, otherwise the first existing smoke workspace is reused; created workspace ids are registered for cleanup | Create workspace; idempotent re-runs |
| 9 | `open-new-chat` | a New-chat control exists and an **editable** prompt composer becomes visible (the DSH “Choose workspace” chip is not the composer) | New conversation entry point |
| 10 | `type-first-prompt` | prompt text is verified inside the composer after typing | Input path |
| 11 | `send-and-conversation-id` | send activates and a `conversationId` appears in URL params, `/conversations|threads|c/` path, `[data-conversation-*]` state, a new `/chat/` recent, or `GET /api/sessions` as a last-resort identity lookup — **not** a session-count assertion | “conversationId appears in URL/state”; the client may look up the created row, but it still does not assert server session counts |
| 12 | `poll-assistant-events-60s` | assistant/tool evidence appears within a **60s bound** (`--timeout-ms` adjustable) | Run loop actually streams; a stall fails, nothing waits forever |
| 13 | `tool-card-visible` | a tool/tool-card element appears in the transcript | Tool execution surface is rendered |
| 14 | `followup-enabled-while-running` | composer is **not disabled** while (or after) an assistant turn | Follow-up input stays usable during a run |
| 15 | `send-followup-queued` | while the first turn still holds the composer on **Stop generating**, busy-Enter queues the follow-up **and the DSH queue dock (`[data-queue-dock]`) shows a new queued row for that follow-up** | Second prompt queuing is surfaced in the UI; generic first-turn “in progress/submitting/pending” chrome is not enough |
| 16 | `reload-reconstructs` | after a hard reload the **same** conversationId is present and the first prompt’s text is restored | Conversation state survives reload |

### Selector strategy

Every page query uses layered candidates (`data-testid`, then
role/aria-label/text matches, then generic fallbacks such as any visible
`textarea`). When the real product surface differs from the layer list, the
step fails with the exact expectation instead of silently passing — the
harness targets the *contracted* routes (`/login`, `POST /login`,
`/api/me`, `/api/projects`) and their canonical DOM markers, so it can run
against a candidate stack as soon as it lands.

## Operator verification: zero-session-on-open

The in-browser flow **never** asserts a server-side session *count* — that
operator check stays out of band below. Step 11 may read `GET /api/sessions`
only as a last-resort way to recover the newly created `conversationId` when
the create POST body and recents list have not yet surfaced it. After a run,
an operator verifies `zero sessions on open` out-of-band:

```sh
# 1) after login the smoke run records the cookie in the browser jar;
#    export it for curl from the failed/passed run's artifacts if needed
# 2) close the browser so the session bean is the only one
# 3) call the control-plane session endpoint with the recorded cookie
curl -sS -b "SESSION=<cookie-from-run>" "$VOIE_SMOKE_ORIGIN/api/sessions" | jq '.count'
```

Expect: the **sessions table row count for that account is 0** immediately
after *opening the portal* (before a workspace conversation starts), and
increments only when `send-and-conversation-id` later creates a
conversation. The harness records the step 11 timestamp and conversationId
(`--verbose` may print the cid) to correlate.

> Wait: adjust host/port and the cookie name to the crate’s actual session
> cookie (`VOIE_SMOKE_SESSION_COOKIE` lets the harness pin the expected
> one). The session-list **endpoint path must exist on the deployed
> stack**; line it up with the backend’s session API when the candidate
> lands, or use `SELECT … FROM sessions` when direct DB access is in play.

## Idempotency and cleanup

- Unique-per-run names carry `<run-tag>` = base36 timestamp + pid
  (`VOIE_SMOKE_RUN_TAG` overrides) so parallel re-runs never collide.
- On re-run, an existing `Smoke <tag>` workspace is **reused** rather than
  recreated — create-or-select is idempotent.
- Workspaces created by the harness are deleted **best-effort** at the end
  via `DELETE {origin}/api/workspaces/{id}` with the logged-in cookie;
  failure is reported but does not fail the run (the API may not exist
  until the candidate lands).

## Artifacts

Every failure writes to `tests/browser/artifacts/` (gitignored):
`fail-<run-id>-<step-id>.png` screenshot and `.html` DOM dump, plus the
last 15 page console lines in the failure report. A fully green run
removes the directory (it is recreated on demand).

## Developing the harness

```sh
nix develop -c bash -c 'node --check tests/browser/harness.mjs && node --check tests/browser/steps.mjs'
```

- `harness.mjs` — browser lifecycle, CDP client, wait/assert primitives,
  network/cookie capture, artifact writing. No product knowledge.
- `steps.mjs` — the 16 step definitions, the CLI (`--dry-run`,
  `--base-url`, `--headful`, `--executable`, `--timeout-ms`), env
  validation, and cleanup. All product knowledge lives here.