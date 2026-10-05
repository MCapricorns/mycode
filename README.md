# mycode

本地优先的桌面编码代理。对话、工具调用和会话都在你自己的电脑上；你填 API 密钥，应用负责回合、工具和界面。发布包支持 Windows 10/11 x64、Windows 11 ARM64 与 macOS Apple Silicon。

[许可证](LICENSE) · [文档](docs/README.md) · [发布](https://github.com/MCapricorns/mycode/releases) · [更新日志](CHANGELOG.md)

English notes are [below](#english).

## 功能

- **命名工作区。** 一个工作区挂多个文件夹（例如前端和后端）。会话属于工作区；当前文件夹解析相对路径，其它文件夹用绝对路径。
- **对话。** 输入框旁切换模型和思考强度。
- **改动。** 右侧是当前模型和该文件夹的 git 改动，点文件看 diff。
- **模型。** 内置 [models.dev](https://models.dev) 目录，粘贴密钥即可用。自定义端点使用 `anthropic-messages`、`openai-completions` 或 `openai-responses`。支持 Copilot、Codex、xAI 的设备码登录。
- **工具。** 进程内的 `read` / `write` / `edit` / `find` / `grep`，以及钉住程序映像的 `exec` 和走 shell 的 `shell`。网页检索、向你提问、子代理和 MCP 走同一张注册表。
- **子代理。** 内置 scout、artisan、steward、sentinel，也可以在 `agents/` 里加 Markdown 角色。
- **界面。** 中英双语，深色界面和几套配色。对话里的代码块用 gpui-kit 的 Tree-sitter 高亮。发现新版本后下载校验，确认后再重启安装。

工具行会写明目标：读了哪个文件、搜了什么、跑了哪条命令。密钥只在 `secrets.json`，设置页对已保存的钥匙显示一把锁。

## 下载

发布页：[Releases](https://github.com/MCapricorns/mycode/releases)

| 文件 | 平台 |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-pc-windows-msvc.zip` | Windows 11 ARM64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |

每个 zip 旁有 `.sha256`。0.4.0 之后不再提供 Intel macOS 构建。0.7.2 至 0.7.4 的发布包含 Linux x86_64 压缩包，之后不再提供。

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

质量门与 CI 相同：格式、clippy，以及一次 `exec` 启动烟测。除此之外没有测试套件。

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p mycode-tools --lib native_image_launches --locked
```

推送到 `main` 的 pull request 在 Windows x64、Windows ARM64、Linux x86_64 和 macOS Apple Silicon 上跑这三步，不发布。质量门通过的 `main` 推送每次都会新建 GitHub Release：新标签 `v<version>`、三个平台的 zip（Windows x64、Windows ARM64、macOS Apple Silicon）和对应 `.sha256`，发布说明取 `CHANGELOG.md` 里该版本的条目，不从提交记录生成。`Cargo.toml` 里的版本如果已经有标签，发布计划会把补丁号加一，写回 `Cargo.toml`、`Cargo.lock` 和 `CHANGELOG.md`，再用这个新版本发版。写在 `CHANGELOG.md` 的 `## [Unreleased]` 下的内容会移到这个新版本下；该节为空时用一句固定说明。旧版本的压缩包已经齐，也不会跳过这次发布。手动把 `Cargo.toml` 改到一个还没有标签的版本时，`CHANGELOG.md` 里必须已经有该版本的条目。

## 许可

[Apache-2.0](LICENSE)

---

## English

mycode is a local-first desktop coding agent for Windows and macOS.
The conversation, the tool calls, and every session stay on your machine.
You bring the API keys.

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
- **UI.** English and Chinese, a dark theme, and several palettes.
  Fenced code in the transcript is highlighted with gpui-kit's Tree-sitter
  grammars. Updates download and verify, then wait for a restart confirmation.

A tool line names its target: which file was read, what was searched,
which command ran. Keys live only in `secrets.json`. Saved keys show as
a lock in Settings.

### Download

[Releases](https://github.com/MCapricorns/mycode/releases)

| Asset | Platform |
| --- | --- |
| `mycode-desktop-v<version>-x86_64-pc-windows-msvc.zip` | Windows 10/11 x64 |
| `mycode-desktop-v<version>-aarch64-pc-windows-msvc.zip` | Windows 11 ARM64 |
| `mycode-desktop-v<version>-aarch64-apple-darwin.zip` | macOS Apple Silicon |

Each zip has a `.sha256` sidecar. Intel macOS builds stopped after 0.4.0.
Linux x86_64 archives shipped in 0.7.2 through 0.7.4 and are not part of
later releases.

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

CI is rustfmt, clippy, and one `exec` launch smoke test. There is no
other test suite.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p mycode-tools --lib native_image_launches --locked
```

Pull requests into `main` run those gates on Windows x64, Windows ARM64,
Linux x86_64, and macOS Apple Silicon, and do not publish. A push to
`main` whose gates pass always publishes a new GitHub Release: a new
`v<version>` tag, the three platform zips (Windows x64, Windows ARM64,
and macOS Apple Silicon), their `.sha256` sidecars, and the matching
`CHANGELOG.md` section (not generated commit notes). When
that version already has a tag, release-plan bumps the patch in
`Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`, then publishes the new
version. Notes under `## [Unreleased]` in `CHANGELOG.md` move into that
version; an empty section gets one fixed sentence. Archives already
uploaded for an older tag do not skip the release. A hand-bumped version
that has no tag yet still needs its own `CHANGELOG.md` section.

### License

[Apache-2.0](LICENSE)
