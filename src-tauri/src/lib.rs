use atomicwrites::{AtomicFile, OverwriteBehavior};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::Channel;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionStatus {
    Starting, Ready, Running, Waiting, Done, Stopping, Exited, Error, Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum LaunchKind {
    #[default]
    Codex,
    Shell,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    directory: String,
    expanded: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSession {
    id: String,
    project_id: String,
    name: String,
    #[serde(default)]
    auto_rename_pending: bool,
    #[serde(default)]
    name_locked: bool,
    #[serde(default)]
    launch_kind: LaunchKind,
    status: SessionStatus,
    unread: bool,
    last_activated_at: u64,
    turn_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_name: Option<String>,
    #[serde(skip)]
    reported_app: Option<String>,
    #[serde(skip)]
    finished_turn_ids: VecDeque<String>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedWorkspace {
    version: u8,
    projects: Vec<Project>,
    sessions: Vec<AgentSession>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    projects: Vec<Project>,
    sessions: Vec<AgentSession>,
    startup_error: Option<String>,
}

struct Runtime {
    run_id: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    transcript: Mutex<Option<TranscriptWatch>>,
}

struct TranscriptWatch {
    path: PathBuf,
    offset: u64,
    skip_partial: bool,
    turn_id: Option<String>,
}

struct Inner {
    projects: Vec<Project>,
    sessions: Vec<AgentSession>,
    runtimes: HashMap<String, Arc<Runtime>>,
    deleting: HashSet<String>,
    startup_error: Option<String>,
    shutting_down: bool,
}

struct WorkspaceState {
    inner: Mutex<Inner>,
    output_channel: Mutex<Option<Channel<PtyOutput>>>,
    config_path: PathBuf,
    hook_port: u16,
    hook_token: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyOutput {
    session_id: String,
    run_id: String,
    bytes: String,
}

#[derive(Deserialize)]
struct HookMessage {
    token: String,
    session_id: String,
    run_id: String,
    kind: String,
    turn_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

#[derive(Default)]
struct OscScanner {
    phase: u8,
    payload: Vec<u8>,
}

impl OscScanner {
    fn feed(&mut self, bytes: &[u8]) -> Vec<(u16, String)> {
        let mut events = Vec::new();
        for &byte in bytes {
            match (self.phase, byte) {
                (0, 0x1b) => self.phase = 1,
                (1, b']') => { self.phase = 2; self.payload.clear(); }
                (1, 0x1b) => {},
                (1, _) => self.phase = 0,
                (2, 0x07) | (3, b'\\') => {
                    if let Ok(value) = std::str::from_utf8(&self.payload) {
                        if let Some((code, data)) = value.split_once(';') {
                            if let Ok(code) = code.parse::<u16>() { events.push((code, data.to_string())); }
                        }
                    }
                    self.phase = 0;
                    self.payload.clear();
                }
                (2, 0x1b) => self.phase = 3,
                (3, _) => { self.phase = 0; self.payload.clear(); }
                (2, _) => {
                    if self.payload.len() < 1024 { self.payload.push(byte); }
                    else { self.phase = 0; self.payload.clear(); }
                }
                _ => {},
            }
        }
        events
    }
}

fn codex_osc_state(data: &str) -> Option<&str> {
    let fields: Vec<_> = data.split(':').collect();
    if !fields.contains(&"app=codex") { return None; }
    match fields.iter().find_map(|field| field.strip_prefix("state=")) {
        Some(state @ ("working" | "idle" | "clear")) => Some(state),
        _ => None,
    }
}

impl TranscriptWatch {
    fn new(path: PathBuf, turn_id: String) -> Self {
        let end = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        let offset = end.saturating_sub(64 * 1024);
        Self { path, offset, skip_partial: offset > 0, turn_id: Some(turn_id) }
    }

    fn completed_turn(&mut self) -> Option<String> {
        let expected = self.turn_id.clone()?;
        let mut file = fs::File::open(&self.path).ok()?;
        if file.metadata().ok()?.len() < self.offset {
            self.offset = 0;
            self.skip_partial = false;
        }
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        if self.skip_partial {
            let size = reader.read_until(b'\n', &mut line).ok()?;
            if line.last() != Some(&b'\n') { return None; }
            self.offset += size as u64;
            self.skip_partial = false;
        }
        loop {
            line.clear();
            let size = reader.read_until(b'\n', &mut line).ok()?;
            if size == 0 || line.last() != Some(&b'\n') { return None; }
            self.offset += size as u64;
            let Ok(event) = serde_json::from_slice::<serde_json::Value>(&line) else { continue; };
            if event.get("type").and_then(|value| value.as_str()) == Some("event_msg")
                && event.pointer("/payload/type").and_then(|value| value.as_str()) == Some("task_complete")
                && event.pointer("/payload/turn_id").and_then(|value| value.as_str()) == Some(expected.as_str()) {
                self.turn_id = None;
                return Some(expected);
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn save(state: &WorkspaceState, inner: &Inner) -> Result<(), String> {
    if let Some(error) = &inner.startup_error {
        return Err(format!("配置读取失败，请先修复原文件：{error}"));
    }
    let snapshot = SavedWorkspace {
        version: 1,
        projects: inner.projects.clone(),
        sessions: inner.sessions.iter().cloned().map(|mut s| {
            s.status = SessionStatus::Exited;
            s.unread = false;
            s.last_activated_at = 0;
            s.turn_id = None;
            s.run_id = None;
            s.process_name = None;
            s.reported_app = None;
            s.finished_turn_ids.clear();
            s.error = None;
            s
        }).collect(),
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
    if let Some(parent) = state.config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    AtomicFile::new(&state.config_path, OverwriteBehavior::AllowOverwrite)
        .write(|file| file.write_all(&bytes)).map_err(|e| e.to_string())
}

fn persist_change<T>(state: &WorkspaceState, inner: &mut Inner, change: impl FnOnce(&mut Inner) -> Result<T, String>) -> Result<T, String> {
    let projects = inner.projects.clone();
    let sessions = inner.sessions.clone();
    let result = change(inner).and_then(|value| save(state, inner).map(|_| value));
    if result.is_err() {
        inner.projects = projects;
        inner.sessions = sessions;
    }
    result
}

fn load(path: &Path) -> Result<(Vec<Project>, Vec<AgentSession>), String> {
    if !path.exists() { return Ok((Vec::new(), Vec::new())); }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let saved: SavedWorkspace = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if saved.version != 1 { return Err(format!("不支持配置版本 {}", saved.version)); }
    let projects: HashSet<_> = saved.projects.iter().map(|p| p.id.as_str()).collect();
    if saved.sessions.iter().any(|s| !projects.contains(s.project_id.as_str())) {
        return Err("配置中存在没有所属项目的 Agent".into());
    }
    let sessions = saved.sessions.into_iter().map(|mut s| {
        s.status = SessionStatus::Exited;
        s.unread = false;
        s.last_activated_at = 0;
        s.turn_id = None;
        s.run_id = None;
        s.process_name = None;
        s.reported_app = None;
        s.finished_turn_ids.clear();
        s.error = None;
        s
    }).collect();
    Ok((saved.projects, sessions))
}

fn writable(inner: &Inner) -> Result<(), String> {
    if let Some(error) = &inner.startup_error {
        return Err(format!("配置文件需要修复：{error}"));
    }
    if inner.shutting_down { return Err("应用正在退出".into()); }
    Ok(())
}

fn emit_session(app: &AppHandle, session: &AgentSession) {
    let _ = app.emit("session-updated", session);
}

fn busy(status: &SessionStatus) -> bool {
    matches!(status, SessionStatus::Starting | SessionStatus::Running | SessionStatus::Waiting | SessionStatus::Stopping | SessionStatus::Unknown)
}

fn session_busy(session: &AgentSession) -> bool {
    busy(&session.status) || session.launch_kind == LaunchKind::Shell && session.status == SessionStatus::Ready
}

#[cfg(target_os = "macos")]
fn process_name_for_pid(pid: u32) -> Option<String> {
    let mut buffer = [0u8; 256];
    let size = unsafe { libc::proc_name(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if size <= 0 { return None; }
    let name = std::str::from_utf8(&buffer[..size as usize]).ok()?.trim_end_matches('\0');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_os = "linux")]
fn process_name_for_pid(pid: u32) -> Option<String> {
    let name = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_name_for_pid(_pid: u32) -> Option<String> { None }

fn default_process_name(launch_kind: LaunchKind) -> Option<String> {
    if launch_kind == LaunchKind::Codex { return Some("codex".into()); }
    std::env::var_os("SHELL").or_else(|| std::env::var_os("COMSPEC"))
        .and_then(|path| Path::new(&path).file_name().map(|name| name.to_string_lossy().into_owned()))
}

fn resolve_codex() -> Result<PathBuf, String> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    #[cfg(unix)] {
        dirs.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]);
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.extend([home.join(".local/bin"), home.join(".volta/bin"), home.join(".bun/bin")]);
            let versions = home.join(".nvm/versions/node");
            if let Ok(entries) = fs::read_dir(versions) {
                let mut nvm_dirs: Vec<_> = entries.flatten().map(|entry| entry.path().join("bin")).collect();
                nvm_dirs.sort();
                nvm_dirs.reverse();
                dirs.extend(nvm_dirs);
            }
        }
    }
    #[cfg(windows)] {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(app_data).join("npm"));
        }
    }
    for dir in dirs {
        #[cfg(windows)] let names = ["codex.exe", "codex.cmd"];
        #[cfg(not(windows))] let names = ["codex"];
        for name in names {
            let path = dir.join(name);
            if path.is_file() { return Ok(path); }
        }
    }
    Err("未找到 Codex CLI。请先安装并登录 Codex，再重新启动应用。".into())
}

fn codex_child_path(codex: &Path, inherited_path: Option<&OsStr>) -> Result<OsString, String> {
    let bin_dir = codex.parent().ok_or("Codex CLI 路径无效")?;
    let mut dirs = vec![bin_dir.to_path_buf()];
    if let Some(path) = inherited_path {
        dirs.extend(std::env::split_paths(path));
    }
    std::env::join_paths(dirs).map_err(|error| format!("无法设置 Codex CLI 的 PATH：{error}"))
}

fn codex_shell_path(codex: &Path, wrapper_dir: &Path, inherited_path: Option<&OsStr>) -> Result<OsString, String> {
    let mut dirs = vec![wrapper_dir.to_path_buf()];
    if let Some(path) = inherited_path { dirs.extend(std::env::split_paths(path)); }
    dirs.push(codex.parent().ok_or("Codex CLI 路径无效")?.to_path_buf());
    std::env::join_paths(dirs).map_err(|error| format!("无法设置终端的 PATH：{error}"))
}

#[cfg(unix)]
fn quote_command(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(windows)]
fn quote_command(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy())
}

fn hook_config(name: &str, helper: &str) -> String {
    let command = format!("{name}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
        serde_json::to_string(helper).unwrap());
    format!("hooks.{command}")
}

fn title_from_prompt(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|line| !line.is_empty())?;
    let end = line.char_indices().find_map(|(index, character)| {
        let after = index + character.len_utf8();
        if matches!(character, '。' | '！' | '？' | '!' | '?')
            || character == '.' && line[after..].chars().next().map_or(true, char::is_whitespace) {
            Some(after)
        } else { None }
    }).unwrap_or(line.len());
    let sentence = line[..end].trim();
    let title: String = sentence.chars().take(40).collect();
    if title.is_empty() { None }
    else if sentence.chars().count() > 40 { Some(format!("{title}…")) }
    else { Some(title) }
}

fn apply_title(session: &mut AgentSession, title: String) -> bool {
    if session.launch_kind != LaunchKind::Shell || !session.auto_rename_pending || session.name_locked { return false; }
    session.name = title;
    session.auto_rename_pending = false;
    true
}

fn returned_to_shell(session: &mut AgentSession, shell_name: &str) -> bool {
    if session.launch_kind != LaunchKind::Shell
        || session.reported_app.is_none() && session.process_name.as_deref() == Some(shell_name) { return false; }
    session.reported_app = None;
    session.process_name = Some(shell_name.into());
    session.auto_rename_pending = !session.name_locked;
    session.turn_id = None;
    if matches!(session.status, SessionStatus::Running | SessionStatus::Waiting) {
        session.status = SessionStatus::Ready;
    }
    true
}

fn spawn_session(state: &WorkspaceState, app: &AppHandle, session_id: &str) -> Result<(), String> {
    let (directory, old_status, launch_kind) = {
        let mut inner = state.inner.lock().unwrap();
        writable(&inner)?;
        let index = inner.sessions.iter().position(|s| s.id == session_id).ok_or("会话不存在")?;
        let project = inner.projects.iter().find(|p| p.id == inner.sessions[index].project_id).ok_or("项目不存在")?;
        if inner.deleting.contains(&project.id) { return Err("项目正在删除".into()); }
        if inner.runtimes.contains_key(session_id) { return Err("会话已经在运行".into()); }
        let directory = PathBuf::from(&project.directory);
        let previous = inner.sessions[index].status.clone();
        inner.sessions[index].status = SessionStatus::Starting;
        inner.sessions[index].error = None;
        emit_session(app, &inner.sessions[index]);
        (directory, previous, inner.sessions[index].launch_kind)
    };
    let run_id = Uuid::new_v4().to_string();
    let result = (|| {
        if !directory.is_dir() { return Err("项目目录不存在或不是文件夹".to_string()); }
        let helper_path = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = match launch_kind {
            LaunchKind::Shell => {
                let mut cmd = CommandBuilder::new_default_prog();
                #[cfg(windows)] let wrapper_name = "codex.cmd";
                #[cfg(not(windows))] let wrapper_name = "codex";
                // The shared Codex daemon cannot inherit this terminal's Hook identity; localize only Codex launched here.
                if let Ok(codex) = resolve_codex() {
                    if let Ok(wrapper) = app.path().resolve(format!("resources/{wrapper_name}"), BaseDirectory::Resource) {
                        if wrapper.is_file() {
                            let inherited_path = std::env::var_os("PATH");
                            let wrapper_dir = wrapper.parent().unwrap();
                            cmd.env("PATH", codex_shell_path(&codex, wrapper_dir, inherited_path.as_deref())?);
                            cmd.env("AGENT_WORKSPACE_CODEX_COMMAND", codex);
                            cmd.env("AGENT_WORKSPACE_CODEX_WRAPPER_DIR", wrapper_dir);
                            #[cfg(unix)] if std::env::var_os("SHELL").as_ref().and_then(|shell| Path::new(shell).file_name().map(|name| name == "zsh")).unwrap_or(false) {
                                let proxy_dir = wrapper_dir.join("zsh");
                                if proxy_dir.join(".zshrc").is_file() {
                                    cmd.env("AGENT_WORKSPACE_ZSH_ORIGINAL_DIR", std::env::var_os("ZDOTDIR").or_else(|| std::env::var_os("HOME")).unwrap_or_default());
                                    cmd.env("AGENT_WORKSPACE_ZSH_PROXY_DIR", &proxy_dir);
                                    cmd.env("ZDOTDIR", proxy_dir);
                                }
                            }
                        }
                    }
                }
                cmd
            }
            LaunchKind::Codex => {
                let codex = resolve_codex()?;
                let mut cmd = {
                    #[cfg(windows)] {
                        if codex.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd")) {
                            let mut c = CommandBuilder::new("cmd.exe");
                            c.args(["/D", "/S", "/C", &format!("\"{}\"", codex.display())]);
                            c
                        } else { CommandBuilder::new(codex.clone()) }
                    }
                    #[cfg(not(windows))] { CommandBuilder::new(codex.clone()) }
                };
                // npm-installed Codex may use `#!/usr/bin/env node`; GUI apps often lack its Node bin directory.
                let inherited_path = std::env::var_os("PATH");
                cmd.env("PATH", codex_child_path(&codex, inherited_path.as_deref())?);
                let helper = quote_command(&helper_path);
                cmd.arg("-c");
                cmd.arg(hook_config("PreModelRequest", &format!("{helper} --hook-event submit")));
                cmd.arg("-c");
                cmd.arg(hook_config("PermissionRequest", &format!("{helper} --hook-event waiting")));
                cmd.arg("-c");
                cmd.arg(hook_config("Interrupt", &format!("{helper} --hook-event interrupted")));
                cmd.arg("-c");
                cmd.arg(format!("notify=[{},\"--hook-event\",\"complete\"]",
                    serde_json::to_string(&helper_path.to_string_lossy()).unwrap()));
                cmd
            }
        };
        cmd.env("AGENT_WORKSPACE_HOOK_HELPER", &helper_path);
        cmd.env("AGENT_WORKSPACE_HOOK_PORT", state.hook_port.to_string());
        cmd.env("AGENT_WORKSPACE_HOOK_TOKEN", &state.hook_token);
        cmd.env("AGENT_WORKSPACE_SESSION_ID", session_id);
        cmd.env("AGENT_WORKSPACE_RUN_ID", &run_id);
        // The shell must know which terminal controls xterm.js understands, even when launched from Finder.
        cmd.env("TERM", "xterm-256color");
        cmd.cwd(&directory);
        let system = native_pty_system();
        let pair = system.openpty(PtySize { rows: 28, cols: 100, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let process_name = child.process_id().and_then(process_name_for_pid).or_else(|| default_process_name(launch_kind));
        Ok((Arc::new(Runtime { run_id, master: Mutex::new(pair.master), writer: Mutex::new(writer), child: Mutex::new(child), transcript: Mutex::new(None) }), reader, process_name))
    })();
    let (runtime, mut reader, process_name) = match result {
        Ok(pair) => pair,
        Err(error) => {
            let mut inner = state.inner.lock().unwrap();
            if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == session_id) {
                session.status = if matches!(old_status, SessionStatus::Exited) { SessionStatus::Exited } else { SessionStatus::Error };
                session.error = Some(error.clone());
                emit_session(app, session);
            }
            return Err(error);
        }
    };
    {
        let mut inner = state.inner.lock().unwrap();
        if !inner.sessions.iter().any(|s| s.id == session_id) || inner.shutting_down {
            let _ = runtime.child.lock().unwrap().kill();
            return Err("会话已关闭".into());
        }
        inner.runtimes.insert(session_id.to_string(), runtime.clone());
        if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == session_id) {
            session.status = SessionStatus::Ready;
            session.turn_id = None;
            session.finished_turn_ids.clear();
            session.run_id = Some(runtime.run_id.clone());
            session.process_name = process_name;
            session.reported_app = None;
            if session.launch_kind == LaunchKind::Shell { session.auto_rename_pending = !session.name_locked; }
            emit_session(app, session);
        }
    }
    let output_app = app.clone();
    let output_id = session_id.to_string();
    let output_run_id = runtime.run_id.clone();
    thread::spawn(move || {
        let mut bytes = [0u8; 8192];
        let mut osc = OscScanner::default();
        loop {
            match reader.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    let state = output_app.state::<WorkspaceState>();
                    for (code, data) in osc.feed(&bytes[..size]) {
                        if code != 7501 { continue; }
                        let Some(terminal_state) = codex_osc_state(&data) else { continue; };
                        let mut inner = state.inner.lock().unwrap();
                        if !inner.runtimes.get(&output_id).is_some_and(|runtime| runtime.run_id == output_run_id) { continue; }
                        if let Some(session) = inner.sessions.iter_mut().find(|session| session.id == output_id) {
                            if apply_terminal_state(session, terminal_state) { emit_session(&output_app, session); }
                        }
                    }
                    let channel = state.output_channel.lock().unwrap().clone();
                    if let Some(channel) = channel {
                        let _ = channel.send(PtyOutput {
                            session_id: output_id.clone(), run_id: output_run_id.clone(), bytes: BASE64.encode(&bytes[..size]),
                        });
                    }
                }
            }
        }
    });
    let monitor_app = app.clone();
    let monitor_id = session_id.to_string();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(250));
        let completed_turn = runtime.transcript.lock().unwrap().as_mut().and_then(TranscriptWatch::completed_turn);
        if let Some(turn_id) = completed_turn {
            let state = monitor_app.state::<WorkspaceState>();
            let mut inner = state.inner.lock().unwrap();
            if inner.runtimes.get(&monitor_id).is_some_and(|current| current.run_id == runtime.run_id) {
                if let Some(session) = inner.sessions.iter_mut().find(|session| session.id == monitor_id) {
                    let event = HookMessage {
                        token: String::new(), session_id: monitor_id.clone(), run_id: runtime.run_id.clone(),
                        kind: "complete".into(), turn_id, prompt: None, transcript_path: None,
                    };
                    if apply_event(session, &event) { emit_session(&monitor_app, session); }
                }
            }
        }
        #[cfg(unix)]
        let foreground_pid = runtime.master.lock().unwrap().process_group_leader().map(|pid| pid as u32);
        #[cfg(not(unix))]
        let foreground_pid: Option<u32> = None;
        let (exited, shell_pid) = {
            let mut child = runtime.child.lock().unwrap();
            let pid = child.process_id();
            (child.try_wait(), pid)
        };
        match exited {
            Ok(Some(status)) => {
                let state = monitor_app.state::<WorkspaceState>();
                let mut inner = state.inner.lock().unwrap();
                inner.runtimes.remove(&monitor_id);
                if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == monitor_id) {
                    let was_stopping = matches!(session.status, SessionStatus::Stopping);
                    session.status = if status.success() || was_stopping { SessionStatus::Exited } else { SessionStatus::Error };
                    session.process_name = None;
                    session.reported_app = None;
                    if !status.success() && !was_stopping { session.error = Some(format!("{} 已退出，状态码 {}", if launch_kind == LaunchKind::Shell { "终端" } else { "Codex" }, status.exit_code())); }
                    emit_session(&monitor_app, session);
                }
                break;
            }
            Ok(None) => {
                let actual = foreground_pid.filter(|pid| Some(*pid) != shell_pid).and_then(process_name_for_pid)
                    .or_else(|| shell_pid.and_then(process_name_for_pid))
                    .or_else(|| default_process_name(launch_kind));
                let state = monitor_app.state::<WorkspaceState>();
                let mut inner = state.inner.lock().unwrap();
                if !inner.runtimes.get(&monitor_id).is_some_and(|current| current.run_id == runtime.run_id) { break; }
                if let Some(session) = inner.sessions.iter_mut().find(|session| session.id == monitor_id) {
                    let returned = foreground_pid == shell_pid && actual.as_deref().is_some_and(|name| returned_to_shell(session, name));
                    let name = if launch_kind == LaunchKind::Codex { Some("codex".into()) } else { session.reported_app.clone().or(actual) };
                    if returned || session.process_name != name {
                        session.process_name = name;
                        emit_session(&monitor_app, session);
                    }
                }
                continue;
            }
            Err(error) => {
                let state = monitor_app.state::<WorkspaceState>();
                let mut inner = state.inner.lock().unwrap();
                if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == monitor_id) {
                    session.status = SessionStatus::Unknown;
                    session.error = Some(error.to_string());
                    emit_session(&monitor_app, session);
                }
                break;
            }
        }
    });
    Ok(())
}

fn stop_runtime(runtime: &Runtime) -> Result<(), String> {
    let mut child = runtime.child.lock().unwrap();
    if child.try_wait().map_err(|e| e.to_string())?.is_some() { return Ok(()); }
    #[cfg(unix)] if let Some(pid) = child.process_id() {
        // Signal the PTY process group only when its child is the session leader.
        unsafe {
            if libc::getpgid(pid as i32) == pid as i32 { libc::kill(-(pid as i32), libc::SIGTERM); }
        }
    }
    let graceful_deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < graceful_deadline {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() { return Ok(()); }
        thread::sleep(Duration::from_millis(15));
    }
    if let Err(error) = child.kill() {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() { return Ok(()); }
        return Err(error.to_string());
    }
    let forced_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < forced_deadline {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() { return Ok(()); }
        thread::sleep(Duration::from_millis(15));
    }
    Err("进程停止超时".into())
}

fn apply_event(session: &mut AgentSession, message: &HookMessage) -> bool {
    if !message.turn_id.is_empty() && session.finished_turn_ids.iter().any(|id| id == &message.turn_id) { return false; }
    match message.kind.as_str() {
        "submit" => {
            if message.turn_id.is_empty() || matches!(session.status, SessionStatus::Stopping | SessionStatus::Exited | SessionStatus::Error) { return false; }
            session.turn_id = Some(message.turn_id.clone());
            session.status = SessionStatus::Running;
            session.unread = false;
        }
        "waiting" if matches!(session.status, SessionStatus::Running)
            && session.turn_id.as_deref() == Some(message.turn_id.as_str()) => session.status = SessionStatus::Waiting,
        "complete" if matches!(session.status, SessionStatus::Running | SessionStatus::Waiting | SessionStatus::Ready | SessionStatus::Unknown)
            && !message.turn_id.is_empty() && session.turn_id.as_deref() == Some(message.turn_id.as_str()) => {
            session.status = SessionStatus::Done;
            session.unread = true;
            remember_finished(session, &message.turn_id);
        }
        "interrupted" if matches!(session.status, SessionStatus::Running | SessionStatus::Waiting)
            && session.turn_id.as_deref() == Some(message.turn_id.as_str()) => {
            session.status = SessionStatus::Ready;
            session.unread = false;
            remember_finished(session, &message.turn_id);
        }
        _ => return false,
    }
    true
}

fn apply_terminal_state(session: &mut AgentSession, state: &str) -> bool {
    if session.launch_kind != LaunchKind::Shell { return false; }
    let mut changed = false;
    if state == "working" || state == "idle" {
        if session.reported_app.as_deref() != Some("codex") || session.process_name.as_deref() != Some("codex") {
            session.reported_app = Some("codex".into());
            session.process_name = Some("codex".into());
            changed = true;
        }
    } else if state == "clear" {
        session.reported_app = None;
    }
    match state {
        "working" if session.status == SessionStatus::Ready || session.status == SessionStatus::Done && session.turn_id.is_none() => {
            session.status = SessionStatus::Running;
            session.unread = false;
        }
        "idle" if session.status == SessionStatus::Running => session.status = SessionStatus::Ready,
        "clear" if matches!(session.status, SessionStatus::Running | SessionStatus::Waiting) => {
            session.status = SessionStatus::Ready;
        }
        _ => return changed,
    }
    true
}

fn remember_finished(session: &mut AgentSession, turn_id: &str) {
    if turn_id.is_empty() { return; }
    session.finished_turn_ids.push_back(turn_id.to_string());
    if session.finished_turn_ids.len() > 32 { session.finished_turn_ids.pop_front(); }
}

fn apply_hook(app: &AppHandle, message: HookMessage) {
    let state = app.state::<WorkspaceState>();
    if message.token != state.hook_token { return; }
    let mut inner = state.inner.lock().unwrap();
    let Some(runtime) = inner.runtimes.get(&message.session_id).filter(|runtime| runtime.run_id == message.run_id).cloned() else { return; };
    if message.kind == "title" {
        let Some(title) = message.prompt.as_deref().and_then(title_from_prompt) else { return };
        if !inner.sessions.iter().any(|session| session.id == message.session_id && session.auto_rename_pending && session.launch_kind == LaunchKind::Shell) { return; }
        if let Ok(Some(session)) = persist_change(&state, &mut inner, |inner| {
            let Some(session) = inner.sessions.iter_mut().find(|s| s.id == message.session_id) else { return Ok(None) };
            Ok(apply_title(session, title).then(|| session.clone()))
        }) {
            emit_session(app, &session);
        }
        return;
    }
    let Some(session) = inner.sessions.iter_mut().find(|s| s.id == message.session_id) else { return };
    if apply_event(session, &message) {
        if message.kind == "submit" {
            let mut watch = runtime.transcript.lock().unwrap();
            if let Some(path) = message.transcript_path.as_ref().map(PathBuf::from) {
                match watch.as_mut() {
                    Some(current) if current.path == path => current.turn_id = Some(message.turn_id.clone()),
                    _ => *watch = Some(TranscriptWatch::new(path, message.turn_id.clone())),
                }
            } else { *watch = None; }
        } else if matches!(message.kind.as_str(), "complete" | "interrupted") {
            if let Some(watch) = runtime.transcript.lock().unwrap().as_mut() { watch.turn_id = None; }
        }
        emit_session(app, session);
    }
}

#[tauri::command]
fn bootstrap(state: State<'_, WorkspaceState>) -> Bootstrap {
    let inner = state.inner.lock().unwrap();
    Bootstrap { projects: inner.projects.clone(), sessions: inner.sessions.clone(), startup_error: inner.startup_error.clone() }
}

#[tauri::command]
fn subscribe_pty_output(state: State<'_, WorkspaceState>, on_output: Channel<PtyOutput>) {
    *state.output_channel.lock().unwrap() = Some(on_output);
}

#[tauri::command]
fn create_project(state: State<'_, WorkspaceState>, directory: String, name: String) -> Result<Project, String> {
    let path = fs::canonicalize(&directory).map_err(|e| format!("无法访问项目目录：{e}"))?;
    if !path.is_dir() { return Err("所选路径不是文件夹".into()); }
    let name = name.trim();
    if name.is_empty() { return Err("请输入项目名称".into()); }
    let mut inner = state.inner.lock().unwrap();
    writable(&inner)?;
    if let Some(index) = inner.projects.iter().position(|p| Path::new(&p.directory) == path) {
        return persist_change(&state, &mut inner, |inner| {
            inner.projects[index].expanded = true;
            Ok(inner.projects[index].clone())
        });
    }
    let project = Project { id: Uuid::new_v4().to_string(), name: name.into(), directory: path.to_string_lossy().into_owned(), expanded: true };
    persist_change(&state, &mut inner, |inner| {
        inner.projects.push(project.clone());
        Ok(project)
    })
}

#[tauri::command]
fn set_project_expanded(state: State<'_, WorkspaceState>, id: String, expanded: bool) -> Result<(), String> {
    let mut inner = state.inner.lock().unwrap(); writable(&inner)?;
    persist_change(&state, &mut inner, |inner| {
        let project = inner.projects.iter_mut().find(|p| p.id == id).ok_or("项目不存在")?;
        project.expanded = expanded;
        Ok(())
    })
}

#[tauri::command]
fn create_session(state: State<'_, WorkspaceState>, app: AppHandle, project_id: String, name: String) -> Result<AgentSession, String> {
    let name = name.trim();
    if name.is_empty() { return Err("请输入会话名称".into()); }
    let session = {
        let mut inner = state.inner.lock().unwrap(); writable(&inner)?;
        if !inner.projects.iter().any(|p| p.id == project_id) { return Err("项目不存在".into()); }
        if inner.deleting.contains(&project_id) { return Err("项目正在删除".into()); }
        let session = AgentSession { id: Uuid::new_v4().to_string(), project_id, name: name.into(), auto_rename_pending: true, name_locked: false, launch_kind: LaunchKind::Shell, status: SessionStatus::Starting, unread: false, last_activated_at: now_ms(), turn_id: None, run_id: None, process_name: None, reported_app: None, finished_turn_ids: VecDeque::new(), error: None };
        persist_change(&state, &mut inner, |inner| {
            inner.sessions.push(session.clone());
            Ok(session)
        })?
    };
    if let Err(error) = spawn_session(&state, &app, &session.id) {
        // The failed row stays visible so the user can retry after fixing the cause.
        let _ = app.emit("session-start-error", (&session.id, &error));
    }
    let inner = state.inner.lock().unwrap();
    inner.sessions.iter().find(|s| s.id == session.id).cloned().ok_or("会话已关闭".into())
}

#[tauri::command]
fn restart_session(state: State<'_, WorkspaceState>, app: AppHandle, id: String) -> Result<AgentSession, String> {
    spawn_session(&state, &app, &id)?;
    let inner = state.inner.lock().unwrap();
    Ok(inner.sessions.iter().find(|s| s.id == id).ok_or("会话不存在")?.clone())
}

#[tauri::command]
fn rename_session(state: State<'_, WorkspaceState>, id: String, name: String) -> Result<AgentSession, String> {
    let name = name.trim();
    if name.is_empty() { return Err("请输入会话名称".into()); }
    let mut inner = state.inner.lock().unwrap(); writable(&inner)?;
    persist_change(&state, &mut inner, |inner| {
        let session = inner.sessions.iter_mut().find(|s| s.id == id).ok_or("会话不存在")?;
        session.name = name.into();
        session.auto_rename_pending = false;
        session.name_locked = true;
        Ok(session.clone())
    })
}

#[tauri::command]
fn auto_rename_from_terminal(state: State<'_, WorkspaceState>, app: AppHandle, id: String, run_id: String, title: String) -> Result<(), String> {
    let Some(title) = title_from_prompt(&title) else { return Ok(()) };
    let mut inner = state.inner.lock().unwrap();
    if !inner.runtimes.get(&id).is_some_and(|runtime| runtime.run_id == run_id) { return Ok(()); }
    if !inner.sessions.iter().any(|session| session.id == id && session.auto_rename_pending && session.launch_kind == LaunchKind::Shell) { return Ok(()); }
    if let Some(session) = persist_change(&state, &mut inner, |inner| {
        let session = inner.sessions.iter_mut().find(|session| session.id == id).ok_or("会话不存在")?;
        Ok(apply_title(session, title).then(|| session.clone()))
    })? { emit_session(&app, &session); }
    Ok(())
}

#[tauri::command]
fn mark_seen(state: State<'_, WorkspaceState>, id: String) -> Result<(), String> {
    let mut inner = state.inner.lock().unwrap();
    let session = inner.sessions.iter_mut().find(|s| s.id == id).ok_or("会话不存在")?;
    session.unread = false;
    session.last_activated_at = now_ms();
    Ok(())
}

#[tauri::command]
fn write_pty(state: State<'_, WorkspaceState>, app: AppHandle, id: String, data: String) -> Result<(), String> {
    let runtime = {
        let mut inner = state.inner.lock().unwrap();
        if inner.shutting_down { return Err("应用正在退出".into()); }
        let session_project = inner.sessions.iter().find(|s| s.id == id).ok_or("会话不存在")?.project_id.clone();
        if inner.deleting.contains(&session_project) { return Err("项目正在删除".into()); }
        let runtime = inner.runtimes.get(&id).cloned().ok_or("会话未运行")?;
        if data.contains('\r') || data.contains('\n') {
            if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == id) {
                if session.launch_kind == LaunchKind::Codex && matches!(session.status, SessionStatus::Ready | SessionStatus::Done) {
                    // An Enter key is not proof of task start; wait for a trusted CLI hook.
                    session.status = SessionStatus::Unknown;
                    session.unread = false;
                    session.turn_id = None;
                    emit_session(&app, session);
                }
            }
        }
        runtime
    };
    let result = runtime.writer.lock().unwrap().write_all(data.as_bytes()).map_err(|e| e.to_string());
    result
}

#[tauri::command]
fn resize_pty(state: State<'_, WorkspaceState>, id: String, rows: u16, cols: u16) -> Result<(), String> {
    if rows == 0 || cols == 0 { return Ok(()); }
    let runtime = state.inner.lock().unwrap().runtimes.get(&id).cloned().ok_or("会话未运行")?;
    let result = runtime.master.lock().unwrap().resize(PtySize {rows, cols, pixel_width: 0, pixel_height: 0}).map_err(|e| e.to_string());
    result
}

#[tauri::command]
fn delete_session(state: State<'_, WorkspaceState>, app: AppHandle, id: String) -> Result<(), String> {
    let (runtime, previous) = {
        let mut inner = state.inner.lock().unwrap(); writable(&inner)?;
        let index = inner.sessions.iter().position(|s| s.id == id).ok_or("会话不存在")?;
        if inner.deleting.contains(&inner.sessions[index].project_id) { return Err("项目正在删除".into()); }
        let previous = inner.sessions[index].status.clone();
        inner.sessions[index].status = SessionStatus::Stopping;
        emit_session(&app, &inner.sessions[index]);
        (inner.runtimes.get(&id).cloned(), previous)
    };
    if let Some(runtime) = runtime {
        if let Err(error) = stop_runtime(&runtime) {
            let mut inner = state.inner.lock().unwrap();
            if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == id) {
                session.status = previous;
                session.error = Some(error.clone());
                emit_session(&app, session);
            }
            return Err(error);
        }
    }
    let mut inner = state.inner.lock().unwrap();
    let previous_sessions = inner.sessions.clone();
    inner.sessions.retain(|s| s.id != id);
    if let Err(error) = save(&state, &inner) {
        inner.sessions = previous_sessions;
        if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == id) {
            session.status = SessionStatus::Exited;
            session.error = Some(format!("进程已停止，但会话记录保存失败：{error}"));
            emit_session(&app, session);
        }
        return Err(error);
    }
    inner.runtimes.remove(&id);
    Ok(())
}

#[tauri::command]
fn delete_project(state: State<'_, WorkspaceState>, app: AppHandle, id: String) -> Result<(), String> {
    let runtimes = {
        let mut inner = state.inner.lock().unwrap(); writable(&inner)?;
        if !inner.projects.iter().any(|p| p.id == id) { return Err("项目不存在".into()); }
        if inner.deleting.contains(&id) { return Err("项目正在删除".into()); }
        let owned: Vec<_> = inner.sessions.iter().filter(|s| s.project_id == id).map(|s| s.id.clone()).collect();
        if inner.sessions.iter().any(|s| s.project_id == id && session_busy(s)) {
            return Err("项目中仍有运行中的终端或未结束任务".into());
        }
        inner.deleting.insert(id.clone());
        owned.iter().filter_map(|session_id| inner.runtimes.get(session_id).cloned()).collect::<Vec<_>>()
    };
    for runtime in runtimes {
        if let Err(error) = stop_runtime(&runtime) {
            state.inner.lock().unwrap().deleting.remove(&id);
            return Err(format!("终端关闭失败，项目未删除：{error}"));
        }
    }
    let mut inner = state.inner.lock().unwrap();
    let projects = inner.projects.clone();
    let sessions = inner.sessions.clone();
    let removed: Vec<_> = inner.sessions.iter().filter(|s| s.project_id == id).map(|s| s.id.clone()).collect();
    inner.sessions.retain(|s| s.project_id != id);
    inner.projects.retain(|p| p.id != id);
    if let Err(error) = save(&state, &inner) {
        inner.projects = projects;
        inner.sessions = sessions;
        inner.deleting.remove(&id);
        return Err(format!("项目记录保存失败，项目仍保留：{error}"));
    }
    for session_id in removed { inner.runtimes.remove(&session_id); }
    inner.deleting.remove(&id);
    let _ = app.emit("project-deleted", id);
    Ok(())
}

#[tauri::command]
fn shutdown(state: State<'_, WorkspaceState>, app: AppHandle, confirmed: bool) -> Result<(), String> {
    let runtimes = {
        let mut inner = state.inner.lock().unwrap();
        if inner.shutting_down { return Err("应用正在退出".into()); }
        if !confirmed && inner.sessions.iter().any(|s| session_busy(s) && inner.runtimes.contains_key(&s.id)) {
            return Err("仍有会话在运行。退出将结束其中的程序。".into());
        }
        inner.shutting_down = true;
        inner.runtimes.values().cloned().collect::<Vec<_>>()
    };
    let results = thread::scope(|scope| {
        let handles: Vec<_> = runtimes.iter().map(|runtime| scope.spawn(|| stop_runtime(runtime))).collect();
        handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err("停止线程异常".into()))).collect::<Vec<_>>()
    });
    if let Some(error) = results.into_iter().find_map(Result::err) {
        state.inner.lock().unwrap().shutting_down = false;
        return Err(format!("有会话未能退出：{error}"));
    }
    app.exit(0);
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_path = app.path().app_data_dir()?.join("workspace.json");
            let (projects, sessions, startup_error) = match load(&config_path) {
                Ok((projects, sessions)) => (projects, sessions, None),
                Err(error) => (Vec::new(), Vec::new(), Some(error)),
            };
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let hook_port = listener.local_addr()?.port();
            let hook_token = Uuid::new_v4().to_string();
            let handle = app.handle().clone();
            let state = WorkspaceState {
                inner: Mutex::new(Inner {projects, sessions, runtimes: HashMap::new(), deleting: HashSet::new(), startup_error, shutting_down: false}),
                output_channel: Mutex::new(None),
                config_path, hook_port, hook_token,
            };
            app.manage(state);
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut line = String::new();
                    let _ = stream.take(65536).read_to_string(&mut line);
                    if let Ok(message) = serde_json::from_str::<HookMessage>(&line) {
                        apply_hook(&handle, message);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![bootstrap, subscribe_pty_output, create_project, set_project_expanded,
            create_session, restart_session, rename_session, auto_rename_from_terminal,
            mark_seen, write_pty, resize_pty,
            delete_session, delete_project, shutdown])
        .run(tauri::generate_context!())
        .expect("unable to start Two");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> AgentSession {
        AgentSession {
            id: "session".into(), project_id: "project".into(), name: "Codex".into(),
            auto_rename_pending: false, name_locked: false,
            launch_kind: LaunchKind::Codex,
            status: SessionStatus::Ready, unread: false, last_activated_at: 0,
            turn_id: None, run_id: Some("run".into()), process_name: None, reported_app: None,
            finished_turn_ids: VecDeque::new(), error: None,
        }
    }

    #[test]
    fn old_sessions_still_launch_codex() {
        let mut saved = serde_json::to_value(session()).unwrap();
        saved.as_object_mut().unwrap().remove("launchKind");
        saved.as_object_mut().unwrap().remove("nameLocked");
        let restored: AgentSession = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.launch_kind, LaunchKind::Codex);
        assert!(!restored.name_locked);
        assert!(!session_busy(&restored));
        let shell = AgentSession { launch_kind: LaunchKind::Shell, ..restored };
        assert!(session_busy(&shell));
    }

    fn event(kind: &str, turn_id: &str) -> HookMessage {
        HookMessage {
            token: "token".into(), session_id: "session".into(), run_id: "run".into(),
            kind: kind.into(), turn_id: turn_id.into(), prompt: None, transcript_path: None,
        }
    }

    #[test]
    fn first_agent_prompt_sets_title_once() {
        assert_eq!(title_from_prompt("\n  修复登录错误。再补测试\n"), Some("修复登录错误。".into()));
        assert_eq!(title_from_prompt("Fix src/main.ts. Add tests"), Some("Fix src/main.ts.".into()));
        assert_eq!(title_from_prompt("\n \t \n"), None);
        let mut shell = AgentSession {
            name: "终端 1".into(), auto_rename_pending: true,
            launch_kind: LaunchKind::Shell, ..session()
        };
        assert!(apply_title(&mut shell, "修复登录错误。".into()));
        assert_eq!(shell.name, "修复登录错误。");
        assert!(!apply_title(&mut shell, "下一轮".into()));
        shell.auto_rename_pending = false;
        assert!(!apply_title(&mut shell, "手动命名后".into()));
        shell.name_locked = true;
        shell.auto_rename_pending = true;
        assert!(!apply_title(&mut shell, "新 Agent 也不覆盖手动名称".into()));
        let mut old_codex = session();
        old_codex.auto_rename_pending = true;
        assert!(!apply_title(&mut old_codex, "不改旧会话".into()));
    }

    #[test]
    fn codex_terminal_state_only_changes_live_shell_session() {
        let mut shell = AgentSession { launch_kind: LaunchKind::Shell, ..session() };
        assert!(apply_terminal_state(&mut shell, "working"));
        assert_eq!(shell.status, SessionStatus::Running);
        assert!(apply_terminal_state(&mut shell, "idle"));
        assert_eq!(shell.status, SessionStatus::Ready);
        assert!(apply_event(&mut shell, &event("submit", "turn-1")));
        assert!(apply_terminal_state(&mut shell, "idle"));
        assert_eq!(shell.turn_id.as_deref(), Some("turn-1"));
        assert!(apply_terminal_state(&mut shell, "working"));
        assert_eq!(shell.turn_id.as_deref(), Some("turn-1"));
        assert!(apply_terminal_state(&mut shell, "clear"));
        assert_eq!(shell.turn_id.as_deref(), Some("turn-1"));
        assert!(apply_event(&mut shell, &event("complete", "turn-1")));
        let _ = apply_terminal_state(&mut shell, "working");
        assert_eq!(shell.status, SessionStatus::Done);
        assert!(shell.unread);
        assert!(!apply_terminal_state(&mut shell, "idle"));
        assert_eq!(shell.turn_id.as_deref(), Some("turn-1"));
        assert!(!apply_terminal_state(&mut shell, "working"));
        assert!(apply_event(&mut shell, &event("submit", "turn-2")));
        assert_eq!(shell.status, SessionStatus::Running);
        assert!(apply_terminal_state(&mut shell, "idle"));
        assert!(!apply_terminal_state(&mut shell, "idle"));
        assert!(!apply_terminal_state(&mut shell, "unknown"));
        shell.status = SessionStatus::Exited;
        assert!(!apply_terminal_state(&mut shell, "working"));
        let mut old_codex = session();
        assert!(!apply_terminal_state(&mut old_codex, "working"));
    }

    #[test]
    fn returning_to_shell_rearms_only_automatic_names() {
        let mut shell = AgentSession { launch_kind: LaunchKind::Shell, name: "旧任务".into(), ..session() };
        shell.process_name = Some("codex".into());
        shell.reported_app = Some("codex".into());
        shell.status = SessionStatus::Running;
        shell.turn_id = Some("turn-1".into());
        assert!(returned_to_shell(&mut shell, "zsh"));
        assert_eq!(shell.status, SessionStatus::Ready);
        assert!(shell.auto_rename_pending);
        assert!(shell.turn_id.is_none());
        assert!(apply_title(&mut shell, "新任务".into()));
        shell.name_locked = true;
        shell.process_name = Some("codex".into());
        assert!(returned_to_shell(&mut shell, "zsh"));
        assert!(!shell.auto_rename_pending);
        assert!(!apply_title(&mut shell, "不能覆盖".into()));
    }

    #[test]
    fn osc_status_is_parsed_across_pty_reads() {
        let mut osc = OscScanner::default();
        assert!(osc.feed(b"text\x1b]7501;state=work").is_empty());
        let events = osc.feed(b"ing:app=codex\x1b\\other\x1b]0;title\x07");
        assert_eq!(events.len(), 2);
        assert_eq!(codex_osc_state(&events[0].1), Some("working"));
        assert_eq!(events[1], (0, "title".into()));
        assert_eq!(codex_osc_state("state=idle:app=other"), None);
    }

    #[test]
    fn transcript_completion_requires_matching_turn_and_complete_line() {
        let path = std::env::temp_dir().join(format!("two-transcript-{}.jsonl", Uuid::new_v4()));
        fs::write(&path, b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"old\"}}\n").unwrap();
        let mut watch = TranscriptWatch::new(path.clone(), "current".into());
        assert_eq!(watch.completed_turn(), None);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"current\"}}").unwrap();
        assert_eq!(watch.completed_turn(), None);
        file.write_all(b"\n").unwrap();
        assert_eq!(watch.completed_turn().as_deref(), Some("current"));
        assert_eq!(watch.completed_turn(), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn active_run_id_is_sent_to_frontend() {
        let value = serde_json::to_value(session()).unwrap();
        assert_eq!(value["runId"], "run");
    }

    #[test]
    fn turn_events_ignore_stale_and_duplicate_messages() {
        let mut session = session();
        assert!(!apply_event(&mut session, &event("complete", "first")));
        assert!(apply_event(&mut session, &event("submit", "first")));
        assert_eq!(session.status, SessionStatus::Running);
        assert!(apply_event(&mut session, &event("waiting", "first")));
        assert_eq!(session.status, SessionStatus::Waiting);
        assert!(!apply_event(&mut session, &event("complete", "other")));
        assert!(apply_event(&mut session, &event("complete", "first")));
        assert_eq!(session.status, SessionStatus::Done);
        assert!(session.unread);
        assert!(!apply_event(&mut session, &event("complete", "first")));
        assert!(apply_event(&mut session, &event("submit", "second")));
        assert!(!apply_event(&mut session, &event("waiting", "first")));
        assert!(!apply_event(&mut session, &event("complete", "first")));
        assert!(apply_event(&mut session, &event("interrupted", "second")));
        assert_eq!(session.status, SessionStatus::Ready);
        assert!(!apply_event(&mut session, &event("complete", "second")));
        session.turn_id = None;
        assert!(!apply_event(&mut session, &event("complete", "first")));
    }

    #[test]
    fn failed_save_rolls_back_in_memory_changes() {
        let root = std::env::temp_dir().join(format!("agent-workspace-test-{}", Uuid::new_v4()));
        let config_path = root.join("workspace.json");
        fs::create_dir_all(&config_path).unwrap();
        let state = WorkspaceState {
            inner: Mutex::new(Inner {
                projects: Vec::new(), sessions: Vec::new(), runtimes: HashMap::new(),
                deleting: HashSet::new(), startup_error: None, shutting_down: false,
            }),
            output_channel: Mutex::new(None),
            config_path, hook_port: 0, hook_token: String::new(),
        };
        let mut inner = state.inner.lock().unwrap();
        let result = persist_change(&state, &mut inner, |inner| {
            inner.projects.push(Project {
                id: "project".into(), name: "project".into(),
                directory: root.to_string_lossy().into_owned(), expanded: true,
            });
            Ok(())
        });
        assert!(result.is_err());
        assert!(inner.projects.is_empty());
        drop(inner);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn two_pty_processes_use_separate_working_directories() {
        let root = std::env::temp_dir().join(format!("agent-workspace-pty-{}", Uuid::new_v4()));
        let paths = [root.join("first"), root.join("second")];
        for path in &paths { fs::create_dir_all(path).unwrap(); }
        for path in &paths {
            let pair = native_pty_system().openpty(PtySize {
                rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
            }).unwrap();
            let mut reader = pair.master.try_clone_reader().unwrap();
            let mut command = CommandBuilder::new("/bin/pwd");
            command.cwd(path);
            let mut child = pair.slave.spawn_command(command).unwrap();
            drop(pair.slave);
            let mut output = [0u8; 1024];
            let size = reader.read(&mut output).unwrap();
            let printed = String::from_utf8_lossy(&output[..size]);
            assert!(child.wait().unwrap().success());
            assert_eq!(fs::canonicalize(printed.trim()).unwrap(), fs::canonicalize(path).unwrap());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_shebang_finds_node_with_gui_path() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("agent-workspace-path-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let node = root.join("node");
        let codex = root.join("codex");
        fs::write(&node, "#!/bin/sh\nprintf 'node-ok\\n'\n").unwrap();
        fs::write(&codex, "#!/usr/bin/env node\n").unwrap();
        for path in [&node, &codex] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let pair = native_pty_system().openpty(PtySize {
            rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut command = CommandBuilder::new(&codex);
        command.env("PATH", codex_child_path(&codex, Some(OsStr::new("/usr/bin:/bin"))).unwrap());
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut output = [0u8; 1024];
        let size = reader.read(&mut output).unwrap();
        let success = child.wait().unwrap().success();
        fs::remove_dir_all(root).unwrap();
        assert!(success);
        assert!(String::from_utf8_lossy(&output[..size]).contains("node-ok"));
    }

    #[cfg(unix)]
    #[test]
    fn default_shell_starts_in_project_directory() {
        let root = std::env::temp_dir().join(format!("agent-workspace-shell-{}", Uuid::new_v4()));
        let directory = root.join("project with spaces");
        fs::create_dir_all(&directory).unwrap();
        let pair = native_pty_system().openpty(PtySize {
            rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new_default_prog();
        command.cwd(&directory);
        command.env("SHELL", "/bin/sh");
        command.env("HOME", &root);
        command.env("PATH", "/usr/bin:/bin");
        command.env("AGENT_WORKSPACE_HOOK_HELPER", "/tmp/two-hook-helper");
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        writer.write_all(b"pwd\nprintf '%s\\n' \"$AGENT_WORKSPACE_HOOK_HELPER\"\nexit\n").unwrap();
        let expected = fs::canonicalize(&directory).unwrap().to_string_lossy().into_owned();
        let mut output = String::new();
        for _ in 0..64 {
            let mut bytes = [0u8; 1024];
            let size = reader.read(&mut bytes).unwrap();
            if size == 0 { break; }
            output.push_str(&String::from_utf8_lossy(&bytes[..size]));
            if output.contains(&expected) && output.contains("/tmp/two-hook-helper") { break; }
        }
        let success = child.wait().unwrap().success();
        fs::remove_dir_all(root).unwrap();
        assert!(success);
        assert!(output.contains(&expected), "shell output: {output}");
        assert!(output.contains("/tmp/two-hook-helper"), "shell output: {output}");
    }

    #[cfg(unix)]
    #[test]
    fn shell_codex_command_uses_local_server() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("two-codex-wrapper-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let codex = root.join("original-codex");
        fs::write(&codex, "#!/bin/sh\nprintf 'wrapper-args:%s\\n' \"$*\"\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        let wrapper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let path = codex_shell_path(&codex, &wrapper_dir, Some(OsStr::new("/usr/bin:/bin"))).unwrap();
        let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new_default_prog();
        command.cwd(&root);
        command.env("HOME", &root);
        command.env("SHELL", "/bin/sh");
        command.env("PATH", path);
        command.env("AGENT_WORKSPACE_CODEX_COMMAND", &codex);
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        writer.write_all(b"codex test-arg\nexit\n").unwrap();
        let mut output = String::new();
        for _ in 0..64 {
            let mut bytes = [0u8; 1024];
            let size = reader.read(&mut bytes).unwrap();
            if size == 0 { break; }
            output.push_str(&String::from_utf8_lossy(&bytes[..size]));
            if output.contains("wrapper-args:--no-daemon test-arg") { break; }
        }
        let success = child.wait().unwrap().success();
        fs::remove_dir_all(root).unwrap();
        assert!(success);
        assert!(output.contains("wrapper-args:--no-daemon test-arg"), "shell output: {output}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn zsh_login_keeps_user_config_and_prioritizes_wrapper() {
        let root = std::env::temp_dir().join(format!("two-zsh-wrapper-{}", Uuid::new_v4()));
        let original_bin = root.join("original-bin");
        fs::create_dir_all(&original_bin).unwrap();
        fs::write(original_bin.join("codex"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(root.join(".zshrc"), "export TWO_ORIGINAL_RC=loaded\nexport PATH=\"$HOME/original-bin:$PATH\"\n").unwrap();
        let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let output = std::process::Command::new("/bin/zsh")
            .args(["-lic", "printf '%s\\n' \"$TWO_ORIGINAL_RC\"; command -v codex"])
            .env("HOME", &root)
            .env("PATH", format!("{}:/usr/bin:/bin", resources.display()))
            .env("ZDOTDIR", resources.join("zsh"))
            .env("AGENT_WORKSPACE_ZSH_ORIGINAL_DIR", &root)
            .env("AGENT_WORKSPACE_ZSH_PROXY_DIR", resources.join("zsh"))
            .env("AGENT_WORKSPACE_CODEX_WRAPPER_DIR", &resources)
            .output().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let lines = String::from_utf8_lossy(&output.stdout);
        assert!(lines.contains("loaded\n"), "{lines}");
        assert!(lines.contains(&format!("{}/codex", resources.display())), "{lines}");
    }

    #[cfg(unix)]
    #[test]
    fn interactive_shell_stops_promptly() {
        let pair = native_pty_system().openpty(PtySize {
            rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let writer = pair.master.take_writer().unwrap();
        let child = pair.slave.spawn_command(CommandBuilder::new_default_prog()).unwrap();
        drop(pair.slave);
        let runtime = Runtime {
            run_id: "test".into(), master: Mutex::new(pair.master),
            writer: Mutex::new(writer), child: Mutex::new(child), transcript: Mutex::new(None),
        };
        let started = Instant::now();
        stop_runtime(&runtime).unwrap();
        assert!(started.elapsed() < Duration::from_millis(800), "stop took {:?}", started.elapsed());
        assert!(runtime.child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn term_ignoring_process_is_forced_to_stop_promptly() {
        let pair = native_pty_system().openpty(PtySize {
            rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "trap '' TERM; exec /bin/sleep 30"]);
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let runtime = Runtime {
            run_id: "test".into(), master: Mutex::new(pair.master),
            writer: Mutex::new(writer), child: Mutex::new(child), transcript: Mutex::new(None),
        };
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        stop_runtime(&runtime).unwrap();
        assert!(started.elapsed() < Duration::from_millis(800), "stop took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn pty_backspace_edits_shell_input() {
        let pair = native_pty_system().openpty(PtySize {
            rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "IFS= read -r line; printf 'RESULT:%s\\n' \"$line\""]);
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        writer.write_all(b"AB\x7fC\n").unwrap();
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(output.contains("RESULT:AC"), "backspace did not edit shell input: {output:?}");
    }
}
