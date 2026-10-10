# mycode

本地优先的桌面编码代理。对话、工具调用和会话都在你自己的电脑上；你填 API 密钥，应用负责回合、工具和界面。发布包支持 Windows 10/11 x64、Windows 11 ARM64、macOS Apple Silicon 与 Linux x86_64。

[许可证](LICENSE) · [文档](docs/README.md) · [发布](https://github.com/MCapricorns/mycode/releases) · [更新日志](CHANGELOG.md) · [X（@M_Capricorns）](https://x.com/M_Capricorns)

English notes are [below](#english).

## 功能

- **命名工作区。** 一个工作区挂多个文件夹（例如前端和后端）。会话属于工作区；当前文件夹解析相对路径，其它文件夹用绝对路径。
- **对话。** 输入框旁切换模型和思考强度。思考选项来自 models.dev 为该模型公布的档位；没有公布时只有「默认」。每个会话各自记住模型和思考强度（存在 `ui.json`），不写进 `settings.json`。输入 `/` 可选命令（`/new`、`/compact`、`/settings`）、技能和 MCP。
- **上下文压缩。** 上下文用到模型窗口约 85% 时自动压缩，也可以手动 `/compact`。最近约 2 万 token 原样保留，更早的部分由当前模型写成摘要（摘要请求关闭思考），在对话里显示为摘要卡片（英文界面标 SUMMARY）。会话太短时提示没有可压缩的内容；摘要失败或过短时保留原历史。会话账本不改写。
- **缓存与用量。** `anthropic-messages` 请求带 `cache_control` 断点；OpenAI 兼容端点走自动前缀缓存。各家返回的缓存命中都会解析。输入框的上下文计量显示 `已用 / 窗口 · N% · N 缓存`。标题栏右侧按钮打开「详情」面板（可固定），显示模型、思考、上下文、输入、输出、缓存（总量与占比）和轮次。
- **改动。** 详情面板里是当前文件夹的 git 改动，点文件看 diff。列表最多 80 个路径，超出时提示「还有更多改动未列出。」
- **模型。** 内置 [models.dev](https://models.dev) 目录：启动时从 `models.dev/api.json` 更新（缓存 6 小时），取不到就用内置快照；「关于」页可以手动刷新。仓库每周由 CI 刷新内置快照。粘贴密钥即可用。自定义端点使用 `anthropic-messages`、`openai-completions` 或 `openai-responses`。GitHub Copilot、OpenAI Codex（ChatGPT）和 xAI（SuperGrok / X）在服务商页用设备码登录；xAI 与 Codex 的令牌过期后自动刷新，Copilot 用登录令牌换取短期 bearer。
- **按服务商调整请求。** 思考参数参照 opencode 按服务商和模型发送：GLM / 智谱走 Anthropic 接口时发 `thinking` 和 `output_config.effort`；MiniMax 用 adaptive thinking（chat 接口另加 `reasoning_split`）；DeepSeek 发 `reasoning_effort`；Kimi 优先用运行时目录（再退回内置快照）里公布的 `reasoning_effort` 档位；目录里没有该模型 id 时，K3 仍按 id 映射发送。输出上限顺序：设置 `maxOutput` → models.dev `limit.output` → 32000。
- **工具。** 进程内的 `read` / `write` / `edit` / `find` / `grep`，以及一个按平台选择的进程工具（`script` 走当前解释器，可以用 Python 改文件；`program` 直接启动钉住的程序映像）。模型只看见这一个工具：Windows 上是 `powershell`（优先 PowerShell 7 `pwsh`，其次 Windows PowerShell 5.1，再其次 Git Bash，工具名仍是 `bash`），Linux / macOS 上是 `bash`。PowerShell 和 Git Bash 都不在时，运行时才用 `cmd`（`cmd.exe`），且不写入设置。不自动选择 WSL。命令按该解释器的语法原样执行，不再把 bash 翻译成 PowerShell。PowerShell 用 UTF-16LE 的 `-EncodedCommand` 启动，并把管道设为 UTF-8；标准输出和标准错误里的 CLIXML 与 ANSI 颜色会收成可读文本。这个工具没有沙箱，也不逐条确认。这些改动都没有文件撤销。`grep` / `find` 不跟随符号链接，也不跨挂载点（overlay 按挂载 ID 判断，同一挂载不算跨越）。网页检索、向你提问、`agent` 和 MCP 走同一张注册表。环境变量 `MYCODE_SHELL` 或设置里的 `tools.shell` 可以指定解释器。
- **网页搜索。** `web_search` 和 `fetch_content` 使用 Querit 或 AnySearch，也可以加 Querit / AnySearch 兼容的 https 端点；最多启用一个。密钥在「网页搜索」设置页填写（存进 `secrets.json`），或用环境变量 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`。
- **MCP。** 全局 MCP 服务器在设置里配置。项目里的 `.mycode/mcp.json` 只在信任该项目后读取：点侧栏「添加目录」打开菜单，选「信任项目 MCP」或「不再信任项目 MCP」。信任列表存在 `ui.json` 的 `trusted_projects`（与侧栏、MCP 加载同一套路径比较）；打开文件夹不等于信任。
- **技能。** 项目 `.agents/skills/` 或 `~/.agents/skills/` 下的 `SKILL.md` 成为 `/` 技能，设置的「技能」页列出它们。
- **子代理。** 内置 scout、artisan，也可以在 `agents/` 里加 Markdown 角色。委派工具是 `agent`。并发默认 4 个；设置里的 `0` 表示这个默认值，不是零个子代理。子代理没有墙钟超时。
- **界面。** 中英双语，深色界面。全平台使用客户端绘制的标题栏（CSD）：系统标题栏隐藏，最小化、最大化和关闭在右侧。侧栏底部显示当前构建版本。没有打开工作区时，空工作台只有「打开目录」和最近目录；打开文件夹之后才出现「新建任务」。设置里的外观包括语言、色板（石板灰、海洋、森林、暮色、余烬、极光）、界面字体，以及界面字号 S / M / L / XL。对话里的代码块用 gpui-kit 的 Tree-sitter 高亮。发现新版本后下载并校验 `.sha256`，确认后再重启安装。读不了的 `settings.json`、`ui.json`、`secrets.json` 和模型目录缓存会先备份再恢复默认，会话账本不动。

工具行会写明目标：读了哪个文件、搜了什么、跑了哪条命令。密钥只在 `secrets.json`，设置页对已保存的钥匙显示一把锁。

## 界面

以下为界面截图。

[![主窗口：对话中的压缩摘要（SUMMARY）卡片，输入框上下文计量显示缓存 token，右侧打开详情面板](./docs/images/main-window/main-window.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/main-window/main-window.png)

主窗口：压缩后的摘要（SUMMARY）卡片、带缓存数的上下文计量，以及详情面板（上下文、输入、输出、缓存、轮次）。

[![输入框思考强度芯片展开的思考档位菜单](./docs/images/thinking-menu/thinking-menu.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/thinking-menu/thinking-menu.png)

思考菜单：档位来自 models.dev，选择只对当前会话生效。

[![设置 → 外观：语言、色板、界面字体、界面字号 S–XL](./docs/images/appearance-page/appearance-page.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/appearance-page/appearance-page.png)

外观页：语言、色板、界面字体、界面字号 S–XL。

## 下载

发布页：[Releases](https://github.com/MCapricorns/mycode/releases)

| 文件 | 平台 |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-pc-windows-msvc.zip` | Windows 11 ARM64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |
| `mycode-desktop-v<version>-aarch64-apple-darwin.dmg` | macOS Apple Silicon（`MYCode.app` 安装镜像） |
| `mycode-desktop-v<version>-x86_64-unknown-linux-gnu.zip` | Linux x86_64 |

每个 zip 和 dmg 旁有 `.sha256`。0.4.0 之后不再提供 Intel macOS 构建。dmg 内的 app 只做 ad-hoc 签名（仓库没有 Developer ID 证书），首次打开需右键「打开」或运行 `xattr -d com.apple.quarantine /Applications/MYCode.app`。

## 从源码构建

需要 Rust stable。Windows x64 与 Windows ARM64 都使用 MSVC。Linux x86_64 还需要 pkg-config、fontconfig、freetype、xkbcommon、X11、xcb、Wayland、OpenSSL 和 libclang 的开发包。

```text
cargo build --release -p mycode-desktop
```

发布配置打开 fat LTO，链接会比开发构建慢。开发配置只保留行号表，避免调试信息占满磁盘。

## 数据

`MYCODE_HOME` 未设置时，数据在 `~/.mycode`。

```text
~/.mycode/
├─ settings.json          提供商、MCP、网页搜索、外观。不含密钥
├─ secrets.json           API 密钥、登录令牌、搜索密钥
├─ ui.json                工作区、文件夹、会话归属、每个会话的模型与思考强度、受信任项目
├─ catalog-cache.json     模型目录缓存
├─ sessions.db            会话索引（SQLite：标题、分支头、JSONL 偏移）
├─ sessions/<id>/         `<branch>.jsonl`、载荷与压缩检查点
├─ agents/                自定义子代理角色（Markdown）
└─ scratch/               未绑定文件夹时的工作目录
```

更早版本留下的 `checkpoints/<id>/` 会在删除会话时清掉。新的回合不再写文件快照。

把整个目录换到另一台机器时，设置 `MYCODE_HOME` 指向它。应用不会跟随符号链接走出这个根。上述四个 JSON 损坏时会在同目录留下 `.broken-*` 备份并恢复默认；`sessions.db` 和 `sessions/` 不会因此被清空。

## 文档

模块怎么划分、一条消息怎么走完，见 [docs/README.md](docs/README.md)。

## 开发

本地可以这样检查格式和 clippy。持续集成不跑这道检查。

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p mycode-tools --lib native_image_launches --locked
```

pull request 和推送到 `main` 都会在 Windows x64、Windows ARM64、macOS Apple Silicon 和 Linux x86_64 上构建并打包四个平台的 release 二进制（zip 与 `.sha256`；macOS 另产出一个 dmg）。pull request 不创建标签、不发 GitHub Release、不改版本、不推 `ci/release-*`。推送到 `main` 并完成四个平台构建后，每次都会新建 GitHub Release：新标签 `v<version>`、四个平台的 zip（Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）、macOS 的 dmg 和对应 `.sha256`，发布说明先放 `CHANGELOG.md` 里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。`Cargo.toml` 里的版本如果已经有标签，发布计划会把补丁号加一，写回 `Cargo.toml`、`Cargo.lock` 和 `CHANGELOG.md`，并把这次提交推到临时引用 `ci/release-<version>-<run id>`。Windows x64、Windows ARM64、macOS Apple Silicon 和 Linux x86_64 都从这次提交构建。四个构建都成功之后，先用该提交创建标签并上传压缩包，再把版本写回 `main`：能快进就快进；若构建期间 `main` 有了新提交，就把版本提交重放到当前 `main` 上再推送（不强制推送；只自动处理 `Cargo.toml`、`Cargo.lock`、`CHANGELOG.md` 的冲突，计划中的版本号保留，构建期间写进 `## [Unreleased]` 的新说明也保留）。写回使用 `GITHUB_TOKEN`，`release-plan` 会跳过 `github-actions[bot]` 的推送，避免同一次发布把补丁号再加一次。临时引用会在成功或失败后删除。因此二进制里的版本与标签一致。写在 `CHANGELOG.md` 的 `## [Unreleased]` 下的内容会移到这个新版本下；该节为空时用一句固定说明。旧版本的压缩包已经齐，也不会跳过这次发布。手动把 `Cargo.toml` 改到一个还没有标签的版本时，`CHANGELOG.md` 里必须已经有该版本的条目。这些压缩包没有签名，仓库里没有可用的代码签名证书。

## 许可

[Apache-2.0](LICENSE)

---

## English

mycode is a local-first desktop coding agent for Windows, macOS, and Linux x86_64.
The conversation, the tool calls, and every session stay on your machine.
You bring the API keys. Follow [@M_Capricorns](https://x.com/M_Capricorns) on X.

### Features

- **Named workspaces.** One workspace mounts several folders. Sessions
  belong to the workspace. The current folder resolves relative paths;
  the others are absolute.
- **Chat.** Switch model and thinking effort beside the composer. The
  thinking options are the levels models.dev publishes for that model;
  with none published, the menu offers Default only. Each session keeps
  its own model and thinking level (in `ui.json`, not `settings.json`).
  Type `/` for commands (`/new`, `/compact`, `/settings`), skills, and MCP.
- **Compaction.** Context is compacted automatically at about 85% of the
  model window, or by hand with `/compact`. The last ~20k tokens stay
  verbatim; the older part is summarized by the current model (thinking
  off for that request) and shown in the chat as a SUMMARY card. A short
  session reports nothing to compact. A failed or too-short summary keeps
  the history as it was. The session ledger is never rewritten.
- **Caching and usage.** `anthropic-messages` requests carry
  `cache_control` breakpoints; OpenAI-compatible endpoints use automatic
  prefix caching. Cache hits are parsed for each provider. The composer
  context meter reads `used / window · N% · N cached`. The title-bar button on
  the right opens the Details panel (it can be pinned): model, thinking,
  Context, Input, Output, Cache (total and %), and Turns.
- **Changes.** The Details panel lists the current folder's git changes;
  click a file for its diff. The list stops at 80 paths and then says
  "More changes are not listed."
- **Models.** The built-in [models.dev](https://models.dev) catalog is
  updated from `models.dev/api.json` at startup (cached for 6 hours) and
  falls back to the bundled snapshot. About has a manual Refresh. A weekly
  CI job refreshes the bundled snapshot. Paste a key and go. Custom
  endpoints speak `anthropic-messages`, `openai-completions`, or
  `openai-responses`. GitHub Copilot, OpenAI Codex (ChatGPT), and xAI
  (SuperGrok / X) sign in with a device code from the provider page.
  Expired xAI and Codex tokens refresh on their own; Copilot exchanges its
  sign-in token for a short-lived bearer.
- **Per-provider requests.** Thinking fields follow opencode, per provider
  and model: GLM / Zhipu on the Anthropic route sends `thinking` and
  `output_config.effort`; MiniMax uses adaptive thinking (plus
  `reasoning_split` on chat completions); DeepSeek sends
  `reasoning_effort`; Kimi prefers live/cached catalog effort levels
  (then the bundled snapshot); if that model id is missing from the
  catalog, K3 still sends effort by id.
  Output limit order: setting `maxOutput` → models.dev `limit.output` →
  32000.
- **Tools.** In-process `read`, `write`, `edit`, `find`, and `grep`.
  One process tool is registered for the active interpreter: `mode`
  `program` launches a pinned executable with no shell, and `mode`
  `script` runs that interpreter, including a Python edit. The model sees
  only that tool. On Windows it is `powershell` (PowerShell 7 `pwsh`, then
  Windows PowerShell 5.1, then Git Bash, which is still named `bash`). On
  Linux and macOS it is `bash`. When neither PowerShell nor Git Bash
  exists, the runtime falls back to `cmd` (`cmd.exe`) and does not store
  that fallback. WSL is not selected automatically. Commands are not
  translated from bash into PowerShell. PowerShell is started with a
  UTF-16LE `-EncodedCommand` and UTF-8 pipes. CLIXML and ANSI color in
  stdout and stderr are turned into readable text. The tool is
  not sandboxed and does not ask per call. Those edits are not undone.
  `grep` and `find` do not follow symlinks or cross mounts (overlay is
  judged by mount ID, so one mount is not a crossing).
  Web search, questions, the `agent` tool, and MCP share one registry.
- **Web search.** `web_search` and `fetch_content` use Querit or
  AnySearch, or an https endpoint compatible with either; at most one is
  enabled. Enter the key on the Web search settings page (stored in
  `secrets.json`) or set `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`.
- **MCP.** Global MCP servers live in Settings. A project's
  `.mycode/mcp.json` is read only after you trust that project: the menu
  behind the sidebar's Add folder button has Trust project MCP and Revoke
  project MCP trust. Trusted paths are kept in `ui.json`
  (`trusted_projects`; same path compare as the sidebar and MCP load);
  opening a folder does not trust it.
- **Skills.** `SKILL.md` files under the project's `.agents/skills/` or
  `~/.agents/skills/` become `/` skills, listed on the Skills settings
  page.
- **Subagents.** Built-in scout and artisan roles,
  or custom Markdown roles under `agents/`. The delegation tool is
  `agent`. Concurrent sub-agents default to 4; a setting of `0` uses
  that default and does not mean zero agents. A sub-agent has no
  wall-clock timeout.
- **UI.** English and Chinese, a dark theme, and a client-drawn title
  bar (CSD) on every platform: the system title bar stays hidden, and
  minimize, zoom, and close sit on the right. The sidebar footer shows
  the running build version. With no workspace open, the empty desk offers Open folder
  and recent folders only; New task appears after a folder is open.
  Appearance has Language, then the palettes slate, ocean, forest, dusk,
  ember, and aurora, plus Interface font and sizes S–XL. Fenced code in
  the transcript is highlighted with gpui-kit's Tree-sitter grammars.
  Updates download and verify the `.sha256`, then wait for a restart
  confirmation. Unreadable `settings.json`, `ui.json`, `secrets.json`,
  and the model catalog cache are backed up and reset to defaults.
  Session ledgers are left alone.

A tool line names its target: which file was read, what was searched,
which command ran. Keys live only in `secrets.json`. Saved keys show as
a lock in Settings.

### Screenshots

Screenshots.

[![Main window: a conversation with a SUMMARY compaction card, the composer context meter showing cached tokens, and the Details panel open](./docs/images/main-window/main-window.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/main-window/main-window.png)

Main window: a SUMMARY card after compaction, the context meter with cached tokens, and the Details panel (Context, Input, Output, Cache, Turns).

[![Thinking level menu open on the composer chip](./docs/images/thinking-menu/thinking-menu.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/thinking-menu/thinking-menu.png)

Thinking menu: levels come from models.dev; a pick applies to the current session only.

[![Settings → Appearance: Language, palette, Interface font, and size S–XL](./docs/images/appearance-page/appearance-page.png)](https://raw.githubusercontent.com/MCapricorns/mycode/main/docs/images/appearance-page/appearance-page.png)

Appearance: Language, palette, Interface font, and S–XL.

### Download

[Releases](https://github.com/MCapricorns/mycode/releases)

| Asset | Platform |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-pc-windows-msvc.zip` | Windows 11 ARM64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |
| `mycode-desktop-v<version>-aarch64-apple-darwin.dmg` | macOS Apple Silicon (`MYCode.app` install image) |
| `mycode-desktop-v<version>-x86_64-unknown-linux-gnu.zip` | Linux x86_64 |

Each zip and dmg has a `.sha256` sidecar. Intel macOS builds stopped
after 0.4.0. The app inside the dmg is only ad-hoc signed (this
repository has no Developer ID certificate); right-click → Open once, or
run `xattr -d com.apple.quarantine /Applications/MYCode.app`.

### Build

Rust stable. MSVC on Windows x64 and Windows ARM64. Linux x86_64 also needs the pkg-config,
fontconfig, freetype, xkbcommon, X11, xcb, Wayland, OpenSSL, and libclang
development packages.

```text
cargo build --release -p mycode-desktop
```

Release builds use fat LTO. Dev builds keep line tables only, so
debuginfo stays small.

### Data

`MYCODE_HOME` defaults to `~/.mycode`.

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ catalog-cache.json
├─ sessions.db
├─ sessions/<id>/
├─ agents/
└─ scratch/
```

`settings.json` holds providers, MCP, web search, and appearance, never
keys. `secrets.json` holds API keys, sign-in tokens, and search keys.
`ui.json` holds workspaces, folders, session ownership, each session's
model and thinking level, and trusted projects. `sessions.db` is the
SQLite index. `sessions/<id>/` holds `<branch>.jsonl`,
payloads, and compaction checkpoints. `agents/` holds custom subagent
roles (Markdown). Older installs may still have
`checkpoints/<id>/`. Deleting a session removes that directory. New turns
do not write file snapshots.

Point `MYCODE_HOME` at a copied tree to move the app. Path resolution
does not follow symlinks out of that root. A damaged copy of the four
JSON files above is kept as a `.broken-*` backup and replaced with
defaults. `sessions.db` and `sessions/` are not cleared for that.

### Documentation

Module boundaries and the path of one user message:
[docs/README.md](docs/README.md).

### Development

Format and clippy can be checked locally. Continuous integration does not run that check.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p mycode-tools --lib native_image_launches --locked
```

Pull requests and pushes to `main` build and package the four platform
release binaries on Windows x64, Windows ARM64, macOS Apple Silicon, and
Linux x86_64 (zip and `.sha256`; macOS also produces a dmg). Pull requests do not publish, tag, bump the version, or push
a `ci/release-*` ref. A push to
`main` publishes a new GitHub Release after those four builds succeed: a new
`v<version>` tag, the four platform zips (Windows x64, Windows ARM64,
macOS Apple Silicon, and Linux x86_64), the macOS dmg, their `.sha256` sidecars, and release notes that start with the matching
`CHANGELOG.md` section, then list the pull requests and fixed issues since the previous tag. When
that version already has a tag, release-plan bumps the patch in
`Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` and pushes that commit
only to `ci/release-<version>-<run id>`. Windows x64, Windows ARM64,
macOS Apple Silicon, and Linux x86_64 are built from that commit. After those four builds
succeed, the tag and archives are published from that commit, then `main`
is updated: fast-forward when it still can, otherwise the version bump is
replayed onto current `main` and pushed without force. That push uses
`GITHUB_TOKEN`, and release-plan ignores `github-actions[bot]`, so the
bump cannot publish a second patch. A replay resolves
conflicts only in `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`, keeping
the planned version and any `## [Unreleased]` notes that landed on `main`
during the build. The temporary ref is deleted after success or failure.
The version compiled into the binaries matches the tag. Notes under
`## [Unreleased]` in `CHANGELOG.md` move into that version; an empty
section gets one fixed sentence. Archives already uploaded for an older
tag do not skip the release. A hand-bumped version that has no tag yet
still needs its own `CHANGELOG.md` section. The zip and dmg archives are
unsigned; this repository has no code-signing certificate.

### License

[Apache-2.0](LICENSE)
