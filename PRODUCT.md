# Product

<!-- impeccable:product-schema 1 -->

## Platform

desktop

本记录覆盖 MYCode 原生桌面客户端和配套 Web Wiki。`desktop` 是本项目确认的平台值，表示 Windows/macOS 原生桌面；当前 Impeccable 的标准平台枚举尚未包含这一类别。桌面使用 Rust 和 GPUI，Wiki 使用静态 HTML，两者的交互与验证方式分别处理。

当前发布平台为 Windows x64、Windows ARM64 和 macOS Apple Silicon。平台支持范围以 [README](README.md) 和发布配置为准。

## Users

首要用户是项目所有者本人。MYCode 首先服务个人高频开发流程：围绕本地项目理解代码、修改文件、执行命令、验证结果，并保留可继续使用的会话上下文。产品取舍优先依据实际个人工作流。

Wiki 服务使用、排查和维护 MYCode 的读者，帮助他们理解功能行为、运行机制和源码位置。更广泛的目标受众尚未确认。

## Product Purpose

MYCode 让用户通过自然语言驱动本地编码任务，在桌面中管理项目、对话、模型、工具执行和代码改动。完成任务意味着实际工作区结果可检查，执行过程与当前状态可理解，历史任务可重新打开。

Wiki 提供与实现一致的使用和开发说明，把模块关系、运行过程和源码入口连接起来；其内嵌文档与源码可离线查阅。

## Positioning

产品机制是 Rust/GPUI 原生桌面、本地项目与会话管理，以及 magpie 网关接入模型和资源的组合。MYCode 管理本机工作区操作、工具执行、会话记录和界面状态；magpie 负责连接用户配置的上游模型服务。

本地保存数据与本机执行工具不代表离线模型推理或数据不会离开设备。发送给模型的消息、附件和工具结果会经 magpie 转发至配置的上游服务。

## Operating Context

桌面端的常用流程：

1. 启动并配置 magpie，安装 MYCode 插件，为集成入口 **MCode** 选择模型。
2. 在 MYCode 中选择或拖入项目目录，打开已有对话或创建新对话。
3. 输入任务，按需通过 `@` 引用文件、拖入附件或粘贴图片；模型和思考强度按会话保存。
4. 查看回复、工具与子代理进度；任务运行中可以补充引导、打断并发送，或停止执行。
5. 通过改动面板查看 Git diff，检查工作区结果，继续当前任务或切换项目与会话。

默认本地数据目录为 `~/.mycode`；可通过绝对路径环境变量 `MYCODE_HOME` 改写。使用说明和具体操作限制由 [README](README.md) 维护。

Wiki 从根目录 `index.html` 跳转进入 `wiki/index.html`，既可通过 GitHub Pages 访问，也可直接打开本地文件。读者可以查阅架构说明、模块关系和内嵌源码，再定位到仓库中的实现。

## Capabilities and Constraints

### Desktop

- 工作区包含 `mycode` 和 `mycode-desktop` 两个 Cargo package。前者管理运行时、模型、工具、会话和界面状态，后者负责 GPUI 窗口、渲染、剪贴板和桌面交互。
- 现有能力包括项目与会话切换、流式回复、文件与图片附件、模型选择、上下文压缩、工具进度、Git diff、规则、skills、MCP 和子代理。具体能力和限制以实现及对应文档为准。
- 模型目录和连接依赖 magpie；缺少模型时需要检查网关、插件和模型绑定。界面中的模型来源与思考档位应忠实反映网关提供的信息。
- 文件和 shell 工具以当前用户权限运行。CodeMode 使用内置 QuickJS，不提供 Node.js、浏览器 API 或 `fetch`；资源限制和 Git worktree 不构成操作系统沙箱。
- 停止操作取消当前回合、工具和子代理，保留已收到的内容及已经发生的文件修改。历史消息的“撤回并编辑”只改变对话历史，不回滚工作区文件。
- 移除项目入口不会删除项目文件或历史。写入型子代理默认使用独立 Git worktree，由主代理检查和整合；子代理不会自行提交、合并或推送。
- 没有内置网页搜索；外部工具能力通过已配置的 MCP 服务提供。

### Wiki

- `wiki/index.html` 是生成产物。布局、导航、搜索和源码窗口由 `scripts/wiki_shell.html` 定义；文档、模块数据与源码经 `scripts/build_wiki.py` 生成离线页面。
- 修改文档、模块图、模板或内嵌源码后，重新生成页面并运行 `python scripts/build_wiki.py --check`。不要直接编辑生成页面作为持久修改。
- 保留已发布的模块 ID 和章节锚点；移动源码时同步更新模块图中的路径。
- Wiki 使用 Python 标准库生成，不依赖前端框架或服务端应用。具体维护方式见 [Wiki 维护说明](wiki/README.md)。

## Brand Commitments

产品名为 **MYCode**。magpie 中的 **MCode** 是现有集成入口名称，相关操作说明应保留这一标识。

现有品牌图标原稿位于 `crates/mycode-desktop/assets/icon.svg`，Wiki 的 logo 和 banner 从该原稿生成。文案沿用现有文档的事实表达：先说明功能和行为，再说明操作方法或源码位置；限制与状态必须准确。

## Evidence on Hand

| 资料 | 可支持的事实 |
| --- | --- |
| [README](README.md) | 安装、magpie 接入、日常操作、平台支持、数据边界与开发命令 |
| [Runtime](wiki/architecture/runtime.md) | 消息处理、会话持久化、停止行为、子代理与模块职责 |
| [CodeMode](wiki/architecture/code-mode-engine.md) | 脚本执行方式、异步调用、并发和错误处理 |
| [Wiki 维护说明](wiki/README.md) 与 [模块图](wiki/graph.json) | 文档生成方式、模块关系、源码入口和链接约定 |
| [性能记录](wiki/memory-report.md) | 已记录条件下的测量结果；引用时保留版本和测量条件 |
| [品牌图标](crates/mycode-desktop/assets/icon.svg) | 当前产品标识原稿 |
| [语言支持](crates/mycode/src/frontend/i18n.rs) 与 [快捷键](crates/mycode/src/frontend/shortcuts.rs) | 已实现的界面语言和平台快捷键行为 |
| [LICENSE](LICENSE) | 仓库的 Apache-2.0 许可 |

尚未确认客户案例、用户规模、推荐语或普遍适用的性能承诺；后续内容不得将推测或单次测量改写成这类证据。

## Product Principles

1. 优先解决个人开发中的真实任务，以实际工作流和可检查的结果决定取舍。
2. 保持项目、对话、模型来源和执行状态清晰，让用户能理解任务进展并及时引导或停止。
3. 准确区分对话历史、工作区文件和上游模型请求，保留现有数据与操作边界。
4. 桌面行为遵循所在平台的既有操作习惯，Wiki 帮助读者理解行为并找到依据。
5. 文档与实现保持一致；能力、限制和性能描述必须能追溯到源码或明确的测量条件。

## Accessibility & Inclusion

现有桌面界面支持 English 和简体中文，可跟随操作系统语言或显式选择；诊断信息保留 English。快捷键按 Windows 的 Ctrl 和 macOS 的 Cmd 区分，文案与实际绑定保持一致。

特定辅助技术、正式可访问性标准和验收等级尚未确认；现有语言与快捷键支持不代表已完成可访问性认证。

## Open Decisions

- 扩展平台、更广泛受众、商业目标与服务承诺尚未确定。
- Windows 与 macOS 之间的额外优先级未设定，当前支持范围继续以仓库事实为准。
- 除现有实现外，尚未增加新的合规或可访问性验收要求。
