<div align="center">

<img src="wiki/images/banner.svg" alt="MYCode：本地编码代理" width="100%">

<p>
  <a href="https://github.com/MCapricorns/mycode/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/MCapricorns/mycode?style=flat-square&color=c72e38"></a>
  <a href="https://github.com/MCapricorns/mycode/actions"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/MCapricorns/mycode/ci.yml?branch=main&style=flat-square"></a>
  <img alt="Windows and macOS" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-555?style=flat-square">
  <a href="LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/MCapricorns/mycode?style=flat-square"></a>
</p>

**从一句想法，到一次可检查的改动。**

叠纸像素风的原生工作台，把对话、代码和子代理协作放在一起。

[下载](https://github.com/MCapricorns/mycode/releases/latest) · [快速开始](#快速开始) · [开发](#参与开发) · [Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) · [更新日志](CHANGELOG.md)

</div>

MYCode 是使用 Rust 和 GPUI 构建的原生桌面编码代理。打开就能聊天；选择项目后，可以读代码、修改文件、运行命令，再逐项检查结果。

| 你想做什么 | 在 MYCode 中 |
| --- | --- |
| 先聊一个想法 | 欢迎页直接发送消息，使用默认聊天空间 |
| 开始修改项目 | 选择或拖入目录，附上文件、图片与任务要求 |
| 看懂模型在做什么 | 对话中查看思考、带高亮的代码和工具进度 |
| 把复杂任务拆开 | 子代理分别负责调研、实现与审查，保留完整结果 |
| 检查实际成果 | Details 查看任务状态、改动与用量，打开 diff 检查文件 |

模型通过本机 [magpie](https://usemagpie.ai) 网关连接。项目操作、会话记录、工具执行和界面状态由 MYCode 管理；发给模型的消息、附件和工具结果经网关转发到你配置的上游服务。

## 安装

从 [Releases](https://github.com/MCapricorns/mycode/releases/latest) 下载对应安装包。每个包附带 SHA-256 校验文件。

| 平台 | 手动安装包 |
| --- | --- |
| Windows x64 | `mycode-desktop-v*-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `mycode-desktop-v*-aarch64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `mycode-desktop-v*-aarch64-apple-darwin.dmg` |

Windows 解压后运行 `mycode-desktop.exe`，应用会安装到 `~/.mycode/bin/mycode-desktop.exe` 并创建桌面快捷方式。下载副本会在安装完成后清理。macOS 打开 dmg，将 MYCode 拖进「应用程序」；首次运行可右键选择「打开」。应用使用 ad-hoc 签名，必要时执行：

```sh
xattr -d com.apple.quarantine /Applications/MYCode.app
```

标题栏会提示可用更新，确认后重启安装。macOS Release 中的 zip 供自动更新使用。当前支持 Windows x64、Windows ARM64、macOS Apple Silicon，不提供 Linux、Intel macOS 或 Windows x86 安装包。

## 快速开始

1. 安装并启动 magpie，在其中配置模型服务与账号。
2. 安装 MYCode 插件：

   ```sh
   magpie plugin add github:MCapricorns/magpie-mycode
   ```

3. 在 magpie 的 Agents 页为 **MCode** 选择模型。插件会写入 MYCode 的网关绑定。
4. 打开 MYCode，在欢迎页直接开始对话。需要处理代码时，选择或拖入项目目录，再输入任务。

MYCode 的「设置 → Magpie」显示网关连接和模型目录。修改 magpie 绑定后，点击「刷新模型」即可同步。没有模型时，先确认 magpie 正在运行、插件已安装、MCode 已完成模型选择。

## 日常使用

欢迎页展示 MYCode 标识和最近项目。未选择项目时，发送消息会使用默认聊天空间；随时可以切换到本地项目。左侧分别显示项目和当前空间的对话。选择项目会打开最近的对话，没有历史则创建新对话。「新对话」或 `/new` 保留当前空间；移除项目入口不会删除文件或历史。未绑定项目的旧会话位于「默认聊天」。

对话、思考区和代码块随窗口宽度伸展。Details 按子代理、文件改动、模型与用量分组，可以固定在右侧，也可以作为浮层查看。设置页集中管理外观、字号、快捷键、模型、角色、技能和 MCP。

使用 `@` 搜索文件，或拖入文本文件、图片。每条消息最多 4 张图片与 8 个文本文件；单张图片原始数据最多 3.75 MB，全部图片 base64 合计最多 6 MiB；文本单文件最多 256 KiB，合计最多 512 KiB。超长文本会标记截断。

模型和思考强度按会话保存。模型标签显示网关提供的来源；不同来源的同名模型分别保留。思考菜单只提供模型公布的档位。

| 操作 | Windows | macOS |
| --- | --- | --- |
| 命令面板 | Ctrl-K | Cmd-K |
| 换行 | Shift-Enter | Shift-Enter |
| 运行中补充引导 | Ctrl-G | Cmd-G |
| 打断并发送 | Ctrl-Enter | Cmd-Enter |
| 粘贴图片 | Ctrl-V | Cmd-V |

`/compact` 压缩当前上下文，`/settings` 打开设置。运行中按 Enter 可选择引导或打断；未读引导可以撤回。停止会取消当前回合、工具和子代理，保留已收到的内容及已经发生的文件修改。

历史消息的「撤回并编辑」会回退对话并将原文放回输入框。**它只改变对话历史，不回滚工作区文件。** 修改后可在改动面板查看 Git diff。

## 规则、技能与工具

magpie Library 写入全局规则、技能和 MCP 配置。项目规则依次读取 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`，项目规则覆盖全局默认，同一目录后读的规则覆盖前面的冲突项。用户明确指令优先。

主代理直接调用平台 shell、`run_code` 和 `wait_code`。其他工具可以在 CodeMode 的 JavaScript 脚本里调用：`await callAsync(name, args)` 等一个结果，`await Promise.all([...])` 同时等待独立的读取或搜索。旧的同步 `call(name, args)` 继续可用。写文件、shell 等独占操作仍按提交顺序单独运行。

CodeMode 使用内置 QuickJS，支持顶层 `await` 和 Promise，没有 Node.js、浏览器 API 或 `fetch`。每次脚本最多调用 256 次工具。长任务返回 `cell_id` 后，用 `wait_code` 继续等待同一次执行；重新提交脚本会再做一遍。示例和错误处理见 [CodeMode 使用说明](wiki/architecture/code-mode-engine.md)。

MYCode 没有内置网页搜索。配置 MCP 服务后，通过 `search_tool` / `tool_search` 发现 schema，再用 `use_tool` 调用。工具以当前用户权限运行；worktree 和脚本资源限制不构成操作系统沙箱。

Library 中的 MCP、skills、角色与设置文件变化会自动刷新界面。运行中的主代理和子代理在下一次模型请求前重新读取规则、技能目录与 MCP 配置；新增服务后台连接，删除或改配的服务从后续工具目录移除。已经开始的调用不重放。接收到 MCP `notifications/tools/list_changed` 后会重新获取工具目录；空闲连接的通知在下一次协议交互时读取，不保证服务端变化立即推送到界面。

新派遣的子代理读取最新角色启停、模型路由和技能设置。已有子代理保持启动时的角色与权限边界；主回合开始时未注册的派遣能力要到下一回合启用。技能仅将名称、说明和路径放入提示，正文仍按需读取。

## 子代理

| 角色 | 职责 | 边界 |
| --- | --- | --- |
| scout | 检索代码、文档和事实，给出证据 | 只读，无 shell |
| artisan | 实施范围明确的修改并验证 | 默认使用 Git worktree |
| sentinel | 独立审查实现与 diff，或对特定假设寻找反例 | 只读，无 shell |

角色可在全局 `~/.mycode/agents/` 或项目 `.mycode/agents/` 覆盖、扩展，并在设置中启停。内置只读角色不会因覆盖提示词而获得写入能力。`agent` 调用顺序执行，同时修改的路径或符号范围不能重叠。子代理不会自动提交、合并或推送。

主模型会收到主动派遣指引：较大的调研、边界明确的实现与独立审查适合交给子代理，简单问答和小改动直接完成。

任务结束后，可以用 `agent_status` 查看结果、错误和文件路径；还在执行的任务可以用 `agent_stop` 停止。这两个工具在全部角色关闭后仍然可用。`agent_message` 能给任务补充说明；任务已完成时，可用摘要启动后续工作（写入角色会复用原保留 worktree）。它不会恢复原来的模型会话，已取消的任务也不能续接。

## 本地数据

默认目录是 `~/.mycode`，可在启动前用绝对路径环境变量 `MYCODE_HOME` 改写。已有数据需自行迁移，并保持 magpie 与 MYCode 指向同一目录。

| 路径 | 内容 |
| --- | --- |
| settings.json | 网关绑定、界面、网络、角色和技能设置 |
| ui.json | 项目入口、会话目录关联、模型选择、窗口状态 |
| sessions.db、sessions/ | 会话索引、事件账本、附件和压缩检查点 |
| AGENTS.md、skills/、mcp.json | 全局规则、技能与 MCP 服务 |
| secrets.json | MYCode 保存的 MCP 密钥 |
| agents/、agent-worktrees/、agent-results/ | 自定义角色、保留工作树和完整结果 |
| mcp-results/、scratch/ | 大型 MCP 结果与临时工作目录 |
| bin/、update/ | Windows 安装和更新暂存 |

配置文件使用标准 JSON。文件损坏时会先备份，再尽量保留能读出的设置；临时读取失败不会清空正常配置。分享配置或诊断文件前，请删除密钥等凭据。

## 参与开发

工作区仅有两个 package，内部类型直接从所属模块导入：

| package | 职责 |
| --- | --- |
| mycode | core、config、providers、tools、agent、app、frontend 模块；不依赖 GPUI |
| mycode-desktop | 原生窗口、渲染、剪贴板和桌面交互；依赖 mycode |

需要 Rust stable 和原生构建工具。Windows 使用 MSVC 与 C++ Build Tools；macOS 使用 Xcode Command Line Tools。QuickJS 需要 C 编译器。

```sh
git clone https://github.com/MCapricorns/mycode.git
cd mycode
cargo run -p mycode-desktop --locked
cargo build --release -p mycode-desktop --locked
```

Debug 构建跳过 Windows 自安装；直接运行 release binary 时，设置 `MYCODE_NO_INSTALL=1` 可留在构建目录运行。

Linux 本地测试先运行 `bash .github/scripts/install_linux_deps.sh` 安装开发库，再执行下方检查。真实窗口视觉走查需要 X11/Wayland 会话及 Vulkan/OpenGL 驱动；无显示器环境可使用 Xvfb、窗口管理器和 Mesa 软件渲染。`test-support` 布局测试不需要显示服务，但不能替代真实字体与像素检查。

提交前执行：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo test -p mycode-desktop --features test-support --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python scripts/make_icon.py --check
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

CI 在 Windows x64、Windows ARM64、macOS Apple Silicon 和 Ubuntu 24.04 x64 原生运行测试，Windows x64 和 Linux x64 运行 Clippy 与桌面布局测试。Linux 仅作为开发验证平台，不生成 Release 安装包。带 `[release]` 的 main 推送须先在 `CHANGELOG.md` 的 `Unreleased`（或手动指定的新版本条目）写入具体更新说明，空条目和占位说明会被 CI 拒绝；全部检查与构建成功后发版。版本号由发布脚本统一更新两个 package，发布失败不会提前推进 main。

代码入口、消息处理和停止行为见 [MYCode 如何运行](wiki/architecture/runtime.md)；批量工具调用见 [CodeMode 使用说明](wiki/architecture/code-mode-engine.md)；测量结果见 [性能记录](wiki/memory-report.md)；更新文档的方法见 [Wiki 维护说明](wiki/README.md)。

## Contributors

感谢参与开发的真人贡献者：

- [MCapricorns](https://github.com/MCapricorns)
- [YangChengxxyy](https://github.com/YangChengxxyy)

此名单仅列人类贡献者，自动化账号不列入。

## 社区与许可

问题与建议：[GitHub Issues](https://github.com/MCapricorns/mycode/issues)。作者动态：[@M_Capricorns](https://x.com/M_Capricorns)。

采用 [Apache-2.0](LICENSE) 许可。
