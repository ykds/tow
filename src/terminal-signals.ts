export type CodexState = "working" | "idle" | "clear";

export function parseCodexState(data: string): CodexState | null {
  const fields = data.split(":");
  if (!fields.includes("app=codex")) return null;
  const state = fields.find(field => field.startsWith("state="))?.slice(6);
  return state === "working" || state === "idle" || state === "clear" ? state : null;
}

export function titleFromTerminal(title: string, projectName: string, directory: string): string | null {
  const directoryName = directory.split(/[\\/]/).filter(Boolean).pop();
  const parts = title.split(" | ").map(part => part.trim().replace(/^[⠁-⣿●○]\s*/u, "").replace(/^:\s*/, "").trim());
  const candidate = parts.find(part => part && part !== projectName && part !== directoryName && part.toLowerCase() !== "codex"
    && !["working", "ready", "idle", "done"].includes(part.toLowerCase())
    && !part.startsWith("/") && !part.startsWith("~") && !/^\d+x\d+$/i.test(part));
  return candidate || null;
}
