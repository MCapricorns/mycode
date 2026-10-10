# 架构

mycode 是跑在本机的桌面编码代理（Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。对话、工具调用和会话都在你的机器上；模型请求发到你自己的 API。界面不读密钥、不写文件、不发网络请求。发布包是这四个平台。

## 七个 crate

依赖只向下。上面的 crate 可以调用下面的，下面的不知道上面的存在。`mycode-core` 和 `mycode-config` 都是叶子，互不依赖。

```text
mycode-desktop     GPUI 窗口。渲染，并把动作交给桥           → app, config, providers, tools
mycode-app         应用核。会话、回合、工具宿主、MCP、网页、更新 → agent, providers, tools, config, core
mycode-agent       回合循环 + 会话账本                       → tools, config, core
mycode-providers   三协议适配、模型目录、OAuth 协议           → config, core
mycode-tools       工具 trait、注册表、内置文件/搜索/进程工具   → core
mycode-config      家目录与严格 JSON 文档                    （叶子）
mycode-core        消息、事件、工具规格、Provider 端口         （叶子）
```

| Crate | 干什么 | 设计思路 |
| --- | --- | --- |
| `mycode-core` | 共享词汇 | 叶子 crate。类型能序列化，能原样穿过事件和请求。没有协议、没有磁盘、没有策略 |
| `mycode-config` | 拥有的文件 | 路径只在词法上拼接，不跟随符号链接。文档有版本、种类、大小上限和 revision CAS。`settings.json`、`secrets.json`、`ui.json` 只多了尾逗号时就地改写成规范形式，改写失败照常报错；真正损坏的才备份后回到默认；会话账本不重置 |
| `mycode-providers` | 模型出口 | 协议种类、地址和模型是 `settings.json` 里的数据；各家请求体的差异（思考开关、缓存键）在按家族匹配的表里，未知主机走通用兜底行。密钥由调用方传入，不进设置 |
| `mycode-tools` | 工具实现 | 一份 JSON Schema 同时给模型和运行时。搜索和读文件在进程内完成 |
| `mycode-agent` | 循环和账本 | 循环可以没有磁盘。账本是另一层，用期望头把一次写入做成可恢复的提交 |
| `mycode-app` | 把上面拼成产品 | 前端只看见命令、回复和事件。回合在这里装配 provider、工具和压缩 |
| `mycode-desktop` | 窗口 | 视图模型是纯函数。GPUI 只画状态、收集点击 |

工作区成员只有这七个。配置、密钥、界面状态和角色在 `mycode-config`，会话账本在 `mycode-agent`；网页和 MCP 客户端是 `mycode-app` 里的模块，没有独立的 crate。

## 一条消息怎么走完

```text
输入框
  → DesktopAction（纯 reducer，先改界面状态）
  → BridgeCommand::SendMessage（先把用户消息写入账本）
  → BridgeCommand::ChatTurn（先取消同一会话还在跑的回合）
       读 settings + secrets，解析 endpoint 和认证（OAuth 令牌按需刷新）
       跟上账本最新头，回放整段分支
       注册本回合工具（内置、ask_user、网页、agent、MCP），拼 system prompt
       超过阈值先压缩一次
       Agent::prompt
         请求前压缩钩子（失败则仍发全文）
         provider 流式事件 → BridgeEvent → 唤醒界面
         工具调用和结果、中间助手消息随到随提交，直到模型停止
       最后的助手消息、用量行提交进账本，然后 ChatDone
```

取消只作用于这一回合的子 token。用户打断时，已经到达的思考、正文和已完成的工具调用写入账本，并标成被用户打断；下一条请求仍带着这段历史和同一条 prompt cache key。回合进行中另发的文字走 `Steer`，注入当前回合，不取消正在跑的子代理。排队发送和「打断并发送」仍是先取消再开新回合。工具失败变成 `is_error` 结果，循环继续，模型能看见失败原因。

## 快、稳、省

这三条是实现时的取舍，不是三套互相独立的子系统。

**快。** 热路径少绕路。`grep` / `find` 在进程内搜索，不启动外部 `rg` 或 `fd`。HTTP 用操作系统 TLS。界面在事件到达时被唤醒，核心线程和界面互不阻塞。改动面板跟着文件夹的文件系统事件读 `git status`，用只读方式，不按固定间隔轮询。发布构建用 fat LTO 和单个 codegen unit，换的是链接时间。

**稳。** 能弄丢用户数据的写都走同一套约束：拥有的路径、不跟随链接、带锁的原子替换、revision 比较交换。会话以 `sessions.db` 为提交权威，事件在 JSONL 里；日志尾部超出已提交长度的部分在打开或下一次追加时丢掉；日志比已提交长度短，或读到的行和索引里的 digest、结构对不上，就报损坏（fail closed），不猜。删除会话先取消回合和子代理，再赶出内存账本，然后删索引行和目录。单个坏会话出现在列表里供删除，不让整个侧栏失败。压缩失败或写检查点失败时继续发送完整历史。`write`、`edit` 和 `shell` 改过的文件没有撤销。对话里的编辑和撤回只截断账本，不恢复、也不删除工作区文件。

**省。** 上限写在类型旁边，而不是靠调用方记得。设置、密钥、单条事件、工具输出、压缩摘要、目录缓存都有字节或条数上限。压缩只缩短发往模型的历史，不重写账本里已有的事件；摘要被截断或太短时最多重试一次，再失败就发全文。同一会话的 system prompt 在文件不变时保持字节稳定，请求带会话 id 作 `prompt_cache_key`，提供商能命中前缀缓存。模型目录把一份快照编进二进制，家目录里再缓存一份，刷新用条件请求。开发档只保留行号表，避免多 GiB 的调试信息。

## 家目录

`MYCODE_HOME` 有值时，它就是根。否则用 `~/.mycode`（Windows 在没有 `HOME` 时用 `USERPROFILE`）。解析不访问磁盘，也不按当前工作目录拼接。

```text
~/.mycode/
├─ settings.json          提供商、MCP、网页、用量、外观、子代理、shell、User-Agent。不含密钥
├─ secrets.json           API 密钥、OAuth 令牌与网页 / MCP 凭证
├─ ui.json                工作区、文件夹、会话归属、最近项目、信任的项目、每个会话的模型与思考档位。可丢弃
├─ catalog-cache.json     models.dev 目录缓存
├─ sessions.db            会话索引（标题、分支头、JSONL 偏移）
├─ sessions/<id>/         `<branch>.jsonl`、`payloads/`、`compaction.json`
├─ agents/                用户自定义的子代理角色
├─ agent-worktrees/       子代理 worktree 租约，启动时回收
├─ mcp-results/           被截断的 MCP 结果全文
└─ scratch/               没有绑定文件夹时的工具工作目录
```

更早版本可能还有 `checkpoints/<id>/`。删除会话时会清掉对应目录。新的回合不再写它。

界面标题用会话第一条用户消息（前 60 个字符）。空标题时用文件夹名。`ses1-…` 只是内部身份。

## 当前边界

这些是现在代码的真实边界，读模块文档时一起记住。

- 工具在当前用户权限下执行，没有沙箱，也没有每次调用前的许可弹窗。校验过的调用会直接跑。
- 进程工具会钉住要启动的程序映像并回收进程树。模型只看见一个工具：PowerShell 时叫 `powershell`，bash 时叫 `bash`，最后才是 `cmd`。`script` 模式走这个解释器，`program` 模式直接启动可加载映像。Windows 上脚本模式优先 PowerShell 7（`pwsh`），其次 Windows PowerShell 5.1，然后 Git Bash；都不在时运行时才退到 `cmd.exe`，且不写入设置。不自动选择 WSL bash。PowerShell 用 UTF-16LE 的 `-EncodedCommand` 启动，用户命令不从 bash 翻译，标准输出和标准错误里的 CLIXML 与 ANSI 颜色会收成可读文本。环境变量过滤不是隔离。
- 持续集成在 Windows x64、Windows ARM64、macOS Apple Silicon 和 Linux x86_64 上构建发布包。pull request（`ci.yml`）和 `main`（`release.yml`）都跑这四个构建；只有 `main` 在构建成功后打标签发布。这两条流水线不跑 `cargo test`、fmt 或 clippy；只有每周的 `models-snapshot.yml` 在目录快照变化时跑 `cargo test -p mycode-providers`。
- 会话写入是单写者。generation fence 把正在提交和正在删除排开，提交用期望头 CAS。
- 生产环境的回合钩子只有请求前压缩，子代理连这个也没有。没有工具前观察者，也不再为改文件装快照。
- 项目里的 `.mycode/mcp.json` 只有在项目菜单里信任该文件夹后才会加载。
- 进程工具的 `script` 模式可以改文件：bash 用 Python 的引号 heredoc 或短脚本，PowerShell 用 here-string 管道给 `python`。`program` 模式不经过 shell。`write` 和 `edit` 仍然可用。Windows 上的侦查、编码和 stderr 清洗见 [tools.md](tools.md)。
