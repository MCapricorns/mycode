# mycode-tools

`mycode-tools` 实现模型直接调用的工具：`read`、`write`、`edit`、`grep`、`find`、当前平台的一个进程工具，以及本 crate 定义、宿主提供实现的 `ask_user`、`agent`。MCP 工具由 app 装配。`content` 回到模型；`details` 只给界面（diff、字节数、退出码），不进上下文。

```text
tool_call
    → ToolRegistry 按名字取出 ToolDyn
    → 参数归一化、JSON Schema 校验
    → 文件或搜索根 preflight
    → 执行 → ToolResult(content, is_error, details)
```

## 调用与注册

- 一份 schema 两处用：`schemars` 从参数结构体生成 JSON Schema，既发给模型，也在 `execute_dyn` 里校验。`$schema`、`$id`、`$ref` 在发出和校验前都会被处理掉。
- 注册表按名字 last-wins；`specs()` 按名字排序。注册的工具全部出现在模型工具列表里，`prompt_entries()` 另外给 system prompt 提供名字和一句说明。
- 参数先归一化：camelCase 转 snake_case；`timeout`→`timeout_secs`、`cmd`→`command`、`file_path`/`filepath`→`path`、`old_str`/`new_str`；整个参数对象被编码成字符串时先解析回来。
- 随后按 schema 修补形状，再校验：`required` 里的字段如果模型用了别的工具的拼法（`file`/`filename`→`path`、`text`/`body`→`content`、`query`/`glob`→`pattern`、`task`/`instructions`→`prompt`、`name`/`role`→`agent` 等，且该拼法不是 schema 声明的属性）会改成 schema 名字；字符串形式的数字和布尔在任意深度按 schema 转成标量；带 `const`/单值 `enum` 标签的 `oneOf`/`anyOf` 对象缺少标签时按字段推断变体（恰好一个变体覆盖对象，或最贴合的那个；同样贴合时取声明顺序靠前的），标签拼写只差大小写/连字符时改正；推断出变体后，若该变体 `additionalProperties: false`，丢掉它没有声明的多余键（`type_note` 这类说明字段）。空对象、认不出的字段、没有标签的联合、解析失败的值都原样留给校验报错，不会被猜成某个变体。
- MCP 工具用服务器给的 schema 做同一套修补（不做 camelCase 转 snake_case，服务器可能就声明成驼峰），校验错误也走同一套渲染：联合失败只列允许的 `type` 取值，其它消息限制在 320 字节。
- 未知工具、schema 不合法、执行错误都变成 `is_error` 的工具结果，回合继续。没有“先问用户是否允许”的回调。
- `prepare_tool_ctx` 在执行前绑定文件或搜索句柄并改锚路径。只有不改变东西的工具可以并行：`read`、`grep`、`find`、`search_tool`、声明 `readOnlyHint` 的 MCP 工具，以及目标是这类工具的 `use_tool`；写、进程、`ask_user` 按模型顺序单独执行。同一条回复里的 `agent` 调用会一起启动。
- `ToolStream` 保证任意条进度后恰好一个终止事件；终止发送成功后，后续进度被静默丢弃。

## 内置工具

| 工具 | 行为 |
| --- | --- |
| `read` | 读 UTF-8 文本，返回行窗口和 revision |
| `write` | 整文件创建或替换，带 revision / overwrite 保护 |
| `edit` | 同一快照上的批量替换，一次发布 |
| `find` | 按 glob 找文件和目录 |
| `grep` | 按行搜索文件内容 |
| `powershell` / `bash` / `zsh` / `sh` / `cmd` | 唯一的进程工具，名字跟解析到的解释器 |

### read

只接受 UTF-8；UTF-16、UTF-32 和非法 UTF-8 直接报错。BOM 只从显示内容去掉，revision 按原始字节计算。隐藏文件可读。

`offset` 是 1-based 行号，schema 和运行时都要求 ≥1，`0` 不会当成第一行；省略时从第一行开始。`limit` 是行数。

每次最多返回 2000 行或 50 KiB，哪个先到就截断，并附说明让模型用 `offset`/`limit` 继续；单行超过 50 KiB 时只显示该行开头。文件超过 32 MiB 直接拒绝。

返回的是纯文本行，不加行号，末尾追加 `[revision mycode-rev1-…]`。revision 覆盖文件身份、大小、mtime 和原始字节 hash。

### write

创建或整文件替换，内容最多 8 MiB。新文件不需要 revision，原子创建，缺的父目录一并创建；创建前目标若已被别人创建则拒绝。隐藏文件可写。

替换已有文件必须给 `expected_revision`（上次 `read`/`edit`/`write` 返回的 `[revision …]`）或 `overwrite: true`，两者不能同时给；目标不存在时不能给 `expected_revision`。

写入走临时文件 + rename，保留原文件的 mode/DACL 和属性，发布后重新读取并校验内容。已存在的硬链接会被替换成新 inode，结果里标 `detached_hardlink=true`。路径经过符号链接或重解析点时写到它指向的真实文件。文件内容不会写进 `details`。

### edit

两种用法：`{path, old_string, new_string}` 做唯一替换（`replace_all: true` 替换全部）；或 `{path, operations: [...]}` 做一批操作。`operations` 出现时，`old_string`/`new_string`/`replace_all` 全部忽略。一次最多 32 个操作。

- `literal`：`pattern`+`replacement`，或最多 32 组 `patterns`+`replacements`；`occurrence` 为 `unique`（默认）、`all` 或 `nth`。先精确匹配；失败时按文件主导换行重试；最后忽略行尾空格/制表符再试。缩进不忽略。替换文本的换行会改成文件的主导换行。
- `fuzzy`：按 token 归一化后算 Levenshtein，`max_distance` 必须是 1–3；只接受唯一最佳，且领先第二名的距离至少为 `1 + 归一化字符数/8`。归一化 needle 最多 256 字符，haystack 最多 262,144 个 token、8,192 个候选窗口；分词和扫描每约 4 KiB 检查一次取消。
- `regex`：Rust `regex`，`occurrence` 只支持 `unique`/`all`，不支持 `nth`；不支持 lookaround 和反向引用；嵌套上限 32，正则和 DFA 大小上限各 1 MiB；零宽匹配拒绝；捕获展开每次最多 1 MiB。
- `line_range`：1-based 闭区间。必须给 `expected_text` 和/或 `expected_hash`（Blake3 hex，包含最后一行换行）；`expected_text` 可省略最后一个行尾。空 `replacement` 删除这些行，缺行尾的替换会继承被替换行的行尾。

`operations` 里的对象在通用修补之上还有一组 edit 专用叫法：`old_string`/`old_str`/`old`/`find`/`search`/`needle` 当作 `pattern`，`new_string`/`new_str`/`new`/`replace` 当作 `replacement`，`start`/`end` 当作 `start_line`/`end_line`，`distance` 当作 `max_distance`，`replace_all: true` 等于 `occurrence: all`，`first`/`each`/`once` 这类 `occurrence` 写法也归一。没有标签时按上面通用规则推断（`start_line`/`end_line` → `line_range`，`max_distance` → `fuzzy`，其余 → `literal`）；补全后仍按变体校验，缺 `pattern`、`pattern` 与 `patterns` 同时出现等错误照旧拒绝。

所有操作都基于同一个快照，按字节范围排序；重叠、同一位置、非法 UTF-8 边界都会拒绝。任何一个操作失败，整次调用不修改文件。总匹配数最多 32,768，结果最多 8 MiB。写入用快照 revision 做 compare-and-swap；给了 `expected_revision` 就必须与快照一致。返回 replacement 数量、字节数和 revision；diff 摘要最多 4096 字节，只放在 `details`。BOM 会保留。超过 8 MiB 的文件不能编辑。

## 搜索

`find` 按 glob 找路径，搜索根默认是 cwd，`path` 可以是目录或单个文件。`*` 跨 `/`；不含 `/` 的 pattern 还会匹配任意深度的文件名。`max_depth` 从搜索根算，`1` 是直接子项，`0` 被拒绝；到达深度上限的目录仍会报告，但不再下钻。目录以 `/` 结尾。默认最多 1000 条，硬上限 10,000 条，结果按路径排序。隐藏和 `.gitignore`/`.ignore` 跳过；显式 `path` 命中隐藏或忽略路径时返回说明，而不是空结果。

`grep` 默认按字面量搜索，`is_regex` 打开正则。全小写 pattern 忽略大小写，含大写则精确匹配。pattern 含 `|`、`.*`、`\s+`、`\(` 等正则特征但没开 `is_regex` 时，如果正则能编译且不匹配空串，会同时按字面量和正则匹配并说明；否则只按字面量。`include`/`exclude` 是 glob，不含 `/` 时匹配任意深度的文件名，逗号列表按 `{a,b}` 处理。按行返回 `path:line:text`，默认 200 行，`max_results` 可调，硬上限 10,000 行；每行最多 500 字节。跳过二进制（含 NUL）以及隐藏、忽略文件。

搜索预算：60 秒、单个 grep 最多读 512 MiB、匹配回调最多 100,000 次、结果最多存 10,000 条、结果堆最多 4 MiB、输出最多 100 KiB、pattern/glob 最多 16 KiB、I/O 错误样本 5 条。遍历另有 100,000 个条目、256 层深度、16,384 的目录宽度、512 个活句柄和 ignore 文件预算。时间或取消返回错误，不给半截结果；条目/宽度/结果等预算到顶会停止并在结果里写 `stopped_early` 和停因；普通逐路径 I/O 错误返回带 lower-bound 说明的结果。

## 搜索根与路径

- 相对路径相对会话 cwd。绝对路径落在额外工作区根下时，改锚到最深的那个根，path key 相对那个根。
- 路径不限于工作区。cwd 和所有根之外的路径，或在 cwd 里逐分量走到符号链接、重解析点、挂载点的路径，先由文件系统解析最深的已存在祖先（链接、8.3 短名、大小写都在这一步落定），再从解析出的真实位置绑定；这类路径的 path key 是解析后的绝对路径。经过链接的写入落到链接指向的文件。
- `read` 还会改锚到宿主发布的技能根（`~/.agents`、`~/.agents/skills`），key 相对技能根；其他工具访问同一处文件走上面的真实位置解析。
- cwd 和搜索根只解析一次，之后用保留的目录句柄相对打开，逐分量不跟随符号链接或重解析点、不跨挂载点；走不通时退到上面的真实位置解析。搜索遍历目录树时同样不跟随链接、不跨挂载点。Unix 比较 `st_dev`，Windows 每个分量用 `NtOpenFile`。
- 文件工具和搜索都允许硬链接；`write`/`edit` 发布新 inode。
- 向上查找 ignore 文件时碰到挂载边界就停，这不算搜索失败。

## 进程工具

模型只看到一个进程工具，名字由启动时解析到的解释器决定，整个进程不变：PowerShell 7 和 5.1 都叫 `powershell`，其余是 `bash`、`zsh`、`sh`；只有 Windows 上两种 PowerShell 都找不到时才叫 `cmd`。

`mode` 默认 `script`：`command` 交给解释器执行。`mode` `program`：用 `program` + `args` 直接启动，不解析 shell 字符串。`script` 拒绝 `program`/`args`，`program` 拒绝 `command`。`timeout_secs` 默认 120，小于 1 按 1 算。

脚本模式：PowerShell 不拼 `-Command`，脚本按 UTF-16LE Base64 走 `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand`；UTF-8 管道编码前奏插在 PowerShell 要求的开头语句（注释、`using`、`param`）之后，空脚本补一行注释。命令行按 UTF-16 码元计，上限 32,767。bash/zsh/sh 用 `-c`，cmd 用 `/d /s /c`。命令原样执行，不做 bash→PowerShell 翻译；PowerShell 会在启动前拒绝命令位置的 Unix 专属命令（`touch`、`export`、`source`、`which`、`chmod`、`chown`、`sudo`、Unix 包管理器、`man`）以及 `>/dev/null`、`<<` heredoc，并返回 PowerShell 写法提示，字符串、here-string 和注释内的文本不受影响。

程序模式：PE、ELF、Mach-O 直接启动。直接 shebang 包装只跟一层：解释器加最多一个参数，脚本路径和 `args` 追加在后面；`#!/usr/bin/env name` 要求 `name` 是单个程序名。批处理、`-c`/`--command`/`env -S` 拒绝。裸 `program` 只在绝对 PATH 项里查找；路径型 `program` 相对 cwd 解析，结果必须是绝对路径。`program` 最多 32,767 字节；`args` 最多 4096 个、单个 64 KiB、合计 1 MiB；映像最多 256 MiB。程序直启只实现了 Windows x64/ARM64 和 macOS Apple Silicon。

解释器选择：Windows 先找常规 `pwsh`（PATH、`Program Files\PowerShell\7`、预览版、WinGet、scoop 等），再找 Windows PowerShell 5.1（`System32`，PATH 里排除 SysWOW64）；`WindowsApps\pwsh.exe` 商店执行别名只在没有常规 `pwsh` 时使用。不选 Git Bash、MSYS2、Cygwin 或 WSL。两者都没有时退到 `%SystemRoot%\System32\cmd.exe`。Unix 上 `$SHELL` 是绝对路径的 bash/zsh/sh 就用它，否则依次试 `/bin/zsh`、`/bin/bash`、PATH 里的 zsh/bash、`/bin/sh`、PATH 里的 sh。`MYCODE_SHELL` 不再切换解释器；有非空且非 `auto` 的值时提示一次并忽略。

执行与清理：环境变量按白名单重建，PATH 只保留绝对项；这只是过滤，不是隔离。超时或取消会终止并回收进程树：macOS 先按父子关系杀子孙（包括 `setsid` 出去的，重复 3 轮），再 `killpg` 清同组孤儿；Windows 用不允许 breakaway 的 kill-on-close Job，进程创建后先挂 Job 再恢复主线程。shell 自己退出后最多再等 50 ms 让输出离开管道，然后返回，不等后台子进程关闭继承的写端。丢掉 future 会把清理交给监督线程。每次 `script` 都是新进程，`cd` 和变量赋值不会保留到下一次调用。

输出：stdout 和 stderr 各保留头尾，合并后最多 50 KiB，截断处标出省略字节。非零退出是 `is_error` 结果，不是回合故障；超时和取消也返回错误结果并附说明。解码顺序是 BOM（UTF-8/UTF-16）→ 无 BOM 的 UTF-16LE 启发式 → 严格 UTF-8（截断的合法前缀按 UTF-8）→ Windows OEM/ANSI/控制台输出代码页。随后抽取 CLIXML 的 `Error`/`Warning`、还原 `_xHHHH_`、去掉 ANSI/VT。提示词和 `<environment>`/`<shell>` 跟当前解释器一致：PowerShell 7 可用 `&&`，5.1 用 `;` 和 `$LASTEXITCODE`。

## 宿主工具

| 工具 | 行为 |
| --- | --- |
| `ask_user` | 一次 1–4 个问题，等用户回答；用户关闭问题是一次错误结果，回合继续；停止整个回合会中止调用。答案渲染最多 4096 字符；`multiple` 允许复选。 |
| `agent` | 按角色跑一个子循环。`prompt` 最多 16,000 字符，答案最多 24,000 字符。内置角色只有 `scout` 和 `artisan`，也可用家目录 `agents/*.md` 或工作区 `.mycode/agents/*.md` 自定义；工具描述不写角色名，可用角色以 system prompt 的 `<dispatch>` 为准（只列设置里启用的）。子代理不能问用户，也不能再调 `agent`；没有墙钟超时，只能由答案、取消、停止或删除对话结束。 |

`scout` 只读：工具限制为 `read`、`grep`、`find`，MCP 也只给 `readOnlyHint: true` 的工具（联网检索由 magpie Library 下发的 MCP 服务提供）。`artisan` 默认在独立 git worktree 中改文件，结束时把最多 256 KiB 的 diff 附在答案后并删除 worktree；改动不会自动合回主工作区，也不提交。`isolation` 可选 `shared`/`worktree`，只读角色强制 `shared`。每个父回合最多同时跑 8 个子代理，多出的排队；同一条回复里的多个 `agent` 都会启动。

## MCP 与技能

MCP 在回合开始时连接启用的服务器，连接池随后复用。单个服务器的工具不超过 4 个、且每个输入 schema 编码后不超过 2048 字节时，直接注册成普通工具；否则只能通过 `search_tool` 和 `use_tool` 调用：`search_tool` 传精确名字返回 schema，传关键词返回匹配的工具（每个一行摘要，最多 25 个；只匹配一个时直接给 schema），传 `"list"` 列出全部；`names` 一次最多取 10 个工具的 schema。`use_tool` 的目标声明了 `readOnlyHint` 时，这次调用和其它只读调用一起并行。两个服务器有同名工具时，后一个的名字变成 `server__tool`，两个都能调用；小服务器的工具和已注册的工具（内置工具等）重名时不直接注册，改由 `search_tool` 提供。调用使用原始工具名；直接注册不会覆盖同名内置工具；schema 可编译时参数先在本地校验。结果超过 20 KiB 时只内联头尾，全文存到本地并给出路径。

技能目录放在 system prompt 的 `<skills>` 里，正文不注入；模型用 `read` 按目录中的路径读取对应的 `SKILL.md`，用户消息以 `/slug` 开头时也读取对应技能。目录来自会话 cwd、用户主目录的 `.agents` 和用户添加的其它工作区文件夹（不含临时目录）；子代理（含 scout）用同一个函数生成目录。

## 残留风险

- 进程工具以当前用户权限运行。环境变量白名单只过滤环境，不是隔离；没有 OS 级沙箱。
- `write`、`edit` 和进程启动只用进程内全局锁串行化，revision 检查也是进程内 compare-and-swap。同一账号的外部进程仍可能并发改文件；没有 advisory lock。发布后会重新读取并核对 inode 和内容，但复核完成之后的替换无法发现。
- 搜索拒绝符号链接、重解析点、跨挂载点和 Unix 多链接文件；文件工具把路径限制在保留的根句柄内，但 `write`/`edit` 只保证自己发布的新 inode，不阻止其它进程写同一路径。
- `program` 模式的 shebang 只跟一层；解释器必须是可加载映像，`#!/usr/bin/env name` 只接受一个程序名。
- 网页（MCP 检索结果）、MCP 结果和文件内容都按不可信数据处理；工具描述和子代理提示里明确要求不把其中的文字当指令。
- 撤回或编辑一条对话不会恢复或删除工作区文件。
