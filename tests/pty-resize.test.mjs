import assert from "node:assert/strict";
import { test } from "node:test";
import { createPtyResizeQueue } from "../src/pty-resize.ts";

const tick = () => new Promise(resolve => setImmediate(resolve));

test("rapid resizes keep the last PTY size in order", async () => {
  const sizes = [];
  let releaseFirst;
  const first = new Promise(resolve => { releaseFirst = resolve; });
  const resize = createPtyResizeQueue(async size => {
    sizes.push(size);
    if (sizes.length === 1) await first;
  }, error => { throw error; });

  resize.send({ rows: 28, cols: 100 });
  resize.send({ rows: 28, cols: 90 });
  resize.send({ rows: 28, cols: 80 });
  assert.deepEqual(sizes, [{ rows: 28, cols: 100 }]);

  releaseFirst();
  await tick();
  assert.deepEqual(sizes, [{ rows: 28, cols: 100 }, { rows: 28, cols: 80 }]);
});

test("closing a terminal drops pending resizes", async () => {
  const sizes = [];
  let releaseFirst;
  const first = new Promise(resolve => { releaseFirst = resolve; });
  const resize = createPtyResizeQueue(async size => {
    sizes.push(size);
    await first;
  }, error => { throw error; });

  resize.send({ rows: 28, cols: 100 });
  resize.send({ rows: 28, cols: 80 });
  resize.close();
  releaseFirst();
  await tick();
  assert.deepEqual(sizes, [{ rows: 28, cols: 100 }]);
});
