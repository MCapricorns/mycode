# mycode

本地优先的桌面编码代理。对话、工具调用和会话都在你自己的电脑上；你填 API 密钥，应用负责回合、工具和界面。支持 Windows 10/11 x64 与 macOS Apple Silicon。

[许可证](LICENSE) · [文档](docs/README.md) · [发布](https://github.com/MCapricorns/mycode/releases) · [更新日志](CHANGELOG.md)

English notes are [below](#english).

## 功能

- **命名工作区。** 一个工作区挂多个文件夹（例如前端和后端）。会话属于工作区；当前文件夹解析相对路径，其它文件夹用绝对路径。
- **对话。** 输入框旁切换模型和思考强度。
- **改动。** 右侧是当前模型和该文件夹的 git 改动，点文件看 diff。
- **模型。** 内置 [models.dev](https://models.dev) 目录，粘贴密钥即可用。自定义端点使用 `anthropic-messages`、`openai-completions` 或 `openai-responses`。支持 Copilot、Codex、xAI 的设备码登录。
- **工具。** 进程内的 `read` / `write` / `edit` / `find` / `grep`，以及钉住程序映像的 `exec` 和走 shell 的 `shell`。网页检索、向你提问、子代理和 MCP 走同一张注册表。
- **子代理。** 内置 scout、artisan、steward、sentinel，也可以在 `agents/` 里加 Markdown 角色。
- **界面。** 中英双语，浅色 / 深色和几套配色。发现新版本后下载校验，确认后再重启安装。

工具行会写明目标：读了哪个文件、搜了什么、跑了哪条命令。密钥只在 `secrets.json`，设置页对已保存的钥匙显示一把锁。

## 下载

发布页：[Releases](https://github.com/MCapricorns/mycode/releases)

| 文件 | 平台 |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |

每个 zip 旁有 `.sha256`。0.4.0 之后不再提供 Intel macOS 构建。

## 从源码构建

需要 Rust stable。Windows 使用 MSVC。

```text
cargo build --release -p mycode-desktop
```

发布配置打开 fat LTO，链接会比开发构建慢。开发配置只保留行号表，避免调试信息占满磁盘。

## 数据

`MYCODE_HOME` 未设置时，数据在 `~/.mycode`。

```text
~/.mycode/
├─ settings.json          提供商、MCP、网页、外观。不含密钥
├─ secrets.json           API 密钥
├─ ui.json                工作区、文件夹、会话归属
├─ catalog-cache.json     模型目录缓存
├─ sessions/<id>/         账本、压缩检查点
├─ checkpoints/<id>/      改文件之前的快照
└─ scratch/               未绑定文件夹时的工作目录
```

把整个目录换到另一台机器时，设置 `MYCODE_HOME` 指向它。应用不会跟随符号链接走出这个根。

## 文档

模块怎么划分、一条消息怎么走完，见 [docs/README.md](docs/README.md)。

## 开发

质量门与 CI 相同，目前没有测试套件：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

推送到 `main` 的 pull request 在 Windows x64 和 macOS Apple Silicon 上跑这两步。质量门通过后，如果 `Cargo.toml` 里的版本还没有带齐两个平台压缩包的 GitHub Release，就会自动创建 `v<version>`，发布说明取 `CHANGELOG.md` 里该版本的条目，不从提交记录生成。版本没变时，推送不会重新打包。

## 许可

[Apache-2.0](LICENSE)

---

## English

mycode is a local-first desktop coding agent for Windows and macOS. The
conversation, the tool calls, and every session stay on your machine. You
bring the API keys.

### Features

- **Named workspaces.** One workspace mounts several folders. Sessions
  belong to the workspace. The current folder resolves relative paths;
  the others are absolute.
- **Chat.** Switch model and thinking effort beside the composer.
- **Changes.** The right-hand pane shows the session model and the
  current folder's git diff.
- **Models.** The built-in [models.dev](https://models.dev) catalog
  covers common providers. Custom endpoints speak
  `anthropic-messages`, `openai-completions`, or `openai-responses`.
  Copilot, Codex, and xAI can sign in with a device code.
- **Tools.** In-process `read`, `write`, `edit`, `find`, and `grep`.
  `exec` launches a pinned executable; `shell` uses your shell profile.
  Web search, questions, subagents, and MCP share one registry.
- **Subagents.** Built-in scout, artisan, steward, and sentinel roles,
  or custom Markdown roles under `agents/`.
- **UI.** English and Chinese, light and dark themes, several palettes.
  Updates download and verify, then wait for a restart confirmation.

A tool line names its target: which file was read, what was searched,
which command ran. Keys live only in `secrets.json`. Saved keys show as
a lock in Settings.

### Download

[Releases](https://github.com/MCapricorns/mycode/releases)

| Asset | Platform |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |

Each zip has a `.sha256` sidecar. Intel macOS builds stopped after 0.4.0.

### Build

Rust stable. MSVC on Windows.

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
├─ sessions/<id>/
├─ checkpoints/<id>/
└─ scratch/
```

Point `MYCODE_HOME` at a copied tree to move the app. Path resolution
does not follow symlinks out of that root.

### Documentation

Module boundaries and the path of one user message:
[docs/README.md](docs/README.md).

### Development

CI is rustfmt and clippy. There is no test suite.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Pull requests to `main` run those gates on Windows x64 and macOS Apple
Silicon. After the gates pass, a push to `main` publishes `v<version>`
when that GitHub Release is missing either platform archive. The body
is the matching `CHANGELOG.md` section, not generated commit notes. A
push that does not change the version does not rebuild the archives.

### License

[Apache-2.0](LICENSE)
