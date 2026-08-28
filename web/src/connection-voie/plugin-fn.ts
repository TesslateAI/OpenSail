/**
 * Cordis client plugin face for the `connection-voie` carrier.
 *
 * The served app talks to the real same-origin VOIE API by default:
 * baseline, bounded long-poll, and single-attempt mutations ride the
 * canonical `VoieCarrier` over `/api/sessions`, `/api/events`,
 * `/api/conversations` (provisional until its first prompt promotes it),
 * `/api/conversations/:id/messages`, and `/api/runs/:id/cancel`. No Whaled
 * bearer gate, no DSH provider, no separate web process.
 *
 * The stock DSH runtime injects `connection`, `remote`, and
 * `remote.commands` as three separate service keys. VOIE owns minimal
 * carrier-backed `remote` and `remote.commands` services so boot never sees
 * a missing service. They carry no gateway, no remotes package, no
 * credentials, and no provider config — slash-command execution is not
 * served (the composer's `/`-prompts answer `unknown-command`), and
 * `$dispatch` is a no-op because the carrier emits no `host/remote-event`
 * frames.
 */
import { createConnectionHandle } from "./api.ts";
import { VoieCarrier } from "../carrier/voie.ts";
import { VoieHeroWorkspace } from "./hero-workspace.tsx";
import { bindVoieNewChatListener } from "./new-chat.ts";

const DSH_MOUNT_ID = "voie-dsh-root";

/**
 * Scope seam: the portal ChatHost writes nonsecret identity ids onto the
 * mount root's dataset; this plugin reads them once per boot and builds a
 * carrier bound to that boundary. Ids only — no credentials cross it.
 */
function mountScopedCarrier(): VoieCarrier {
  const host = document.getElementById(DSH_MOUNT_ID);
  const scopeId = host?.dataset.voieScopeId ?? "";
  return new VoieCarrier(scopeId === "" ? {} : { scopeId });
}

/** Nothing is injected: the carrier self-provides every service at apply. */
export const inject: string[] = [];

type RpcResult<T> = { ok: true; value: T } | { ok: false; error: { code: string; message: string; details: unknown } };

function commandRefused(): RpcResult<never> {
  return {
    ok: false,
    error: {
      code: "unknown-command",
      message: "VOIE serves no slash-command executor through this carrier",
      details: {},
    },
  };
}

/** Minimal VOIE-owned `remote.commands` service consumed by the stock DSH runtime. */
export function createVoieRemoteCommands() {
  return {
    execute: async (): Promise<RpcResult<never>> => commandRefused(),
  };
}

/** Minimal VOIE-owned `remote` service consumed by the stock DSH runtime. */
export function createVoieRemote() {
  return {
    // `remote.commands` is a separate injected service key; this accessor
    // keeps the stock `remote.commands.execute(...)` call shape working for
    // consumers that read it as a property of `remote`.
    commands: createVoieRemoteCommands(),
    $dispatch: (): void => {
      // No host/remote-event frames are emitted by the VOIE carrier.
    },
  };
}

type SettingsSnapshot = {
  status: "unavailable";
  value: undefined;
  base: undefined;
  user: undefined;
  revision: undefined;
  writable: false;
  mode: "memory";
};

/** Process-local settings namespace: ui-settings is not in the VOIE graph. */
export function createVoieSettingsScope() {
  const snapshot: SettingsSnapshot = {
    status: "unavailable",
    value: undefined,
    base: undefined,
    user: undefined,
    revision: undefined,
    writable: false,
    mode: "memory",
  };
  return {
    bind(_spec: { namespace: string }) {
      return {
        getSnapshot: () => snapshot,
        subscribe: () => () => {},
        set: async () => {},
        unset: async () => {},
      };
    },
  };
}

/** Static light theme: ui-theme is not in the VOIE graph. */
export function createVoieTheme() {
  const snapshot = {
    active: {
      colorScheme: "light" as const,
      tokens: {} as Record<string, string>,
    },
  };
  return {
    getTheme: () => snapshot,
  };
}

type SlotPluginCtx = {
  slots: {
    inject: (name: string, factory: () => unknown) => unknown;
    register: (decl: { name: string }, component: unknown) => unknown;
  };
};

type PluginCtx = {
  slots?: SlotPluginCtx["slots"];
  workspaces?: { startSession: (workspaceId?: string) => void };
};

export function apply(ctx: {
  provide: (name: string, value: unknown) => void;
  inject: (deps: string[], callback: (ctx: PluginCtx) => void) => void;
}): void {
  // Per-mount instance: every boot reads the fresh root dataset, so a scope
  // change remounts into a newly bounded carrier.
  ctx.provide("connection", createConnectionHandle(mountScopedCarrier()));
  ctx.provide("remote", createVoieRemote());
  ctx.provide("remote.commands", createVoieRemoteCommands());
  ctx.provide("settingsScope", createVoieSettingsScope());
  ctx.provide("theme", createVoieTheme());
  // Connection is immediate; slots exist only after the runtime plugin.
  // Wait, then occupy the hero workspace hole the conversation package
  // declares but does not fill in the VOIE graph.
  ctx.inject(["slots"], (slotCtx) => {
    slotCtx.slots?.inject("conversation.hero.workspace", () =>
      slotCtx.slots?.register({ name: "conversation.hero.workspace" }, VoieHeroWorkspace),
    );
  });
  ctx.inject(["workspaces"], (workspaceCtx) => {
    bindVoieNewChatListener((workspaceId) => {
      if (workspaceId !== undefined) workspaceCtx.workspaces?.startSession(workspaceId);
      else workspaceCtx.workspaces?.startSession();
    });
  });
}
