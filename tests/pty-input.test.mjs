import assert from "node:assert/strict";
import { test } from "node:test";
import { createPtyInputQueue } from "../src/pty-input.ts";

const tick = () => new Promise(resolve => setImmediate(resolve));

test("rapid keys and backspaces reach the PTY in their original order", async () => {
  const writes = [];
  let releaseFirst;
  const first = new Promise(resolve => { releaseFirst = resolve; });
  const input = createPtyInputQueue(async data => {
    writes.push(data);
    if (writes.length === 1) await first;
  }, error => { throw error; });

  input.send("code");
  input.send("\x7f");
  input.send("\x7f");
  input.send("ex");
  assert.deepEqual(writes, ["code"]);

  releaseFirst();
  await tick();
  assert.deepEqual(writes, ["code", "\x7f\x7fex"]);
});

test("failed writes drop stale input and allow later keys", async () => {
  const writes = [];
  const errors = [];
  let rejectFirst;
  const first = new Promise((_, reject) => { rejectFirst = reject; });
  const input = createPtyInputQueue(async data => {
    writes.push(data);
    if (writes.length === 1) await first;
  }, error => errors.push(error.message));

  input.send("old");
  input.send("\x7f");
  rejectFirst(new Error("PTY closed"));
  await tick();
  input.send("new");
  await tick();
  assert.deepEqual(writes, ["old", "new"]);
  assert.deepEqual(errors, ["PTY closed"]);

  input.close();
  input.send("ignored");
  await tick();
  assert.deepEqual(writes, ["old", "new"]);
});
