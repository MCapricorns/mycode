<div align="center">

<img src="wiki/images/banner.svg" alt="MYCode：原生桌面编码代理" width="100%">

<p>
  <a href="https://github.com/MCapricorns/mycode/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/MCapricorns/mycode?style=flat-square&color=c72e38"></a>
  <a href="https://github.com/MCapricorns/mycode/actions"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/MCapricorns/mycode/ci.yml?branch=main&style=flat-square"></a>
  <img alt="Windows and macOS" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-555?style=flat-square">
  <a href="LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/MCapricorns/mycode?style=flat-square"></a>
</p>

**从一句想法，到一次可检查的改动。**

采用叠纸像素（Paper-Pixel）视觉语言打造的 Rust + GPUI 原生桌面编码代理，将代码对话、QuickJS CodeMode 工具编排、子代理协作与 Git Diff 审查集成在同一个高效工作台中。

[下载版本](https://github.com/MCapricorns/mycode/releases/latest) · [项目主页](https://mcapricorns.github.io/mycode/) · [架构 Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) · [快速开始](#快速开始) · [参与开发](#参与开发) · [更新日志](CHANGELOG.md)

</div>

---

## 概览

MYCode 由核心运行时 `mycode` 与原生桌面壳 `mycode-desktop` 构成。打开即可在默认空间发起对话；绑定本地项目后，模型可通过内置工具与 QuickJS 脚本读取仓库、并发检索、编辑文件、执行命令，并通过独立的 `Diff` 与 `Details` 面板逐项检验结果。

| 你想做什么 | 在 MYCode 中的工作流 |
| --- | --- |
| 先理清一个想法 | 直接在欢迎页或默认聊天空间发送消息，无需先挂载仓库 |
| 修改并验证本地项目 | 选择或拖入项目目录，用 `@` 引用文件、粘贴截图或拖入文本附件 |
| 高效批处理与并行检索 | 通过 QuickJS `CodeMode` 编排 `await callAsync` 与 `Promise.all` 并发读查 |
| 拆解调研、实现与审查 | 自动或按需调度 `scout` / `artisan` / `sentinel` 子代理，隔离修改与验证 |
| 逐行核对实际改动 | 在 `Details` 面板查看子代理进度、文件清单与用量，一键展开内联 Git diff |

模型连接由本机 [magpie](https://usemagpie.ai) 网关统一提供。MYCode 负责管理本地工作区、工具执行、会话账本与界面状态；发送给模型的消息、附件与工具输出经 Magpie 转发至你配置的上游服务。

---

## 安装与支持平台

前往 [Releases](https://github.com/MCapricorns/mycode/releases/latest) 下载对应平台包（每个发布包均附带 SHA-256 校验文件）：

| 平台 | 手动安装包 | 安装说明 |
| --- | --- | --- |
| Windows x64 | `mycode-desktop-v*-x86_64-pc-windows-msvc.zip` | 解压后运行 `mycode-desktop.exe`，自动安装到 `~/.mycode/bin/mycode-desktop.exe` 并创建桌面快捷方式（随后清理下载副本） |
| Windows ARM64 | `mycode-desktop-v*-aarch64-pc-windows-msvc.zip` | 同上 |
| macOS Apple Silicon | `mycode-desktop-v*-aarch64-apple-darwin.dmg` | 打开 DMG 并将 `MYCode` 拖入「应用程序」；zip 包供内置自动更新器使用 |

- macOS 安装包采用 ad-hoc 签名，首次打开可右键选择「打开」，或在终端移除隔离标记：
  ```sh
  xattr -d com.apple.quarantine /Applications/MYCode.app
  ```
- 检测到新版本时，标题栏会展示更新提示，确认后重启即可完成安装。
- 当前发布面向 **Windows x64**、**Windows ARM64** 与 **macOS Apple Silicon**（暂不发布 Linux、Intel macOS 或 Windows x86 二进制）。

---

## 快速开始

1. 安装并启动 **magpie**，在其中配置可用的模型服务与账号。
2. 安装 MYCode 插件以建立网关绑定：
   ```sh
   magpie plugin add github:MCapricorns/magpie-mycode
   ```
3. 在 magpie 的 Agents 页为 **MCode** 选择目标模型。
4. 启动 MYCode，即可在欢迎页直接对话；处理代码时选择或拖入项目目录即可开始。

> 若未看到模型列表，请在 MYCode 的「设置 → Magpie」确认网关状态并点击「刷新模型」，或检查 magpie 是否已安装插件且 **MCode** 已绑定模型。

---

## 日常使用与工作台交互

### 项目、会话与检查视图

- **欢迎页与默认空间**：未绑定目录时发言会存入「默认聊天」，可随时切换到本地项目；移除项目入口不会删除磁盘代码或历史会话。
- **Paper-Pixel 工作台**：支持亮色/深色两套纸感主题与 `S / M / L / XL` 四档界面字号（详见 [DESIGN.md](DESIGN.md)）；窗口大小调整不会静默覆盖字号设置。
- **Details 检查面板**：按「子代理任务」「文件改动」「模型与用量」清晰分组，支持右侧固定停靠或浮层展开，点击变更文件即可查看高亮 Git diff。
- **会话状态隔离**：模型与思考强度按会话独立保存，模型徽章保留网关返回的来源标识（不同来源的同名模型分别列出），思考档位仅展示模型声明的支持级别。

### 附件与输入限制

使用 `@` 模糊搜索项目文件，或直接向输入框拖入文本、粘贴图片：

| 类别 | 单条消息上限 | 体积限制 |
| --- | --- | --- |
| 图片附件 | 最多 4 张 | 单张原始数据 ≤ 3.75 MB，全部 base64 合计 ≤ 6 MiB |
| 文本附件 | 最多 8 个 | 单文件 ≤ 256 KiB，合计 ≤ 512 KiB（超长内容自动标记截断） |

### 快捷键与控制

| 操作 | Windows | macOS |
| --- | --- | --- |
| 打开命令面板 | `Ctrl-K` | `Cmd-K` |
| 输入框换行 | `Shift-Enter` | `Shift-Enter` |
| 运行中补充引导 | `Ctrl-G` | `Cmd-G` |
| 打断当前输出并发送 | `Ctrl-Enter` | `Cmd-Enter` |
| 粘贴剪贴板图片 | `Ctrl-V` | `Cmd-V` |

- 支持 `/new`（新建会话）、`/compact`（手动压缩上下文）与 `/settings`（打开设置）。
- 回合运行期间按 `Enter` 可选择追加引导或打断重发，未读取的引导支持撤回；点击「停止」会立即取消当前回合、工具及子代理，同时保留已接收的内容和已完成的文件写入。
- 历史消息支持「撤回并编辑」以回退对话状态，**该操作不会回滚已修改的工作区文件**。

---

## 规则、Skills、MCP 与 CodeMode

### 规则与配置热更新

- Magpie Library 会同步全局规则、Skills 与 MCP 配置；项目级规则按 `AGENTS.md` → `MYCODE.md` → `.mycode/agents.md` → `.agents/AGENTS.md` 顺序加载，后者覆盖前者冲突项，用户显式指令具有最高优先级。
- Library 中的 MCP、Skills、角色与设置变动会自动刷新桌面状态；运行中的主代理与子代理在下一次模型请求前读取最新规则与目录（Skills 默认仅注入名称、描述与路径，正文按需读取）。
- MCP 工具通过 `search_tool` / `tool_search` 发现 schema 后经 `use_tool` 执行；收到 `notifications/tools/list_changed` 后重新同步目录。

### CodeMode（QuickJS 脚本执行）

主代理除了原生平台 shell，还通过 `run_code` 与 `wait_code` 在内置 QuickJS 沙箱中执行顶层 `async/await` 脚本：

```js
// 并发执行只读检索；写入类与 shell 工具自动保持严格串行
const [hits, cargo] = await Promise.all([
  callAsync("grep", { pattern: "spawn" }),
  callAsync("read_file", { path: "Cargo.toml" }),
]);
```

- 每次脚本最多执行 **256 次** 工具调用（旧的同步 `call(name, args)` 同样保留兼容）。
- 不提供 Node.js、浏览器 DOM 或 `fetch`；异步长任务返回 `cell_id` 后使用 `wait_code` 等待同一次运行（工具和 shell 仍以当前用户权限运行，worktree 与脚本配额不构成 OS 级隔离）。
- 详细机制见 [CodeMode 使用说明](wiki/architecture/code-mode-engine.md)。

---

## 子代理协作

主代理在面对多模块调研、较大范围改动或独立验证时，会按需调度内置子代理：

| 角色 | 核心职责 | 执行边界 |
| --- | --- | --- |
| `scout` | 快速检索代码结构、文档与调用证据 | 只读，禁用 shell |
| `artisan` | 实施范围明确的修改并运行检查验证 | 默认隔离于独立 Git worktree |
| `sentinel` | 独立审查实现、diff 与假设反例 | 只读，禁用 shell |

- 可在全局 `~/.mycode/agents/` 或项目 `.mycode/agents/` 自定义或覆盖角色，并在设置中随时启停；内置只读角色不会因提示词覆盖而获得写入能力。
- 写入类调用默认严格分离工作路径与变更目标，子代理不会自动执行 git commit、merge 或 push。
- 通过 `agent_status`、`agent_stop` 与 `agent_message` 跟踪任务进度、中断运行或复用已有工作树延续后续任务。

---

## 本地数据目录

所有持久化数据默认保存在 `~/.mycode`（可在启动前通过绝对路径环境变量 `MYCODE_HOME` 覆盖，需与 Magpie 保持一致）：

| 路径 | 说明 |
| --- | --- |
| `settings.json` | 网关绑定、外观与字号、网络、代理角色和技能配置 |
| `ui.json` | 项目清单、会话目录绑定、模型与窗口几何状态 |
| `sessions.db`、`sessions/` | SQLite 会话索引、事件账本、附件缓存与压缩检查点 |
| `AGENTS.md`、`skills/`、`mcp.json` | 全局规则、技能清单与 MCP 服务定义 |
| `secrets.json` | 本地保存的 MCP 密钥凭据 |
| `agents/`、`agent-worktrees/`、`agent-results/` | 自定义子代理、保留的工作树和完整输出归档 |
| `mcp-results/`、`scratch/`、`bin/`、`update/` | 大型工具结果转存、临时文件与安装包暂存 |

所有 JSON 配置文件采用容错读取与备份策略，单次读取异常不会清空已有配置。导出或分享诊断前请注意脱敏 `secrets.json` 等敏感字段。

---

## 参与开发

工作区简洁划分为两个 Cargo package：

| Crate | 职责 |
| --- | --- |
| `mycode` | 核心运行时：`core`、`config`、`providers`、`tools`、`agent`、`app`、`frontend`（零 GPUI 依赖） |
| `mycode-desktop` | GPUI 原生窗口、Paper-Pixel 主题界面渲染、剪贴板与桌面交互 |

### 环境要求

- **Rust stable**（2024 edition）
- **Windows**：MSVC + C++ Build Tools（QuickJS 编译需要 C 编译器）
- **macOS**：Xcode Command Line Tools
- **Linux**：参考 `.github/scripts/install_linux_deps.sh` 安装图形与桌面开发库（Linux 当前用于 CI 与开发测试）

### 常用命令

```sh
git clone https://github.com/MCapricorns/mycode.git
cd mycode

# 开发运行与发行构建（Debug 构建跳过自安装；Release 测试可设置 MYCODE_NO_INSTALL=1）
cargo run -p mycode-desktop --locked
cargo build --release -p mycode-desktop --locked
```

提交前建议完整执行以下检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo test --workspace --locked
cargo test -p mycode-desktop --features test-support --locked
python scripts/make_icon.py --check
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

- 架构设计与源码深入导览：[运行时架构](wiki/architecture/runtime.md) · [CodeMode 引擎](wiki/architecture/code-mode-engine.md) · [性能记录](wiki/memory-report.md) · [Wiki 维护说明](wiki/README.md)
- CI 自动覆盖 Windows x64 / ARM64、macOS Apple Silicon 与 Ubuntu 24.04 x64；触发 `[release]` 推送需提前在 `CHANGELOG.md` 写入具体变更说明。

---

## Contributors & 许可

感谢参与开发的人类贡献者：

- [MCapricorns](https://github.com/MCapricorns)
- [YangChengxxyy](https://github.com/YangChengxxyy)

反馈与建议：[GitHub Issues](https://github.com/MCapricorns/mycode/issues) · 作者动态：[@M_Capricorns](https://x.com/M_Capricorns) · 开源许可：[Apache-2.0](LICENSE)
