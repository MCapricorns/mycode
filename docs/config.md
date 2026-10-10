# mycode-config

本机拥有的配置权威。设置、密钥、界面状态、压缩检查点、角色和提示资源都从这里读写。没有网络。

## 设计思路

家目录是可整体搬走的一棵树。`HomeLayout` 只做词法拼接：非空的 `MYCODE_HOME` 替换整棵根，否则是用户主目录（`HOME`，Windows 再退到 `USERPROFILE`）下的 `.mycode`。不 `canonicalize`，不跟随符号链接，不看进程的当前目录。相对路径里出现 `..`、绝对分量或空字节会拒绝。

| 文件（家目录下） | 内容 |
| --- | --- |
| `settings.json` | `AppSettings`：外观、提供商、网页、MCP、子代理、shell、User-Agent。上限 256 KiB |
| `secrets.json` | `providerKeys`：每个 id 一把钥匙。上限 64 KiB |
| `ui.json` | 工作区、最近项目、会话归属、模型钉选、信任的项目。上限 256 KiB |
| `sessions/<id>/compaction.json` | 压缩检查点 |
| `catalog-cache.json` | models.dev 目录缓存，归 `mycode-providers` 读写 |

文档是严格 JSON，带 `formatVersion` 和 `kind`。`settings.json` 和 `secrets.json` 还有 `revision`：写入用文件锁加上比较交换，编辑器带上读到的 revision，过期就失败，避免两个保存互相覆盖。`ui.json` 没有 revision，在锁下最后写入者生效。

读入时修掉旧写法留下的尾逗号并写回规范形式；缺字段补默认值。没有就地升级：`settings.json`、`secrets.json`、`ui.json` 如果解析失败、校验失败（未知字段、错误的 `formatVersion` / `kind`、非法取值等）、不是合法 UTF-8 或超过上限（8 MiB 以内），会把原字节复制到旁边的 `{文件名}.broken-<纳秒时间戳>`，再写入该文档的默认内容，启动继续，界面提示一次。密钥备份里仍是原来的字节，恢复过程不把它们写进日志。会话账本和 `sessions.db` 不走这套恢复。文件读不出来、权限不够，或备份写不进去时，原文件保持不动并返回错误。

密钥和设置分开。`settings.json` 可以进导出包；`secrets.json` 单独存，`Debug` 输出只列 id。读文件的缓冲区用 `zeroize`，避免密钥在临时缓冲里多停一轮。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `home` | 解析根、`sessions/`、`scratch/`，以及会话内相对路径 |
| `secure_fs` | 有界读取、持久锁、相对句柄的原子替换。Unix 与 Windows 各有一套不跟随链接的打开方式。本模块不含文档 schema |
| `authority` | 文档 revision，供比较交换 |
| `json_recover` | 尾逗号修复，这是唯一修的语法。修好后由调用方写回规范形式 |
| `document_repair` | 把无法解析或无法校验的产品文档复制到 `.broken-*`，再发布默认内容 |
| `settings` | `settings.json`。损坏时备份后回到默认设置（无提供商、两个未启用的内置网页后端） |
| `secrets` | `secrets.json`：每个 id 一把密钥，按 id 排序。空字符串表示清除。损坏时备份后变成空存储，钥匙只留在备份里 |
| `ui_state` | `ui.json`：命名工作区、文件夹、会话归属、最近项目、上次选的模型、模型选择器的最近使用和星标、每会话模型钉选、信任的项目、自动更新开关。只作界面恢复，不当作产品配置的真相 |
| `project_mcp` | 读 `<项目>/.mycode/mcp.json`，只在项目已被信任时才打开 |
| `compaction` | `compaction.json`：摘要覆盖到哪条消息、哪个分支头，摘要最多 24000 字符。账本本身不改写 |
| `subagents` | 内置 scout 与 artisan，再加上家目录 `agents/*.md` 和项目 `.mycode/agents/*.md`。角色是带少量 frontmatter 的 Markdown |
| `resources` | 发现 `AGENTS.md` / `MYCODE.md` 和 `.agents` 技能，裁剪后交给 system prompt |
| `mcp_import` | 把粘贴的 MCP JSON（`mcpServers`、`servers`、单个对象或数组）收成服务器行，并把 `Authorization` / `x-api-key` 抽成密钥 |

## 设置里有什么

`AppSettings` 是设置页的唯一文档。

- **外观**：`appearance.theme` 只接受 `dark`，其它值校验失败，整份文档按上面的规则备份并重置。`appearance.palette` 是 `slate`、`ocean`、`forest`、`dusk`、`ember`、`aurora`，默认 `slate`；旧色板 id 不迁移，同样触发重置。`appearance.language` 是 `auto` / `en` / `zh`。`appearance.fontSize` 是 `s` / `m` / `l` / `xl`，缺省为 `m`。`appearance.fontFamily` 缺省、空字符串或 `system` 表示操作系统界面字体；具名值为 `Inter`、`Segoe UI`、`PingFang`、`Noto Sans`
- **User-Agent**：`userAgent`，缺省是 pi agent 形状 `pi (<平台> <版本>; <架构>)`
- **提供商**：id、`kind`（只有 `anthropic-messages`、`openai-completions`、`openai-responses`）、`https://` base URL、模型 id 列表（第一个是默认）、`enabled`，可选 `contextLimit` / `maxOutput`（`maxOutput` 优先于目录 `limit.output`）。密钥不在此列
- **思考强度**：`reasoningEffort` 是保存的默认值，必须是 models.dev 档位（`off`、`on`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`）。会话里选的模型和强度不写这里，见下文 `ui.json`
- **Shell**：`tools.shell` 的 `kind` 只接受 `pwsh`（PowerShell 7）和 `bash`（Git bash），`program` 是可执行文件路径，`source` 是 `auto`（首次侦查）或 `user`。其它 `kind` 校验失败。自动侦查不选 Windows PowerShell 5.1。`cmd` 只在 `pwsh` 和 Git bash 都没有时当作运行时退路，不能写进这个字段
- **网页**：后端 `kind` 为 `querit`、`anysearch` 或 `custom`，端点必须是 https，最多启用一个。默认带 Querit 和 AnySearch 两行，均未启用。密钥先看环境变量 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`，再看 `secrets.json` 的 `web-<id>`
- **MCP**：`transport` 为 `stdio`（单个可执行文件加 `args`，不接受 shell 字符串）或 `http`（https 端点，`keyHeader` 为 `bearer` 或 `x-api-key`）。每服务器最多 32 个额外环境变量。密钥用 `mcp-<id>`
- **子代理**：哪些角色启用（没写的角色默认启用）、模型路由、思考强度、并发。`subagents.maxConcurrent` 为 `0` 时使用默认的 4 个并发子代理，不是关闭委派，也不是零个名额。显式上限最高 6
- **用量**：`usage.enabled`，默认开

数量上限写在常量上：提供商 64、每提供商模型 128、MCP 服务器 64、网页后端 16、角色 32。

## 界面状态里有什么

`ui.json` 的上限同样是常量：最近项目 16、工作区 16、每个工作区的文件夹 8、会话→项目 256、会话→工作区 512、最近模型 8、星标模型 24。

- **会话模型**：每个会话一条 `SessionModelPin`（服务商、模型、可选思考强度），最多 128 条，新的在前。切换会话时恢复各自的钉选，一个会话改模型不影响别的会话
- **信任的项目**：`trustedProjects` 是允许贡献 `.mycode/mcp.json` 的绝对路径，最多 64 个，满了不再加入。写入与比较用同一套规范化（去掉末尾斜杠；Windows 上折叠斜杠方向与 ASCII 大小写），侧栏、MCP 加载和信任列表共用，避免同一文件夹因斜杠或大小写被当成两个。打开文件夹不会自动信任。未信任时 `project_mcp_servers` 不打开该文件；已信任但文件不是合法 MCP JSON 或 stdio 命令是 shell 字符串时返回错误。项目里的服务器与设置里同 id 的服务器冲突时，项目那条覆盖
- 删除会话时一并清掉它的项目、工作区和模型钉选

## 角色和提示资源

内置角色编进二进制：

| 角色 | 职责 |
| --- | --- |
| scout | 只读。返回简明地图或发现后停止。隔离是 `shared`，思考强度低，工具只有 `read`、`grep`、`find`、`web_search`、`fetch_content`，项目覆盖也加不进改动工具 |
| artisan | 做到 brief 的结果，检查与改动相称，不提交、不推送。隔离是 `worktree`，思考强度高 |

同名文件的覆盖顺序是：内置 → `<家目录>/agents/` → 项目 `.mycode/agents/`。每份角色文件最多 32 KiB，目录最多 32 个角色。frontmatter 声明隔离方式（`shared` 或 `worktree`）、默认思考强度和工具白名单。启用与否、实际模型在设置里，不写进角色文件。

提示资源按固定位置发现：工作区的 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`，再是家目录的 `AGENTS.md` 和用户主目录的 `.agents/AGENTS.md`。每份最多 64 KiB，最多 16 份，合计进 prompt 最多 96K 字符。技能是 `.agents` 和 `.agents/skills` 下的 `*.md` 或 `<名字>/SKILL.md`，工作区和用户主目录各扫一遍，同名时工作区优先；发现不设上限，进 system prompt 时最多 32 个。

## 不放在这里的东西

刷新 models.dev 要发 HTTP，所以目录缓存的下载在 `mycode-providers`。真正跑回合、连接 MCP 的步骤在 `mycode-app`。
