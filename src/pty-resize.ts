export type PtySize = { rows: number; cols: number };

export function createPtyResizeQueue(write: (size: PtySize) => Promise<unknown>, onError: (error: unknown) => void) {
  let pending: PtySize | null = null;
  let writing = false;
  let closed = false;

  async function flush() {
    if (writing || closed) return;
    writing = true;
    try {
      while (pending && !closed) {
        const size = pending;
        pending = null;
        try { await write(size); }
        catch (error) { onError(error); }
      }
    } finally { writing = false; }
  }

  return {
    send(size: PtySize) {
      if (closed || size.rows < 1 || size.cols < 1) return;
      pending = size;
      void flush();
    },
    close() { closed = true; pending = null; }
  };
}
