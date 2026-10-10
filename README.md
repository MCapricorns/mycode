# mycode

本地优先的桌面编码代理。对话、工具和会话都在你的电脑上；你提供 API 密钥。支持 Windows 10/11 x64、Windows 11 ARM64、macOS Apple Silicon、Linux x86_64。

[许可证](LICENSE) · [文档](docs/README.md) · [发布](https://github.com/MCapricorns/mycode/releases) · [更新日志](CHANGELOG.md)

English notes are [below](#english).

## 功能

- **工作区。** 一个工作区挂多个文件夹。会话属于工作区；当前文件夹解析相对路径，其它文件夹用绝对路径。
- **对话。** 输入框旁切换模型和思考强度（档位来自 models.dev；每个会话记在 `ui.json`）。`/` 列出 `/new`、`/compact`、`/settings`、技能和 MCP。
- **上下文。** 大约用到窗口 85% 时自动压缩，也可以 `/compact`。最近约 2 万 token 原样保留，更早的内容写成摘要。账本不改写。
- **工具。** 进程内的 `read`、`write`、`edit`、`find`、`grep`。进程工具只注册一个：Windows 上按 PowerShell 7、Windows PowerShell 5.1、Git Bash 的顺序选择（工具名是 `powershell` 或 `bash`），都没有时才用 `cmd`，且不写入设置。Linux / macOS 是 `bash`。不选 WSL，也不把 bash 翻译成 PowerShell。`MYCODE_SHELL` 或设置里的 `tools.shell` 可以指定解释器。`mode` `script` 交给这个解释器；`mode` `program` 直接启动映像。
- **分组执行。** `run_code` 在进程内跑一段 Python（顶层 `await` / `return`）。不启动 Node.js，也不要求安装 Python。一次调用里组合多次读、搜、改和网页操作，只有 `return` 和 `print` 回到上下文。主代理和子代理用同一套。
- **子代理。** 内置 `scout`（只读研究）和 `artisan`（有界实现），也可以在 `agents/` 里加角色。委派工具是 `agent`。简单查找留在原地用 `run_code`；较宽的代码库或网页研究可以由模型选择交给 `scout`。并发默认 4；设置为 `0` 表示这个默认值。
- **网页。** `web_search` 返回短摘要（去重、缓存）。`fetch_content` 带 `goal` 时只回匹配摘录。后端是 Querit、AnySearch，或兼容的 https 端点，最多启用一个。密钥在设置页或 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`。
- **模型。** 内置 [models.dev](https://models.dev) 目录。自定义端点用 `anthropic-messages`、`openai-completions` 或 `openai-responses`。GitHub Copilot、OpenAI Codex 和 xAI 用设备码登录。
- **MCP 与技能。** 全局 MCP 在设置里。项目 `.mycode/mcp.json` 要先信任。`SKILL.md` 在 `.agents/skills/` 或 `~/.agents/skills/`。
- **界面。** 中英双语，深色主题，客户端标题栏。侧栏底部是当前构建版本。外观有语言、色板、字体和字号。

## 界面截图

主窗口、思考菜单和外观页的旧截图已去掉：它们仍显示旧的 `shell` 工具名和 v0.9.23。这三处需要按当前界面重拍。

## 下载

[Releases](https://github.com/MCapricorns/mycode/releases)

| 文件 | 平台 |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-pc-windows-msvc.zip` | Windows 11 ARM64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |
| `mycode-desktop-v<version>-aarch64-apple-darwin.dmg` | macOS Apple Silicon（`MYCode.app`） |
| `mycode-desktop-v<version>-x86_64-unknown-linux-gnu.zip` | Linux x86_64 |

每个压缩包旁有 `.sha256`。没有 Intel macOS 包。dmg 里的 app 只做 ad-hoc 签名，首次打开需右键「打开」，或运行 `xattr -d com.apple.quarantine /Applications/MYCode.app`。

## 从源码构建

需要 Rust stable。Windows 使用 MSVC。Linux x86_64 还需要 pkg-config、fontconfig、freetype、xkbcommon、X11、xcb、Wayland、OpenSSL 和 libclang 的开发包。

```text
cargo build --release -p mycode-desktop
```

## 数据

未设置 `MYCODE_HOME` 时，数据在 `~/.mycode`：`settings.json`（不含密钥）、`secrets.json`、`ui.json`、`catalog-cache.json`、`sessions.db`、`sessions/<id>/`、`agents/`、`scratch/`。解析路径不跟随符号链接走出这个根。上述 JSON 损坏时会留下 `.broken-*` 备份并恢复默认；会话账本不清空。

## 文档

模块划分和一条消息的路径见 [docs/README.md](docs/README.md)。

## 许可

[Apache-2.0](LICENSE)

---

## English

mycode is a local-first desktop coding agent for Windows, macOS, and Linux x86_64. The conversation, tool calls, and sessions stay on your machine. You bring the API keys.

### Features

- **Workspaces.** One workspace mounts several folders. The current folder resolves relative paths; the others are absolute.
- **Chat.** Switch model and thinking effort beside the composer. `/` lists commands, skills, and MCP.
- **Context.** Automatic compaction near 85% of the window, or `/compact`. The last ~20k tokens stay verbatim.
- **Tools.** In-process `read`, `write`, `edit`, `find`, and `grep`. One process tool is registered: on Windows, PowerShell 7, then Windows PowerShell 5.1, then Git Bash (`powershell` or `bash`); `cmd` only if none of those exist, and that fallback is not saved. Linux and macOS use `bash`. WSL is not selected. Commands are not translated. `MYCODE_SHELL` or `tools.shell` overrides the choice.
- **Grouping.** `run_code` runs a Python function body inside the binary. Node.js is not used, and Python does not need to be installed. Several reads, searches, edits, or page fetches share one call; only `return` and `print` come back. The main agent and every subagent get the same tool.
- **Subagents.** Built-in `scout` (read-only research) and `artisan` (a bounded change), or custom roles under `agents/`. The model chooses: a narrow lookup stays inline with `run_code`; broad repo or web research can go to `scout`. Default concurrency is 4 (`0` means that default).
- **Web.** `web_search` returns short snippets (deduped, cached). `fetch_content` with `goal` returns matching excerpts. Querit, AnySearch, or one compatible https endpoint.
- **Models.** Built-in [models.dev](https://models.dev) catalog. Custom endpoints use `anthropic-messages`, `openai-completions`, or `openai-responses`. Copilot, Codex, and xAI sign in with a device code.
- **MCP and skills.** Trust a project before reading `.mycode/mcp.json`. `SKILL.md` files under `.agents/skills/` or `~/.agents/skills/` become `/` skills.
- **UI.** English and Chinese, a dark theme, and a client-drawn title bar.

### Screenshots

Screenshots of the main window, thinking menu, and appearance page were removed. They still showed the old `shell` tool name and v0.9.23. Those three views need new captures of the current UI.

### Download

[Releases](https://github.com/MCapricorns/mycode/releases). Each zip and the macOS dmg has a `.sha256` file. There is no Intel macOS build. The app inside the dmg is ad-hoc signed; right-click → Open once, or `xattr -d com.apple.quarantine /Applications/MYCode.app`.

### Build

Rust stable. MSVC on Windows. Linux x86_64 also needs the pkg-config, fontconfig, freetype, xkbcommon, X11, xcb, Wayland, OpenSSL, and libclang development packages.

```text
cargo build --release -p mycode-desktop
```

### Data

`MYCODE_HOME` defaults to `~/.mycode` (`settings.json`, `secrets.json`, `ui.json`, `catalog-cache.json`, `sessions.db`, `sessions/<id>/`, `agents/`, `scratch/`). Path resolution does not follow symlinks out of that root. Damaged JSON files are backed up as `.broken-*` and reset; session ledgers are not cleared.

### Documentation

[docs/README.md](docs/README.md).

### License

[Apache-2.0](LICENSE)
