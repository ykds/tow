export function createPtyInputQueue(write: (data: string) => Promise<unknown>, onError: (error: unknown) => void) {
  let pending = "";
  let writing = false;
  let closed = false;

  async function flush() {
    if (writing || closed) return;
    writing = true;
    try {
      while (pending && !closed) {
        const data = pending;
        pending = "";
        try { await write(data); }
        catch (error) {
          pending = "";
          onError(error);
          break;
        }
      }
    } finally { writing = false; }
  }

  return {
    send(data: string) {
      if (closed || !data) return;
      pending += data;
      void flush();
    },
    close() { closed = true; pending = ""; }
  };
}
