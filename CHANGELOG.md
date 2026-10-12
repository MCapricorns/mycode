# Changelog

0.9.25 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

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

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.11.5...HEAD
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
