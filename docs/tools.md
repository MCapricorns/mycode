# mycode-tools

模型能调用的工具。这里定义 trait、注册表，以及文件、搜索和进程这组内置实现，也定义 `ask_user`、`agent`、`web_search`、`fetch_content` 这几个工具本身；它们背后的宿主（`AskChannel`、`AgentHost`、`WebHost`）和 MCP 在 `mycode-app`，在每个回合注册进去。

```text
模型的 tool_call
    → 按名字取出 ToolDyn
    → 用该工具的 JSON Schema 校验参数
    → 执行
    → ToolResult
         content  回到模型
         details  只给界面（diff、字节数），不进上下文
```

## 设计思路

一份 schema 两处用。`schemars` 从参数结构体生成 JSON Schema，既放进 `ToolSpec` 给模型，也在 `execute_dyn` 里校验。模型看见的和运行时接受的是同一份（发给服务商前 `$ref` 会被展开，见 providers.md）。

注册表按名字最后写入者生效，规格按名字排序后发给模型，保证请求体稳定。已经注册且参数合法的调用直接执行。未知工具、参数错误、取消和执行错误都变成工具结果里的 `is_error`，循环不因此停掉。没有「先问用户准不准」的回调。

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
| `edit` | 一个快照上的一批操作（最多 32 个），一次发布：`literal`（memmem / Aho-Corasick）、`fuzzy`（归一化后的 Levenshtein，只接受唯一且领先足够的最佳匹配）、有界 `regex`、行范围、`ast`（用 gpui-kit 注册的 Tree-sitter 语法做捕获替换，改完重新解析，新增语法错误就拒绝）。也接受旧的 `{old_string, new_string}` 唯一替换。返回 revision 和有界 diff 摘要，不回整个文件。模糊匹配在分词和扫描候选窗口时每约 4 KiB 原文检查一次取消 |
| `find` | 按 glob 找文件和目录，限制在搜索根内，遵守 `.gitignore`，跳过隐藏和被忽略的路径。默认最多报告 1000 条，排序后用 `/` 分隔。取消或超时返回错误，不给半截结果 |
| `grep` | 进程内的内容搜索，默认字面量，`is_regex` 开正则，支持 include / exclude glob。默认最多报告 200 行 `path:line:text`，每行最多 500 字节。跳过隐藏和被忽略的文件 |
| `powershell` / `bash` / `zsh` / `sh` / `cmd` | 唯一的进程启动工具，模型只看见其中一个。名字跟启动时解析到的解释器走，整个会话不变：PowerShell 是 `powershell`，bash 是 `bash`，zsh 是 `zsh`，POSIX sh 是 `sh`，只有 Windows 上两种 PowerShell 都没有时才是 `cmd`。`mode` `script` 把 `command` 交给这个解释器（管道、重定向、展开、脚本，以及用 Python 改文件）。`mode` `program` 用显式 `args` 直接启动一个可加载映像（PE / ELF / Mach-O），不经过 shell，shebang 脚本和批处理被拒 |

进程工具的两条模式共用启动路径：钉住程序身份、限制参数和环境变量、截断约 50 KiB 输出、默认 120 秒超时（`timeout_secs` 可改）。超时和取消时终止并回收整棵进程树，包括 `setsid` 脱离进程组的子孙，以及子 shell 退出后仍留在原进程组里的后台进程。shell 自己退出后工具就返回，不等后台子进程关掉继承的管道。丢掉 future 也会把清理交出去。非零退出码是 `is_error` 结果，不是循环故障。执行没有沙箱，环境变量白名单不是隔离。每次 `script` 都是新进程，`cd` 和变量赋值不会留到下一次调用。

Windows 上脚本模式按层侦查，命中一层就停：PowerShell 7（`pwsh`），然后 Windows PowerShell 5.1（`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`）。`pwsh` 的候选顺序是 PATH 里的 `pwsh.exe`、`Program Files\PowerShell\7`、预览版、WindowsApps 里的 `Microsoft.PowerShell_*` 包、WinGet 和 scoop。常规映像优先。`WindowsApps\pwsh.exe` 这种直接放在 `WindowsApps` 目录下的商店执行别名只在没有常规 `pwsh` 时使用，并且按路径形状识别，不看文件大小；包目录里的 `pwsh.exe` 仍算常规映像。不选择 Git Bash、MSYS2、Cygwin，也不选 WSL 的 `System32\bash.exe` / `SysWOW64\bash.exe`：会话 cwd 是 Windows 路径。SysWOW64 里的 32 位 `powershell.exe` 也不选。两层都没有时，运行时退到 `%SystemRoot%\System32\cmd.exe`（`/d /s /c`），工具名变成 `cmd`，这个退路不写入设置。Windows 自带的是 5.1，不是 5.0；它和 PowerShell 7 的 `&&` 不通用，所以只有没找到 `pwsh` 时才用它，提示词也改成 5.1 的语法。这和 Codex、Gemini CLI 的 Windows 顺序一致；pi 和 Claude Code 在 Windows 上以 bash 为先，这里不跟，因为要优先 PowerShell 7。

Linux / macOS 先看 `$SHELL`。它是绝对路径且文件名是 bash、zsh 或 sh 时就用它。否则 macOS 依次试 `/bin/zsh`、`/bin/bash`、PATH 里的 zsh 和 bash，再试 `sh`；其它 Unix 依次试 bash、zsh、`sh`。`$SHELL` 若是 pwsh、fish 或其他家族，就跳过。这些系统不选择 pwsh 或 cmd。工具名跟实际解释器走。

`MYCODE_SHELL` 和设置里的 `tools.shell` 都不再切换解释器。环境变量有具体取值时提示一次并忽略；设置里的旧字段仍能读入，启动时清掉，下次保存就不再写出。

PowerShell（7 和 5.1）不用 `-Command` 拼接用户字符串，也不把 bash 改写成 cmdlet。脚本先留下 PowerShell 要求放在最前的空行、注释、`using` 和 `param (...)` 块，再插入一段把管道编码设成 UTF-8 的前奏（主机禁止改编码时这段被跳过，用户脚本照常跑），然后整段按 UTF-16LE 做 Base64，用 `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand` 启动。空脚本会补一行注释，避免空的 `-EncodedCommand` 被拒绝。命令行按 UTF-16 码元计，上限 32767。bash、zsh 和 sh 用 `-c`。用户命令原样执行。

模型只注册这一个进程工具。描述、参数说明和系统提示词的 `<environment>` / `<shell>` 块按当前解释器来写：PowerShell 7 可以用 `&&`；Windows PowerShell 5.1 上 `&&` 和 `||` 是语法错误，要用 `;` 和 `$LASTEXITCODE`。提示词不再提 bash 到 PowerShell 的翻译。子代理用同一套解析：白名单里的 `shell` / `bash` / `powershell` / `cmd` 都变成这一个工具，提示词里的语法说明跟它一致。scout 不注册进程工具。

进程输出先认 BOM，再认没有 BOM 的 UTF-16LE（PowerShell 往管道写中文时经常这样，这些字节有时也是合法 UTF-8），然后才是严格 UTF-8。还不行时，Windows 依次试 OEM 代码页、ANSI 代码页和控制台输出代码页（中文 Windows 上 `cmd` 的 `dir` 往往是 GBK）。标准输出和标准错误都会再整理一次：有 `#< CLIXML` 时抽出 `Error` 和 `Warning` 节点，`_xHHHH_`（包括 `_x001B_`）还原成字符，ANSI / VT 序列去掉。没有这些标记的普通文本保持原样。

`program` 模式的实现按平台拆开（Windows x64 与 Windows ARM64 共用 `CreateProcessW`、macOS Apple Silicon、Linux x86_64 glibc）。发布包包含这四个平台。摘要复查和参数组装共用。其它 Unix 目标（musl、Android、BSD）不支持直接启动映像。模型只看见当前这一个进程工具（`powershell`、`bash` 或 `cmd`），没有 `exec`，也不会同时看见 `bash` 和 `powershell`。角色白名单里的 `shell` 仍表示这个进程工具。

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
| `ask_user` | 把 1–4 个问题送到界面，等用户答完再继续 |
| `agent` | 按角色再跑一个有白名单的子循环。只在能独立并行、边界清楚、并且能降低成本或提高完成质量时使用。工具名是 `agent`，没有 `task` 别名。内置角色只有 scout 和 artisan，也可以用 `agents/*.md` 里的自定义角色。子代理不能问用户。子代理没有墙钟超时。并发默认 4；设置为 `0` 表示这个默认值，不是零个 |
| `web_search` / `fetch_content` | 有界 HTTP：每次最多 8 条结果 / 8 个 URL，每页正文最多 8000 字符。正文被宿主截断或被这里截到 8000 字符时，都在 URL 后标 `(truncated)` |

网页后端由设置决定：启用的那个优先；都没启用时用第一个有钥匙（环境变量或 `web-<id>`）的后端。Querit 和自定义后端走 `POST {endpoint}/v1/search` 与 `/v1/contents`，AnySearch 走 `/v1/search` 与 `/v1/extract`。Querit 没钥匙时调用失败并提示去设置页粘贴或设 `QUERIT_API_KEY`；已启用的 AnySearch 没钥匙时匿名访问。

MCP 不把远端工具名注册进这张表。应用层注册 `search_tool` 和 `use_tool`：先取 schema，再按 schema 调用。这样远端工具不会盖住 `read` 或 `shell`。

## 改文件

`write` 和 `edit` 仍然可用。进程工具的 `mode` `script` 也可以改文件：bash 里用 Python（`python3` 或 `python`）的引号 heredoc 或短脚本，PowerShell 里用 here-string 管道给 `python`。`mode` `program` 不经过 shell，只启动可加载映像。这些路径都不做文件快照。撤回或编辑一条对话不会把工作区文件恢复或删掉。

## 不放在这里的东西

本 crate 不知道哪个会话正在打开，也不保存密钥。工作目录和派发前绑定的句柄由 `ToolCtx` 带进来；额外文件夹、网页钥匙和 MCP 连接由宿主在回合开始时放进来。
