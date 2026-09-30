# mycode

A local-first desktop coding agent for Windows and macOS. The model
conversation, the tool calls, and every session live on your machine —
you bring your own API keys, the app does the rest.

## The window

- **Left — workspace.** One workspace can mount several folders (a
  frontend and a backend, say). Sessions hang off their folder; the
  current session's working directory resolves relative paths, other
  folders use absolute paths.
- **Center — chat.** Switch model and thinking effort next to the
  composer.
- **Right — context.** The model the session is using plus the current
  folder's git changes; click a file to see its diff.

Light and dark themes with several color schemes.

## Models

The built-in [models.dev](https://models.dev) catalog covers the common
providers — paste an API key and go. Custom endpoints speak one of three
protocols: `anthropic-messages`, `openai-completions`,
`openai-responses`.

## Tools

**Files**

- `read` — read a file with line numbers.
- `write` — create or replace a file.
- `edit` — apply exact string replacements; fuzzy matching recovers from
  small drift.

**Search**

- `find` — find files by glob, bounded by the search root.
- `grep` — ripgrep-style content search with include/exclude globs.

**Execution**

- `exec` — run a pinned executable with argument limits and output caps.
- `shell` — run a command through the user's shell profile.

**Web**

- `web_search` — bounded web search (Querit, AnySearch, or a custom
  backend).
- `fetch_content` — fetch page text before citing it.

**Coordination**

- `ask_user` — ask you a clarifying question mid-turn.
- `task` — delegate to subagents: the built-in scout, artisan, steward,
  and sentinel roles, or custom roles from `agents/`. Subagents can use
  connected MCP servers and skills.

**MCP**

Connect local commands over stdio or remote servers over Streamable
HTTP. Tools are used in two steps: `search_tool` fetches a schema, then
`use_tool` calls it.

Tool lines always say what happened — which file was read, what was
searched, which command ran.

## Download

[Releases](https://github.com/MCapricorns/mycode/releases)

- `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` — Windows 10/11 x64
- `mycode-desktop-v<version>-aarch64-apple-darwin.zip` — macOS Apple Silicon

Intel macOS builds were dropped after 0.4.0. See
[CHANGELOG.md](CHANGELOG.md).

## Build

Rust stable; MSVC toolchain on Windows.

```text
cargo build --release -p mycode-desktop
```

Checks (no test suites; CI runs the same fmt + clippy gates):

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Releases are manual: tag first, then run the `ci` workflow via
`workflow_dispatch` with the tag and hand-written notes.

## Data

`MYCODE_HOME` defaults to `~/.mycode`.

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ catalog-cache.json
├─ sessions/<id>/
├─ checkpoints/<id>/
└─ scratch/
```

API keys live only in `secrets.json`.

## License

Apache-2.0

---

# 中文说明

mycode 是本地优先的桌面编码代理，支持 Windows 与 macOS。对话、工具调用和全部会话都保存在你自己的电脑上；API 密钥自己填，其余交给应用。

## 窗口

- 左边是工作区：一个工作区可同时挂多个文件夹（比如前端和后端），会话挂在对应文件夹下；当前会话的工作目录决定相对路径，其它文件夹用绝对路径。
- 中间是对话：输入框旁可直接换模型和思考强度。
- 右边是上下文：当前会话使用的模型和该文件夹的 git 改动，点文件看 diff。

支持浅色/深色主题和几套配色。

## 模型

内置 models.dev 目录，填 API key 即可使用。自定义 endpoint 支持 `anthropic-messages`、`openai-completions`、`openai-responses` 三种协议。

## 工具

- **文件**：`read` 读文件、`write` 写文件、`edit` 做精确替换（模糊匹配兜底小偏移）。
- **搜索**：`find` 按 glob 找文件，`grep` 做内容搜索（支持 include/exclude）。
- **执行**：`exec` 运行校验过的可执行文件（带参数与输出上限），`shell` 走用户 shell 配置执行命令。
- **网络**：`web_search` 有界网页检索（Querit / AnySearch / 自定义后端），`fetch_content` 在引用前抓取网页正文。
- **协调**：`ask_user` 中途向你提问，`task` 委派子代理（内置 scout / artisan / steward / sentinel，或 `agents/` 里的自定义角色；子代理可用 MCP 与 skills）。
- **MCP**：本地命令走 stdio，远程走 Streamable HTTP；先用 `search_tool` 取 schema，再 `use_tool` 调用。

工具行会写明目标：读了哪个文件、搜了什么、跑了哪条命令。

## 下载与构建

到 [Releases](https://github.com/MCapricorns/mycode/releases) 下载对应平台的 zip（Windows x64 / macOS Apple Silicon；0.4.0 起不再提供 macOS Intel 构建）。

源码构建需要 Rust stable（Windows 用 MSVC）：`cargo build --release -p mycode-desktop`。检查只跑 fmt + clippy，与 CI 一致；发版在 GitHub Actions 手动触发。

## 数据

`MYCODE_HOME` 默认 `~/.mycode`，包含设置、密钥、UI 状态、模型目录缓存、会话、检查点与临时目录；API 密钥只保存在 `secrets.json`。

## 许可

Apache-2.0
