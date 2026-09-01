/**
 * Each disposable activation numbers its session log from seq 0. Canonical
 * history concatenates those turns; resume needs one strictly increasing seq
 * space so dsh-session can load the transcript.
 */
export function offsetBatchSeqs(previousSeq: number, seqs: readonly number[]): {
  seqs: number[];
  previousSeq: number;
  offset: number;
} {
  if (seqs.length === 0) {
    return { seqs: [], previousSeq, offset: 0 };
  }
  const first = seqs[0]!;
  const offset = first <= previousSeq ? previousSeq + 1 - first : 0;
  const out: number[] = [];
  let prev = previousSeq;
  for (const seq of seqs) {
    const next = seq + offset;
    if (next <= prev) {
      throw new Error("history seq does not advance the log");
    }
    out.push(next);
    prev = next;
  }
  return { seqs: out, previousSeq: prev, offset };
}

/**
 * dsh-session seed requires seq 0..n-1 with no holes. Flushed batches omit
 * internal events, so a later resume must compact the durable transcript.
 */
export function compactSessionSeqs<T extends { seq: number; sourceEventSeqs?: unknown }>(
  events: T[],
): T[] {
  const seqMap = new Map<number, number>();
  for (const [index, event] of events.entries()) {
    seqMap.set(event.seq, index);
  }
  for (const [index, event] of events.entries()) {
    event.seq = index;
    if (Array.isArray(event.sourceEventSeqs)) {
      event.sourceEventSeqs = event.sourceEventSeqs.map((value) =>
        typeof value === "number" && seqMap.has(value) ? seqMap.get(value)! : value,
      );
    }
  }
  return events;
}
