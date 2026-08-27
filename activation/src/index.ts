/**
 * Disposable DSH activation child.
 *
 * Authority is the inherited parent connection. This process never receives a
 * model key, database credential, Blob credential, Fabric certificate,
 * Workspace bearer, OIDC token, Azure credential, or Headscale enrollment key.
 *
 * The child fails closed unless the kernel reports exactly the expected
 * descriptor set (stdio plus the bridge on fd 3) and the agreed minimal
 * environment. The observed facts are attested to the parent inside `hello`.
 *
 * The parent socket is opened before DSH is loaded so a composition failure
 * still proves the inherited connection.
 */
import { readdirSync, readlinkSync } from "node:fs";
import { ParentLink } from "./parent.js";

const BRIDGE_FD = 3;
/**
 * Exact descriptor contract for INHERITED endpoints: the bridge alone.
 * Stdio and Node-runtime internals (epoll, pipes) never appear here because
 * observation reads kernel link targets and keeps real sockets only.
 */
const ALLOWED_SOCKET_FD: Record<number, true> = { [BRIDGE_FD]: true };
/** Exact environment contract the activation parent may hand the child. */
const ALLOWED_ENV_KEY: Record<string, true> = { HOME: true, TMPDIR: true, LANG: true, PATH: true };

interface Attestation {
  fds: number[];
  env_keys: string[];
}

function observe(): Attestation {
  const fds = readdirSync("/proc/self/fd")
    .map((name) => ({ fd: Number.parseInt(name, 10), name }))
    .filter(({ fd }) => Number.isInteger(fd))
    // A descriptor may close between the directory scan and its link read;
    // a vanished entry was never a stable inherited endpoint.
    .filter(({ name }) => {
      try {
        return readlinkSync(`/proc/self/fd/${name}`).startsWith("socket:");
      } catch {
        return false;
      }
    })
    .map(({ fd }) => fd)
    .sort((a, b) => a - b);
  const env_keys = Object.keys(process.env).sort();
  return { fds, env_keys };
}

function boundaryViolation(attestation: Attestation): string | undefined {
  if (!attestation.fds.includes(BRIDGE_FD)) {
    return "bridge descriptor 3 is absent";
  }
  for (const fd of attestation.fds) {
    if (!Object.hasOwn(ALLOWED_SOCKET_FD, fd)) {
      return `unexpected inherited descriptor ${fd}`;
    }
  }
  const observedKeys = new Set(attestation.env_keys);
  for (const required of Object.keys(ALLOWED_ENV_KEY)) {
    if (!observedKeys.has(required)) {
      return `environment key ${required} is missing`;
    }
  }
  if (attestation.env_keys.length !== Object.keys(ALLOWED_ENV_KEY).length) {
    return "unexpected environment key present";
  }
  for (const key of attestation.env_keys) {
    if (!Object.hasOwn(ALLOWED_ENV_KEY, key)) {
      return `unexpected environment key ${key}`;
    }
  }
  return undefined;
}

async function main(): Promise<void> {
  const parent = ParentLink.open();
  const attestation = observe();
  const violation = boundaryViolation(attestation);
  if (violation !== undefined) {
    // Refuse to mount DSH on a violated boundary; exit without a final frame.
    process.stderr.write(`[activation-child] boundary violation: ${violation}\n`);
    process.exit(98);
  }
  const bootstrap = await parent.hello(attestation);
  // Deferred on purpose: boot.ts pulls the full DSH composition graph and may
  // only load after the inherited connection and its boundary both proved out.
  const { runActivation } = await import("./boot.js");
  await runActivation(parent, bootstrap);
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  // Fail closed and promptly: no final frame, no lingering socket handles.
  process.exit(1);
});
