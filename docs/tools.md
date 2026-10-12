# mycode-tools

模型能直接调用的工具只有 `run_code`。文件、搜索、进程、`ask_user`、`agent`、网页和 MCP 都在脚本里写成 `await tools.name(...)`。本 crate 定义 trait、注册表和这些内置实现；宿主（`AskChannel`、`AgentHost`、`WebHost`）和 MCP 在 `mycode-app`，在每个回合注册进去。

```text
模型的 tool_call（只能是 run_code）
    → Python 进程里的 await tools.name(...)
    → 长度前缀 JSON 控制通道
    → 按名字取出 ToolDyn，走同一套参数预检、路径锚点和权限检查
    → ToolResult
         content  回到脚本（失败则抛 ToolCallError）
         details  只给界面（diff、字节数），不进上下文
```

## 设计思路

一份 schema 两处用。`schemars` 从参数结构体生成 JSON Schema，既放进 `ToolSpec` 给模型，也在 `execute_dyn` 里校验。模型看见的和运行时接受的是同一份（发给服务商前 `$ref` 会被展开，见 providers.md）。

注册表按名字最后写入者生效。发给模型的工具模式只有 `run_code`；其余工具的签名写在 system prompt 的 `<tools>` 里。内层调用仍按那份 JSON Schema 校验。未知工具、参数错误和执行错误变成脚本里可捕获的 `ToolCallError`，回合循环不因此停掉。没有「先问用户准不准」的回调。

文件发现和内容搜索在进程内完成（自己的句柄相对遍历加 ripgrep 的搜索核心），不依赖外部的 `fd` 或 `rg`。这样工具机器上没有这些二进制也能用，也少一次进程启动。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `tool` | `Tool` / `ToolDyn`、`ToolResult`、`ToolError` |
| `registry` | 名字 → 工具。`prompt_entries` 给 system prompt 列名字和一句说明 |
| `ctx` | 一次调用的上下文 `ToolCtx`：会话工作目录、取消令牌、调用 id，以及派发前绑定好的搜索根 / 文件句柄 |
| `roots` | `anchor_tool_path`：落在额外工作区根下的绝对路径改锚到那个根（多个命中取最深的），其余路径留在工作目录，逃出根的照样被拒 |
| `stream` | 进度事件，然后恰好一个终止事件 |
| `builtin` | 下面这六个内置工具，以及 `ask_user`、`agent` 和两个网页工具 |

`ToolStream` 的终止标记在发送成功后才置位：多个克隆里只有一个能写下终止，终止之后的进度被静默丢弃；接收端已经关闭时发送失败、不置位，之后的条目因为通道断开而失败，而不是被一个从没送达的终止吞掉。

## 内置工具

| 工具 | 行为 |
| --- | --- |
| `read` | 带行号读 UTF-8 文件（去掉 BOM），返回不透明的 revision。`offset` 是从 1 开始的行号，schema 写着 `minimum: 1`，传 `0` 在校验阶段就被拒，内核里也再挡一次，不会当成第一行；省略时从第一行读。`limit` 是行数。每次最多 2000 行或 50 KiB，超出时附说明让模型用 offset / limit 再读。文件超过 32 MiB 直接拒绝 |
| `write` | 创建或整文件替换，最多 8 MiB。新文件原子创建（缺的父目录一并建）；已有文件必须带上次 `read` 的 `expected_revision`，或显式 `overwrite: true`，两者不能同用 |
| `edit` | 一个快照上的一批操作（最多 32 个），一次发布：`literal`（memmem / Aho-Corasick）、`fuzzy`（归一化后的 Levenshtein，只接受唯一且领先足够的最佳匹配）、有界 `regex`、行范围。也接受旧的 `{old_string, new_string}` 唯一替换。返回 revision 和有界 diff 摘要，不回整个文件。模糊匹配在分词和扫描候选窗口时每约 4 KiB 原文检查一次取消 |
| `find` | 按 glob 找文件和目录，限制在搜索根内，遵守 `.gitignore`，跳过隐藏和被忽略的路径。默认最多报告 1000 条，排序后用 `/` 分隔。取消或超时返回错误，不给半截结果 |
| `grep` | 进程内的内容搜索，默认字面量，`is_regex` 开正则，支持 include / exclude glob。默认最多报告 200 行 `path:line:text`，每行最多 500 字节。跳过隐藏和被忽略的文件 |
| `powershell` / `bash` / `zsh` / `sh` / `cmd` | 唯一的进程启动工具。模型不能直接点名它，只能在 `run_code` 里写 `await tools.shell(...)`。注册表里的名字跟启动时解析到的解释器走，整个会话不变：PowerShell 是 `powershell`，bash 是 `bash`，zsh 是 `zsh`，POSIX sh 是 `sh`，只有 Windows 上两种 PowerShell 都没有时才是 `cmd`。`mode` `script` 把 `command` 交给这个解释器（管道、重定向、展开、脚本，以及用 Python 改文件）。`mode` `program` 用显式 `args` 启动，不解析 shell 命令字符串。PE / ELF / Mach-O 直接运行。直接 shebang 包装（一个解释器、最多一个参数，例如 `#!/bin/bash` 或 `#!/usr/bin/env python3`）改为执行那个解释器，脚本路径和 `args` 跟在后面。批处理和 shebang 参数 `-c` / `env -S` 被拒，错误里写下一步 |

进程工具的两条模式共用启动路径：钉住程序身份、限制参数和环境变量、截断约 50 KiB 输出、默认 120 秒超时（`timeout_secs` 可改）。超时和取消时终止并回收整棵进程树，包括 `setsid` 脱离进程组的子孙，以及子 shell 退出后仍留在原进程组里的后台进程。shell 自己退出后工具就返回，不等后台子进程关掉继承的管道。丢掉 future 也会把清理交出去。非零退出码是 `is_error` 结果，不是循环故障。环境变量白名单不是隔离。`tools.osSandbox` 默认 true，缺省不写进 `settings.json`。Linux 用 bubblewrap、macOS 用 seatbelt 包一层尽力而为的写范围限制；助手缺失或无法启动则这次调用失败，不退回无沙箱执行。这不是安全边界。Windows 没有这两个助手：设置页标明「暂不支持」，默认开启的产品路径不套沙箱，shell 和 `run_code` 照常运行。运行时若仍被强制打开，这次调用失败，不退回无沙箱执行。每次 `script` 都是新进程，`cd` 和变量赋值不会留到下一次调用。

Windows 上脚本模式按层侦查，命中一层就停：PowerShell 7（`pwsh`），然后 Windows PowerShell 5.1（`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`）。`pwsh` 的候选顺序是 PATH 里的 `pwsh.exe`、`Program Files\PowerShell\7`、预览版、WindowsApps 里的 `Microsoft.PowerShell_*` 包、WinGet 和 scoop。常规映像优先。`WindowsApps\pwsh.exe` 这种直接放在 `WindowsApps` 目录下的商店执行别名只在没有常规 `pwsh` 时使用，并且按路径形状识别，不看文件大小；包目录里的 `pwsh.exe` 仍算常规映像。不选择 Git Bash、MSYS2、Cygwin，也不选 WSL 的 `System32\bash.exe` / `SysWOW64\bash.exe`：会话 cwd 是 Windows 路径。SysWOW64 里的 32 位 `powershell.exe` 也不选。两层都没有时，运行时退到 `%SystemRoot%\System32\cmd.exe`（`/d /s /c`），工具名变成 `cmd`，这个退路不写入设置。Windows 自带的是 5.1，不是 5.0；它和 PowerShell 7 的 `&&` 不通用，所以只有没找到 `pwsh` 时才用它，提示词也改成 5.1 的语法。这和 Codex、Gemini CLI 的 Windows 顺序一致；pi 和 Claude Code 在 Windows 上以 bash 为先，这里不跟，因为要优先 PowerShell 7。

Linux / macOS 先看 `$SHELL`。它是绝对路径且文件名是 bash、zsh 或 sh 时就用它。否则 macOS 依次试 `/bin/zsh`、`/bin/bash`、PATH 里的 zsh 和 bash，再试 `sh`；其它 Unix 依次试 bash、zsh、`sh`。`$SHELL` 若是 pwsh、fish 或其他家族，就跳过。这些系统不选择 pwsh 或 cmd。工具名跟实际解释器走。

`MYCODE_SHELL` 和设置里的 `tools.shell` 都不再切换解释器。环境变量有具体取值时提示一次并忽略；设置里的旧字段仍能读入，启动时清掉，下次保存就不再写出。

PowerShell（7 和 5.1）不用 `-Command` 拼接用户字符串，也不把 bash 改写成 cmdlet。脚本先留下 PowerShell 要求放在最前的空行、注释、`using` 和 `param (...)` 块，再插入一段把管道编码设成 UTF-8 的前奏（主机禁止改编码时这段被跳过，用户脚本照常跑），然后整段按 UTF-16LE 做 Base64，用 `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand` 启动。空脚本会补一行注释，避免空的 `-EncodedCommand` 被拒绝。命令行按 UTF-16 码元计，上限 32767。bash、zsh 和 sh 用 `-c`。用户命令原样执行。

进程工具不出现在模型的工具列表里。SDK 里它叫 `tools.shell`，描述和 `<environment>` / `<shell>` 按当前解释器来写：PowerShell 7 可以用 `&&`；Windows PowerShell 5.1 上 `&&` 和 `||` 是语法错误，要用 `;` 和 `$LASTEXITCODE`。提示词不再提 bash 到 PowerShell 的翻译。子代理用同一套解析：白名单里的 `shell` / `bash` / `powershell` / `cmd` 都变成这一个工具，提示词里的语法说明跟它一致。scout 不注册进程工具。

进程输出先认 BOM，再认没有 BOM 的 UTF-16LE（PowerShell 往管道写中文时经常这样，这些字节有时也是合法 UTF-8），然后才是严格 UTF-8。还不行时，Windows 依次试 OEM 代码页、ANSI 代码页和控制台输出代码页（中文 Windows 上 `cmd` 的 `dir` 往往是 GBK）。标准输出和标准错误都会再整理一次：有 `#< CLIXML` 时抽出 `Error` 和 `Warning` 节点，`_xHHHH_`（包括 `_x001B_`）还原成字符，ANSI / VT 序列去掉。没有这些标记的普通文本保持原样。

`program` 模式的实现按平台拆开（Windows x64 与 Windows ARM64 共用 `CreateProcessW`、macOS Apple Silicon、Linux x86_64 glibc）。发布包包含这四个平台。摘要复查和参数组装共用。其它 Unix 目标（musl、Android、BSD）不支持直接启动映像。SDK 里始终是 `tools.shell`，背后只有当前这一个解释器（`powershell`、`bash` 或 `cmd`），没有 `exec`，也不会同时有 `bash` 和 `powershell`。角色白名单里的 `shell` 仍表示这个进程工具。

## 搜索的边界

搜索有独立的预算：60 秒截止、每次 grep 最多读 512 MiB、结果最多存 10000 条、输出最多 100 KiB、I/O 错误样本 5 条。到顶就停并说明停因，而不是把目录树读完。

`grep` 和 `find` 只解析一次工作目录，之后所有打开都相对保留的根句柄，不再按路径名重新解析，也不跟随符号链接。不跨挂载点：

- **Linux**：每个子项用 `openat2(RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_SYMLINKS)` 打开，内核没有 `openat2` 时直接失败，不退回 `openat`。打开后用 `STATX_MNT_ID` 比较父子的挂载 ID：ID 相同就是同一挂载，即使 overlay 让目录和文件的 `st_dev` 不同；ID 不同就是越界，包括 `st_dev` 相同的 bind mount。拿不到挂载 ID 时退回比较 `st_dev`。`fs_io/unix.rs` 里 `read` / `write` / `edit` 的打开用同一规则
- **其它 Unix**：`openat(O_NOFOLLOW)` 后比较 `st_dev`，那里 `st_dev` 就是挂载身份
- **Windows**：每个分量相对保留的目录句柄用 `NtOpenFile` 打开，拒绝重解析点

搜索时链接数不为 1 的普通文件被拒，避免工作目录里的硬链接暴露根外的 inode（文件工具允许硬链接，写入时发布新 inode）。向上找祖先目录的 ignore 文件时碰到挂载边界就停，这不算失败，搜索照常返回结果。

## 宿主工具（定义在本 crate，装配在 app）

| 工具 | 宿主提供什么 |
| --- | --- |
| `ask_user` | 把 1–4 个问题送到界面，等用户答完再继续。在脚本里拒绝问题会抛出可捕获的 `ToolCallError`，程序继续。停止整个回合会中止程序 |
| `agent` | 按角色再跑一个有白名单的子循环。只在能独立并行、边界清楚、并且能降低成本或提高完成质量时使用。工具名是 `agent`，在脚本里是 `await tools.agent(...)`，没有 `task` 别名。`asyncio.gather` 里的多个 `tools.agent` 会并行；能写的角色用 worktree，不共用会话目录。宿主仍限制同时跑几个。内置角色只有 scout 和 artisan，也可以用 `agents/*.md` 里的自定义角色。子代理不能问用户。子代理有墙钟超时：`subagents.timeoutSecs` 为 `0` 时默认 20 分钟（1200 秒），上限 2 小时，排队等待不计入。并发默认 4；设置为 `0` 表示这个默认值，不是零个 |
| `run_code` | 模型唯一能直接调用的工具。参数是 `description` 然后 `code`：`code` 是系统 Python 3.10+ 里一段 async 函数体。其它工具只能写成 `await tools.name(...)`，名字不是标识符时用 `await tools["name"](...)`。同一会话、同一种隔离（主代理或只读 scout）复用已经核对过令牌的解释器，所以第二次调用不再冷启动；没有会话键时仍是每次新进程。`tools.osSandbox` 在 Linux / macOS 上生效时不复用进程，每次按这一次的 cwd 冷启动，并进入同一套尽力而为的操作系统写限制。Windows 产品路径不打开这层，热进程复用保持原样。每个键最多留 2 个空闲进程，空闲超过约 10 分钟后由下一次 `run_code` 回收，删除会话时立刻关掉。两次调用之间清掉用户全局、本次 import、对 `builtins` 和 `os` 公共名字的替换、环境变量和 `sys.path`，并把工作目录换成这一次的 cwd。精简环境只在进程启动时拍一次。控制通道在 Unix 上是继承的套接字对（没有落在文件系统上的套接字），令牌从这根套接字交给子进程。Windows 以及套接字对打不开时用带一次性令牌的 `127.0.0.1` TCP，令牌在一个 `0600` 文件里，读完即删。令牌不放在命令行，也不放进环境变量。核对之后才下发第一段程序；之后的程序走这根已经核对过的通道。内层调用经过同一套路径预检。`read`、`grep`、`find`、`web_search`、`fetch_content` 和 `agent` 可 `asyncio.gather` 重叠；`write`、`edit`、`shell` 按提交顺序单独执行。只有 `print` 和 `return` 回到模型。一次最多 48 次工具调用、8 路并行、32 个未完成调用、120 秒；回到模型的文本上限 8000 字节。程序里的 `read` 不带 `[revision ...]` 标签。找不到 Python 时返回按操作系统写的安装步骤，窗口用同一段说明做横幅 |
| `web_search` / `fetch_content` | 有界 HTTP：每次最多 8 条结果 / 8 个 URL。搜索结果是标题、URL 和最多 240 字符的摘要，重复 URL 丢掉，相同查询缓存 10 分钟。`fetch_content` 带 `goal` 时只返回匹配摘录（每页约 1200 字符）；不带 `goal` 时每页最多 8000 字符，截断标 `(truncated)` |

网页后端由设置决定：启用的那个优先；都没启用时用第一个有钥匙（环境变量或 `web-<id>`）的后端。Querit 和自定义后端走 `POST {endpoint}/v1/search` 与 `/v1/contents`，AnySearch 走 `/v1/search` 与 `/v1/extract`。Querit 没钥匙时调用失败并提示去设置页粘贴或设 `QUERIT_API_KEY`；已启用的 AnySearch 没钥匙时匿名访问。

MCP 工具在脚本里叫 `server__tool`（例如 `await tools.docs__lookup(...)`）。小服务直接进 SDK；大服务仍走 `tools.search_tool` 和 `tools.use_tool`。调用远端时用原来的工具名。scout 只拿到带 `readOnlyHint: true` 的 MCP 工具，没有这个标注的不给。技能目录在 system prompt 的 `<skills>` 里，正文不进提示；匹配时在 `run_code` 里 `await tools.read(path=...)` 读取对应的 `SKILL.md`。子代理（含 scout）用同一份目录。

## 改文件

`write` 和 `edit` 仍然可用。进程工具的 `mode` `script` 也可以改文件：bash 里用 Python（`python3` 或 `python`）的引号 heredoc 或短脚本，PowerShell 里用 here-string 管道给 `python`。`mode` `program` 不解析 shell 命令字符串。可加载映像直接启动；直接 shebang 包装改由它的解释器执行。这些路径都不做文件快照。撤回或编辑一条对话不会把工作区文件恢复或删掉。

## 残留风险

这些是现在代码的真实边界，不是还没做的待办。

- **控制通道。** 令牌是两个 UUID v4 拼成的十六进制串，等长比较不提前返回。空令牌拒绝。帧是 4 字节大端长度加 UTF-8 JSON；长度为 0 或超过 8 MiB 的帧在分配缓冲之前拒绝，畸形 JSON 结束这次程序。Unix 用 `socketpair`，描述符不在文件系统上，令牌从这根套接字写给子进程，不进环境变量（`/proc/self/environ` 保留的是 exec 时的初始环境，`unsetenv` 清不掉）。Windows 不用命名管道（Python 对同步管道句柄的读写会互相卡住），只听 `127.0.0.1` 上的临时端口；令牌写在引导目录里一个 `0600` 文件，子进程读完就删，环境里只有这个路径。非环回地址的连接丢掉。一个不发 `ready` 或令牌不对的连接最多占 500 毫秒，然后继续等真正的解释器；连续 32 次失败就关掉这次程序。核对通过后监听套接字关闭。同一用户仍能在令牌文件删掉之前读到它，也能看见初始环境里的描述符号或文件路径；这和 shell 一样，不把同账户敌对进程算进边界。
- **引导脚本。** 写在临时目录里一个随机名字的 `0700` 目录中，文件 `0600`，`create_new` 不覆盖已有文件。脚本内容本来就编进二进制，不是秘密。
- **继承的文件描述符。** Linux 上在 exec 前用 `close_range(CLOSE_RANGE_CLOEXEC)` 把 3 号及以上的描述符标成 exec 时关闭，再只对控制通道清掉这个标志。其它 Unix 对 3 到 255 做同样的标记。热进程只在启动时密封一次，不会在后续调用里重新 exec。Linux 上子进程设置了父进程死亡信号，宿主退出时解释器会被杀掉。
- **热解释器。** 复用的是同一根已经核对过令牌的控制通道，不是每段程序一张新令牌。同账户能碰这根套接字的人，在首次核对之后不必再出示令牌。用户脚本装上的 audit hook 卸不掉，可以让后面的调用在这个进程里失败；取消、超时、协议错误、进程退出，或子进程报告不能再复用（还有活着的线程、引导里的关键对象被换掉）时，这个进程会被丢掉，下一次重新冷启动。已经导入的模块对象内部的改动、用户自己开的描述符和 `mmap`，重置盖不住。卡在纯计算里的解释器在 macOS 上要等它再碰控制通道才发现宿主已经没了。`tools.osSandbox` 在 Linux / macOS 上生效时不进入这个池，每次按当时的 cwd 冷启动，避免把上一次的写范围套到下一次调用上。Windows 产品路径不打开这层，仍进入这个池。这些都不是安全边界。
- **scout。** 只读目录（没有 `write` / `edit` / `shell` / `agent`）时 Python 加 `-I`，环境是 shell 那份允许名单，用户代码前安装 audit hook。hook 拦截带写标志的 `open`（标志在任一参数槽里都算，包括和 `O_RDWR` / `O_WRONLY` 一起用的 `O_TMPFILE`）、chmod/chown/utime/xattr、删改名和建目录、`sqlite3.*`（审计事件不带打开模式，所以只读连接也一起拒绝）、subprocess / exec / spawn / `forkpty` / `kill`、ctypes，以及新建套接字。`readOnlyHint` 只有 JSON 布尔 `true` 才算只读；缺省、`false` 或字符串 `"true"` 都不进 scout 的 MCP 目录。同一会话会复用这个进程，用户全局在两次调用之间清掉；它不会改用主代理的解释器。`-I` 和这道 hook 都是尽力而为，不是安全边界。
- **hook 挡不住的东西。** 已经打开的描述符上的 `os.write`（控制通道自己就这样写帧；标准输出那根管道也能写；宿主仍然只派发目录里的工具）。`os.memfd_create` 没有审计事件，匿名 memfd 上的 `os.write` 也不会被 hook 看见。不经过解释器的本机代码，例如工作区里一个 `.so` 被 `import` 之后直接 syscall。`mmap` 如果能拿到可写映射。主代理的 Python 不加 `-I`，用户 site 的 `sitecustomize` 会在令牌从环境删掉之前运行，并且可以直接写工作区文件；scout 有 `-I`，不会加载用户 site。这些都不是安全边界。主代理的 Python 与它的 shell 权限相同。
- **操作系统沙箱。** `tools.osSandbox` 默认 true，缺省不写进 `settings.json`，设为 false 才关闭。通用设置页在 Linux / macOS 上提供开关，默认是开。打开之后，shell 和 `run_code` 的 Python 在 Linux 上由 bubblewrap（`bwrap`，先找 `/usr/bin/bwrap`、`/bin/bwrap`，再找绝对 PATH）包一层，在 macOS 上由 `/usr/bin/sandbox-exec` 的 seatbelt 配置包一层。工作区、额外工作区根和临时目录可写；scout 这类只读目录的 Python 把工作区只读挂上，临时目录也只读。其余路径只读。网络不隔离，不用 PID namespace。读密钥文件、写 `/tmp`（工作区模式会把整个临时目录放开；工作区若落在临时目录里面，同级路径也能写）、同一用户的其它进程，都不在这层的承诺里。助手找不到、路径无法写进 seatbelt 配置、或可写路径是文件系统根时，这次调用失败，不会退回无沙箱执行。Windows 没有这两个助手。设置页标明「暂不支持」，不把开关画成打开。默认开启的产品路径不套沙箱，shell 和 `run_code` 照常运行，热进程也照常复用。运行时若仍被强制套上这层，这次调用失败，不退回无沙箱执行。shell 仍先钉住用户映像，再钉住助手并 exec 助手；助手随后按路径 exec 用户映像，中间窗口仍在，这不是对内层程序的 `execveat`。`run_code` 的 Python 本来就按名字启动，打开沙箱后改为启动助手，再由助手按名字 exec 解释器，解释器本身没有映像钉。结果 details 里的 `os_sandbox` 只表示用了 `bubblewrap` 或 `seatbelt`，`os_sandbox_note` 写明这是尽力而为的写范围限制，不是安全边界。审计 hook 和 `-I` 不因为开了这层就变成边界。打开沙箱时 `run_code` 不复用热进程。
- **`program` 模式的 shebang。** 只跟一层。解释器必须是可加载映像。`#!/usr/bin/env name` 会在允许的 PATH 里找 `name`。`-c`、`--command` 和 `env -S` 拒绝，错误说明改用 `mode` `script` 或自己把解释器放进 `program`。包装脚本的正文仍然按那个解释器执行，参数是 argv，不是拼进 `sh -c` 的字符串。批处理仍然拒绝。

## 不放在这里的东西

本 crate 不自己发现哪个会话正在打开，也不保存密钥。工作目录、派发前绑定的句柄，以及用来复用 Python 的会话键，由 `ToolCtx` 带进来；额外文件夹、网页钥匙和 MCP 连接由宿主在回合开始时放进来。没有会话键的调用仍然每次新进程。
