import "./style.css";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { createPtyInputQueue } from "./pty-input";
import { createPtyResizeQueue } from "./pty-resize";
import { parseCodexState, titleFromTerminal } from "./terminal-signals";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";

type Status = "starting" | "ready" | "running" | "waiting" | "done" | "stopping" | "exited" | "error" | "unknown";
type Project = { id: string; name: string; directory: string; expanded: boolean };
type Session = { id: string; projectId: string; name: string; processName?: string | null; autoRenamePending?: boolean; launchKind?: "codex" | "shell"; status: Status; unread: boolean; lastActivatedAt: number; runId?: string | null; error?: string | null };
type Bootstrap = { projects: Project[]; sessions: Session[]; startupError?: string | null };
type Output = { sessionId: string; runId: string; bytes: string };

const $ = <T extends HTMLElement = HTMLElement>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};
const projects = new Map<string, Project>();
const sessions = new Map<string, Session>();
const terminals = new Map<string, { terminal: Terminal; fit: FitAddon; pane: HTMLDivElement; input: ReturnType<typeof createPtyInputQueue>; resize: ReturnType<typeof createPtyResizeQueue>; runId: string | null; resetRename: () => void }>();
let terminalModules: Promise<[typeof import("@xterm/xterm"), typeof import("@xterm/addon-fit")]> | null = null;
const pendingOutput = new Map<string, Uint8Array[]>();
const closedSessionIds = new Set<string>();
let activeSessionId: string | null = null;
let activeProjectId: string | null = null;
let menuTarget: HTMLElement | null = null;
let toastTimer: number | undefined;
let startupError: string | null = null;

const stateLabels: Record<Status, string> = {
  starting: "启动中", ready: "就绪", running: "工作中", waiting: "待确认",
  done: "完成", stopping: "停止中", exited: "已退出", error: "异常", unknown: "状态未知"
};
const stateMarks: Record<Status, string> = {
  starting: "○", ready: "○", running: "●", waiting: "!", done: "✓",
  stopping: "○", exited: "○", error: "!", unknown: "?"
};
const busy = (session: Session) => ["starting", "running", "waiting", "stopping", "unknown"].includes(session.status) || session.launchKind === "shell" && session.status === "ready";
const shellSession = (session: Session) => session.launchKind === "shell";
const sessionProject = (session: Session) => projects.get(session.projectId);
const children = (projectId: string) => [...sessions.values()].filter(s => s.projectId === projectId);
const element = <K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, content?: string): HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (content !== undefined) node.textContent = content;
  return node;
};
const fmt = (count: number, label: string) => `${count} 个${label}`;

function showToast(message: string) {
  const toast = $("#toast");
  toast.textContent = message;
  toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => { toast.hidden = true; }, 4000);
}

function closeMenu() {
  $("#menu").hidden = true;
  $("#menu").replaceChildren();
  menuTarget = null;
}

function showMenu(anchor: HTMLElement, items: { label: string; danger?: boolean; disabled?: boolean; note?: string; action: () => void }[], position?: { x: number; y: number }) {
  if (!position && menuTarget === anchor && !$("#menu").hidden) { closeMenu(); return; }
  closeMenu();
  menuTarget = anchor;
  const menu = $("#menu");
  items.forEach(item => {
    const button = element("button", item.danger ? "danger" : "", item.label);
    button.disabled = !!item.disabled;
    button.onclick = () => { closeMenu(); item.action(); };
    menu.append(button);
    if (item.note) menu.append(element("p", "", item.note));
  });
  menu.hidden = false;
  const rect = anchor.getBoundingClientRect();
  const width = Math.max(menu.getBoundingClientRect().width, 144);
  menu.style.left = `${Math.max(10, Math.min(position?.x ?? rect.left, innerWidth - width - 10))}px`;
  menu.style.top = `${Math.max(10, Math.min(position?.y ?? rect.bottom + 4, innerHeight - menu.getBoundingClientRect().height - 10))}px`;
}

function showSessionMenu(anchor: HTMLElement, session: Session, position?: { x: number; y: number }) {
  showMenu(anchor, [
    { label: "重命名", action: () => void renameSession(session) },
    { label: "重新启动", disabled: session.status !== "exited" && session.status !== "error", action: () => void restartSession(session) },
    { label: "删除会话", danger: true, action: () => void deleteSession(session) }
  ], position);
}

function dialog(options: { title: string; message: string; label: string; value?: string; danger?: boolean }): Promise<string | null> {
  const box = $<HTMLDialogElement>("#dialog");
  const input = $<HTMLInputElement>("#dialog-input");
  const confirm = $<HTMLButtonElement>("#dialog-confirm");
  $("#dialog-title").textContent = options.title;
  $("#dialog-message").textContent = options.message;
  $("#dialog-label").hidden = options.value === undefined;
  input.hidden = options.value === undefined;
  input.required = options.value !== undefined;
  input.value = options.value ?? "";
  confirm.textContent = options.label;
  confirm.classList.toggle("danger", !!options.danger);
  box.showModal();
  if (!input.hidden) { input.focus(); input.select(); }
  return new Promise(resolve => {
    let submitted = false;
    const form = $<HTMLFormElement>("#dialog-form");
    form.onsubmit = event => {
      event.preventDefault();
      if (!input.hidden && !input.value.trim()) { input.setCustomValidity("请输入名称"); input.reportValidity(); return; }
      submitted = true;
      box.close();
    };
    input.oninput = () => input.setCustomValidity("");
    $<HTMLButtonElement>("#dialog-cancel").onclick = () => box.close();
    box.onclose = () => { resolve(submitted ? input.hidden ? "yes" : input.value.trim() : null); };
  });
}

function selectProject(projectId: string) {
  activeProjectId = projectId;
  activeSessionId = null;
  render();
}

async function selectSession(id: string) {
  const session = sessions.get(id);
  if (!session) return;
  activeSessionId = id;
  activeProjectId = session.projectId;
  try { await invoke("mark_seen", { id }); session.unread = false; }
  catch (error) { showToast(String(error)); }
  render();
  try { await ensureTerminal(session); }
  catch (error) { showToast(String(error)); }
  activateTerminal();
}

function renderProject(project: Project): HTMLElement {
  const group = element("section", "project-group");
  group.dataset.projectId = project.id;
  const row = element("div", "project-row");
  const toggle = element("button", "project-toggle");
  toggle.title = project.directory;
  toggle.setAttribute("aria-expanded", String(project.expanded));
  toggle.setAttribute("aria-label", `${project.expanded ? "收起" : "展开"}项目 ${project.name}`);
  toggle.append(element("span", "chevron", project.expanded ? "⌄" : "›"), element("span", "folder", "▱"), element("span", "project-name", project.name));
  toggle.onclick = async () => {
    const next = !project.expanded;
    try { await invoke("set_project_expanded", { id: project.id, expanded: next }); project.expanded = next; render(); }
    catch (error) { showToast(String(error)); }
  };
  const more = element("button", "icon-button project-actions", "···");
  more.title = `${project.name} 项目操作`;
  more.setAttribute("aria-label", more.title);
  more.onclick = event => {
    event.stopPropagation();
    const blocked = children(project.id).some(busy);
    showMenu(more, [
      { label: "删除项目", danger: true, disabled: blocked,
        note: blocked ? "请先删除运行中的会话" : undefined,
        action: () => void deleteProject(project) }
    ]);
  };
  const add = element("button", "icon-button project-create", "+");
  add.title = `在 ${project.name} 中新建会话`;
  add.setAttribute("aria-label", add.title);
  add.onclick = () => void createSession(project);
  row.append(toggle, add, more);
  group.append(row);
  const owned = children(project.id);
  const waiting = owned.filter(s => s.status === "waiting").length;
  const running = owned.filter(s => s.status === "running").length;
  const unread = owned.filter(s => s.unread).length;
  const summary = element("div", "project-summary");
  if (waiting) summary.append(element("span", "state waiting", fmt(waiting, "待确认")));
  if (running) summary.append(element("span", "state running", fmt(running, "工作中")));
  if (unread) summary.append(element("span", "state done", fmt(unread, "完成未读")));
  group.append(summary);
  if (project.expanded) {
    const content = element("div", "project-children");
    const path = element("div", "project-path", project.directory);
    path.title = project.directory;
    content.append(path);
    for (const session of owned) {
      const button = element("button", `session-row${session.id === activeSessionId ? " active" : ""}`);
      button.dataset.sessionId = session.id;
      button.title = session.error || `${session.processName ? `${session.processName} · ` : ""}${session.name}`;
      button.setAttribute("aria-pressed", String(session.id === activeSessionId));
      const name = element("span", "session-name");
      if (session.processName) name.append(element("span", "process-name", session.processName), element("span", "name-separator", "·"));
      name.append(element("span", "session-title", session.name));
      button.append(name);
      const meta = element("span", "session-meta", `${shellSession(session) ? "终端" : "Codex"}  /  `);
      meta.append(element("span", `state ${session.status}`, `${stateMarks[session.status]} ${shellSession(session) && session.status === "ready" ? "已启动" : stateLabels[session.status]}`));
      button.append(meta);
      if (session.status === "running") {
        const activity = element("span", "session-activity");
        activity.setAttribute("aria-label", "工作中");
        activity.title = "工作中";
        button.append(activity);
      }
      if (session.unread) button.append(element("span", "unread"));
      let selectTimer: number | undefined;
      button.onclick = () => {
        if (session.status === "exited" || session.status === "error") {
          window.clearTimeout(selectTimer);
          selectTimer = window.setTimeout(() => void selectSession(session.id), 450);
        } else void selectSession(session.id);
      };
      button.ondblclick = () => {
        if (session.status === "exited" || session.status === "error") {
          window.clearTimeout(selectTimer);
          void restartSession(session, true);
        }
      };
      button.oncontextmenu = event => {
        event.preventDefault();
        const rect = button.getBoundingClientRect();
        showSessionMenu(button, session, { x: event.clientX || rect.left, y: event.clientY || rect.bottom });
      };
      content.append(button);
    }
    if (!owned.length) {
      content.append(element("div", "no-agents", "还没有会话"));
    }
    group.append(content);
  }
  return group;
}

function render() {
  const tree = $("#project-tree");
  tree.replaceChildren(...[...projects.values()].map(renderProject));
  $("#project-count").textContent = String(projects.size);
  const session = activeSessionId ? sessions.get(activeSessionId) : null;
  const project = session ? sessionProject(session) : activeProjectId ? projects.get(activeProjectId) : null;
  const breadcrumb = $("#breadcrumb");
  breadcrumb.replaceChildren();
  if (project) {
    breadcrumb.append(element("span", "project-part", project.name));
    if (session) breadcrumb.append(element("span", "slash", "/"), element("span", "", session.name));
  } else breadcrumb.textContent = "选择项目";
  const state = $("#session-state");
  state.replaceChildren();
  if (session) state.append(element("span", `state ${session.status}`, `${stateMarks[session.status]} ${shellSession(session) && session.status === "ready" ? "终端已启动" : stateLabels[session.status]}`));
  const actions = $<HTMLButtonElement>("#session-actions");
  actions.hidden = !session;
  actions.onclick = session ? () => showSessionMenu(actions, session) : null;
  const welcome = $("#welcome");
  welcome.hidden = !!session;
  if (!session) {
    welcome.querySelector("h1")!.textContent = project ? project.name : "暂无项目";
    const action = $<HTMLButtonElement>("#welcome-action");
    action.textContent = project ? "新建会话" : "添加项目";
    action.onclick = project ? () => void createSession(project) : () => void addProject();
  }
  $("#terminal-stage").hidden = !session;
  const stage = $("#terminal-stage");
  stage.querySelector(".terminal-empty")?.remove();
  if (session && !terminals.has(session.id) && ["exited", "error"].includes(session.status)) {
    const empty = element("div", "terminal-empty");
    empty.append(element("h2", "", session.status === "error" ? `${shellSession(session) ? "终端" : "Codex"} 启动失败` : "会话已退出"));
    if (session.error) empty.append(element("p", "", session.error));
    const restart = element("button", "button", "重新启动");
    restart.onclick = () => void restartSession(session);
    empty.append(restart);
    stage.append(empty);
  }
  $("#status-line").textContent = session && project ? project.directory : project?.directory || "";
  activateTerminal();
}

async function ensureTerminal(session: Session) {
  if (terminals.get(session.id)?.runId === session.runId || session.status === "exited" && !pendingOutput.has(session.id)) return;
  terminalModules ??= Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]).catch(error => {
    terminalModules = null;
    throw error;
  });
  const [{ Terminal }, { FitAddon }] = await terminalModules;
  const current = sessions.get(session.id);
  if (!current || terminals.get(session.id)?.runId === current.runId || current.status === "exited" && !pendingOutput.has(session.id)) return;
  if (terminals.has(session.id)) disposeTerminal(session.id);
  const terminal = new Terminal({
    cols: 100, rows: 28,
    cursorBlink: true, scrollback: 3000, fontSize: 13, lineHeight: 1.4,
    fontFamily: "SFMono-Regular, Menlo, Consolas, 'Noto Sans Mono CJK SC', monospace",
    theme: { background: "#1e2024", foreground: "#d9dde3", cursor: "#d9a17a", selectionBackground: "#65708388", black: "#25282e", red: "#d49595", green: "#9fbea5", yellow: "#dbb689", blue: "#a5c2e2", magenta: "#c2a8ca", cyan: "#a7c4c5", white: "#e3e6e9" }
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  const pane = element("div", "terminal-pane");
  pane.dataset.sessionId = session.id;
  pane.classList.add("active");
  $("#terminal-stage").append(pane);
  terminal.open(pane);
  const input = createPtyInputQueue(data => invoke("write_pty", { id: session.id, data }), error => showToast(String(error)));
  const resize = createPtyResizeQueue(size => invoke("resize_pty", { id: session.id, ...size }), error => showToast(String(error)));
  terminal.onData(data => input.send(data));
  const runId = current.runId || null;
  let sawCodexIdle = false;
  let renameRequested = false;
  terminal.parser.registerOscHandler(7501, data => {
    const state = parseCodexState(data);
    if (!state || sessions.get(session.id)?.runId !== runId) return false;
    if (state === "idle") sawCodexIdle = true;
    if (state === "working" && !sawCodexIdle) return true;
    return true;
  });
  terminal.onTitleChange(title => {
    const live = sessions.get(session.id);
    if (!sawCodexIdle || renameRequested || live?.runId !== runId || !live.autoRenamePending) return;
    const project = sessionProject(session);
    const name = project && titleFromTerminal(title, project.name, project.directory);
    if (name) {
      renameRequested = true;
      void invoke("auto_rename_from_terminal", { id: session.id, runId, title: name }).catch(() => { renameRequested = false; });
    }
  });
  fit.fit();
  pane.classList.toggle("active", session.id === activeSessionId);
  terminals.set(session.id, { terminal, fit, pane, input, resize, runId, resetRename: () => { renameRequested = false; } });
  resize.send({ rows: terminal.rows, cols: terminal.cols });
  const pending = pendingOutput.get(session.id);
  if (pending) {
    for (const chunk of pending) terminal.write(chunk);
    pendingOutput.delete(session.id);
  }
}

function activateTerminal() {
  for (const [id, item] of terminals) item.pane.classList.toggle("active", id === activeSessionId);
  if (!activeSessionId) return;
  const item = terminals.get(activeSessionId);
  if (!item) return;
  requestAnimationFrame(() => {
    if (activeSessionId !== item.pane.dataset.sessionId) return;
    item.fit.fit();
    item.resize.send({ rows: item.terminal.rows, cols: item.terminal.cols });
    item.terminal.focus();
  });
}

function disposeTerminal(id: string) {
  const item = terminals.get(id);
  if (item) { item.input.close(); item.resize.close(); item.terminal.dispose(); item.pane.remove(); terminals.delete(id); }
  pendingOutput.delete(id);
}

async function addProject() {
  if (startupError) { showToast(`配置读取失败：${startupError}`); return; }
  try {
    const directory = await open({ directory: true, multiple: false, title: "选择项目目录" });
    if (!directory || typeof directory !== "string") return;
    const defaultName = directory.split(/[\\/]/).filter(Boolean).pop() || "新项目";
    const name = await dialog({ title: "添加项目", message: directory, label: "添加项目", value: defaultName });
    if (name === null) return;
    const project = await invoke<Project>("create_project", { directory, name });
    projects.set(project.id, project);
    selectProject(project.id);
  } catch (error) { showToast(String(error)); }
}

async function createSession(project: Project) {
  const name = `终端 ${children(project.id).length + 1}`;
  try {
    const session = await invoke<Session>("create_session", { projectId: project.id, name });
    sessions.set(session.id, session);
    await selectSession(session.id);
    if (session.error) showToast(session.error);
  } catch (error) { showToast(String(error)); }
}

async function restartSession(session: Session, activate = false) {
  if (activate) { activeSessionId = session.id; activeProjectId = session.projectId; }
  try { const updated = await invoke<Session>("restart_session", { id: session.id }); sessions.set(session.id, updated); render(); await ensureTerminal(updated); activateTerminal(); }
  catch (error) { showToast(String(error)); }
}

async function renameSession(session: Session) {
  const name = await dialog({ title: "重命名会话", message: sessionProject(session)?.directory || "", label: "保存", value: session.name });
  if (name === null) return;
  try { sessions.set(session.id, await invoke("rename_session", { id: session.id, name })); render(); }
  catch (error) { showToast(String(error)); }
}

async function deleteSession(session: Session) {
  const running = busy(session);
  const message = shellSession(session) && running
    ? `删除「${session.name}」会结束终端及其中运行的程序。项目文件不会被删除。`
    : running ? `「${session.name}」的任务尚未结束，删除会中断任务。项目文件不会被删除。`
      : `删除「${session.name}」及其会话记录？项目文件不会被删除。`;
  const answer = await dialog({ title: "删除会话？", message, label: "删除会话", danger: true });
  if (!answer) return;
  try {
    await invoke("delete_session", { id: session.id });
    closedSessionIds.add(session.id);
    sessions.delete(session.id);
    disposeTerminal(session.id);
    if (activeSessionId === session.id) {
      const recent = children(session.projectId).sort((a, b) => b.lastActivatedAt - a.lastActivatedAt)[0];
      activeSessionId = recent?.id || null;
      if (recent) await selectSession(recent.id);
    }
    render();
  } catch (error) { showToast(String(error)); }
}

async function deleteProject(project: Project) {
  const owned = children(project.id);
  const answer = await dialog({ title: `删除「${project.name}」？`, message: `从工作台移除该项目及其 ${owned.length} 个 Agent 会话。\n磁盘目录和文件不会被删除。`, label: "删除项目", danger: true });
  if (!answer) return;
  try {
    await invoke("delete_project", { id: project.id });
    projects.delete(project.id);
    owned.forEach(session => { closedSessionIds.add(session.id); sessions.delete(session.id); disposeTerminal(session.id); });
    if (activeProjectId === project.id) {
      const recent = [...sessions.values()].sort((a, b) => b.lastActivatedAt - a.lastActivatedAt)[0];
      activeSessionId = recent?.id || null;
      activeProjectId = recent?.projectId || projects.keys().next().value || null;
      if (recent) { const parent = projects.get(recent.projectId); if (parent) parent.expanded = true; }
    }
    render();
  } catch (error) { showToast(String(error)); }
}

function decodeBase64(base64: string): Uint8Array {
  const raw = atob(base64);
  return Uint8Array.from(raw, char => char.charCodeAt(0));
}

async function initialize() {
  await listen<Session>("session-updated", event => {
    const session = event.payload;
    if (closedSessionIds.has(session.id)) return;
    if (!sessions.has(session.id) && !projects.has(session.projectId)) return;
    if (session.autoRenamePending && !sessions.get(session.id)?.autoRenamePending) terminals.get(session.id)?.resetRename();
    sessions.set(session.id, session);
    if (activeSessionId === session.id && session.status === "done" && document.hasFocus()) {
      void invoke("mark_seen", { id: session.id }).then(() => { session.unread = false; render(); }).catch(() => {});
    }
    render();
    if (activeSessionId === session.id) void ensureTerminal(session).then(activateTerminal).catch(error => showToast(String(error)));
  });
  const outputChannel = new Channel<Output>();
  outputChannel.onmessage = ({ sessionId, runId, bytes }) => {
    if (sessions.get(sessionId)?.runId !== runId) return;
    const chunk = decodeBase64(bytes);
    const item = terminals.get(sessionId);
    if (item?.runId === runId) item.terminal.write(chunk);
    else {
      const list = pendingOutput.get(sessionId) || [];
      list.push(chunk);
      while (list.length > 128) list.shift();
      pendingOutput.set(sessionId, list);
    }
  };
  await invoke("subscribe_pty_output", { onOutput: outputChannel });
  const snapshot = await invoke<Bootstrap>("bootstrap");
  startupError = snapshot.startupError || null;
  snapshot.projects.forEach(project => projects.set(project.id, project));
  snapshot.sessions.forEach(session => sessions.set(session.id, session));
  $("#add-project").onclick = () => void addProject();
  document.addEventListener("click", event => { if (menuTarget && !$("#menu").contains(event.target as Node) && !menuTarget.contains(event.target as Node)) closeMenu(); });
  document.addEventListener("keydown", event => {
    if (event.key === "Escape") closeMenu();
  });
  new ResizeObserver(() => {
    const item = activeSessionId ? terminals.get(activeSessionId) : null;
    if (item) { item.fit.fit(); item.resize.send({ rows: item.terminal.rows, cols: item.terminal.cols }); }
  }).observe($("#terminal-stage"));
  await getCurrentWindow().onCloseRequested(async event => {
    event.preventDefault();
    try { await invoke("shutdown", { confirmed: false }); }
    catch (error) {
      const answer = await dialog({ title: "退出 Two？", message: String(error), label: "结束会话并退出", danger: true });
      if (answer) try { await invoke("shutdown", { confirmed: true }); } catch (failure) { showToast(String(failure)); }
    }
  });
  if (startupError) showToast(`配置读取失败：${startupError}`);
  render();
}

initialize().catch(error => showToast(`应用启动失败：${String(error)}`));
