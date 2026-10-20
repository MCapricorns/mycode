<div align="center">

<img src="docs/images/banner.svg" alt="MYCode：本地编码代理" width="100%">

<p>
  <a href="https://github.com/MCapricorns/mycode/releases/latest"><img alt="最新版本" src="https://img.shields.io/github/v/release/MCapricorns/mycode?style=flat-square&label=%E7%89%88%E6%9C%AC&color=7c8cff"></a>
  <a href="https://github.com/MCapricorns/mycode/releases"><img alt="下载量" src="https://img.shields.io/github/downloads/MCapricorns/mycode/total?style=flat-square&label=%E4%B8%8B%E8%BD%BD"></a>
  <a href="https://github.com/MCapricorns/mycode/actions"><img alt="构建" src="https://img.shields.io/github/actions/workflow/status/MCapricorns/mycode/ci.yml?branch=main&style=flat-square&label=%E6%9E%84%E5%BB%BA"></a>
  <img alt="平台" src="https://img.shields.io/badge/%E5%B9%B3%E5%8F%B0-Windows%20%7C%20macOS-555?style=flat-square">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-dea584?style=flat-square&logo=rust">
  <a href="LICENSE"><img alt="许可" src="https://img.shields.io/github/license/MCapricorns/mycode?style=flat-square&label=%E8%AE%B8%E5%8F%AF"></a>
  <a href="https://x.com/M_Capricorns"><img alt="关注 @M_Capricorns" src="https://img.shields.io/badge/X-%40M__Capricorns-000?style=flat-square&logo=x"></a>
</p>

**对话、工具调用和会话记录都留在本机。模型请求只发往你自己配置的服务商。**

[下载](https://github.com/MCapricorns/mycode/releases/latest) · [快速开始](#快速开始) · [功能](#功能) · [数据](#数据与配置) · [架构](#架构) · [文档](docs/README.md) · [更新日志](CHANGELOG.md)

</div>

---

## 是什么

MYCode 是一个本地编码代理，有桌面版和终端版。两个前端共用同一套会话、模型、工具和设置实现，所以换前端不会换行为。

支持 Windows x64、Windows ARM64 和 macOS Apple Silicon，暂无 Intel macOS 和 Linux 包。

## 功能

| | |
| --- | --- |
| **工作目录** | 最多同时挂 8 个目录，可以拖进来（从旧版合并的列表可能暂时更多，新增时保持现有数量）。会话绑定到一个目录；切换目录不会把当前会话改绑。改动面板跟随文件系统事件读 `git status`，不按固定间隔轮询。 |
| **模型** | 模型列表来自 [models.dev](https://models.dev) 云端目录，缓存 24 小时。GitHub Copilot 和 OpenAI Codex 用设备码登录；其它服务商（含 xAI、自定义网关）粘贴 API key。 |
| **思考强度** | 只列模型实际公布的档位。每个会话单独记住自己的模型和强度，不影响别的会话。 |
| **子代理** | `scout` 只读，适合宽范围代码搜索、网页检索和查文档；`artisan` 在独立的 git worktree 里改代码，结束时回传 diff，不自动合回、不提交。每个回合最多同时跑 8 个，没有超时；取消回合、单独停止或删除对话都会结束它们。也可以在 `agents/` 里加自定义角色。 |
| **技能** | 从 `.agents` 和 `.agents/skills` 发现 `SKILL.md`，`/` 菜单可插入；设置里每个技能可以单独关闭。 |
| **MCP** | 支持 stdio 和 Streamable HTTP，最多 64 台服务器。工具不超过 4 个、且每个 schema 不超过 2048 字节的小服务直接注册成普通工具，其余走 `search_tool` / `use_tool`。 |
| **网页** | 内置 Querit、AnySearch、Exa 三种搜索/抓取后端，结果有数量和字节上限。 |
| **界面** | 中英双语、深色/浅色、四档字号，`Ctrl-K` / `Cmd-K` 打开命令面板。 |
| **终端版** | `mycode-cli` 和桌面版共用同一个驱动、会话和设置；`/settings` 里可以选模型、填密钥、开关 MCP、检查更新。 |
| **代理** | 跟随系统代理，或手动填写 `http` / `socks5` 地址。设置页的「测试」会请求 `google.com/generate_204` 并显示线路、状态码和耗时。 |

## 安装

从 [Releases](https://github.com/MCapricorns/mycode/releases/latest) 下载。每个安装包旁边都有对应的 `.sha256`。

| 平台 | 桌面版 | 终端版 |
| --- | --- | --- |
| Windows x64 | `mycode-desktop-v*-x86_64-pc-windows-msvc.zip` | `mycode-cli-v*-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `mycode-desktop-v*-aarch64-pc-windows-msvc.zip` | `mycode-cli-v*-aarch64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `mycode-desktop-v*-aarch64-apple-darwin.dmg` | `mycode-cli-v*-aarch64-apple-darwin.zip` |

- **Windows 桌面版**：解压后双击 `mycode-desktop.exe`。它会把自己复制到 `~/.mycode/bin/mycode-desktop.exe`，启动装好的那一份，删掉刚双击的旧 exe，并在桌面创建 `MYCode` 快捷方式。以后从快捷方式打开。
- **macOS 桌面版**：打开 dmg，把 `MYCode.app` 拖进「应用程序」。应用是 ad-hoc 签名，首次打开需要右键「打开」，或执行 `xattr -d com.apple.quarantine /Applications/MYCode.app`。
- **终端版**：首次运行会把自己复制到 `~/.mycode/bin`，并把该目录加进用户 `PATH`（Windows 写 `HKCU\Environment`，macOS 写 shell 配置）。看到安装提示后按 Enter 继续；新开一个终端就能直接运行 `mycode-cli`。

两个程序都内置自动更新，并且只下载与自己同名的包。桌面版在标题栏提示更新，确认后重启安装；终端版在 `/settings` 的「Update」一行检查和安装。Release 里的 macOS 桌面 zip 是给自动更新用的，手动安装请用 dmg。

`MYCODE_NO_INSTALL=1` 可以跳过自安装，适合直接从 `target/` 运行。

### 从源码构建

需要 Rust stable（Windows 用 MSVC 工具链）：

```sh
git clone https://github.com/MCapricorns/mycode.git
cd mycode
cargo build --release -p mycode-desktop -p mycode-cli
```

从源码运行发布版时建议设置 `MYCODE_NO_INSTALL=1`，免得它把自己装进 `~/.mycode/bin`。

## 快速开始

1. 打开设置 → 模型。登录 Copilot / Codex，或粘贴任意服务商的 API key。
2. 打开一个目录。
3. 在输入框里描述你要做的事。

| 操作 | 方式 |
| --- | --- |
| 斜杠命令 | 输入 `/`：`/new`、`/compact`、`/settings`、已启用的技能和 MCP |
| 引用文件 | 输入 `@` 搜索当前目录里的文件 |
| 切换模型 / 思考强度 | 输入框旁的两个菜单 |
| 命令面板 | `Ctrl-K`（Windows）/ `Cmd-K`（macOS），搜索会话、模型、命令和设置页 |
| 回合进行中继续输入 | `Enter` 把消息排到本轮之后（最多 8 条）；`Ctrl+Enter` 停止当前回合并立即发送；`Ctrl+G` 引导：不停止回合，模型在下一步之前读到这条消息。桌面版在 macOS 上用 `Cmd`。终端版里 `Esc` / `Ctrl+C` 停止；终端分辨不出 `Ctrl+Enter` 时（Windows 以外较常见）它按 `Enter` 或换行处理，可先 `Esc` 停止再发 |
| 快捷键一览 | 桌面版「设置 → 快捷键」，终端版 `/settings` 底部 |

## 数据与配置

未设置 `MYCODE_HOME` 时，数据在 `~/.mycode`：

```text
~/.mycode/
├─ settings.json        提供商、MCP、网页、外观、代理、子代理、技能开关（不含密钥）
├─ secrets.json         API 密钥与 OAuth 令牌
├─ ui.json              最近目录、会话归属、每个会话的模型与思考档位、窗口状态
├─ catalog-cache.json   models.dev 目录缓存
├─ sessions.db          会话索引
├─ sessions/            会话账本（JSONL、payloads、compaction.json）
├─ agents/              自定义子代理角色
├─ agent-worktrees/     子代理的临时 worktree
├─ mcp-results/         被截断的 MCP 结果全文
├─ scratch/             没有绑定目录时的工具工作目录
├─ bin/                 自安装的 mycode-cli / mycode-desktop
└─ update/              自动更新暂存
```

设置、密钥和界面状态是严格 JSON。写坏的文档会备份到旁边的 `*.broken-*`，能救的字段会保留，救不回来的回到默认值。会话账本用 `sessions.db` 作为提交权威，事件写在 JSONL 里；崩溃留下的半截写入会被截掉，索引和日志对不上则明确报损坏，不猜。

工具以当前用户权限运行。环境变量白名单只是过滤，不是沙箱。详见 [docs/tools.md](docs/tools.md) 和 [docs/config.md](docs/config.md)。

## 架构

<p align="center">
  <img src="docs/images/architecture.svg" alt="九个 crate 的分层架构" width="100%">
</p>

<p align="center">
  <img src="docs/images/message-flow.svg" alt="一条消息从输入到 ChatDone 的流程" width="100%">
</p>

工作区分成九个 crate，依赖只向下：`mycode-desktop` 和 `mycode-cli` 是前端，`mycode-frontend` 放共享的视图模型和 reducer，`mycode-app` 是应用核，下面是 `mycode-agent`、`mycode-providers`、`mycode-tools`，最底层是 `mycode-core` 和 `mycode-config`。前端只通过 `CoreBridge` 发命令、收事件；模型请求、工具执行、账本写入和设置读取都在核心里完成。

一条消息先写进账本，再装配回合；回合中工具结果随到随提交；回复结束发 `ChatDone`。取消只停止当前回合，已经收到的思考、正文和完成的工具调用会保留在账本里。

细节见 [docs/architecture.md](docs/architecture.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/README.md](docs/README.md) | 文档索引 |
| [docs/architecture.md](docs/architecture.md) | crate 分层、消息生命周期、家目录、运行时边界 |
| [docs/core.md](docs/core.md) | 共享消息、事件、工具规格和 Provider 端口 |
| [docs/config.md](docs/config.md) | 家目录、设置、密钥、界面状态和恢复规则 |
| [docs/providers.md](docs/providers.md) | 三种协议、流式传输、思考强度、缓存、目录和 OAuth |
| [docs/tools.md](docs/tools.md) | 内置工具、搜索边界、进程工具、宿主工具和 MCP |
| [docs/agent.md](docs/agent.md) | 回合循环、取消、工具分发和会话账本 |
| [docs/app.md](docs/app.md) | 桥、回合装配、压缩、子代理、MCP、OAuth、更新 |
| [docs/desktop.md](docs/desktop.md) | 桌面与终端界面、视图模型、和核心的节拍 |
| [docs/model-catalog-updates.md](docs/model-catalog-updates.md) | Codex / OpenDesign 模型表的维护流程 |

## 社区

- X（Twitter）：[@M_Capricorns](https://x.com/M_Capricorns)
- 问题与建议：[GitHub Issues](https://github.com/MCapricorns/mycode/issues)

## 许可

[Apache-2.0](LICENSE)

<div align="center">
<sub>用 Rust 和 GPUI 构建 · <a href="https://x.com/M_Capricorns">@M_Capricorns</a></sub>
</div>
