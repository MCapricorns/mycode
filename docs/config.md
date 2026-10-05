# mycode-config

本机拥有的配置权威。设置、密钥、界面状态、压缩检查点、角色和提示资源都从这里读写。没有网络。

## 设计思路

家目录是可整体搬走的一棵树。`HomeLayout` 只做词法拼接：`MYCODE_HOME` 替换整棵根，否则是用户主目录下的 `.mycode`。不 `canonicalize`，不跟随符号链接，不看进程的当前目录。相对路径里出现 `..`、绝对分量或空字节会拒绝。

文档是严格 JSON。每份权威文件有 `formatVersion`、`kind`、字节上限，以及 revision。写入用文件锁加上比较交换：编辑器带上读到的 revision，过期就失败，避免两个保存互相覆盖。读入时修掉旧写法留下的尾逗号，缺字段补默认值，然后把规范化结果写回去。未知字段和错误的 `kind` 直接失败。

密钥和设置分开。`settings.json` 可以进导出包；`secrets.json` 单独存，`Debug` 输出脱敏。读文件的缓冲区用 `zeroize`，避免密钥在临时缓冲里多停一轮。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `home` | 解析根、`sessions/`、`scratch/`，以及会话内相对路径 |
| `secure_fs` | 有界读取、持久锁、相对句柄的原子替换。Unix 与 Windows 各有一套不跟随链接的打开方式。本模块不含文档 schema |
| `authority` | 文档 revision，供比较交换 |
| `json_recover` | 尾逗号修复 |
| `settings` | `settings.json`：外观与语言、提供商、shell、网页后端、MCP、子代理路由、User-Agent |
| `secrets` | `secrets.json`：每个提供商一把密钥，按 id 排序。空字符串表示清除 |
| `ui_state` | `ui.json`：命名工作区、文件夹、会话归属、最近项目、上次选的模型，以及模型选择器的最近使用和星标。坏文件重置为默认，不当作产品配置的真相 |
| `compaction` | `compaction.json`：摘要覆盖到哪条消息、哪个分支头。账本本身不改写 |
| `subagents` | 内置 scout 与 artisan，再加上家目录和项目 `.mycode/agents/*.md`。角色是带少量 frontmatter 的 Markdown |
| `resources` | 发现 `AGENTS.md` / `MYCODE.md` 和 `.agents` 技能，裁剪后交给 system prompt |
| `mcp_import` | 把粘贴的 MCP JSON 收成服务器行，并把 Authorization 抽成密钥 |

## 设置里有什么

`AppSettings` 是设置页的唯一文档。

- **外观**：`appearance.theme` 只接受 `dark`（读到 `light` 会改成 `dark` 并写回）。`appearance.palette` 是深色色板：`slate`、`ocean`、`forest`、`dusk`、`sand`、`rose`、`ink`、`moss`、`ember`、`glacier`、`plum`、`copper`、`aurora`，默认 `slate`。`appearance.language` 是 `auto` / `en` / `zh`。`appearance.fontSize` 是 `s` / `m` / `l` / `xl`，缺省为 `m`。`appearance.fontFamily` 缺省、空字符串或 `system` 表示操作系统界面字体；具名值为 `Inter`、`Segoe UI`、`PingFang`、`Noto Sans`
- **提供商**：id、`kind`（只有 `anthropic-messages`、`openai-completions`、`openai-responses`）、base URL、模型 id 列表、上下文窗口。密钥不在此列
- **Shell**：Windows 只接受 `pwsh` 和 `bash`。存过的 `powershell` / `cmd` 在加载时丢掉，重新侦查
- **网页**：Querit、AnySearch 或自定义后端。密钥放在 `secrets.json` 的 `web-<id>`，或环境变量 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`
- **MCP**：stdio 命令或 https 端点，每服务器最多 32 个额外环境变量。密钥用 `mcp-<id>`
- **子代理**：哪些角色启用、模型路由、思考强度、并发。`subagents.maxConcurrent` 为 `0` 时使用默认的 4 个并发子代理，不是关闭委派，也不是零个名额。显式上限最高 6

数量上限写在常量上：提供商、每提供商模型数、MCP 服务器、网页后端、角色、工作区、每个工作区的文件夹。

## 角色和提示资源

内置角色编进二进制：

| 角色 | 职责 |
| --- | --- |
| scout | 只读。返回简明地图或发现后停止。隔离是 `shared`，工具只有读和网页 |
| artisan | 做到 brief 的结果，检查与改动相称，不把 diff 倒回父级。隔离是 `worktree`，思考强度高 |

同名文件的覆盖顺序是：内置 → `~/.mycode/agents/` → 项目 `.mycode/agents/`。frontmatter 声明隔离方式（`shared` 或 `worktree`）、默认思考强度和工具白名单。启用与否、实际模型在设置里，不写进角色文件。

提示资源按固定位置发现，每份最多 64 KiB，合计进 prompt 的字符有总上限。技能是 `.agents` 下的斜杠命令，工作区和用户主目录各扫一遍，最多 24 个。

## 不放在这里的东西

刷新 models.dev 要发 HTTP，所以目录缓存的下载在 `mycode-providers`。真正跑回合、连接 MCP 的步骤在 `mycode-app`。
