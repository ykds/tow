import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { parseCodexState, titleFromTerminal } from "../src/terminal-signals.ts";

const { Terminal } = createRequire(import.meta.url)("@xterm/xterm");

test("only Codex program-status reports change task activity", () => {
  assert.equal(parseCodexState("state=working:app=codex"), "working");
  assert.equal(parseCodexState("state=idle:app=codex"), "idle");
  assert.equal(parseCodexState("state=clear:app=codex"), "clear");
  assert.equal(parseCodexState("state=working:app=other"), null);
  assert.equal(parseCodexState("state=unknown:app=codex"), null);
});

test("terminal title ignores project-only and shell titles", () => {
  const directory = "/Users/dyk/project/balance-mfrr";
  assert.equal(titleFromTerminal("⠋ balance-mfrr", "Energy", directory), null);
  assert.equal(titleFromTerminal("/Users/dyk/project/balance-mfrr", "Energy", directory), null);
  assert.equal(titleFromTerminal("codex", "Energy", directory), null);
  assert.equal(titleFromTerminal("balance-mfrr | 设计多 Agent 并行工作工具方案", "Energy", directory), "设计多 Agent 并行工作工具方案");
  assert.equal(titleFromTerminal(": 设计多 Agent 并行工作工具方案", "Energy", directory), "设计多 Agent 并行工作工具方案");
});

test("xterm forwards Codex OSC status and terminal title", async () => {
  const terminal = new Terminal();
  let state = "";
  let title = "";
  terminal.parser.registerOscHandler(7501, value => { state = value; return true; });
  terminal.onTitleChange(value => { title = value; });
  await new Promise(resolve => terminal.write("\x1b]7501;state=working:app=codex\x1b\\\x1b]0;project | task title\x07", resolve));
  assert.equal(parseCodexState(state), "working");
  assert.equal(titleFromTerminal(title, "project", "/tmp/project"), "task title");
  terminal.dispose();
});
