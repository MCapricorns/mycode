# Changelog

日期为发布日（UTC）。0.12.0 是第一个正式版本；之前的预览版本及其 Release 已移除，历史可在 git 记录中查看。

`main` 上只有提交主题带 `[release]` 标记的推送才会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64 `x86_64-pc-windows-msvc`、Windows ARM64 `aarch64-pc-windows-msvc`、macOS Apple Silicon `aarch64-apple-darwin`，压缩包名形如 `mycode-desktop-vX.Y.Z-{target}.zip`；没有 Linux 包）。标记匹配的是本次推送新增的任一提交主题（合并提交也算），不要求写在最顶上那条。pull request 不发版，也不跑发布构建：PR CI 只在 Windows 上执行 `cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings`（dev/test profile，没有 fat LTO）。不带标记的 `main` 推送同样只跑检查（Windows 测试加 macOS `cargo check`），不跑 fat LTO 发布构建、不打标签、不 bump 版本；带 `[release]` 的推送才在三个平台做发布构建，同一份检查通过且三个构建都成功后才打标签。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。压缩包未签名。

## [Unreleased]

### Changed

- 转录区各类内容改为卡片区分：助手回答、工具调用组各自成框，思考收进独立卡片——淡紫底色只留在边框与标题上，正文改用常规字体与中性墨色，不再是小号紫色斜体，与正文一眼可分。
- 界面加克制的配色层次，全部取自已有设计 token：工具行图标按类别着色（读取/抓取蓝、编辑与代码紫、搜索琥珀、shell 绿、子代理靛蓝），说话人名字与头像同色，思考卡边框改用思考色描边，表格里的行内代码改用强调底色加强调字色；输入框有草稿时发送键用主按钮的强调填充与描边，引导键用强调底色，发送选择浮层的「引导」「打断」两行图标分别用强调色与危险色区分。
- 工具行渲染开销削减：`run_code` 行每次渲染的状态计数从「完整拆分结果——逐行收集、拼接脚本输出、为每个内层调用建对象」改为零分配的单遍计数扫描；展开的 diff 预览只对实际绘制的前 80 行编号（原来每次渲染为全部行逐行克隆字符串再丢弃 80 行之后的部分）；展开的普通工具结果不再在每次渲染时收集整份行向量，只在 diff 与搜索预览真正需要按行索引时才收集。

### Added

- 回合进行中按回车不再直接引导：先在输入框上方弹出小浮层选择「引导本轮」（代理在下一步之前读取，当前回合不中断）或「打断并发送」（停止本轮和所有子代理后立即发送）；浮层内回车确认引导、Ctrl+Enter 确认打断、Esc 关闭且草稿保留。Ctrl+G 与停止键旁的箭头仍可直接引导，Ctrl+Enter 仍可直接打断发送。
- 已发出的引导可以撤销：「引导中」条上每条待读引导带「撤销」，点击后核心从引导队列中真正移除该条（已被回合读到的不可撤销），文本回到输入框，可修改后再发。
- 复制出去的内容一律是纯文本：助手回答卡片新增悬停动作「复制为纯文本」，整条回答复制时剥除全部 markdown 标记——标题、粗体、斜体、删除线、行内代码与代码围栏只保留文字与代码本身，表格转为制表符分隔的行，图片保留替代文字，链接保留文字、文字里没有地址时在括号里附上 URL，未闭合的标记与词内下划线（如 `snake_case`）按字面保留；转录区的选区复制也明确钉死为纯文本，所见即所复制，不再带字体格式或 markdown 源码。

## [0.12.30] - 2026-10-24

### Fixed

- code mode 下所有内层工具只显示为裸 `run_code`：内层 `call(...)` 现在逐次记录工具名、目标、成败与 diff 并写入结果，`run_code` 行标题回退解析脚本中的 `call("…")`、显示真实工具摘要（如 `read · grep · edit`），展开后每个内层调用按真实工具名渲染、edit/write 带 diff 预览，`run_code`/`wait_code` 也有专属动词、图标与调用计数。
- subagent 面板打不开：dispatch 转发内层进度时给 `agent|role|phase` 协议行加了名字前缀，前端严格匹配失败导致 LiveJob 永远建不起来；现在协议行原样转发，subagent 的运行行、取消按钮与面板恢复。
- Linux 下 workspace 编译失败：`fcntl_fullfsync` 按平台分流（Apple 用 F_FULLFSYNC、其他 unix 用 fsync）、errno 改用 `__errno_location`，并补上 `canonical_from_handle` 与 `list_descendants` 的 Linux 实现。

## [0.12.29] - 2026-10-23

### Changed

- 带 `[release]` 标记的 `main` 推送发布。

## [0.12.28] - 2026-10-23

### Changed

- 技能与 MCP 全部收敛到 magpie Library：技能只读 `~/.mycode/skills`，MCP 只读 `~/.mycode/mcp.json`；`.agents` 技能树、`~/.agents/AGENTS.md`、项目 `.mycode/mcp.json`、settings.json 的 `mcpServers` 行，以及导入 JSON / 添加自定义服务器的设置页入口全部移除。
- 顶层工具面收窄为 code-mode-only：模型只能直接调 `shell`、`run_code`、`wait_code`，`read`/`write`/`edit`/`grep`/`find`/`ask_user`/`agent`/MCP 元工具改为在 `run_code` 脚本里经 `call("name", args)` 调用；子代理同样。

### Fixed

- 设置页技能列表空白：前端给 `discover_skills` 传了空的 `mycode_home`，现在传真实 home，`~/.mycode/skills` 的库技能正常显示。
- Windows 上 magpie 插件整文件替换 `settings.json` 后文件回落为目录继承 ACL（多出 `BUILTIN\Administrators:(F)`），原先严格校验把它当作外部写者、隔离并重置配置导致「已重置无法读取的配置」反复弹窗；现在把 Administrators 与 SYSTEM 一样视为环境主体，直接原地静默修复 DACL，其他外部主体的写权限仍然拒绝修复。

### Removed

- `McpSubview`/`ShowMcpSubview`/`McpTransportMenuToggled`、`mcp_transport_menu_open`、`SettingsBack::McpList`、`pending_mcp_enable`、`import_confirm`、`IMPORT_NEEDS_MCP_CONFIRM`、数据导入的 MCP 确认门，以及 `AppSettings.mcp_servers` 和 `validate_mcp`。

## [0.12.27] - 2026-10-23

### Added

- 代码模式：新增 `run_code` 与 `wait_code` 两个内置工具。`run_code` 执行模型写的 Rhai 脚本，脚本里每个工具都是 `call(name, args)`（返回 `{ content, is_error }`），`log(text)` 报进度，`complete(value)` 结束；`wait_code` 用 cell id 跨回合继续等待或停止脚本。脚本跑在独立 `std::thread` 上，不阻塞异步运行时；`yield_time_ms` 默认 30 秒、上限 5 分钟，超时先返回 cell id。cell 表挂在核心状态上、按会话隔离，已完成 cell 最多保留 32 个。
- 脚本沙箱的固定上限：100 万次操作、64 层调用栈、4 MiB 字符串、65536 项数组/映射、每次脚本 256 次宿主调用；禁用 `eval`/`print`/`debug` 与模块解析器，宿主函数白名单只有 `call`、`log`、`budget`、`complete`、`yield_control`。script journal 按顺序记录每次步骤的 `blake3`（规范化 JSON 的参数与结果），重放时能发现「同一次调用返回了不同结果」。
- `tool_search`：对已就绪的 MCP 工具建 BM25 索引，按关键词返回最多 8 个工具的完整 `inputSchema`，返回的名字可直接交给 `use_tool`，不必再逐个 `search_tool`。
- `wiki/architecture/code-mode-engine.md`：代码模式引擎的分层、预算、取消与 cell 生命周期，以及 MCP 渐进连接的设计。

### Changed

- MCP 改为渐进连接：回合开始只读密钥并合并三处服务器来源（几毫秒磁盘 IO），然后为每台服务器起一个后台握手并立即返回；谁先就绪就先登记谁（先写 `McpCatalog`、再写 `ToolRegistry`），首个请求不再等待任何握手。失败按服务器各自 120 秒冷却重试；握手任务归连接池所有，回合结束只停该回合的监听。
- 系统提示词的 `<mcp>` 段与子代理说明：`search_tool` / `tool_search` 标为只读、可与其他独立调用同批发出；仍在握手的服务器明确标为 connecting，工具就绪后出现在工具表上。
- 流式增量事件 `BridgeEvent::ChatText` / `ChatThinking` 的 `session_id` 改为 `Arc<str>`，前端按借用比较，不再每 token 复制一次字符串；`reduce` 对增量动作跳过项目目录快照。
- 去掉热路径上的多余拷贝：JSONL 解码用 `payload_text.take()` 移出内联 payload、无附件提示词按 `Cow` 借用、`shrink_overflowed` 重建请求而不是克隆、`stream_assistant` 返回历史里已共享的 assistant 消息、`session_id` 只在非增量事件上转成 `String`。
- 发版从"每次 `main` 推送自动一版"改为按标记触发：提交主题带 `[release]` 的推送才发布并 bump 补丁号，平时推送只跑测试与检查，不再每次多一个 `chore(release)` 提交；标记匹配本次推送的任一提交（含合并提交）。

### Removed

- 在本机 Windows 上间歇性挂起的测试：依赖 `%TEMP%` 文件 IO 的 `actor/tests.rs`、`store/tests.rs`、`turn/tests.rs`、`turn/tests/compaction.rs`、`turn/tests/pairing.rs`、`turn/tests/prefix.rs`、`ledger` 与 `bridge_tests` 的会话/桥接用例，以及会真实执行 `git init/add/commit` 的 `git_status/git_integration_tests.rs` 和 driver 的 git 监视测试；纯逻辑覆盖保留（消息顺序、`user_has_content`、通道失败路径等）。
- 无调用者的公开项：`active_shell`、`script_shell_line`、`rank_model_ids`/`model_strength`、`ReasoningLevel::is_valid_token`、`MAGPIE_PROVIDER_NAME`/`DEFAULT_MAGPIE_BASE_URL`/`MAGPIE_KEY_PREFIX`、`KIND_ANTHROPIC_MESSAGES`/`KIND_OPENAI_RESPONSES`、`FrontendDriver::take_composer_prefill`/`ui_state_loaded`、`launch()`、`send_message()` 测试辅助。
- `store::open_connection` 的 `create_schema` 参数：所有调用方都建表。

## [0.12.26] - 2026-10-22

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.12.25] - 2026-10-22

### Changed

- MCP 工具不再按服务器大小分两类：之前「工具不超过 4 个、且每个 schema 不超过 2048 字节」的服务器直接注册成普通工具，服务器多一个工具或改一次 schema 就换一类，模型工具表随之变动；现在一律不注册，只留 `search_tool` 与 `use_tool` 两个 schema，工具清单与服务器声明的工具无关。
- 系统提示词的 `<mcp>` 段改为逐行列出本轮连上的 MCP 服务器：服务器 id、工具数，以及服务器握手 `instructions` 的首句（压成一行，最多 160 字符），后面说明工具都走 `search_tool` / `use_tool`；之前那段只列服务器名和直接注册的工具。
- `search_tool` / `use_tool` 的三个按名解析失败分支（`use_tool` 未命中、`search_tool` 精确名与关键词搜索双未命中、`names` 批量里的单名失败）对内置工具名给出「该工具已在工具表上，直接调用」的定向纠正；子代理的纠正名单取自其角色 allowlist，之前这些误调只得到泛化的「没有这个 MCP 工具」。
- 技能发现的元数据（title 与 description）按 `(path, mtime, len)` 做进程级缓存：只缓存成功读取、上限 512 条，回合、子代理运行与前端刷新不再每次整文件读盘；SKILL.md 正文永不缓存，改写文件后缓存随 `mtime` 或长度变化失效。

## [0.12.24] - 2026-10-22

### Added

- 系统提示词的 `<mcp>` 段现在列出本轮已连接的 MCP 服务器名（含 magpie Library 写入的服务器），再列直接注册的工具并说明其余工具走 `search_tool` / `use_tool`：之前只写「还有其他 MCP 工具」，模型无从知道哪台服务器在、该搜什么。

### Changed

- 回合进行中按 `Enter` 改为引导（steer）：草稿在本轮下一步之前交给模型，不打断回合；`Ctrl-Enter` 仍是停止本轮并紧接着发送，`Ctrl-G` 与 `Enter` 等价保留。输入框旁的第二个圆钮由「排队」改为「引导」，回合中也不再显示排队面板。
- 打断时立即丢弃正在跑的工具调用，不再等它跑完：`shell` 的进程树随 future 丢弃一起结束，`edit` 等阻塞工具的写入也被放弃，它们以「已取消」的记账收尾。之前停止后还会等着那条命令执行完。
- magpie Library 的 `~/.mycode/mcp.json` 只读服务器现在参与设置页的 MCP 计数、`/` 菜单的服务器列表和「来自 magpie」卡片（同一份状态，同 id 的自有服务器优先）；`mcp.json` 读不了时卡片显示原因，而不是当作没有服务器。

### Removed

- 排队功能：`queued` / `queue_paused` / `parked_queues` 状态、`MessageQueued` 与召回/发送/打断整队那组动作、排队面板与 Esc 撤回队列、以及设置快捷键表里的排队描述。引导在本轮结束前没被读到时会回到输入框（原本是回到队列）。

### Fixed

- 停止时消息已经发出、模型却停下：之前 `Ctrl-Enter` 先把草稿排进队列再打断，队列在本轮结束时自动起新回合，紧接着那次停止会落到新回合上；现在打断发送的草稿由驱动暂存，等本轮真正结束（`ChatFailed` 的静默取消）后再作为下一条发出，消息不会再被同一次停止杀掉。

## [0.12.23] - 2026-10-22

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.12.22] - 2026-10-22

### Added

- `wiki/memory-report.md`：内存占用静态审查与构建基准（依赖图 427 个包、dev 69.8 MB / release 38.1 MB 二进制、mimalloc 覆盖验证与全部内存上限清单）。
- 桌面版输入框支持粘贴剪贴板图片：`Ctrl+V`（macOS `Cmd+V`）把剪贴板里的图片附加到下一条消息，粘贴内容只有一个图片路径或 `file://` URL 时也按图片附加。平台剪贴板已给出 PNG / JPEG / WebP / GIF 的按原样使用，Windows 截图给的位图先转成 PNG；图片同样受 4 张 / 3.75 MB / 合计 6 MiB 和「所选模型能否看图」的限制，纯文本粘贴仍然照常插入正文。

### Changed

- 工作区重排为严格线性链：每个 crate 只依赖链上的上一层（`pub use` 转发其公共 API），`mycode-frontend` / `mycode-desktop` 不再跨层直连底层 crate；`mycode-config` 接入 `mycode-core` 后，链上全部二进制与测试目标统一获得 mimalloc 全局分配器与 VC-LTL 链接，随之删除 `mycode-config` 重复的 VC-LTL 声明与三份重复的 `unix_now` 帮助函数。
- 精简 Cargo.toml 与 crate 级文档注释：去掉叙事段落与 ASCII 图，压成一句式「为什么」；线性链图示见 `wiki/images/architecture.svg`。
- 移除终端版 `mycode-cli`：桌面版是唯一的前端，工作区、发布构建、自更新资产、自安装与下载清理、快捷键表都只保留 `mycode-desktop`。启动参数、`--help` / `--version`、以及自更新助手仍由 `mycode-frontend` 的 `launch` 模块提供。
- `docs/` 换成 `wiki/index.html`：一份可搜索的知识图谱，节点覆盖 crate、模块、流程与落盘数据。点节点看说明、该层的关联节点，以及它背后的源文件；点文件名直接在页面里读源码（行号、跳到 GitHub、一键复制）。整页自包含，双击也能离线打开。
- 把超过 1000 行的文件拆开：`highlight`、`tool`、`edit/engine`、`session/store/index`、`view_model/reduce`、`builtin/shell`、`builtin/grep`、`mcp_tools`、`compaction`、`ui/chat/tools`、`workspace` 及若干测试文件，各自按职责落到子模块。

### Removed

- 源码注释里描述已不存在的实现与历史缺陷的段落（终端版、已删除的 `docs/`、旧版本行为），以及几条只断言常量字面量的测试。

### Fixed

- 桌面版自安装的 PATH 代码只在 Windows 编译，macOS 发布构建不再因 dead code 失败。
- `install.rs` 里 unix 侧的 PATH 注册实现（改 shell profile 的那套）：桌面版只在 Windows 自安装，macOS 走 dmg 拖进 `/Applications`，这些函数从来没被调用。

### Fixed

- macOS 发布构建重新通过：`install_file`、`ensure_path_entry` 及其 PATH 读写辅助函数（连同只在 Windows 用得到的 `std::io` 导入）只在 Windows 编译，不再在 macOS 上被判为 dead code、unused import（发布构建按 `-Dwarnings` 把警告当错误，上一轮的三平台发布因此没有发出去）。
- README 顶部恢复 banner，架构一节恢复分层架构图与消息流程图；图从 `docs/images/` 移到 `wiki/images/`，架构图按现在的八个 crate 重画（去掉已删除的终端版）。
- 新增 `check-macos` 作业：在 macos-latest 上跑 `cargo check --workspace --all-targets` 并把警告当错误。Windows 上的检查看不到 `cfg(not(windows))` 的代码，0.12.22 的三平台发布就是被这样一段 unix PATH 安装代码卡住的。发布现在也要求这个作业通过。

## [0.12.21] - 2026-10-21

### Fixed

- 桌面版侧栏的目录块不再固定 148px 高：它按标题栏、操作条、两个分段标题和底栏之外的剩余空间取一半，第三个目录的路径不再被「会话」标题切掉，会话列表整体下移；目录更多时在块内滚动，会话区保留另一半。
- 作曲栏的模型芯片放宽到 240px，放得下就完整显示 `服务商 · 模型`；名字过长时从左边省略、保留模型名，并去掉会盖住旁边思考芯片的悬停提示，完整名称在模型菜单里。

## [0.12.20] - 2026-10-21

### Added

- magpie 侧按会话归组请求：会话请求带 `x-magpie-session: <会话 id>` 头（与 `prompt_cache_key` 同值），magpie 的 Sessions 页据此列出、录制和删除对话；标题生成和压缩摘要这类不属于对话的请求不带。只有打开 magpie 的「录制对话」时才保留正文，关闭时只看到会话本身。
- 终端版可以滚屏：鼠标滚轮与 `PageUp` / `PageDown` 都能翻看转录（滚轮一格三行），设置页的滚轮移动游标。之前只开备用屏幕、不收滚轮事件，滚轮什么也不做。开启鼠标上报后终端自己的划选要让开鼠标：Windows 终端和 GNOME 终端按住 `Shift` 拖选、`Ctrl+Shift+C` 复制，iTerm2 等 macOS 终端按住 `Option` 拖选、`Cmd+C` 复制。

### Fixed

- 终端版设置页的 MCP 段不再只列 `settings.json` 里的服务器：magpie Library 写入 `~/.mycode/mcp.json` 的服务器也作为只读行显示（含地址或命令行），magpie 侧已禁用的行标 `· off`，`mcp.json` 读不了时如实报错；两个来源都为空才显示「(none configured)」。之前只有 magpie 托管服务器时这一栏看起来一个都没配置，即使它们已经连上、工具可用。
- `search_tool` 的 `"list"` 在工具全部直接注册时列出这些名字（`Direct tools are called by name: …`），在有延迟工具时把直接工具附在后面；之前只回一句「No deferred MCP tools」，看起来像 MCP 没有连上。

## [0.12.19] - 2026-10-21

### Added

- 输入框支持拖放附件：图片按多模态内容直接发给模型，代码/文本文件以 `File: <名称>` 围栏块随消息发送，用户消息下方列出每个附件名称。上限为 4 张图片（每张 3.75 MB，base64 后 5 MiB，合计 base64 不超过 6 MiB）、8 个文件（每个 256 KiB、合计 512 KiB），二进制文件和非 UTF-8 文本会被拒绝并说明原因；图片只发给目录里声明了图像输入的模型，发送前再检查一次，其他模型会提示换模型或去掉图片。欢迎页只拖入附件也能直接发送，用户消息里的附件说明只出现一次。
- 终端版也能附加图片：`Ctrl+V` 把剪贴板里的图片挂到下一次发送（Windows 读原生剪贴板，优先取应用发布的 PNG，否则把 DIB 重新编码为 PNG；macOS 通过 `osascript` 取剪贴板的 PNG，只有 TIFF 时用 `sips` 转成 PNG），粘贴内容只有一个图片路径或 `file://` URL 时也按图片附加，`Ctrl+X` 清除；输入框上方一行列出待发送的附件，图片同样受 4 张 / 3.75 MB / 合计 6 MiB 和模型是否支持图像输入的限制。
- `read` 工具能读图片：按文件头识别 PNG、JPEG、GIF、WebP，并以图像块直接交给模型查看（截图存到磁盘后可以再读回来），不再报「不是 UTF-8」；`offset`/`limit` 只用于文本。Anthropic Messages、Chat Completions、Responses 三种协议都会把工具结果里的图片传给模型。

### Fixed

- 本机 magpie 网关的明文 `http://127.0.0.1` 不再被「requests must use https」拒绝：只有首跳、主机是回环名、且解析出的每个地址都是回环时才允许明文 http；公网主机、混合地址、以及从公网跳转过来的降级仍然拒绝。
- Windows 上配置文件的 DACL 漂移（同用户工具写入时多出的只读 logon-session ACE）现在在读取时就地重装受保护 DACL 后继续，不再每次启动都「配置已重置」并弹修复提示；有其他主体可写、所有者为他人、DACL 为 NULL 或含无法解析的 ACE 时仍按损坏处理。
- 设置 → Magpie 的模型列表收成一个下拉，状态行和下拉用同一个显示名（之前状态行显示原始模型 id）；输入框的模型条显示 `服务商 · 模型`，不再只显示模型名，两个服务商同名的模型也能分清。
- 终端版外观里的 Theme 之前只写进 `settings.json`、界面永远按深色画；现在 `dark` / `light` 会切换次要文字、强调色和思考/排队/报错颜色（浅色下 `DarkGray` 几乎看不见，改用可读的深灰）。`/new` 和 `Ctrl+N` 打开的空会话显示欢迎页，不再是空白转录。
- 附件在发送被拒时留在输入框：没有打开目录、回合仍在运行、欢迎页的发送已挂起或已失败时，`submit` 会把附件交还前端，不再静默丢掉或粘到下一条消息；发送失败时文字和附件一起回到输入框。回合进行中只排队文本，附件提示等回合结束。

## [0.12.18] - 2026-10-21

### Added

- 设置文档的权限被外部写入者（magpie 的 agent 插件替换文件）改掉后，启动修复现在会从隔离备份里抢救字段再重新发布：`magpie` 网关绑定不再因为插件重写 `settings.json` 而整段丢失。之前每次插件写入都会让 MCode 报「magpie 未连接」并清空模型列表，直到手工恢复。

### Fixed

- 桌面版默认窗口从 1760×1040 改为 1440×900，最小尺寸从"和默认一样大"改为 960×600：之前默认尺寸同时是窗口下限，在 1080p、150% 缩放的屏幕上窗口几乎铺满整屏且一点都拖不小。现在打开尺寸居中且留出余量，用户可以拖到 960×600，`ui.json` 会记住拖动后的尺寸（低于下限时按 960×600 打开）。
- `Ctrl+G`（引导）在没有运行中的回合时不再退化成发送：之前会把草稿排进压缩队列，现在只提示"没有可引导的任务"，不再产生队列消息，空草稿也不会顺带释放暂停的队列。
- `/com` 这类前缀弹窗打开时按一次 Enter 只执行一次：之前捕获阶段先补全并执行 `/compact`，延迟到达的 Enter 又把原始 `/com` 当成跟进消息排队，压缩结束后还会真的发给模型。
- 差异面板现在和「改动」抽屉一样响应外部点击：之前点别处只会关掉抽屉或详情，Diff 面板没有遮罩，只能按 Esc 或点「关闭」。
- 用户消息按纯文本渲染，粘贴进来的 Markdown、字体和富文本样式不再重新排版；换行保留。
- 连不上 magpie 网关时，第一次启动会提示安装或启动 magpie 并给出 `magpie plugin add` 命令；提示过之后记在 `ui.json`，不会每次启动重复。设置里的 Magpie 页也显示同样的安装步骤。
- `edit` 的旧文本差在缩进、行尾空格或换行位置时自动按空白不敏感的方式匹配并明确注明；`line_range` 的行号过期但 `expected_text` 仍唯一时，改到该文本所在位置而不是整次失败。
- 工具参数在 schema 校验前统一修补，不再让模型为拼写差异多花一轮：`required` 字段接受其它框架的叫法（`file`/`filename`→`path`、`text`/`body`→`content`、`query`/`glob`→`pattern`、`task`/`instructions`→`prompt`、`name`/`role`→`agent`、`q`→`query` 等，命中的拼法不是 schema 声明属性时才生效）；字符串形式的数字/布尔在任意深度按 schema 转成标量；带 `const`/单值 `enum` 标签的 `oneOf`/`anyOf` 对象缺少标签时按字段推断变体、拼写只差大小写或连字符时改正，并丢掉该变体没有声明的多余键（例如 `type_note`）；整个参数对象被编码成 JSON 字符串时先解析。空对象、认不出的字段、没有标签的联合仍然按原样报错，不会猜一个变体。MCP 工具用服务器 schema 走同一套修补与错误渲染（不做 camelCase 转换，服务器可能就声明成驼峰）。
- `edit` 的 `operations` 不再因为缺少 `type` 标签或多出一个说明字段（例如 `type_note`）而整次拒绝：按操作里实际出现的字段推断 `literal`／`regex`／`fuzzy`／`line_range`，并只保留该变体的字段（`old_string`/`new_string`、`search`/`replace`、`start`/`end`、`replace_all`、`distance` 这些叫法也一并映射，字符串形式的行号和距离会转成数字）。缺少标签的调用之前直接返回 `invalid arguments: value at /operations/0 is missing required field type`，模型只能重发一次；现在同一批操作直接生效。字段仍然按变体校验（例如 `literal` 必须给 `pattern`+`replacement`），认不出字段的操作照旧报错。
- 设置保存遇到 revision 冲突后不再永久卡死：失败时会重新加载文档对齐当前 revision，之后每次编辑都能正常落盘；重新加载同时把磁盘上新的 magpie 绑定同步进编辑器。
- 手动「刷新模型」在 magpie 未连接时不再假报成功：之前清空模型列表却提示「服务商目录已刷新」，现在如实报错并区分「从未绑定」和「绑定不完整」。
- 自动压缩结束后的补停止不再丢失：压缩期间切到别的会话时，停止请求直接发给对应会话，而不是随会话切换被丢弃。
- 切换会话不再丢掉刚结束的子代理运行时长：transcript 行上的耗时照常显示。
- `edit` 被取消时不再向模型误报「pattern not found」：模糊回退扫描里的取消如实返回取消错误。
- Chat Completions 流的 `reasoning_details` 累积帧不再每次全量重建拼接文本（长推理输出 O(L²) 拷贝），首帧直接采纳、非累积帧合并逻辑不变。
- 会话 actor 的互斥锁中毒不再让整个会话服务永久 panic；重命名/删除会话索引不再开第二个执行 `journal_mode` pragma 的写连接，避免与正在运行的回合锁冲突。
- 请求体序列化失败不再静默发送空 body，按协议错误报告。
- shell 工具的参数 schema 缓存键加入 shell 种类，`Pwsh` 与 `WindowsPowerShell`（同名 `powershell`）不再可能共享一份缓存 schema。

### Changed

- 整套模型、MCP、技能和规则改为由 [magpie](https://usemagpie.ai) 统一提供：新增 `magpie` 设置段（网关地址、`magpie-mycode` 密钥、选中的模型），模型列表实时读取网关 `GET /v1/models`；自带供应商体系（models.dev 目录、厂商预设、Copilot/Codex 设备登录、手动 provider 配置）与 `catalog-cache.json` 全部移除。MCode 现在只通过本机 magpie 网关请求模型，关掉 magpie 就没有模型来源。
- 内置 `web_search` / `fetch_content` 与 Settings → 网页搜索一起移除：联网检索改为 magpie Library 下发的 MCP 服务（如 anysearch），系统提示词不再有 `<web>` 段，`scout` 角色只保留 `read` / `grep` / `find` 和只读 MCP 工具。
- MCP 设置页去掉内置的 Context7 / GitHub 目录；magpie Library 写入的 `~/.mycode/mcp.json` 作为只读托管行显示，技能从 `~/.mycode/skills` 发现，规则从 `~/.mycode/AGENTS.md` 读取。
- 新增独立的 [magpie-mycode](https://github.com/MCapricorns/magpie-mycode) 仓库：magpie 的 agent 插件，把网关地址、密钥和 magpie 选中的模型写进 `~/.mycode/settings.json`，并声明 instructions / MCP / skills 三个 Library 位置。用 `magpie plugin add github:MCapricorns/magpie-mycode` 安装。
- 设置页「关于」去掉已无意义的「服务商目录」日期行（模型列表的刷新入口在 Magpie 页）；模型选择器为空时提示安装 magpie 而不是「去设置添加服务商」；`docs/` 与各 crate 注释同步 magpie 迁移后的现实（移除 OAuth、网页、models.dev 目录的描述）。

## [0.12.17] - 2026-10-21

### Changed

- 整套模型、MCP、技能和规则改为由 [magpie](https://usemagpie.ai) 统一提供：新增 `magpie` 设置段（网关地址、`magpie-mycode` 密钥、选中的模型），模型列表实时读取网关 `GET /v1/models`；自带供应商体系（models.dev 目录、厂商预设、Copilot/Codex 设备登录、手动 provider 配置）与 `catalog-cache.json` 全部移除。MCode 现在只通过本机 magpie 网关请求模型，关掉 magpie 就没有模型来源。
- 内置 `web_search` / `fetch_content` 与 Settings → 网页搜索一起移除：联网检索改为 magpie Library 下发的 MCP 服务（如 anysearch），系统提示词不再有 `<web>` 段，`scout` 角色只保留 `read` / `grep` / `find` 和只读 MCP 工具。
- MCP 设置页去掉内置的 Context7 / GitHub 目录；magpie Library 写入的 `~/.mycode/mcp.json` 作为只读托管行显示，技能从 `~/.mycode/skills` 发现，规则从 `~/.mycode/AGENTS.md` 读取。
- 新增独立的 [magpie-mycode](https://github.com/MCapricorns/magpie-mycode) 仓库：magpie 的 agent 插件，把网关地址、密钥和 magpie 选中的模型写进 `~/.mycode/settings.json`，并声明 instructions / MCP / skills 三个 Library 位置。用 `magpie plugin add github:MCapricorns/magpie-mycode` 安装。

### Fixed

- 桌面版默认窗口从 1760×1040 改为 1440×900，最小尺寸从"和默认一样大"改为 960×600：之前默认尺寸同时是窗口下限，在 1080p、150% 缩放的屏幕上窗口几乎铺满整屏且一点都拖不小。现在打开尺寸居中且留出余量，用户可以拖到 960×600，`ui.json` 会记住拖动后的尺寸（低于下限时按 960×600 打开）。
- `Ctrl+G`（引导）在没有运行中的回合时不再退化成发送：之前会把草稿排进压缩队列，现在只提示"没有可引导的任务"，不再产生队列消息，空草稿也不会顺带释放暂停的队列。
- `/com` 这类前缀弹窗打开时按一次 Enter 只执行一次：之前捕获阶段先补全并执行 `/compact`，延迟到达的 Enter 又把原始 `/com` 当成跟进消息排队，压缩结束后还会真的发给模型。
- 差异面板现在和「改动」抽屉一样响应外部点击：之前点别处只会关掉抽屉或详情，Diff 面板没有遮罩，只能按 Esc 或点「关闭」。
- 用户消息按纯文本渲染，粘贴进来的 Markdown、字体和富文本样式不再重新排版；换行保留。
- 连不上 magpie 网关时，第一次启动会提示安装或启动 magpie 并给出 `magpie plugin add` 命令；提示过之后记在 `ui.json`，不会每次启动重复。设置里的 Magpie 页也显示同样的安装步骤。
- `edit` 的旧文本差在缩进、行尾空格或换行位置时自动按空白不敏感的方式匹配并明确注明；`line_range` 的行号过期但 `expected_text` 仍唯一时，改到该文本所在位置而不是整次失败。
- 工具参数在 schema 校验前统一修补，不再让模型为拼写差异多花一轮：`required` 字段接受其它框架的叫法（`file`/`filename`→`path`、`text`/`body`→`content`、`query`/`glob`→`pattern`、`task`/`instructions`→`prompt`、`name`/`role`→`agent`、`q`→`query` 等，命中的拼法不是 schema 声明属性时才生效）；字符串形式的数字/布尔在任意深度按 schema 转成标量；带 `const`/单值 `enum` 标签的 `oneOf`/`anyOf` 对象缺少标签时按字段推断变体、拼写只差大小写或连字符时改正，并丢掉该变体没有声明的多余键（例如 `type_note`）；整个参数对象被编码成 JSON 字符串时先解析。空对象、认不出的字段、没有标签的联合仍然按原样报错，不会猜一个变体。MCP 工具用服务器 schema 走同一套修补与错误渲染（不做 camelCase 转换，服务器可能就声明成驼峰）。
- `edit` 的 `operations` 不再因为缺少 `type` 标签或多出一个说明字段（例如 `type_note`）而整次拒绝：按操作里实际出现的字段推断 `literal`／`regex`／`fuzzy`／`line_range`，并只保留该变体的字段（`old_string`/`new_string`、`search`/`replace`、`start`/`end`、`replace_all`、`distance` 这些叫法也一并映射，字符串形式的行号和距离会转成数字）。缺少标签的调用之前直接返回 `invalid arguments: value at /operations/0 is missing required field type`，模型只能重发一次；现在同一批操作直接生效。字段仍然按变体校验（例如 `literal` 必须给 `pattern`+`replacement`），认不出字段的操作照旧报错。

## [0.12.16] - 2026-10-20

### Changed

- `read`、`edit`、`write`、`grep`、`find` 的路径不再限于工作区：可以直接读写、搜索其他项目或依赖源码（例如 `~/.cargo/registry/src` 下的 crate），相对路径里的 `..` 也能走出会话目录。工作区内经过符号链接、联接点或挂载点的路径改为解析到真实位置后再访问，不再报“crosses a link”；macOS 上搜索也不再跳过有多个硬链接的文件。工作区外文件的结果显示解析后的绝对路径。之前这些路径都会被拒绝，模型只能改用 PowerShell/bash 去读。

### Fixed

- 桌面版里用 shell 读文件的调用（`Get-Content`、`cat`、`head`、`tail`、`sed -n` 等只输出一个文件的命令，可接 `Select-Object`/`head`/`tail` 过滤）按文件扩展名语法高亮，和 `read` 的结果一致；之前这类输出只显示纯文本。

## [0.12.15] - 2026-10-20

### Fixed

- 桌面版不再允许对同一个家目录多开：第二次启动会把已打开的窗口切到前台（Windows）后退出。之前两个桌面进程打开同一个对话并各自发送时，后写的一方会把另一方已提交的记录当作残尾截掉，导致会话校验失败、消息丢失。终端版不受限制，仍可与桌面版同时运行。
- 同一条回答里并行的多个子代理中，有的已经答完、有的还在运行时取消回合，答完的子代理保留真实结果。之前它会再收到一条「已中止」结果，同一个调用在会话里被关闭两次，回合以 `invalid request` 报错结束而不是正常停止，仍在运行的子代理的中止结果也没有写入会话。
- 启动时清理更新临时目录不再删掉另一个仍在运行的 MYCode 进程的下载目录。之前桌面版下载更新期间打开终端版（或反过来），正在下载的文件会被删掉，更新下载失败。
- 启动时回收子代理 worktree 不再删掉另一个仍在运行的 MYCode 进程正在使用的 worktree。之前桌面版的子代理在 worktree 里改代码时打开终端版（或再开一个终端版），那个 worktree 会被强制删除，子代理的改动随之丢失。
- `edit` 在 CRLF 文件里替换以换行开头的 `old_string`（例如 `\n    fn foo`）时连同 `\r` 一起替换；之前只替换了 `\n`，会留下一个孤立的 `\r`。

## [0.12.14] - 2026-10-20

### Changed

- 去掉桌面版输入框左下角的 `+` 按钮：它弹出系统原生的文件选择框，和应用内的目录浏览器风格割裂。引用文件改为在输入框里输入 `@` 搜索项目文件。

### Fixed

- 桌面版的供应商中断行（`[error] the response was interrupted: …`）改为红色提示框，不再作为普通回答正文显示；回答里引用的构建日志（如 sbt 的 `[error] …` 行）仍按正文显示。
- 终端版只把代理写入的中断行标成 `✖` 错误行；之前任何以 `[error]` 开头的行都会被标红，引用 sbt 构建日志时会把代码块拆开。
- 输入框的上下文圆环改为按已用比例填充（`200k / 1.0M` 约 20% 的弧），与旁边的用量数字一致，并用主题强调色让弧清晰可见；之前按剩余比例反向绘制，看起来几乎是一整圈。

## [0.12.13] - 2026-10-20

### Fixed

- 桌面版展开 `read` 结果时按文件扩展名给代码着色，扩展名未知时保持纯文本；失败结果不再在正文重复 `failed:` 标记。

## [0.12.12] - 2026-10-20

### Changed

- 终端版的 `crates/mycode-cli/src/app.rs` 按职责拆成 `app.rs` 与 `app/input.rs`、`app/render.rs`、`app/settings.rs`、`app/settings_rows.rs`、`app/ui.rs`：驱动循环、按键与发送、设置页及选择器、设置行、绘制和转录行各自独立，单文件都不再超过 1000 行；行为不变。
- 逐个 crate 复核 `docs/` 与源码注释，修正与现实不符的说明：字段类型、模块清单、重试与超时、压缩上限、会话目录、设置页数量和代码块高亮等；`mycode-config` 的 README 补上文档损坏时的备份与抢救规则，`docs/desktop.md` 补上终端版的模块划分。
- 文档把工作目录上限写成软上限：正常最多 8 个，从旧版合并的列表可能暂时更多，新增目录时保持现有数量。
- 依赖升级到当前最新版本：ratatui 0.30 / crossterm 0.29 / tui-textarea-2 0.13、gpui-kit 0.8、jsonschema 0.59、zip 9.0.5、tokio 1.54、serde_json 1.0.154、uuid 1.28、sha2 0.11.1 等；现有接口无需改写，工作区测试与 clippy 全绿。

### Fixed

- 桌面版「设置 → 快捷键」里 Esc 的说明改为实际行为：运行中不会停止任务（Stop 按钮才取消），没有回合时把暂停的队列退回输入框。
- 升级 ratatui 0.30 后 `lru` 由 0.12 升到 0.18.5，不再受 `LruCache::iter_mut` 的 Stacked Borrows 告警影响。

## [0.12.11] - 2026-10-20

### Changed

- 桌面版的快捷键说明从「设置 → 通用」移到单独的「设置 → 快捷键」页（导航里在「通用」下方）：按「撰写与发送」「界面与导航」两张卡片分组，每行写明动作和说明，按键画成键帽。CLI `/settings` 里的快捷键也带上同样的说明。
- 桌面版没有回合或压缩在运行时，点窗口关闭按钮（`×`）、按 Alt+F4 直接退出；只有任务运行中才需要 6 秒内再点一次确认，提示改为「任务运行中，再点一次 × 退出」。

## [0.12.10] - 2026-10-20

### Added

- 自动压缩生成摘要期间，桌面版和 CLI 的状态行显示「正在压缩上下文」，不再一直停在「等待模型响应」。
- 供应商以超出上下文长度拒绝请求（如 `context_length_exceeded`、`prompt is too long`）且还没有任何输出时，自动压缩历史后重发一次，不再直接报错结束；主回合和子代理都适用。
- 回合进行中按 Ctrl+G（macOS 桌面版为 Cmd+G）引导：不停止当前回合，草稿在模型下一步之前作为用户消息交给它，读到之前显示为「引导中」；回合在那之前结束的话自动退回队列。桌面版和 CLI 都支持。
- 桌面版「设置 → 通用」和 CLI 的 `/settings` 新增快捷键说明，两边共用同一份列表。

### Changed

- 压缩改为按供应商返回的用量判断上下文大小：以上一次请求回复里的 prompt token 数为准，再加上之后新增内容的估算；中日韩文字按每字约 1 token 估算（原来按 4 个字符 1 token，中文会话被严重低估，常常还没压缩就已超长）。保留的尾部和摘要长度随模型窗口缩放，省不下多少空间时不压，小窗口模型不再每一步都重新压缩；每次请求的输出上限压到窗口剩余空间以内。
- 压缩摘要后面原样附上被压缩部分里用户自己发过的消息（按预算保留最新的），长工具回合之后模型不再只能看到对用户要求的转述；摘要超长时去掉中间、保留结尾的下一步，不再截掉末尾。
- 桌面版的摘要卡片默认折叠成一行「较早的上下文已压缩」，点开查看全文，不再把后面的消息挤出视野。
- 自动压缩期间按 Ctrl+Enter 打断发送，会等摘要生成完再停止当前回合，不再丢掉生成到一半的摘要、下次发送又从头压缩。
- CLI 的按键与桌面版对齐：Ctrl+Enter 停止当前回合并立即发送（原来是换行；终端分辨不出 Ctrl+Enter 时仍按 Enter 或换行处理），Shift+Enter 换行（原来会直接发送），Ctrl+J 和 Alt+Enter 仍是换行。

### Fixed

- 回合结束后（尤其是压缩过的会话）立刻发送的消息不再卡在队列里、像是「这句话没发出去」：核心先注销回合再报告结束，重开会话时能正确识别回合和手动压缩都已结束；压缩进行中打开的会话在压缩完成后会解锁输入。
- 发送失败时消息放回输入框，不再随清空的输入框一起丢失；回合进行中切到别的会话再切回，排队的消息会跟着回来；发送后立刻重开同一会话，刚发出的消息不会再从界面上消失。
- 一个回合里压缩多次时，每张摘要卡片都会显示，不再只显示最后一张。
- 压缩检查点记录的消息数与当前账本对不上时整份忽略，不再据此把本该写进摘要的消息漏掉。

## [0.12.9] - 2026-10-20

### Added

- 代码块语法高亮：桌面版和 CLI 共用一个轻量词法高亮器（不引入 tree-sitter，二进制不变大），覆盖 Rust、Python、JS/TS、Go、C/C++、Java/Kotlin、C#、Swift、PHP、Ruby、Lua、Shell、PowerShell、SQL、JSON、YAML、TOML、CSS、diff、HTML/XML；关键字、类型、函数、字符串、数字、注释、键名等分色，深浅两套配色都满足 4.5:1 对比度。未标语言或未知语言的代码块保持原样，命令输出不会被误染色。

### Changed

- 桌面版回答正文改用偏冷的浅蓝灰（浅色模式为深靛灰），思考内容改用与「思考」标题呼应的淡紫，不再是一片纯白，对比度仍满足阅读要求。
- 改动面板与 diff 面板改为同顶同底的左右分栏：改动列表收窄为 320px 并撑满高度（不再只有 220px 高的小框），diff 面板加宽到 640px，标题栏显示 +新增 / −删除 行数。
- ask_user 提问卡片改为磨砂玻璃风格：带强调色渐变的半透明底、图标徽章、多题进度条、带单选圆点/多选勾选框的选项行，「回答 / 下一题」使用主按钮样式，底栏加分隔线。

## [0.12.8] - 2026-10-20

### Changed

- 桌面版窗口的最小尺寸改为默认尺寸（按主屏可用区域缩放后的大小）：窗口只能放大，不再能缩小到布局错乱；上次保存的较小窗口尺寸会恢复为默认大小。
- 会话标题改用更严格的标题生成提示：只概括用户意图、不回答问题，模型按 `{"title":"..."}` JSON 返回；模型不按格式返回时仍取第一行作为标题。

### Fixed

- 自更新在退出时静默替换后，下次启动会提示「已更新到 vX.Y.Z」（CLI 打印同样的提示），关闭再打开不再看不出更新是否已安装。
- 侧边栏「目录」「会话」两个分区标题与下方列表文字左右对齐（统一 16px 内边距）。

## [0.12.7] - 2026-10-19

### Fixed

- 撤回回合中途的插话（steer）后，不再把覆盖了被撤回步骤的压缩摘要带到新分支：回合中途写下的检查点记的是回合开始时的 head，但摘要已包含之后的步骤；现在只有检查点覆盖的消息都还在新分支上时才随之保留。
- PowerShell 预检拦截 `man` 时给出准确原因：Windows PowerShell 里 `man` 是会分页的 `help` 别名，并非不存在，仍提示改用 `Get-Help`。

## [0.12.6] - 2026-10-19

### Fixed

- 长会话（超过 256 条账本事件）发送新提示或撤回重发后不再回退到早期中断的回合：`for_each_event` 分页游标不再在首满页末尾提前终止，完整历史与最新用户提示都能被读取；撤回消息时若压缩检查点覆盖的 head 仍在新分支上，检查点会随新分支保留。
- 桌面版输入框的 `/` 与 `@` 自动补全菜单宽度与输入框对齐，最多显示 3 行并支持方向键/滚轮滚动选择；`/` 补全在前缀匹配之外支持按子串与标题自动匹配（如输入 `/git` 匹配 `/ferris-git`）。

## [0.12.5] - 2026-10-19

### Fixed

- 新开会话或重启后输入框旁的上下文环不再消失：聊天视图在有选中模型时就会预加载模型目录，上下文窗口不再因为目录尚未加载而取到 0；CLI 状态行的 `ctx%` 同理修复。
- 回合统计里的「入」不再把缓存读重复计成输入：OpenAI 系的 `input_tokens` 已包含缓存读，统一按 `prompt_tokens` 减去缓存读折算新鲜输入，多轮工具调用不再把重发的上下文每轮都计成新输入（上下文条仍按最近一次完整提示计算）。
- 子代理详情窗口的「目标」「任务」不再因为两条一次性进度行被丢而长期空白：`agent` 调用的角色与目标改从调用本身的 target（与账本行同源）回填，`queued`/`prompt` 丢失时子代理启动后会用 `goal`/`task` 补偿行补发一次；中途重开会话重建的 job 也能从已重放的调用行取回目标。
- Windows PowerShell 5.1 下工具输出不再乱码：UTF-8 编码前导改为纯属性赋值并逐条 `try`/`catch`，避免首个 cmdlet 调用把重定向的 stderr 提前锁定在 OEM 代码页（GBK）。
- Unix 命令拦截对大小写不敏感：`EXPORT`、`Sudo`、`TOUCH` 等写法也会被拦下并给出 PowerShell 写法提示。
- 改动抽屉改为贴合内容高度，不再一直拉到窗口底部；文件列表在抽屉内限高滚动。
- 桌面版标题栏的最小化/最大化/关闭按钮恢复 Windows 原生样式：三键无缝排列、图标固定大小，悬停时关闭键用系统红底白字。
- 关闭确认提示改用 Warning 级别气泡，保证 6 秒确认窗口内提示始终可见。

## [0.12.4] - 2026-10-19

### Fixed

- Windows 桌面版升级后补上缺失的桌面快捷方式：新增 `~/.mycode/mycode-desktop.shortcut` 记录快捷方式已写过，升级上来、从未走过下载交接的旧安装会在启动时补建；用户主动删除的快捷方式不会被复活，创建失败时下次启动重试。
- 用户主动终止回合时不再在正文里渲染 `[error] the response was interrupted: interrupted by user`，界面只保留红色的「已被用户停止」横幅（模型侧历史仍保留打断标记；真实的流错误仍照常显示）。

### Changed

- 思考记录的展开/折叠改为全局且持久化的开关（写入 `~/.mycode/ui.json`，桌面版与 CLI 共用）：桌面版点击任意一条「思考」或通用设置里的开关切换全部思考块，折叠标题改用洋红色斜体以区别白色正文；CLI 支持 `Ctrl+O` 快捷键与 `/settings` 开关切换。
- 桌面版点击窗口关闭按钮（`×`）需要二次确认：首次关闭弹出「再点一次 × 退出」提示，6 秒内再次触发才退出，防止误关窗口。
- PowerShell（`pwsh` / `powershell.exe`）工具在执行前拦截命令位置的 Unix/bash 专属命令（`touch`、`export`、`source`、`which`、`chmod`、`chown`、`sudo`、Unix 包管理器、`man`）及 `>/dev/null`、`<<` heredoc 语法，直接返回对应的 PowerShell 写法提示（字符串、here-string 与注释内的文本不受影响，macOS/POSIX shell 不受影响）；同时在 PowerShell 提示词中补充精简的 Unix→PowerShell 对照。

## [0.12.3] - 2026-10-19

### Fixed

- 同一回合里第二次压缩不再丢消息：历史已经以摘要开头时，不再用磁盘检查点的账本下标去切这份缝合后的历史，内存里更新的摘要也会带进下一份摘要；压缩之后的请求始终从摘要开始，回合继续执行。
- `AGENTS.md`、`MYCODE.md` 和按任务打开的 `SKILL.md` 明确是用户的常驻指令，不再被"文件内容是数据"的规则一并否定；其它文件、工具输出、MCP 结果和网页仍是数据。
- 子代理的 system prompt 带上会话工作区的 `AGENTS.md` 资源；MCP 说明与父回合共用同一份文本（含 `"list"` 与关键词查找）。
- 设置里关掉 scout 或 artisan 后，`<dispatch>` 和 `agent` 工具描述不再提到它；说明 `agent` 调用会与同一条回复里的编辑和 shell 并行。
- 会话与子代理用同一个函数生成技能目录，都包含用户添加的工作区文件夹（不含临时目录）；以 `/slug` 开头的消息会读取对应技能。
- 两个 MCP 服务器有同名工具时，后一个改名为 `server__tool`，两个都能调用；与内置工具重名的小服务器工具改由 `search_tool` 提供，不再被内置工具顶替。
- 回合中途压缩出的摘要在回合结束时写入检查点（先核对未压缩的尾部与账本一致），下一回合直接从这份摘要继续，不再重新总结；一个回合里压缩三次、四次也不丢消息。
- 没有可用的网页搜索后端时不再注册 `web_search` 和 `fetch_content`，子代理也不会拿到它们。

### Changed

- `search_tool` 支持关键词查找（每个工具一行摘要，最多 25 个；只匹配一个时直接返回 schema），`"list"` 每个工具只列一行。
- 工作规范新增：不为通过测试去改测试，按项目锁定的版本写代码，不打印或提交密钥。压缩摘要会记录用户中途的纠正、各项检查的结果和子代理结论，用用户的语言写，并且不抄入密钥。
- `search_tool` 的 `names` 一次最多取 10 个工具的 schema；`use_tool` 调用的目标是只读工具时，与其它只读调用一起并行执行。
- 子代理会自动压缩自己的上下文，与主会话是同一套压缩逻辑；上下文窗口按子代理实际使用的模型计算（设置里给角色指定了模型时用那个模型），压缩后卡片显示「已压缩上下文」。
- 模型还在写 `agent` 调用时，对话里就出现对应的子代理卡片，角色和任务描述边写边显示，不再等整条回复结束后一次性冒出几个；状态栏显示「正在准备 N 个子代理」。
- 桌面版和终端版对话里的子代理行都显示它当前在做的步骤和已运行时间，不必打开面板查看。终端版的压缩摘要折成一行提示，不再当作用户消息整段打印。
- 会话目录改为 `sessions/<项目键>/sess-YYYYMMDD-HHMMSS-<8 位十六进制>/`，按创建时所在的项目分文件夹，目录名就是创建时间；不再写 `title.txt`。这是破坏性改动：旧版会话不迁移，启动时从索引里移除、侧栏不再显示；旧目录 `sessions/ses1-*` 留在磁盘上，可以手动删除。

## [0.12.2] - 2026-10-19

### Fixed

- 密钥文件修复时只丢弃无效的单个条目（空、过长、含换行或 NUL 的 key），其余服务商的密钥保留；以前一个坏 key 会让整份密钥被丢弃。
- 工具 schema 内联不再删除名为 `definitions` / `$defs` 的参数属性；不含 `$ref` 的 schema 直接跳过、不再复制。
- 代理层不再叠加一层服务商重试：驱动层已在出现输出前重试瞬时失败，叠加后同一请求最多会发 16 次，且会在已显示的输出之后重放。
- 进行中的工具行使用账本里的调用 id（取消时仍用服务商 id），回合中途重新打开会话时调用和结果保持配对；子代理中断时返回错误结果。
- 切回运行中的会话后，从进度重建的子代理任务带上服务商调用 id，停止按钮能真正取消子代理。
- 会话标题请求把首条消息包在 `<message>` 里，模型不再回答消息本身而是给出标题；标题里允许 `C++`、括号这类内部标点。
- `ask_user` 的提示和文档去掉已废弃的 program 说法：回合会等待回答，关闭提问只让这次调用失败、回合继续。
- MCP 提示说明 `search_tool` 是只读调用，可与其他独立调用（包括 `agent`）同一轮发出。
- 读取资源文件时在上限处停止，不再先读完整个文件。
- 网页搜索设置页：密钥来自环境变量（`QUERIT_API_KEY` 等，运行时优先读取）或 AnySearch 匿名访问时，不再显示「未连接」。
- 关于页在下载更新期间禁用「检查更新」按钮；MCP 服务器行里带空格的 stdio 参数加上引号显示。

### Performance

- 服务商重试共享同一份请求体，不再每次复制。
- 桥接命令按值交给处理函数，去掉设置、UI 状态、密钥等的逐分支复制；回合开始与压缩时不再复制整段转录。
- 代理按引用分发工具调用；斜杠菜单与 `@` 菜单每次按键不再复制 MCP 列表和输入片段。
- CLI 每帧不再复制提问卡片和转录行；配置修复、UI 状态与密钥写入改为借用序列化。
- 内置工具的参数 schema 每个进程只生成并编译一次，不再每次调用都重新生成、编译校验器。
- 工具完成事件与对话历史共享同一份工具结果，广播时不再复制工具输出；系统提示在每轮请求间共享，不再每轮复制。
- 桌面设置页：角色目录共享而非每次绘制复制整份（含全部角色提示）；模型库与模型选择器按下标读取共享目录、借用主题，不再为 190+ 服务商逐个复制字符串；常规、模型、服务商详情等页去掉逐帧复制。

### Changed

- MCP 导入与设置共用同一套参数数量和环境变量名校验。
- README 与模块文档按当前代码重写，修正过时的注释。

## [0.12.1] - 2026-10-19

### Changed

- 自动更新改为「下载即备好」：校验通过的安装包落到 `~/.mycode/update/` 并立即挂上平台侧退出交换——Windows 由独立 helper 等进程退出后替换，macOS 由 shell 脚本原地替换；正常退出、崩溃、被杀都会完成安装，下次启动就是新版，终端无需再进设置安装。桌面上「重启并安装」仍走带 `--relaunch` 的同一个交换路径；终端二进制不重启进程（helper 无法归还控制台），下一次 `mycode-cli` 即新版。若上一次退出交换没跑成（重启了机器、helper 没了），下次启动会重新武装 pending 负载：桌面版交给 relaunch helper 后直接退出，终端重新武装后继续打开，绝不弹窗阻塞。同一安装的多个 helper 靠目标哈希收敛：已替换的目标等于「已应用」，relaunch 只跑一次。
- 删除侧栏目录时，若目录下还有会话，需要再点一次确认（按钮变红写「删除会话？」，3 秒内有效）：移除目录会一并删除绑定在它上面的会话，以前一次误点就全没了。空目录仍然一次移除。
- 模型搜索统一成一套规则：桌面模型选择器、模型库（服务商和预设模型）与 CLI 选择器都按词匹配，不区分大小写、忽略 `-` `_` `.` `/` `:` 等分隔符，`gpt5` 能找到 `gpt-5`，`sonnet 4.6` 能找到 `claude-sonnet-4-6`。三份重复的选择器数据层合并进 `mycode-frontend::picker`。
- 系统提示精简：网页工具（`web_search` / `fetch_content`）的规则只在 `<web>` 段写一次，工具列表里那一行不再重复；`edit` 的工具列表行缩成一句，完整用法留在工具说明里；MCP 工具在工具列表里带上各自的说明。
- 调用不存在的工具时，报错会列出当前可用的工具；带引号的标量参数按 schema 类型自动转换。
- 子代理 scout / artisan 的角色提示收紧。
- 详情面板的模型和服务商显示名称，不再是原始 id，和输入框的模型芯片一致。
- CLI：Esc 先关掉菜单和提问，再取消回合；多选提问行可以一次输入多个序号（逗号或空格分隔）。

### Fixed

- 修复 macOS 发布构建：`install.rs` 的 `PathBuf::as_ref()` 存在多个 `AsRef` 实现导致类型推断失败（E0282/E0277）；Windows PR CI 编不到 `#[cfg(not(windows))]` 分支所以漏掉，改用 `fs::read_to_string(&file)` 消歧。
- 斜杠菜单用方向键移动时滚动错位、高亮行滚出视野：菜单早已去掉分组标题，键盘导航还在按标题数量偏移。
- 自定义服务商表单的 API 密钥框改为掩码显示。
- 切换服务商时自动选中该服务商已配置的模型。
- 多处提示补齐中英双语：发送失败（没有服务商 / 没有模型）、设置表单校验、撤回与更换目录、附加文件对话框等。
- 启动后才发生的配置重置也会弹出提示。
- `read` 在按字节截断的窗口之后不再跳行；`edit` 的 `line_range` 不再把下一行拼进来；program 模式可以启动大于 64 MiB 的程序；CLIXML 实体只解码一次。
- 工作区交接包含路径带空格的未跟踪文件；MCP stdio 服务器拿到 SDK 默认的环境变量；压缩和服务商配置失败显示可读文本。
- 服务商：Anthropic 在输出前返回过载帧时重试；正确识别带方括号的 IPv6 和点分主机；XML 风格的提示、查询和目标按纯文本传递。
- 全局 `AGENTS.md` 从操作系统用户主目录读取。

### Performance

- 桌面渲染不再每帧复制工具结果、diff 文本、目录列表、目录条目和设置文档。
- SSE 按行切分而不是逐字节；目录 304 重新验证时不再重新解析；网页传输的 TLS 后端每个进程只探测一次；Windows 上进程用户 SID 只查询一次；会话负载窗口只打开一次日志。

## [0.12.0] - 2026-10-18

### Changed

- 破坏性改名：终端可执行文件 `mycode` 更名 `mycode-cli`，发布资产前缀同步改为 `mycode-cli-v*`；旧 `mycode` 二进制仍走 CLI 系列更新，装上后落在 `mycode-cli`。
- 自安装：CLI 首次运行把自己复制进 `~/.mycode/bin` 并把该目录写入用户 PATH（Windows 注册表 `HKCU\Environment`，macOS `~/.zprofile` / `~/.bash_profile` 等），随后在终端里提示安装位置、按 Enter 继续，之后新开的终端可直接敲 `mycode-cli`；Windows 桌面版首次运行即安装——复制为 `~/.mycode/bin/mycode-desktop.exe`、带原参数启动安装好的副本、删除原来那个 exe 并新建桌面快捷方式。macOS 桌面版只用 dmg 安装、不做自安装，从「应用程序」原地自动更新（Release 里的 macOS 桌面 zip 只给更新器用）。清理下载只删文件，不会删名为 `mycode` 的文件夹。`MYCODE_NO_INSTALL=1` 或 debug 构建跳过全部自安装。
- 终端标题显示 `MYCode Cli <版本>`；Windows 版 `mycode-cli.exe` 带螃蟹图标与文件说明/版本资源。
- 代理只剩两种模式：「跟随系统」（环境变量或系统代理设置；没设就直连，不再偷偷探测本机 Clash / v2rayN 端口兜底）和「手动填写」（`http` / `https` / `socks5` / `socks5h`）。旧配置里的 `auto`、`direct` 都按跟随系统处理。
- `write` 工具的说明和报错写明：替换已有文件必须带该文件最新的 `[revision …]` 或 `overwrite=true`，报错里带上文件路径，模型不用再整份重发一次才知道怎么改。

### Added

- 代理「测试」：桌面通用页和 CLI `/settings` 都能按当前设置请求 google.com（`generate_204`），显示走的线路（直连 / 系统代理地址 / 手动地址）、状态码和耗时。
- CLI `/settings` 设置页：模型（服务商 → 模型两级选择器，带搜索与最近使用）、思考强度、主题/语言/字号、代理、自动更新、服务商与 MCP/网页开关、`a` 填 API 密钥、`u` 改端点、版本与更新行。粘贴会进当前输入框（API 密钥、端点、代理），`Ctrl+C` 先取消正在跑的回合。
- CLI 欢迎区显示 Ferris 螃蟹（hi ferris）；转录里中断/错误行（`[error] …`）红色加粗，思考改为品红斜体，与正文区分开。
- 回合进行中显示已用时间：CLI 每秒刷新并显示最新一行思考，桌面状态行同样带耗时。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.12.30...HEAD
[0.12.30]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.30
[0.12.29]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.29
[0.12.28]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.28
[0.12.27]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.27
[0.12.26]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.26
[0.12.25]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.25
[0.12.24]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.24
[0.12.23]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.23
[0.12.22]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.22
[0.12.21]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.21
[0.12.20]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.20
[0.12.19]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.19
[0.12.18]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.18
[0.12.17]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.17
[0.12.16]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.16
[0.12.15]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.15
[0.12.14]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.14
[0.12.13]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.13
[0.12.12]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.12
[0.12.11]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.11
[0.12.10]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.10
[0.12.9]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.9
[0.12.8]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.8
[0.12.7]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.7
[0.12.6]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.6
[0.12.5]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.5
[0.12.4]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.4
[0.12.3]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.3
[0.12.2]: https://github.com/MCapricorns/mycode/compare/v0.12.1...v0.12.2
[0.12.1]: https://github.com/MCapricorns/mycode/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.0
