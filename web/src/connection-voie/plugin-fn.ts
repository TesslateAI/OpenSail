/**
 * Cordis client plugin face for the `connection-voie` carrier.
 *
 * The served app talks to the real same-origin VOIE API by default:
 * baseline, bounded long-poll, and single-attempt mutations ride the
 * canonical `VoieCarrier` over `/api/sessions`, `/api/events`,
 * `/api/conversations` (durable empty Session),
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
import { createConnectionHandle, syncVoieWorkspaces } from "./api.ts";
import { VoieCarrier } from "../carrier/voie.ts";
import { VoieBrandMark } from "./brand-mark.tsx";
import { VoieConversationPane } from "./conversation-frame.tsx";
import { getVoieDshHostContext } from "./host-context.ts";
import { lastWorkspace } from "./last-workspace.ts";
import { VoieHeroWorkspace } from "./hero-workspace.tsx";
import { VoieLayoutController, createVoieLayoutStore } from "./layout.ts";
import { bindVoieNewChatListener } from "./new-chat.ts";
import { bindVoieSessionNav } from "./session-nav.ts";

/**
 * Scope seam: ChatHost writes nonsecret identity ids into the adapter
 * host context; this plugin reads them once per boot and builds a carrier
 * bound to that boundary. Ids only — no credentials cross it.
 */
function mountScopedCarrier(): VoieCarrier {
  const projectId = getVoieDshHostContext().projectId;
  return new VoieCarrier(projectId === "" ? {} : { projectId });
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

type SlotRegister = {
  name: string;
  children?: Record<string, { kind: string; scope: string }>;
  store?: () => unknown;
  inject?: (actions: { openDetails: () => void; closeDetails: () => void; toggleSidebar: () => void }) => object;
};

type SlotPluginCtx = {
  slots: {
    inject: (name: string, factory: () => unknown) => unknown;
    register: (decl: SlotRegister, component: unknown) => unknown;
  };
};

type SessionListMirror = {
  getSnapshot: () => { byId: Record<string, unknown>; current: string | undefined };
  subscribe: (listener: () => void) => () => void;
};

type PluginCtx = {
  slots?: SlotPluginCtx["slots"];
  workspaces?: { startSession: (workspaceId?: string) => void };
  sessions?: {
    open: (id: string) => void;
    clear: () => void;
    create?: (opts: { workspaceId: string }) => Promise<string>;
    list: SessionListMirror;
  };
};

export function apply(ctx: {
  provide: (name: string, value: unknown) => void;
  inject: (deps: string[], callback: (ctx: PluginCtx) => void) => void;
}): void {
  // Per-mount instance: every boot reads the adapter host context, so a
  // scope change remounts into a newly bounded carrier.
  ctx.provide("connection", createConnectionHandle(mountScopedCarrier()));
  ctx.provide("remote", createVoieRemote());
  ctx.provide("remote.commands", createVoieRemoteCommands());
  ctx.provide("settingsScope", createVoieSettingsScope());
  ctx.provide("theme", createVoieTheme());
  const layout = new VoieLayoutController();
  ctx.provide("layout", layout);
  // Connection is immediate; slots exist only after the runtime plugin.
  // Occupy the built-in root with a VOIE frame (no DSH sidebar column),
  // then fill the hero brand holes the conversation package still declares.
  ctx.inject(["slots"], (slotCtx) => {
    slotCtx.slots?.register(
      {
        name: "root",
        children: {
          conversation: { kind: "single", scope: "session-maybe" },
          details: { kind: "single", scope: "session" },
          "shell.overlay": { kind: "list", scope: "root" },
        },
        store: createVoieLayoutStore,
        inject: (actions) => {
          layout.attachPanels(actions);
          return {};
        },
      },
      VoieConversationPane,
    );
    slotCtx.slots?.inject("conversation.hero.workspace", () =>
      slotCtx.slots?.register({ name: "conversation.hero.workspace" }, VoieHeroWorkspace),
    );
    slotCtx.slots?.inject("conversation.hero.brand.mark", () =>
      slotCtx.slots?.register({ name: "conversation.hero.brand.mark" }, VoieBrandMark),
    );
  });
  ctx.inject(["workspaces", "sessions"], (navCtx) => {
    bindVoieNewChatListener((workspaceId) => {
      void (async () => {
        const projectId = getVoieDshHostContext().projectId;
        const target =
          (workspaceId?.trim()
            || lastWorkspace(projectId)
            || getVoieDshHostContext().workspaceId
            || "").trim();
        if (target === "") return;
        await syncVoieWorkspaces().catch(() => {});
        navCtx.sessions?.clear();
        const create = navCtx.sessions?.create;
        if (create === undefined) return;
        try {
          const id = await create({ workspaceId: target });
          navCtx.sessions?.open(id);
        } catch {
          // Create is fail-closed; do not bind a different Workspace.
        }
      })();
    });
    if (navCtx.sessions !== undefined) bindVoieSessionNav(navCtx.sessions);
  });
}
