# Two

轻量、跨平台的 Agent 终端工作台。按“项目目录 → 终端会话”组织多个终端，在项目目录打开终端后自行运行 `codex`、`agy` 等命令；切换时终端继续运行。

桌面工程已经实现，macOS arm64 和 x64 的 Release `.app` 已成功构建。为 PTY 子进程设置 `TERM=xterm-256color` 后，用户已在 arm64 真实窗口确认退格显示错位修复。新终端、项目选择和删除流程尚未完成端到端验收；macOS Intel、Windows 与 Linux 尚未运行。当前验证范围见 [ROADMAP.md](ROADMAP.md)。

## 运行

开发与构建需要 Node.js、npm、Rust 工具链及 Tauri 2 对应平台前置依赖。使用时按需安装并登录所选 Agent CLI。新会话使用系统登录终端，沿用用户的 shell 初始化配置；本机在精简环境下已确认该终端能找到 `codex` 和 `agy`。旧 Codex 直启会话仍可重启，并会为其子进程补入 CLI 所在目录以寻找 `node`。启动会话前会检查项目目录。

```sh
npm ci
npm run tauri -- dev
```

安装 GNU Make 和对应平台的 Tauri 前置依赖后，构建当前主机的 Release 应用：

```sh
make app
```

也可指定目标。Windows 和 Linux 目标须在对应系统上执行；macOS 两种架构均须在 macOS 上构建，并预先安装相应的 Rust target（例如 `rustup target add x86_64-apple-darwin`）。`make app` 在 macOS 上按主机架构选择目标，在 Windows／Linux 上构建 x64 产物。

| 命令 | 产物 |
| --- | --- |
| `make mac-arm64` | `src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Two.app` |
| `make mac-amd64` | `src-tauri/target/x86_64-apple-darwin/release/bundle/macos/Two.app` |
| `make windows` | `src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe` |
| `make linux` | `src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/*.deb` |

本机使用项目忽略目录 `.tmp/` 下的 Rust 工具链完成构建，运行应用不依赖该目录。项目目录、内部包名和应用数据标识继续使用 `agent-workspace`，以便保留现有配置。当前产物未签名、公证或做干净环境安装验收；macOS x64 已在 arm64 主机构建，但未在 Intel 机器运行。Windows 和 Linux 的打包与运行仍待对应环境验证。

前端单独运行 `npm run dev` 只适合查看布局；普通浏览器没有 Tauri 桥接，无法创建项目或终端。实际功能需要通过桌面应用运行。

## 使用边界

点击侧栏“项目”旁的加号，用系统目录选择器创建项目；点击项目行“···”旁的加号，立即在该项目目录打开新终端。默认名为“终端 N”；Codex／agy 的可信 Hook 能把 Agent 的首条任务改成会话名。退出 Agent 回到 shell 后，再启动 Agent 时会用新进程的首条任务更新自动名称。Codex Hook 未送达时，若 CLI 发出区别于项目名的终端标题，Two 会用该标题补命名；它可能是 Codex 生成的会话标题，不保证与任务首句逐字相同。手动重命名会锁定名称。旧版本未保存名称来源，升级前的手动名称在首次重新启动 Agent 后可能被自动更新；在新版中手动重命名一次即可锁定。单击会话切换，双击已退出或异常的会话重新启动。项目折叠或切换不会结束会话。右击任意会话行，或点击当前会话右上角的“···”，可选择删除会话；删除运行中的终端会结束其中的程序。删除项目只移除工作台记录和会话，绝不删除所选目录；有仍在运行的终端或未结束任务时不能直接删除项目。

新会话中手动运行的 Codex 通过 `UserPromptSubmit`／`Stop` Hook 提供任务开始与完成事件。Codex 的共享 app-server 不继承 Two 终端的会话变量，因此在 Two 中输入 `codex` 时，包内脚本会为该命令附加 `--no-daemon`，使 Hook 在当前终端环境执行；其他终端和全局 Codex 配置不变。macOS 的登录 zsh 会先加载用户原有启动配置，再把脚本目录放回 `PATH` 首位。独立 app-server 可能增加每个 Codex 实例的启动或内存开销，尚未测量。若 Codex 未被 Two 找到，或用户用绝对路径／自定义 alias 绕过脚本，任务 Hook 状态仍可能不可用。Hook 提供本地会话记录路径时，Two 在任务运行期间按文件偏移读取同一任务 ID 的结构化 `task_complete`，用于补足 `Stop` 更新延迟；不保存终端全文。该记录格式不是 Codex 的稳定公开接口，路径缺失或格式变化时仍以 Hook／OSC 为准，不据屏幕文字推断完成。Rust 后端持续读取每个 PTY 中 Codex 发出的终端状态序列，切到其他会话后也会处理 `working` 与 `idle`；OSC 只改变工作／空闲显示，不清除 Hook 的任务 ID。没有匹配的完成事件时，`idle` 移除动画并恢复“已启动”，不把空闲误称为任务完成。会话名前显示当前可识别的进程名，例如 `codex`；macOS／Linux 优先读取 PTY 前台进程组，Codex 自报名称可补足识别，其他情况显示 shell 名称。项目的状态汇总始终预留一行，计数出现或消失不会移动会话列表。agy 目前只接入任务命名，普通 shell 和其他 Agent 只显示终端进程状态。升级前创建的 Codex 直启会话保留原方式。本机 `~/.codex/hooks.json` 和 `~/.gemini/config/hooks.json` 只在 Two 会话环境变量存在时调用 Two helper；受控 TUI 回合确认 `UserPromptSubmit` 与 `Stop` 具有相同的 `turn_id`。用户已在 17:26 新包真实窗口确认当前会话及切走后的 Codex 完成状态自动更新。其他机器需安装并验证对应 CLI Hook，Windows／Linux 尚未验收。所选项目目录不是文件隔离：同一项目内的 Agent 可以访问相同磁盘文件。

关闭窗口会结束本应用管理的终端进程；有仍在运行的终端或未结束任务时先确认。重启只恢复项目和会话名称、启动方式、目录及展开状态，旧会话显示“已退出”，需要手动重启；终端全文不持久化。配置保存在系统应用数据目录的 `workspace.json`，读取失败时保留原文件并禁止修改记录。

## 检查

```sh
npm run build
node --test tests/pty-input.test.mjs
node --test tests/pty-resize.test.mjs
node --test tests/terminal-signals.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

输入与尺寸队列测试覆盖快速按键、连续退格、写入失败恢复、窗口缩放时取最新尺寸及会话关闭清理。Rust 测试覆盖事件乱序、旧会话兼容、保存失败回滚、PTY 工作目录和 shell 退格。构建通过与这些测试通过，不等于真实桌面终端交互或跨平台验收通过。

## 文档与设计

- [PRD.md](PRD.md)：功能、层级、删除保护、界面、技术约束和验收场景。
- [ROADMAP.md](ROADMAP.md)：实施阶段、当前状态与未决事项。
- [AGENTS.md](AGENTS.md)：本项目协作及实现边界。
- [design/concept.html](design/concept.html)：可交互原型。
- [design/projects.png](design/projects.png)：项目树结构参考截图。
- [src-tauri/icons/wordmark.svg](src-tauri/icons/wordmark.svg)：侧栏英文字标；同目录的 `icon.svg` 是应用图标源文件。

## 查看原型

直接使用 Chrome 或 Edge 打开 `design/concept.html`，保持同目录下的 `interactive.css` 和 `interactive.js` 一起存在。无需安装依赖或启动服务。

macOS 可执行：

```sh
open design/concept.html
```

点击“项目”旁的“＋”选择目录，再在项目下创建 Agent。原型会在约 8 秒后模拟完成，适合查看交互与外观。

原型不连接真实 CLI、不执行命令、不修改磁盘文件；刷新会重置。浏览器目录选择只提供名称和句柄，无法返回完整绝对路径。系统目录弹窗尚未实测，目录返回后的创建和去重流程已用模拟句柄验证。

## 技术栈与平台

Tauri 2 + TypeScript + HTML／CSS + xterm.js + Rust，PTY 使用 `portable-pty 0.9.0`，PTY 输出通过 Tauri Channel 保序传给界面。xterm 使用默认 DOM 渲染；PTY 子进程明确设置 `TERM=xterm-256color`，让 shell 按该终端支持的控制序列输出。本机 Codex 适配基线为 `codex-cli 0.167.2`。首批目标为 macOS 14+ arm64／x64、Windows 11 x64 和 Ubuntu 24.04 LTS x64；两种 macOS 架构已完成构建，macOS Intel 的运行、Windows／Linux 的构建和运行，以及各平台完整功能与性能验收仍未完成。
