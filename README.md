<div align="center">

<img src="wiki/images/banner.svg" alt="MYCode：本地编码代理" width="100%">

<p>
  <a href="https://github.com/MCapricorns/mycode/releases/latest"><img alt="最新版本" src="https://img.shields.io/github/v/release/MCapricorns/mycode?style=flat-square&label=%E7%89%88%E6%9C%AC&color=c72e38"></a>
  <a href="https://github.com/MCapricorns/mycode/releases"><img alt="下载量" src="https://img.shields.io/github/downloads/MCapricorns/mycode/total?style=flat-square&label=%E4%B8%8B%E8%BD%BD"></a>
  <a href="https://github.com/MCapricorns/mycode/actions"><img alt="构建" src="https://img.shields.io/github/actions/workflow/status/MCapricorns/mycode/ci.yml?branch=main&style=flat-square&label=%E6%9E%84%E5%BB%BA"></a>
  <img alt="平台" src="https://img.shields.io/badge/%E5%B9%B3%E5%8F%B0-Windows%20%7C%20macOS-555?style=flat-square">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-dea584?style=flat-square&logo=rust">
  <a href="LICENSE"><img alt="许可" src="https://img.shields.io/github/license/MCapricorns/mycode?style=flat-square&label=%E8%AE%B8%E5%8F%AF"></a>
  <a href="https://x.com/M_Capricorns"><img alt="关注 @M_Capricorns" src="https://img.shields.io/badge/X-%40M__Capricorns-000?style=flat-square&logo=x"></a>
</p>

**打开项目，用自然语言读代码、修改文件、运行命令。**

[下载](https://github.com/MCapricorns/mycode/releases/latest) · [快速开始](#快速开始) · [日常使用](#日常使用) · [扩展与设置](#扩展与设置) · [本地数据](#本地数据与权限) · [参与开发](#参与开发) · [Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) · [更新日志](CHANGELOG.md)

</div>

MYCode 是用 Rust 和 GPUI 构建的原生编码代理桌面应用，支持 Windows x64、Windows ARM64 和 macOS Apple Silicon。你可以在对话中让代理查找代码、实施修改、运行构建与测试，并查看工具进度和代码改动。

MYCode 的模型目录与模型请求统一经过本机 [magpie](https://usemagpie.ai) 网关。对话记录、项目关联和界面状态保存在本机；发送给模型的消息、附件及工具结果会经过网关，由 magpie 转发给你配置的上游模型服务。模型内容的去向取决于 magpie 的上游配置。

## 安装

从 [Releases](https://github.com/MCapricorns/mycode/releases/latest) 下载适合设备的安装包。每个包都附有对应的 `.sha256` 校验文件。

| 平台 | 手动安装包 |
| --- | --- |
| Windows x64 | `mycode-desktop-v*-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `mycode-desktop-v*-aarch64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `mycode-desktop-v*-aarch64-apple-darwin.dmg` |

当前不提供 Intel macOS、Linux 或 Windows x86 安装包。

**Windows**：解压后双击 `mycode-desktop.exe`。应用会把自己安装到 `~/.mycode/bin/mycode-desktop.exe`，启动安装后的副本，并在桌面创建 `MYCode` 快捷方式。原先启动的下载副本随后会被清理，以后可从快捷方式打开。

**macOS**：打开 dmg，把 `MYCode.app` 拖进「应用程序」。应用使用 ad-hoc 签名；首次打开可能需要右键选择「打开」，或执行：

```sh
xattr -d com.apple.quarantine /Applications/MYCode.app
```

应用内置自动更新，标题栏会提示新版本，可确认后重启安装。Release 中的 macOS zip 用于自动更新，手动安装请用 dmg。

## 快速开始

1. 安装并启动 [magpie](https://usemagpie.ai)，在 magpie 中配置要使用的上游模型服务和凭据。
2. 在终端安装 MYCode 的 magpie 插件：

   ```sh
   magpie plugin add github:MCapricorns/magpie-mycode
   ```

   也可以在 magpie 的 Plugins 页添加同一个 `github:` 地址。

3. 在 magpie 的 Agents 页给 **MCode** 选择模型。插件会将网关地址、网关凭据和模型绑定写入 `~/.mycode/settings.json` 的 `magpie` 配置。
4. 启动 MYCode，打开或拖入项目目录，在输入框描述任务。可先让代理解释一段代码，再提出具体修改与验证要求。

上游服务、账号、API key 和可用模型在 **magpie** 中配置。MYCode 的「设置 → Magpie」用于查看网关连接与模型目录，点击「刷新模型」会重新读取绑定并请求网关的 `GET /v1/models`；运行中在 magpie 改了配置也可这样同步，无需重启。这里的网关凭据用于 MYCode 连接 magpie，上游凭据由 magpie 管理。

如果显示未连接或没有模型，先确认 magpie 正在运行、插件已安装，并已在 Agents 页给 MCode 选模型，再刷新。网关能连接但上游请求失败时，应检查 magpie 中对应服务的账号与连接配置。

## 日常使用

### 项目与对话

左侧的 **Projects / 项目** 和 **Conversations / 对话** 分别纵向排列、独立滚动。选择项目后，对话列表、侧栏标题搜索和命令面板中的对话结果只显示当前目录的对话；要找其他项目的历史，先切换到那个项目。没有项目绑定的历史会话可从项目列表中的 **Unassigned / 未分配** 入口查找和打开。

点击项目会打开它最近的对话，没有历史时创建新对话；「新对话」或 `/new` 可以在当前目录开始另一段对话。切换项目不会改变已有对话绑定的目录。需要移动某段对话的工作目录时，使用该对话的目录选择入口，并先完成或停止当前回合。

项目列表通常最多保留 8 个目录入口，满额后新增会移出最旧入口；旧版合并得到的较长列表保留原有容量。**移除项目只移除快捷入口，项目文件和对话历史仍保留**，重新打开目录即可访问。

### 提供上下文与选择模型

在输入框输入 `@` 搜索当前目录的文件，或拖入文本文件与图片作为下一条消息的附件。拖入文件夹会打开项目。每条消息最多附加 4 张图片和 8 个文本文件；文本单文件超过 256 KiB 会截断并标记，文本合计上限为 512 KiB。图片也受大小限制，4 张并不表示可同时附加 4 张最大尺寸图片。

输入框旁的模型和思考强度菜单用于调整当前对话。模型列表中的每个标签都标注来源，包括最近使用和当前选择，如 `copilot · 模型名`、`codex · 模型名`、`zcode · 模型名`。来源优先使用网关提供的标记；缺失时采用完整模型 ID 的前缀，否则显示「来源未提供」。模型选择器将 **Magpie 标为网关**。

同名模型来自不同来源时分别保留。只有网关明确标记为同一来源的完整 ID 与短别名才会合并展示；合并后仍保留别名解析，已有对话的模型选择继续有效。

思考强度只提供模型实际公布的档位。每段对话分别记住模型和思考强度，切换时不会改写其他对话的选择。

### 常用命令与快捷键

以下组合键在 Windows 使用 `Ctrl`，在 macOS 使用 `Cmd`。完整列表见「设置 → 快捷键」。

| 操作 | 方式 |
| --- | --- |
| 新建对话 | `/new` 或「新对话」 |
| 压缩当前对话上下文 | `/compact` |
| 打开设置 | `/settings` |
| 选择命令、技能或 MCP | 输入 `/` 打开菜单 |
| 搜索对话、模型、命令或设置页 | `Ctrl-K` / `Cmd-K` |
| 换行 | `Shift-Enter` |
| 粘贴图片 | `Ctrl-V` / `Cmd-V`；粘贴内容为单个图片路径时附加该文件 |

回合进行中仍可继续输入：按 `Enter` 打开「引导 / 打断并发送」选择，按 `Ctrl-G` / `Cmd-G` 可直接引导，让模型在下一步前读取补充消息；未被读取的引导会回到输入框。按 `Ctrl-Enter` / `Cmd-Enter` 会停止当前回合并立即发送新消息。`Esc` 关闭选择并保留草稿。

对话末尾只显示「工作中」或「思考中」，具体命令、工具进度和结果在对应工具行查看。输入框旁的「停止」会取消当前回合的工具和子代理，并终止它们启动的进程。已经收到的回复与已完成的工具结果仍保留，已有文件修改不会自动回滚。可在改动面板检查 diff；面板随文件系统变化更新 Git 状态。

对话跟随最新消息时，引导卡出现、输入框变高和继续输出都会保持滚动到底部。主动向上滚动查看历史后才暂停跟随；点击回到底部即可恢复。Windows 下的命令和 MCP 后台服务不会另开终端窗口，主动启动的图形应用仍按正常方式显示。

历史用户消息下方始终显示「撤回并编辑」。点击后，对话回到这条消息发送之前，原文回填到输入框，修改后可以重新发送；第一条消息也可以撤回。运行中的任务需要先停止，按钮会保留并显示不可用原因。`rewind` 只改变对话历史，不恢复或删除工作区文件。

## 扩展与设置

### magpie Library、技能与 MCP

magpie Library 将全局规则、技能和 MCP 配置写入 MYCode 的数据目录：

- **规则**：`~/.mycode/AGENTS.md`，作为所有项目的默认指令。
- **技能**：`~/.mycode/skills/` 下的 `SKILL.md`，可从 `/` 菜单插入使用请求，也可在设置中逐个关闭。
- **MCP**：`~/.mycode/mcp.json`，支持 stdio 和 Streamable HTTP，最多 64 台服务器。MYCode 没有内置网页搜索，联网检索通过配置的 MCP 服务提供，例如 anysearch。

项目规则按顺序读取项目根目录下的 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md` 和 `.agents/AGENTS.md`。项目规则覆盖全局默认，同一项目中后读文件覆盖前面文件的冲突项；提示词会标明来源路径，用户当前明确指令优先。

### 子代理

内置四种角色，可在 `~/.mycode/agents/` 或项目的 `.mycode/agents/` 中覆盖提示词或添加自定义角色，并在设置中管理：

| 角色 | 用途 | 权限 |
| --- | --- | --- |
| `scout` | 查找代码、文档与证据 | 只读，不运行命令 |
| `artisan` | 实施范围明确的修改，并完成相关验证 | 可修改文件，默认使用 Git worktree |
| `sentinel` | 独立检查实现、调用方与回归风险 | 只读 |
| `skeptic` | 针对一个明确的假设或结论寻找反例 | 只读 |

`scout`、`sentinel` 和 `skeptic` 的只读边界不会因提示词覆盖而放宽。最多并发 8 个子代理；明确的 phase 和文件 / symbol scope 用于防止重复委派、重叠写入，scope 不构成权限沙箱。

子代理面板保留运行状态、结果、错误和输出文件路径，可查看详情、复制结果或逐个停止。主代理可用 `agent_status` 查询、`agent_stop` 停止，或用 `agent_message` 在运行中补充指令。已完成的独立任务可基于明确的摘要、原结果和新指令启动新运行；这不是恢复原始模型会话。写入任务继续使用保留的原 worktree，已有改动不会从 HEAD 重建。已取消的任务不能直接续跑。运行记录仅在当前应用进程内保留，并有数量上限。

`agent_workflow` 提供固定的 `research`、`code-review` 和 `migration` 流程。每个流程都要求独立的 `sentinel` 明确给出 `PASS` 才算成功；依赖失败时不会继续执行后续节点。`research` 和 `code-review` 等待流程结束再返回结果。review 未通过时，主代理可用 `revise` 将意见交回负责产出的节点，在同一 workflow 开启新一轮；原任务和结果仍保留，受影响的后续检查与独立验收会重新执行。`migration` 在写入任务完成后进入「等待整合」：主代理处理保留的 worktree 改动后，再显式恢复流程，执行整体验证与独立验收。

子代理不会自动合回或提交 worktree。含修改、未跟踪文件或忽略文件的 worktree 会保留，并在结果中给出路径；取消时仍等待子进程退出和改动交接，不会提前释放写入范围或丢弃已有改动。长流程可通过 CodeMode 的 `wait_code` 继续收集同一次运行，不能把启动或等待状态当作已完成。

### CodeMode

主回合向模型提供当前平台的 shell、`run_code` 和 `wait_code`。CodeMode 通过 `rquickjs 0.14` 嵌入 QuickJS，执行 JavaScript，以同步的 `call(name, args)` 组合已注册工具，例如 `call("read", { path: "src/lib.rs" })`。长任务返回 cell ID 后，用 `wait_code` 继续收集同一次执行的结果。

脚本不提供 Node.js 或浏览器 API，也不支持 `async/await`、Promise 任务或模块导入；文件、命令和网络操作通过工具完成。内置工具返回的原生图片会作为图片内容块随 `run_code` / `wait_code` 传递给模型，经过脚本调用仍可保留；当前 MCP 返回的图片与音频会转换为文字占位。MCP 工具先通过 `search_tool` 或 `tool_search` 按需查询 schema，再通过 `use_tool` 调用。

CodeMode 沿用工具的权限检查、取消与进度机制，并限制脚本资源和调用次数。具体协议、示例和限制见 [CodeMode 文档](wiki/architecture/code-mode-engine.md)。

### 外观与网络

设置支持简体中文与英文、深色与浅色主题、四档字号。网络代理默认跟随系统，也可手动填写 HTTP 或 SOCKS5 等代理地址。「测试」会请求 `https://www.google.com/generate_204` 并显示线路、状态码与耗时；它验证 MYCode 的网络设置，上游模型服务的连接仍由 magpie 配置。

## 本地数据与权限

默认数据目录为 `~/.mycode`。可在启动前将环境变量 `MYCODE_HOME` 设置为绝对路径，改用另一个数据目录；已有数据需自行迁移，并确认 magpie 写入的配置与 MYCode 读取的位置一致。

| 路径 | 内容 |
| --- | --- |
| `settings.json` | 网关绑定与网关凭据、外观、网络代理、子代理和技能开关等设置 |
| `ui.json` | 最近目录、对话与目录的关联、各对话的模型与思考强度、窗口状态 |
| `sessions.db`、`sessions/` | 对话索引，以及按项目和会话组织的 JSONL 记录、附件载荷与压缩检查点 |
| `AGENTS.md`、`skills/`、`mcp.json` | magpie Library 提供的规则、技能和 MCP 配置 |
| `secrets.json` | MYCode 保存的 MCP 密钥；Library 的 `mcp.json` 也可能含服务凭据 |
| `agents/`、`agent-worktrees/` | 自定义子代理角色及其工作目录 |
| `mcp-results/`、`scratch/` | 被截断的 MCP 结果全文、未绑定项目时的工具工作目录 |
| `bin/`、`update/` | Windows 自安装程序与自动更新暂存文件 |

设置、密钥和界面状态使用严格 JSON。有效的 magpie 首次绑定可直接导入；损坏的配置会生成 `*.broken-*` 备份并尽量恢复有效字段，临时访问失败会报告错误，不会重置健康配置。分享诊断材料前，请先去除配置中的凭据。格式与恢复细节见 [配置说明](crates/mycode-config/README.md) 和 [运行时文档](wiki/architecture/runtime.md)。

工具以当前用户权限运行，可以修改文件、启动程序和访问网络；有效的命令调用会直接执行。环境变量白名单是过滤机制，不构成系统沙箱。CodeMode 的资源限制和 worktree 隔离也不会改变这一权限边界。

## 参与开发

### 构建与运行

需要 Rust stable 和平台的原生编译工具链。Windows 使用 MSVC，并安装带 C++ 工具的 Visual Studio Build Tools；macOS 需要 Xcode Command Line Tools。嵌入的 QuickJS 需要 C 编译器。开发与验证以 Windows x64 / ARM64、macOS Apple Silicon 三个原生平台为准。

```sh
git clone https://github.com/MCapricorns/mycode.git
cd mycode
cargo build --release -p mycode-desktop --locked
```

开发时可在仓库根目录运行：

```sh
cargo run -p mycode-desktop --locked
```

Debug 构建默认跳过 Windows 自安装。直接运行 release 构建时，可先设置 `MYCODE_NO_INSTALL=1`，保留程序在 `target/` 中运行：PowerShell 使用 `$env:MYCODE_NO_INSTALL = "1"`，macOS shell 使用 `export MYCODE_NO_INSTALL=1`。连接模型仍需要前述 magpie 配置。

### 检查与文档

在仓库根目录按改动范围运行相关检查：

```sh
cargo test --workspace --locked
cargo test -p mycode-desktop --features test-support --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

第二条命令启用桌面的 GPUI 布局测试支持。当前 [CI](.github/workflows/ci.yml) 在三个发布平台执行 workspace 测试，并在 Windows x64 执行 Clippy。

从源码入手时，界面在 `mycode-desktop`，界面状态与交互驱动在 `mycode-frontend`，回合编排、代理循环、工具、模型协议与配置分别在其余核心 crate 中。模块边界、消息流程和存储协议见 [运行时架构](wiki/architecture/runtime.md)；[在线 Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) 可搜索模块并查看源码，点击后直接进入静态页面。需要离线使用时，下载仓库后在浏览器打开本地 `wiki/index.html`。

修改 Wiki 的源文档或生成器时，按 [Wiki 构建说明](wiki/README.md) 更新并校验生成页面。提交贡献时请说明具体问题、修改后的行为和验证结果；问题与建议可通过 [GitHub Issues](https://github.com/MCapricorns/mycode/issues) 反馈。

## 社区

- X（Twitter）：[@M_Capricorns](https://x.com/M_Capricorns)
- 问题与建议：[GitHub Issues](https://github.com/MCapricorns/mycode/issues)

## 许可

[Apache-2.0](LICENSE)

<div align="center">
<sub>用 Rust 和 GPUI 构建 · <a href="https://x.com/M_Capricorns">@M_Capricorns</a></sub>
</div>
