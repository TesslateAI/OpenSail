import { compactSessionSeqs, offsetBatchSeqs } from "./src/history-seq.ts";
import assert from "node:assert/strict";
import test from "node:test";

test("create-mode batches keep their seq and gaps", () => {
  const first = offsetBatchSeqs(-1, [0, 1, 2, 5]);
  assert.deepEqual(first.seqs, [0, 1, 2, 5]);
  const second = offsetBatchSeqs(first.previousSeq, [7, 8]);
  assert.deepEqual(second.seqs, [7, 8]);
});

test("seed history is compacted to contiguous seq from 0", () => {
  const events = compactSessionSeqs([
    { seq: 0, type: "a" },
    { seq: 24, type: "b" },
    { seq: 26, type: "c", sourceEventSeqs: [24] },
    { seq: 49, type: "d", sourceEventSeqs: [26] },
  ]);
  assert.deepEqual(
    events.map((event) => event.seq),
    [0, 1, 2, 3],
  );
  assert.deepEqual(events[2]?.sourceEventSeqs, [1]);
  assert.deepEqual(events[3]?.sourceEventSeqs, [2]);
});

test("a later child turn that restarts at seq 0 continues the log", () => {
  const create = offsetBatchSeqs(-1, [0, 1, 22]);
  assert.equal(create.offset, 0);
  const resume = offsetBatchSeqs(create.previousSeq, [0, 1, 2]);
  assert.deepEqual(resume.seqs, [23, 24, 25]);
  assert.equal(resume.offset, 23);
  const third = offsetBatchSeqs(resume.previousSeq, [0, 4]);
  assert.deepEqual(third.seqs, [26, 30]);
});

test("duplicate seq inside one child turn is refused", () => {
  assert.throws(() => offsetBatchSeqs(-1, [3, 3]));
});
