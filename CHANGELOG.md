# Changelog

0.9.25 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上三个平台构建成功后都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon；没有 Linux 包）。pull request 不发版，也不跑发布构建：PR CI 只在 Linux 上执行 `cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings`（dev/test profile，没有 fat LTO）。推到 `main` 时三个平台才做 fat LTO 发布构建；同一份检查通过且三个构建都成功后才打标签。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。压缩包未签名。

## [Unreleased]

## [0.11.20] - 2026-10-16

### Added

- 文件工具默认可以在系统临时目录写入（连同会话 cwd 和已添加的工作区目录）：往 `%TEMP%` 写临时产物（浏览器测试页、录制文件等）不再被「path escapes the session cwd」拒绝；worktree 隔离子代理保持只在自己的目录里写。系统提示词把临时目录单列为 scratch 目录，不当作工作区目录，子代理也不从里面加载技能；临时目录按解析后的长路径匹配，`TEMP` 是 8.3 短路径时也能写入。

### Changed

- 提问（ask user）从输入框上方的内嵌条改为窗口居中的半透明悬浮层，点遮罩不会误关，只能回答或拒绝；未选中的选项按钮从全透明改为可见的浅色填充。
- 子代理窗口改用差异面板同款的响应式定位，依次避开详情、变更抽屉和差异面板，全开挤不下时收进窗口内；高度从固定 280 改为一直延伸到输入框上方，日志更完整，且不会挡住发送/停止按钮。
- 斜杠命令菜单去掉命令/技能/MCP 分组标题，平铺展示；高度上限从固定 300px 改为随窗口自适应（最高 400px）。
- 模型设置里的「目录」统一改为「模型库」；模型库页去掉与页头重复的内层卡片标题，搜索框置顶，可见的服务商行从 9 行增加到 12 行。
- 模型选择器、模型与思考菜单、设置页选项卡的选中态从强调色改为中性色。
- 默认窗口尺寸从 1520x960 加大到 1760x1040，小屏幕仍按可用区域自动收缩。
- 删除会话不再弹系统确认框，点会话行上的删除按钮直接删除。
- 欢迎页的 MYCode 标题从纯黑改为蓝→紫渐变字标，进入时颜色扫过一次；系统开启减少动画时直接显示静态渐变。

### Fixed

- OpenAI Codex 订阅通道整回合被拒（HTTP 400 `Store must be set to false`）：Responses 请求体现在显式携带 `store: false`（与 pi、Codex CLI 的做法一致）。
- Codex 预置模型没有公布思考档位，强度菜单只剩 Default 没法选：现在按 models.dev 公布的档位补全（o3 有 low/medium/high，5.5/5.6 系到 xhigh/max，gpt-6-astra 到 max；o3-pro 未公布，保持 Default）。

## [0.11.19] - 2026-10-16

### Changed

- 同一条回复里相邻的只读调用（`read`、`grep`、`find`、`web_search`、`fetch_content`、`search_tool` 和声明了 `readOnlyHint` 的 MCP 工具）现在一起并发执行；`write`、`edit`、shell 等会改动东西的调用仍按模型给出的顺序逐个执行，读不会看到改了一半的文件。
- MCP 服务器并发启动握手，回合只等最慢的那一台；某台连不上时不再每回合把已经连上的服务器全部重启，只重试失败的那几台。
- 系统提示词：新增 `<working_style>`（做完并验证、最小改动、破坏性操作先确认、如实汇报、文件和网页内容是数据不是指令、用用户的语言简短回复）；固定内容放在前面、cwd / AGENTS.md / 技能 / MCP 放在后面，换个文件夹开新会话也能命中前缀缓存；去掉重复三遍的 shell 使用规则。
- 子代理：同时运行固定为 8 个，不再有超时，一直运行到给出答案，取消回合、在详情面板停止或删除对话时结束；设置页去掉「最大并发」和「墙钟超时」两行，旧 `settings.json` 里的这两个字段仍能读入，下次保存时去掉。派发提示词改写：鼓励把独立、会塞满主对话的搜索和阅读交出去，同时不委派琐碎或含糊的任务；scout 明确为只读的代码搜索、网络资料、文档和 API 查询，不跑命令、不构建、不改文件。

### Fixed

- 模型目录刷新一直报 `catalog document had no usable providers`：models.dev 里一个模型的思考档位列表出现了 `null`，整份 5.6 MB 文档因此解析失败、退回旧缓存。现在逐条容错，单条脏数据只跳过那一条。
- 同一批并发工具调用的结果在账本里按完成顺序保存，下一回合重放顺序和本回合不同，提示缓存从那里起全部失效；现在重放时按调用顺序排好。
- Anthropic 协议（含 Kimi、GLM、MiniMax 的兼容端点）下，并行工具的结果和随后的追加引导合并进同一条 user 消息，不再一条结果一条消息。
- Claude 5 及之后的模型选「关闭思考」时发 `thinking: disabled` 会被拒（400），压缩摘要请求也因此失败；现在改为最低思考强度。思考签名因系统提示或工具变化失效被拒时，去掉历史里的思考块重试一次。`refusal` 停止原因显示为可见的错误行，`model_context_window_exceeded` 按截断处理。K3 这类关不掉思考的模型，压缩摘要的输出上限按开着思考来留。
- 「HTTP transport unavailable」「could not open the session」等错误现在带上底层原因；TLS 可用性每个进程只检查一次，不再每回合建一个一次性客户端。
- 工具注册表、提问路由、任务进程、搜索状态的锁在一次 panic 后会被毒化，之后每次调用都再 panic；现在和其他地方一样取回锁继续用。

## [0.11.18] - 2026-10-16

### Added

- 代理设置，写入 `settings.json` 的 `proxy`：设置 → 通用 → 网络里选「智能 / 直连 / 自定义」。智能模式下系统代理开着（Clash「系统代理」）就走系统代理；否则直连，直连连不上的公网站点（DNS 失败、连接被拒或被重置、连接超时）自动改走本机正在运行的 Clash（7890）、Clash Verge（7897）或 v2rayN（10809），并在本次运行内记住该站点。自定义支持 `http`、`https`、`socks5`、`socks5h`，只填 `127.0.0.1:7890` 按 http 处理；没填完的地址不会写进设置。本地模型、`localhost` 和局域网地址始终直连。走代理时不再因为本机 DNS 解析失败而直接报错。
- 服务商详情页新增「模型」卡片：列出该服务商的全部模型，每个模型可以单独显示或隐藏，也可以一键全部显示 / 全部隐藏，写进该服务商的 `hiddenModels`。隐藏的模型不会出现在对话的模型切换、最近使用推荐和命令面板里。

### Changed

- 对话里的模型切换去掉星标：顶部只推荐最近使用的 3 个模型（跳过已停用的服务商和已隐藏的模型），其余从服务商列表里选。旧 `ui.json` 里的星标照常读入、被忽略，下次保存时清除。
- 提示气泡从窗口底部（会压住输入框）移到右上角，位于标题栏和设置 / 详情的标题行下方，详情面板打开时停在它左侧，不再挡住按钮。

### Fixed

- 模型请求每次都新建 HTTP 客户端，每个回合都要重新做一次 TCP + TLS 握手（经代理时还要新建一次隧道）；现在按端点复用连接。
- 界面每一帧都向系统查询「减少动画」设置（流式输出时每个字都会触发一次）；现在两秒内复用结果。
- 关闭子代理任务时在持有任务表锁的情况下唤醒等待者；现在先释放锁。
- 清理一批多余的克隆、提前求值和无用的 `Peekable`。

## [0.11.17] - 2026-10-15

### Added

- OpenDesign 网关（`https://amr-link.open-design.ai/v1`）：设置 → 模型的服务商卡片下新增单独的「OpenDesign」按钮，不混在 models.dev 目录列表里。页里先列针对该网关实测过的模型（思考档位、关闭语义和输出上限参照 pi-opendesign-bridge），粘贴密钥后「从网关加载模型」换成这把密钥实际可用的模型；新模型的思考档位、输出上限和图片输入按裸 id 从 models.dev 借（OpenRouter 优先），上下文取网关声明的值。已配置时再次添加是替换模型列表，不会多出 `opendesign-2`。该网关对所有模型只发 `reasoning_effort`，不发厂商的 `thinking` 对象。
- `find` 新增 `max_depth`，目录条目以 `/` 结尾；`*` 加 `max_depth` 1 就是列一个目录。

### Fixed

- 只装了 Microsoft Store 版 PowerShell 7 的 Windows 上，shell 工具每次都报 `program pwsh.exe not found`：`WindowsApps\pwsh.exe` 是执行别名，不能当文件打开。现在顺着别名的重解析点找到包内真实的 `pwsh.exe` 再启动；包不可读时退回 Windows PowerShell 5.1。
- 回合进行中按回车提交的追加引导，在模型下一步读到之前完全看不见；现在立刻显示在输入框上方的「追加引导」列表里。回合在最后一次请求之后才收到的引导以前会被静默丢弃，现在退回排队列表、作为下一条消息发送，正在输入的草稿不受影响。
- 详情面板的「上一轮」和上下文条对不上：前者是这一轮所有请求（每个工具回合一次）的累计 token，后者是最近一次提示的大小。现在标成「上一轮合计 / 本轮合计」，并写出请求次数。
- 思考强度芯片在「默认」时显示成 `Thinking · D…`：芯片改用短名（Max、High、Thinking off 等）。

### Changed

- 思考强度没选过或选的档位当前模型没有时，按该模型公布的最强档运行（芯片和实际请求一致）；只有模型一个档位都没公布时才是「默认」。
- 提示词和 shell 工具说明改为专用工具优先：列目录、读文件、搜文本用 `find` / `read` / `grep`，shell 只做它们做不到的事（构建、测试、git、运行程序）。
- 思考的展开 / 收起改为一个全局开关：点任意一条思考的标题行，全部展开或全部收起，默认展开，写进 `ui.json` 的 `thinkingCollapsed`。收起时显示最新一行的末尾，生成中随内容滚动，不再固定在开头。
- 移除色板选择（六块色板差别太小）。旧 `settings.json` 里的 `appearance.palette` 照常读入、被忽略，下次保存时清除，不会因此重置。

## [0.11.16] - 2026-10-15

### Fixed

- 配置文件有问题时直接重置，不再卡在 `settings error: owned path access control failed`：`settings.json`、`secrets.json`、`ui.json` 除了内容损坏，文件本身权限或所有者不对（从别的机器拷来、ACL 被清空、只读）、是链接或目录时，也会原样改名为 `{文件名}.broken-<时间戳>` 后写入默认内容，启动继续并提示一次。家目录本身不可信时仍然报错。
- 家目录不再出现 `*.lock` 文件：写入锁改为操作系统锁（Unix 对目录句柄 `flock`，Windows 用按目录命名的互斥体），旧版本留下的 `xxx.lock` 会在下次写入时删除。
- 回合准备阶段（读凭据、连 MCP、回合前压缩）现在可以取消：取消令牌和插话收件箱在回合开始时就登记，按 Esc 不再无效，同一会话再发一条也会停掉上一个回合，不会两个回合同时写同一分支。
- 修复回合卡死：在第一次请求的压缩钩子里取消时，代理不发 `TurnEnded`，事件泵一直等待，界面停在“发送中”、插话也被吞掉。现在泵在总线关闭时补发安静取消；压缩摘要请求也会随取消立即停止，不再最多等 3 分钟。
- 同一回合里第二次 `ask_user` 不再立刻判为“用户已取消”而界面却显示问题卡片：每次提问单独登记回答通道；没有问题时送来的回答会被拒绝，不会留给下一个问题。
- 取消父回合时，正在运行的子代理现在会被真正停止，它的 git worktree 也会立即释放，不再留在仓库里直到下次启动；排队或正在连接 MCP 的子代理可以用停止按钮停下。
- Codex 的 OAuth 刷新改为全进程串行：两个回合同时遇到过期令牌时，后者直接用前者刚写回的新令牌，不再拿已作废的 refresh token 失败。
- 导入数据：本机配了 stdio MCP 服务器后导入不再永远被拒；改为只在导入会新增、修改或删除 MCP 命令时，在“数据”卡片里让用户确认后再导入。
- MCP：stdio 输出按行在读取时就限长，异常服务器不能无界占内存；HTTP 的 SSE 回复按事件拼接多行 `data:`，并跳过服务器发来的请求和通知；共享连接等锁时也能被 Esc 取消；超长结果的全文文件 7 天后自动清理。
- OpenAI 兼容流：所有并行调用都报 `index: 0`（或不带 index）的服务器，工具调用不再被拼成一个无效调用；每帧重发函数名的网关不再得到 `readread…`；原生调用与文本里的 XML 调用不再互相覆盖。
- Anthropic 流：`redacted_thinking` 在下一轮请求里原样回传，开启思考的工具循环不再被拒；原生内容块不再覆盖文本里解析出的工具调用。
- Responses 流：只在 `.done` 事件里给出完整参数的网关，工具调用不再以 `{}` 参数执行。
- 网页搜索先过滤被拦截的结果再计数，不再少返回可用结果；`@` 文件提及改为广度优先，浅层文件不再被深层目录挤掉。
- 会话缓存已满且其它会话都有未提交的预留时，新建或重新打开的会话不再把自己挤出缓存；等待生成排空的多个调用方都会被唤醒。
- 退出时已受理的存储和设置写入会先完成，不会因为界面不再等回复而被中断；停止后的请求按原命令返回对应的错误回复。
- Markdown：更长的围栏能包住较短的围栏和其中的表格；表格里转义的 `\|` 留在单元格内，表头与分隔行列数不符时不当表格；列宽按界面字体实际测量，中文和大字号不再被裁切或重叠。
- 输入框上方的模型与思考菜单在窄窗口、矮窗口里夹在窗口内，不再被聊天区域裁掉。
- 工具调用按最近的结果配对，重复使用的调用 id 不会拿到上一轮的结果；长文本截断落在字符边界上。

### Changed

- 移除 xAI 的 OAuth（SuperGrok / X）登录，xAI 和其它服务商一样粘贴 API key。旧版本登录留下的令牌过期后会提示改贴 API key。
- 桌面：Esc 时如果核心已经没有在跑的回合，界面直接结束“发送中”，不会卡住停止按钮和排队消息。

## [0.11.15] - 2026-10-13

### Fixed

- 设置里手动检查更新出现"下载并安装"后点击无反应：手动检查的回复此前只翻转状态、没有保存更新包信息，按钮读到空 offer 就静默返回。现在发布 offer 与"可用"状态是一次性落地，按钮可点，下载完成后的"已就绪"也显示新版本号；手动检查回复还补上了下载中/已就绪的防重入护栏，不会再把状态打回或重复下载。
- 工作区里正在使用的目录现在也能删除：目录行此前在当前会话使用它时隐藏删除按钮，处理函数也直接拒绝，单目录工作区永远删不掉。现在每行都有删除按钮；删掉会话正在用的目录会回到欢迎页，会话本身保留，重新打开该会话会再次挂上目录。

### Changed

- 移除项目 MCP 信任开关：打开项目文件夹后其 `.mycode/mcp.json` 直接加载（同 id 仍覆盖设置里的服务器），项目菜单不再有"信任/不再信任项目 MCP"，`ui.json` 的 `trustedProjects` 字段随之退役（旧文件里的该字段会被忽略并在下次保存时清掉）。

## [0.11.14] - 2026-10-13

### Fixed

- 发送消息的回复带回会话 id：切换会话的一瞬发出的消息不会再被记到后来打开的会话上，也不会在错误的会话里启动模型回合。
- 上下文压缩钩子失败或被取消时，内存中的对话历史不再随请求一起被拿走，整段对话不会丢失。
- 回复因长度截断时，不再为没有 id 的工具调用补写结果；空 `tool_use` id 不会再让下一次请求无法配对。
- 提供方返回错误状态后，响应体的读取同样受空闲超时约束，停滞的服务器不再让请求无限挂起。
- reasoning 明细流计入 8 MiB 累积上限，异常的提供方流不能再用不同 id 的明细帧无界增长内存。
- 应用内更新下载改为 30 秒空闲上限，不再被 30 秒总超时打断，慢连接也能下载大更新包；校验与解压改为流式并按实际写入的字节数设限。
- Web 工具与 MCP HTTP 通道在读取时强制响应大小上限，超限的端点不能先无界占满内存再被拒。
- 子代理的工作树差异捕获不再因为非 ASCII 路径失败（`git status` 关闭 quotepath 转义）。
- `settings.json` 的未知字段重新被拒绝：此前经 flatten 被静默忽略并在下次保存时无声丢失，现在走既有的备份重置路径。
- 点击查看含多字节字符的 diff 不再因固定字节切边界崩溃。
- 版本已是最新而远端恰好缺本平台资源时，更新检查保持"已是最新"，不再弹出取资源失败的报错。
- 崩溃残留的 projects 锁不再在切换项目目录时 panic。

### Changed

- 全库审计清理：移除 `run_code` 与 OS 沙箱的最后残留（shell 工具提示词、scout 提示词、设置页文案、模块文档）、永不发布的 Linux 会话探测、死代码与重复的包装层和平台 helper。
- 界面优化：每帧不再克隆完整会话列表；长工具运行的对话渲染由平方级降为线性；"Use skill" 流程改为提示模型直接调用 `read` 工具。
- 发版脚本的兜底说明与 CI 注释从"四个平台"改为三个平台。

## [0.11.13] - 2026-10-13

### Changed

- 回到原生工具：模型直接调用 `read`、`write`、`edit`、`grep`、`find` 和当前平台的一个 shell 工具，MCP 工具按原始名字直接调用。`run_code` 和它背后的常驻 Python、控制通道、嵌套卡片、预热进程池全部移除；Windows 上的管道、启动和生命周期问题随之消失。
- 移除尽力而为的操作系统沙箱（`tools.osSandbox`）。旧的设置值仍能读入，启动时清掉。进程以当前用户权限运行，环境变量白名单仍然只是过滤环境。
- 只发布 Windows x64、Windows ARM64 和 macOS Apple Silicon 三个平台；没有 Linux 安装包，Linux 上更新检查会说明没有对应平台的资源。
- 思考强度档位标签改为英文（Default、Low、Medium、High、Max 等），中英文界面一致，不再被输入框旁的窄标签截断。
- 上下文用量从对话一开始就显示在输入框旁（`0 / 窗口`），不再等第一次回复才出现。
- 界面新增 Microsoft YaHei 字体选项（安装了才会列出）；工具行和代码块显式选用本机第一个可用的等宽字体。输入框下方的小标签不再斜体。

### Fixed

- 自动更新换好并校验新二进制后，旧版本的备份文件（`*.mycode-previous`）立即删除，不再在安装目录里堆积；staged 校验失败时也不再留下半截备份。
- 思考菜单和模型菜单按窗口高度收紧最大高度，矮窗口里改为菜单内滚动，顶部选项不再被窗口边缘裁掉。

## [0.11.12] - 2026-10-12

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.11.11] - 2026-10-12

### Fixed

- 宿主进程异常退出，或控制连接关闭时，热的系统 Python 和 scout 的 `-I` 进程一起退出，包括正卡在程序里的那次调用。macOS 用 kqueue 监视父进程，父进程没了就退出。Windows 先挂起进程，挂上关闭即杀的 Job，再恢复主线程；挂载失败则这次调用失败，启动器不能把真正的解释器留在 Job 外面。
- 桌面截图换成最终重拍。对照表不再露出原始标记，账本时间不再盖住 You，画面里没有鼠标指针。

### Changed

- MCP 的 stdio 子进程在 Linux 上随拉起它的线程退出而被杀掉，在 Windows 上同样先挂起再入 Job。`tools.shell` 仍不设父进程死亡信号，因为它跑在可能提前退出的阻塞线程上。

## [0.11.10] - 2026-10-12

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.11.9] - 2026-10-12

### Added

- 子代理有墙钟超时。`subagents.timeoutSecs` 为 `0` 时使用默认的 20 分钟，排队等待不计入；到点会取消该子代理。设置里可以改，上限 2 小时。
- 输入框上方和详情面板列出正在运行的 scout 和 artisan，每一行可以单独取消。artisan 因为并发上限在排队时，列表和状态行会警告。
- Details 的实时 / 上一轮用量显示本轮缓存命中率。压缩或追加引导之后，如果下一次请求的命中率大幅下跌，Details 会留下警告并弹出提示。
- 全局命令面板。macOS 用 `Cmd-K`，Windows 和 Linux 用 `Ctrl-K`。可搜索会话、模型、斜杠命令和设置页。
- `tools.osSandbox` 默认开启，缺省不写进 `settings.json`，设为 false 才关闭。Linux 用 bubblewrap、macOS 用 seatbelt 包住 shell 和 `run_code`：工作区、额外工作区根和临时目录可写，只读角色的 Python 把工作区只读挂上，其余路径只读，网络不隔离。助手缺失、无法钉住，或可写路径是文件系统根时，这次调用失败，不退回无沙箱执行。Windows 没有这两个助手：通用设置页标明「暂不支持」，不把开关画成打开；默认开启的产品路径不套沙箱，shell 和 `run_code` 照常运行。运行时若仍被强制打开，这次调用失败，不退回无沙箱执行。这是尽力而为的写范围限制，不是安全边界。结果里的 `os_sandbox` 只表示用了哪个助手。沙箱实际打开时 `run_code` 不复用热进程；Windows 产品路径不打开，热进程复用保持原样。

### Changed

- pull request 的 CI 改为 Linux 上的 `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings`，不再为 PR 打四个平台的 fat LTO 发布包。发布构建仍只在推到 `main` 时跑，并且要等同一份检查通过才打标签。
- `ptc_eval` 补上分组读写、单文件 `tools.write`、技能 `SKILL.md`、MCP `server__tool`（含关键字括号调用）、多路 scout、`tools.shell` 跑测试，以及 `fetch_content` 4xx 不再重试。评分拒绝直接点名工具、`tools.bash` 这类别名、空程序、注释或字符串里的假调用，以及把只读或 shell 当成一次编辑。慢场景默认不打模型，设置 `MYCODE_PTC_EVAL_SLOW=1` 才跑；没有密钥时 live eval 仍跳过。
- 同一会话里的 `run_code` 复用已经启动的系统 Python，后续调用不再冷启动。用户全局、本次导入和对环境的修改会在下一次调用前清掉。scout 仍用单独的 `-I` 进程和 audit hook。取消、超时或解释器退出会丢掉这个进程。`tools.osSandbox` 在 Linux / macOS 上生效时不复用。Windows 产品路径不打开沙箱，仍复用。
- scout 的 audit hook 把 `O_TMPFILE` 算进 `os.open` 的写标志（它本来就和 `O_RDWR` / `O_WRONLY` 一起出现）。`-I` 和 hook 仍然是尽力而为。探针记下仍然开放的残留：已打开描述符上的 `os.write`、`os.memfd_create`，以及主代理 Python 不加 `-I`、可以直接写工作区。未开沙箱时 shell 仍能写工作区外面的路径。

## [0.11.8] - 2026-10-12

### Fixed

- 打开已有会话时，从账本尾部最后一条用量快照恢复上下文计量和缓存命中环，不必再等新的一轮。
- 设置里每个色板悬停都会更新对话预览。后一个色块不再被前一个色块的移出事件清掉。

### Changed

- 对话左侧的事件 id 默认隐藏，悬停该行才显示。你和代理各有头像与名字，用户消息是一条淡色气泡。正文与工具卡片同一左缘，超宽窗口才停在约 1200px，不再居中。代码块和表格可以用整列宽度。输入框仍居中，但更窄、默认更矮，打字时自动换行并随内容增高。
- 工具卡片按工具显示图标，并用颜色点表示进行中、成功和失败。`+N -M` 只出现在 `edit` / `write` 上：此前除网页工具外，所有以 `- ` 开头的行都被当成删除，agent / scout 的 Markdown 列表会显示成 `+0 -21`。连续的工具调用收成一行，例如 `read 3 files, edited 4`，点击展开。差异行带行号，红绿比以前柔和。
- 思考不再是输入框样式，改成一行灰色斜体加箭头；流式输出时有轻微闪烁。
- Markdown 标题缩小。表格可以横向滚动，斑马纹，表头在表体滚动时留在上方。
- 输入栏的目录、模型和思考强度是无边框的灰色斜体芯片，只带一个箭头。用量是小芯片，缓存命中率是一枚小环。标题栏的黄色 `vX ready` 换成一个小圆点，点击仍打开更新对话框。
- 设置卡片加宽，分组间距加大，说明文字对比更高。选择色板时有一段实时对话预览。圆角和间距收成 4px 的主题刻度。现有色板增加浅色模式。

## [0.11.7] - 2026-10-12

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.11.6] - 2026-10-12

### Fixed

- 编辑差异的增删标记改回单个 `+` / `-`。预览行和统计（绿色 `+N`、红色 `-N`）不再显示成 `++` / `--`。旧结果里的双符号在界面上同样收成单符号。
- 对话和输入框不再锁在同一条 760px 居中列里，改为铺满侧栏与 Details 之间的区域。Details 以浮层盖在右侧时，对话和输入框让出这块宽度，避免文字钻到卡片下面。

## [0.11.5] - 2026-10-12

### Fixed

- `shell` 的 `program` 模式会执行直接 shebang 包装（例如 `#!/bin/bash` 的 `/usr/bin/google-chrome`，或 `#!/usr/bin/env python3`）：钉住解释器映像，把脚本路径和 `args` 作为 argv 传进去，不拼 shell 命令字符串。`-c`、`env -S` 和批处理仍拒绝，错误写明改用 `mode` `script` 或自己指定解释器。
- PTC 控制通道：卡住或不带正确令牌的本机 TCP 连接不再占满整个启动窗口；非环回地址直接丢掉。令牌不再放进环境变量（`/proc/self/environ` 会一直保留 exec 时的初始环境）：Unix 从继承的套接字对写给子进程，TCP 放在读完即删的 `0600` 文件里。引导脚本写进随机名的 `0700` 目录。Linux 上 exec 前把多余文件描述符标成 close-on-exec。
- scout 的 audit hook 补上 `sqlite3`、chmod/chown/utime/xattr、`os.open` 的写标志（不论在哪个参数槽）、`forkpty` 和 `kill`。hook 仍然是尽力而为，不是安全边界；残留风险写在 `docs/tools.md`。
- 思考菜单锚在 Thinking 按钮右缘，模型菜单锚在模型按钮左缘。两块菜单都是不透明实底，过高时在菜单内滚动。

### Changed

- README 收成一句话、一张主图、短功能列表、安装、快速开始，以及 PTC 的短说明。旧截图换成和画面一致的 `docs/images/`。细节留在 `docs/`。
- `ptc_eval` 的场景写明「一个文件的查找」和「一次编辑」，评分接受 `tools.name` 和 `tools["name"]`，一次编辑不再把只读或 `tools.shell` 算通过。没有配置模型密钥时测试仍跳过。

## [0.11.4] - 2026-10-12

### Fixed

- 用户级技能目录（`~/.agents` 与其 `skills`）现在是宿主发布的只读锚定根：`read` 可以按文档教的方式直接读取 `SKILL.md`（此前被会话 cwd 边界拒绝，模型只能退回 shell，scout 则完全读不到）。`edit` / `write` 对该根仍然拒绝；主会话、子代理和 scout 继承同一份允许列表，`run_code` 内部调用同样生效。
- 思考强度跨会话记住：作曲栏最后一次选择写入 `ui.json` 的 `selectedReasoning`，新会话从它起步；模型没有记住的档位时自适应收敛到不高于它的最强可用档（记 `max`、模型只公布到 `high` 就落 `high`），完全对不上才回默认。此前每个新会话都重置为默认。

## [0.11.3] - 2026-10-12

### Changed

- 移除 tree-sitter：`edit` 删去 `ast` 操作（连同 36 个静态链接的语法 crate），gpui-kit 不再启用 `tree-sitter-languages`，对话里的围栏代码块单色渲染（gpui-component 退回无操作的高亮 stub）。编辑结果与 git diff 面板的红绿行前缀着色不受影响。release 二进制从 75.3 MB 降到 38.2 MB。

## [0.11.2] - 2026-10-11

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.11.1] - 2026-10-11

### Fixed

- Windows 上 `run_code` 不再挂死。Python 端以同步句柄打开命名管道，同一句柄同时只允许一个未完成 I/O：reader 线程阻塞在 `ReadFile` 时，主线程写 done / call / log 帧被永久挂起，每个程序都要等满 120 秒墙钟超时（"PTC 变慢"的根因）。控制通道在 Windows 上改用带一次性令牌的环回 TCP，socket 天然支持读写线程并发。
- Python 进程退出改为先优雅关闭控制通道（shutdown 唤醒 reader、FIN 排空发送侧）再退出。此前进程退出会带着仍在 `recv` 的 reader 线程硬关 socket，Windows 发出的 RST 会丢弃接收方缓冲里尚未读走的最后一帧（`done`），宿主误报 `python closed the control channel`。
- Windows 上不再拼装 `py.exe` 的绝对路径：探测和启动都直接按名调用 `py.exe -3`（`python.exe`、`python3.exe` 依次兜底），由系统从 PATH 解析；版本不对或商店别名会自然落到下个候选或安装指引。
- Python 探测与每次 `run_code` 子进程启动都带 `CREATE_NO_WINDOW`，不再闪黑色控制台窗口；`py.exe` 拉起的 `python.exe` 同样继承该标志。
- `run_code` 主循环把帧读取移到独立 reader 任务：回复写入与帧读取不再共享同一个 future，长度前缀帧在任意交错下完整送达。
- mycode-tools 在 Windows 上的测试首次可以编译并通过（`ppid_from_stat` 的 `libc` 依赖只在 Unix 存在）；此前 `cargo test -p mycode-tools` 在 Windows 直接编译失败，管道问题完全没有测试覆盖。
- 计时类并发测试改为基于探针起止区间的断言（互斥调用不得重叠），并对进程级 Python 探测覆盖加读写锁隔离，消除并行测试间的状态泄漏与负载抖动。
- 工作区统一启用 `serde_json` 的 `preserve_order`：此前该特性只随桌面 UI 依赖链传入，`run_code` schema 的 `description` 先于 `code` 的契约在不同构建下不稳定。
- 聊天滚动按钮重做：移除"滚动到最上"，只保留一个停靠在输入框上方的"滚动到最新"圆形按钮，未贴底时出现，点击可靠生效（旧的悬浮绝对定位箭头会飘进输入区且点击无效）。

### Changed

- 会话常驻内存治理：已打开会话的账本驻留数量有上限（LRU 驱逐、可从盘上按需重载，活跃回合与未落盘预留不会被驱逐）；事件 / 分支预留表有容量上限，最旧的废弃预留被丢弃；ask 路由槽位随回合结束自动回收。桌面端单个会话的转录条目驻留也有上限，超出时丢弃最旧行、可再次向前翻页。
- 数据导出/导入改为 v2 范围：导出全部配置（设置含 MCP、UI 状态、`secrets.json` 里的 API 密钥），不再包含任何会话 / 对话数据；v1 包仍可导入（会话部分被忽略）。导出文件是含明文密钥的 JSON，界面有明确警示。
- 删除"持久化用量记录"：不再按回合向会话账本写 Usage 事件，也不再在打开会话时重放汇总为每模型累计表；进行中的上下文计量（实时 token、缓存命中、最近回合统计）保留。旧设置文件里的 `usage` 字段可读入并被丢弃，旧账本里的 Usage 事件加载时跳过。
- `mycode-tools` 不再依赖 gpui-kit：tree-sitter 语法表改为直接链接同一批语法 crate，`mycode-app` / 工具库的依赖图里不再出现 smol / gpui（桌面版因 GUI 框架本身仍包含 smol，tokio 之外无其它自有运行时）。

### Added

- `run_code` 管道专项测试：2 MiB 级工具回复完整往返、大量 print 与工具调用交织不死锁、非 ASCII 输出与报错双向 UTF-8、子进程中途 `os._exit` 以 worker-exit 快速返回而不是挂起。
- 会话账本驱逐 / 预留上限 / ask 槽位回收的单元测试，以及导出导入 v2（含密钥、排除会话、v1 兼容、MCP 命令守卫）的测试。

## [0.11.0] - 2026-10-11

### Changed

- `run_code` 改为 PTC-only。模型只能直接调用它；`shell`、文件、搜索、网页、`agent`、`ask_user` 和其它工具都只能在脚本里写成 `await tools.name(...)`。直接点名其它工具会返回明确错误。没有第二种模式。
- 脚本改在系统 Python 3.10+ 上执行，每次一个新进程，环境按 shell 的允许名单裁剪。工具调用走与 stdout 分开的长度前缀 JSON 通道，并经过和原先直接调用相同的路径预检、写顺序、超时、输出上限、取消和进程树清理。只读调用可以 `asyncio.gather` 重叠；其余调用按提交顺序单独执行。只有 `print` 和 `return` 回到模型；出错前已经打印的内容保留；失败的工具调用抛出可捕获的 `ToolCallError`。内层调用不进入模型历史。
- 启动时按操作系统探测 `python3` / `python`（Windows 先用 `py`，并跳过 WindowsApps 商店别名），结果在进程内缓存，因此一次会话里的提示和工具模式保持稳定。找不到或版本不够时，`run_code` 返回带安装步骤的错误，窗口显示警告，提示词写明 Python 不可用。
- scout 以及其它只读工具目录里的 Python 在用户代码前安装 `sys.addaudithook`，拦截写、创建、删除、改名、建目录、subprocess / `os.system` / exec / spawn、ctypes 和原始套接字，并以 `-I` 和精简环境启动。这个 hook 是尽力而为，不是安全边界。主代理的 Python 与它的 shell 一样不受限，但直接调用 subprocess 会在结果里警告。
- 界面把每次内层调用画成原来的工具卡片，嵌在标题为 `description` 的 `run_code` 卡片下，并随会话保存。`tools.agent` 的进度、状态和取消与以前相同。`ask_user` 在脚本里会一直等到用户回答。拒绝问题是可捕获的 `ToolCallError`，程序继续；停止整个回合会中止程序。
- 控制通道优先使用继承的 Unix 套接字或 Windows 命名管道。退回本机 TCP 时，每次运行有一次性令牌，放在环境变量里，核对后才下发程序。未通过核对的连接会被拒绝。
- `asyncio.gather` 里的 `tools.agent` 并行。能写的子代理仍用自己的 worktree，宿主继续限制同时运行的个数。
- `fetch_content` 遇到 HTTP 4xx、HTTP 422 或 `extract_failed` 时把说明作为页面文本返回，并写明不要再请求这个 URL。
- MCP 工具在脚本里的名字是 `server__tool`。scout 只接收带 `readOnlyHint` 的 MCP 工具。
- 技能目录仍在提示词里。加载技能是 `await tools.read(path=...)`，子代理（含 scout）使用同一份目录。

## [0.10.3] - 2026-10-10

### Added

- `run_code`：嵌在二进制里的 Python 子集，一次调用里组合多次 `read` / `grep` / `find` / `edit` / 网页操作，只有 `return` 和 `print` 回到上下文。不依赖本机的 Node.js 或 Python。主代理和每个子代理用同一段提示。简单查找就地分组；较宽的代码库或网页研究可以由模型选择交给 `scout`。
- `web_search` 返回短摘要并去重、缓存；`fetch_content` 增加 `goal`，只回匹配摘录。

### Changed

- 进程工具不再读取 `MYCODE_SHELL`，也不再使用设置里的 `tools.shell`。解释器在进程启动时自动选定，工具名、参数说明和系统提示在整个会话里保持一致；子代理用同一个工具。旧的 `tools.shell` 仍能被读入，启动时忽略并在写回设置时去掉。若环境变量 `MYCODE_SHELL` 有值，进程提示一次后忽略。
- Linux / macOS 使用 `$SHELL` 里的 bash、zsh 或 sh。否则 macOS 依次试 zsh、bash、sh，其它 Unix 依次试 bash、zsh、sh。这些系统不选择 pwsh 或 cmd。工具名是 `bash`、`zsh` 或 `sh`，路径分隔符是 `/`。
- Windows 只自动选择 PowerShell：先是 PowerShell 7（`pwsh`，常规安装优先于 `WindowsApps` 目录下的执行别名），然后是 Windows PowerShell 5.1，最后才是 `cmd.exe`。不再把 Git Bash、MSYS2、Cygwin、`WindowsApps\bash.exe` 或 WSL 的 `System32\bash.exe` 当成 shell。商店别名按路径形状识别，不再靠 64 字节大小。

### Fixed

- 取消或超时会清掉整棵进程树。`setsid` 换了会话的子进程，以及子 shell 退出后被重新挂接、但仍留在原进程组里的后台进程，都会被结束。Linux 上 shell 会成为子进程回收者，这样脱离会话的孙子进程仍留在这棵树上。Windows 仍用不允许脱离的 Job Object。
- 后台子进程不再把工具拖到管道关闭。`sleep 20 & echo bg` 在 shell 退出时返回输出，而不是等 `sleep` 结束。
- `mycode-desktop --version` 和 `--help` 打印后退出，不再打开窗口。未知参数打印用法并以状态码 1 退出，不再打开窗口。Linux 上没有可用显示（变量未设置，或 `DISPLAY=:99` 这种有变量但没有 X 服务器）时，或窗口表面创建失败时，打印说明并以状态码 1 退出，不再以 101 崩溃。
- `run_code` 补上模型常写的 Python：元组解包、`for` 解包、下标赋值、`+=`、字符串重复、`enumerate` / `zip` / `sorted` / `min` / `max` / `sum` / `int`、字典 `items` / `get`。出错时先带回已经 `print` 的内容。`read` 进程序时去掉 `[revision ...]`。报错写明缺了什么、该怎么改。
- 上下文计量里的百分比标明是缓存命中（`cache hit 99%`），不再紧挨窗口用量看起来像已用比例。自动压缩把系统提示和工具模式算进估算。`fetch_content` 的 `goal` 命中某一节时返回这一整节（上限 8000 字符），而不只是标题后的一小段。
- `run_code` 补上 `list`、`dict`、`set`、`repr`、`isinstance`、`str.count`/`find`，以及列表和字典的常用方法（含 `setdefault`、`update`、`dict(a=1)` 和 `g.update(n=3)`）。语法和运行错误按用户源码行号标出那一行。进程工具省略 `mode` 时按 `script` 执行。思考菜单开在 Thinking 按钮上方，背景不透明，底边停在输入框上方，不再裁掉对话，也不贴在窗口右缘。`fetch_content` 命中某一节时包含其下各小节，上限 48000 字符。一次 `grep` 能回答的查找可以保持直接调用。
- Windows 上 `WindowsApps\Microsoft.PowerShell_*\pwsh.exe` 不再直接启动（该路径没有执行权限），改用 App Execution Alias（`WindowsApps\pwsh.exe`）。写回 `settings.json` 时去掉旧的 `tools.shell`。没有 shell 的角色（含 `scout`）提示里不再出现 `shell_tool`。窗口标题改为 `MYCode`。

## [0.10.2] - 2026-10-10

### Changed

- GitHub Release 的说明自动包含本文件里该版本的条目，以及自上一个标签以来合并的 pull request、这些 PR 关闭的 issue 和比较链接。每次发版生成，不用手写。
- 写回 `main` 的版本提交使用 `GITHUB_TOKEN`，`release-plan` 忽略 `github-actions[bot]` 的推送，避免一次发布把补丁号连加两次。pull request 和 `main` 仍共用 `ci.yml`。

### Fixed

- macOS 的 `Build release binary` 在编译前会先访问 `index.crates.io`，并且只对域名解析、下载和 checksum 失败重试，编译错误不重试。`797dfde` 那次发布失败是因为 cargo 在大约 80 秒内耗尽对 `index.crates.io` 的解析重试（`Could not resolve host` / `download of config.json failed`），构建步骤没有外层重试，macOS 一失败就跳过了 Release。

## [0.10.1] - 2026-10-10

### Changed

- 进程工具按启动时解析到的解释器只注册一个：Windows 上优先 PowerShell 7（`pwsh`），其次 Windows PowerShell 5.1（`powershell.exe`），工具名是 `powershell`，描述和参数按该版本的 PowerShell 来写。Git Bash 仅在两种 PowerShell 都没有，或设置 / `MYCODE_SHELL` 指定时使用，工具名是 `bash`。都不在时运行时才用 `cmd`，且不写入设置。不自动选择 WSL。Linux / macOS 仍是 `bash`。命令按该解释器原样执行，不再把 bash 翻译成 PowerShell。
- 系统提示词的 `<environment>` / `<shell>` 写明当前 OS 和这一个 shell 的语法（PowerShell 5.1 不用 `&&` / `||`，路径分隔符和引号跟解释器走）。子代理使用同一个工具和同一套说明；白名单里的 `shell`、`bash`、`powershell`、`cmd` 不会再变成第二个 shell。scout 仍然不注册进程工具。
- 四个平台的构建在 crates.io 域名解析或下载失败时会重试，编译错误不重试。macOS 上一次发布（`797dfde`）就是卡在 `index.crates.io` 解析失败。pull request 和 `main` 共用 `ci.yml`：都会构建四个平台，只有推到 `main` 且版本还没有标签时才打标签并上传 Release。`release.yml` 已删除。

## [0.9.28] - 2026-10-10

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.9.27] - 2026-10-10

### Added

- 系统提示词新增 `<environment>` 块：OS 与架构、会话 cwd、当前解析到的 shell（`script_shell_line()`，与脚本模式同一条解析链：设置 → 检测 → Windows 的 cmd 兜底）。子代理提示词同样注入一份，cwd 是各自的运行目录。模型不再需要靠报错猜平台或 shell 方言。

### Changed

- 系统提示词不再出现两句身份声明：工具清单前的「You are MYCode Agent…」改为纯粹的工具契约（「Complete the task with the tools listed below. Do not invent tools.」），身份由会话或子代理提示词开头各声明一次。
- shell 为 PowerShell 且 bash 单行命令被翻译成 cmdlet 时，结果文本第一行注明 `[bash command translated to PowerShell: …]`，细节里新增 `translated_command`：改写对模型可见，不再静默成功让 bash 习惯看起来直接可用。
- `shell` 工具描述删去跨平台 shell 枚举句，改为指向系统提示词的 `<environment>` 块；翻译说明同步注明结果会标注改写。

## [0.9.26] - 2026-10-10

### Fixed

- 会话里超长的运行状态行（如整条 shell 命令）不再横向溢出对话列，改为单行内截断。
- 对话列右下角的「顶部/底部」胶囊按钮替换为悬停显示的居中圆形箭头：靠近标题栏的向上箭头与输入框上方的向下箭头，仅在鼠标悬停对话区域且对应方向可滚动时出现。
- 详情面板与输入框下方的上下文统计口径对齐：上下文行补上缓存命中率（与输入框逐项一致），输入/输出/缓存/轮次归入「会话累计」分组，不再和最新一次提示的缓存数字混淆。
- 切换模型不再丢失已选的思考强度：当前模型不支持该档位时显示「思考 · 默认」，切回支持的模型自动恢复；请求只携带当前模型支持的档位。
- 手动 `/compact` 对很短的会话也会生成摘要检查点，不再提示没有可压缩的上下文。

### Added

- Windows 上 shell 为 PowerShell 时，常见 bash 单行命令（`ls -la`、`rm -rf`、`cp -r`、`mkdir -p`、`touch`、`which`、`head`/`tail`、`wc -l`、`grep`、`find -name`）自动转换为对应 cmdlet，`2>/dev/null` 转为 `2> $null`；无法识别的命令原样执行。

### Changed

- 修复平台差异导致的测试失败（Windows ACL 测试夹具、绝对路径断言），并清理全部编译警告。
- 发版前的全仓代码审计清理：更新下载的全部磁盘阶段移出单线程核心运行时（下载期间不再卡住界面与对话）；一轮以工具步骤收尾时补记用量统计；Windows 更新器复归正常返回结构；ask 面板的自由文本在提交后清空；悬空的工作区绑定回退到第一个工作区；目录选择器的截断标记与长文件名、Toast 长文本不再溢出；PowerShell 序言语法识别大小写（`Param($x)` 不再插入失败）；Anthropic 网关缺 `content_block_start` 的文本增量不再在最终消息中丢失；清理死代码（七个未发布的调色板规格、无调用的 HTTP 客户端与搜索包装、调试遗留语句）、合并 shell/program 两路执行的四组重复 helper 与跨模块重复函数，并修正十余处过期注释与错误文案。

## [0.9.25] - 2026-10-10

### Added

- init 第一版：mycode 首次公开发布。
- 四个平台的发布包：Windows x64（`x86_64-pc-windows-msvc`）、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`，zip 与 `.dmg`）、Linux x86_64（`x86_64-unknown-linux-gnu`），均附 `.sha256`。
- Windows 上 `shell` 的脚本模式优先 PowerShell 7（`pwsh`），其次 Git bash，不侦查 Windows PowerShell 5.1。两者都没有时，运行时才退到 `cmd.exe`，并且不把这次退路写进设置。`pwsh` 以 UTF-16LE 的 `-EncodedCommand` 启动，并尽量把管道编码设为 UTF-8。标准输出和标准错误里的 CLIXML、`_xHHHH_` 转义和 ANSI 颜色会收成可读文本。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.11.12...HEAD
[0.11.12]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.12
[0.11.11]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.11
[0.11.10]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.10
[0.11.9]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.9
[0.11.8]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.8
[0.11.7]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.7
[0.11.6]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.6
[0.11.5]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.5
[0.11.2]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.2
[0.11.1]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.1
[0.11.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.0
[0.10.3]: https://github.com/MCapricorns/mycode/releases/tag/v0.10.3
[0.10.2]: https://github.com/MCapricorns/mycode/releases/tag/v0.10.2
[0.10.1]: https://github.com/MCapricorns/mycode/compare/v0.9.28...v0.10.1
[0.9.28]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.28
[0.9.27]: https://github.com/MCapricorns/mycode/compare/v0.9.26...v0.9.27
[0.9.26]: https://github.com/MCapricorns/mycode/compare/v0.9.25...v0.9.26
[0.9.25]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.25
