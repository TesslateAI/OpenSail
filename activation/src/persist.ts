import { Context } from "@deepseek-ai/cordis";
import type { SessionEvent, SessionHeader, SessionId } from "@deepseek-ai/dsh-session";
import SessionPersistence, {
  PersistenceCoordinator,
  SessionPersistenceRevision,
} from "@deepseek-ai/dsh-session-persistence";
import type {
  PersistenceBackend,
  SessionInspection,
  SessionLocation,
  SessionPersistenceSnapshot,
  StoredPrefix,
} from "@deepseek-ai/dsh-session-persistence";

interface StoredSession {
  meta: SessionHeader;
  events: SessionEvent[];
  revision: number;
}

/** In-process session log. Checkpoint-before-effect still flushes here. */
export class MemorySessionPersistence extends SessionPersistence implements PersistenceBackend {
  static inject = ["sessions"];

  override readonly supportsRawArtifacts = false;
  override readonly name = "session-persistence-memory";

  private readonly store = new Map<string, StoredSession>();
  private readonly coordinator: PersistenceCoordinator;
  private clock = 0;

  constructor(ctx: Context) {
    super(ctx);
    this.coordinator = new PersistenceCoordinator(ctx, this, {
      preparedSessionCacheSize: 8,
      writeBatchMaxDelayMs: 1,
    });
  }

  locate(_meta: SessionHeader): SessionLocation | undefined {
    return undefined;
  }

  create(meta: SessionHeader): Promise<void> {
    return this.coordinator.create(meta);
  }

  append(id: SessionId, events: readonly SessionEvent[]): Promise<void> {
    return this.coordinator.append(id, events);
  }

  override prepare(id: SessionId, signal?: AbortSignal) {
    return this.coordinator.prepare(id, signal);
  }

  load(id: SessionId): Promise<SessionInspection> {
    return this.coordinator.load(id);
  }

  inspect(id: SessionId, signal?: AbortSignal): Promise<SessionInspection> {
    return this.coordinator.inspect(id, signal);
  }

  readFrom(id: SessionId, fromSeq: number, signal?: AbortSignal) {
    return this.coordinator.readFrom(id, fromSeq, signal);
  }

  async list(signal?: AbortSignal): Promise<SessionHeader[]> {
    signal?.throwIfAborted();
    return [...this.store.values()].map((row) => structuredClone(row.meta));
  }

  async listSnapshots(signal?: AbortSignal): Promise<SessionPersistenceSnapshot[]> {
    signal?.throwIfAborted();
    return [...this.store.values()].map((row) => ({
      header: structuredClone(row.meta),
      revision: SessionPersistenceRevision(`memory:${row.meta.id}:${row.revision}`),
    }));
  }

  async loadStored(id: SessionId): Promise<StoredPrefix | undefined> {
    const row = this.store.get(id);
    if (row === undefined) return undefined;
    return {
      meta: structuredClone(row.meta),
      events: structuredClone(row.events),
      revision: SessionPersistenceRevision(`memory:${row.meta.id}:${row.revision}`),
    };
  }

  async readStoredRevision(id: SessionId) {
    const row = this.store.get(id);
    if (row === undefined) return undefined;
    return SessionPersistenceRevision(`memory:${row.meta.id}:${row.revision}`);
  }

  /**
   * Seeds the canonical transcript handed over by the parent before resume.
   * Only the disposable child's own in-process log is touched; durable state
   * stays server-owned.
   */
  seedHistory(meta: SessionHeader, events: readonly SessionEvent[]): void {
    this.clock += 1;
    this.store.set(meta.id, {
      meta: structuredClone(meta),
      events: structuredClone([...events]),
      revision: this.clock,
    });
  }

  async appendBatch(
    meta: SessionHeader,
    events: readonly SessionEvent[],
    isMaterialized: boolean,
  ): Promise<void> {
    const existing = this.store.get(meta.id);
    const nextEvents = isMaterialized && existing !== undefined
      ? [...existing.events, ...structuredClone(events as SessionEvent[])]
      : structuredClone(events as SessionEvent[]);
    this.clock += 1;
    this.store.set(meta.id, {
      meta: structuredClone(meta),
      events: nextEvents,
      revision: this.clock,
    });
  }

  async commitRepair(
    meta: SessionHeader,
    _torn: unknown,
    closers: readonly SessionEvent[],
  ): Promise<void> {
    if (closers.length === 0) return;
    await this.appendBatch(meta, closers, this.store.has(meta.id));
  }
}
