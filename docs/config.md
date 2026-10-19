# 配置与状态

`mycode-config` 读写本机配置和界面状态，不联网。会话账本、目录下载和运行回合不在这里。

## 家目录

`HomeLayout` 只做词法解析。非空的 `MYCODE_HOME` 直接作为根，不再拼 `.mycode`；否则用非空 `HOME`，Windows 上 `HOME` 为空时再退到 `USERPROFILE`，并在其下拼 `.mycode`。空值视为未设置。

根必须是绝对路径、不含 `..`，不能是文件系统根、盘符根或共享根。解析不访问文件系统，不 `canonicalize`，不跟随链接，也不看进程当前目录。`owned_join` 接受 `/` 或 `\` 分隔的相对路径；每个分量拒绝空、`.`、`..`、NUL、`:`、`*`、`?`、引号、尖括号、竖线、控制字符、尾随点和空格，以及 Windows 设备名。读不会创建目录；写会在锁事务里创建缺失的目录（Unix `0700`，Windows 受保护 DACL）。

| 文件（家目录下） | 内容 | 上限 |
| --- | --- | --- |
| `settings.json` | `AppSettings`：外观、代理、提供商、网页、MCP、子代理、技能 | 256 KiB |
| `secrets.json` | `providerKeys`：provider id → API key，按 id 排序 | 64 KiB |
| `ui.json` | 最近项目、工作区、会话绑定、模型钉选、窗口和面板状态 | 256 KiB |
| `sessions/<项目键>/<id>/compaction.json` | 压缩检查点 | 256 KiB |
| `catalog-cache.json` | models.dev 目录快照，`mycode-providers` 读写 | 32 MiB |
| `agents/*.md` | 用户角色定义 | 每份 32 KiB |

项目自己的文件在项目里，不在家目录：`.mycode/mcp.json`、`.mycode/agents/*.md`、`AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md` 和 `.agents` 下的技能。

## 文档、锁与恢复

`settings.json`、`secrets.json`、`ui.json`、`compaction.json` 都是严格 JSON，带 `formatVersion: 1`，`kind` 分别为 `mycode-app-settings`、`mycode-provider-secrets`、`mycode-ui-state`、`compaction`。未知字段、错误类型、非法取值、错误的 version/kind、非 UTF-8 和超限都算损坏；没有语法修复，也没有就地升级。已移除的字段（`appearance.palette`、`appearance.fontFamily`、`tools`、`usage`、`userAgent`、`subagents.maxConcurrent`、`subagents.timeoutSecs` 等）按未知字段处理。

`settings.json` 和 `secrets.json` 带 `revision`。整份替换（`replace_app_settings`、`replace_provider_secrets`）在锁内比较调用方读到的 revision，对不上返回 `RevisionConflict`；文件不存在时 revision 为 0，每次替换加一。`secrets` 的单键更新 `update_provider_key` 在锁内直接读改写，不走 CAS，所以并发保存的 key 不会互相覆盖。`ui.json` 没有 revision：整份替换最后写入者生效，`update_ui_state` 同样在锁内读改写。`compaction.json` 没有 revision，写入是锁内原子替换。

锁在操作系统里，不在磁盘上。Unix 对父目录句柄 `flock`；Windows 用按目录路径（大小写折叠后哈希）命名的 `Local\` 互斥体。家目录里不会出现 `*.lock` 文件；旧版本留下的 `{文件名}.lock` 会在下次写入时删掉。

启动读取时：

- 文件不存在：返回默认值，不写盘。
- 内容损坏（解析/校验失败、非 UTF-8、超过文档上限但在 8 MiB 内）：原字节复制到 `{文件名}.broken-<纳秒>`，逐字段/逐行抢救，再把通过校验的结果规范写回。抢救只在整体仍能通过校验时采用某个字段或行，通不过校验的字段或行被丢弃。
- 文件本身不可用（所有者或权限不对、链接、目录、非瞬时 I/O 错误）：不读内容，直接把文件改名为 `{文件名}.broken-<纳秒>`，再写入默认值。Unix 上文件模式不是 `0600` 就算不可用，目录权限会在读取时收紧到 `0700`；Windows 上文件必须属于当前用户且 DACL 受保护，目录会被安装受保护 DACL。
- 超过 8 MiB 的文档不备份也不重置，直接报 `Oversized`。
- 家目录本身不可信（所有者不对、是链接）时不被“修好”，返回原错误。`Interrupted`、`WouldBlock`、`TimedOut`、`ResourceBusy` 这类瞬时错误也不触发重置。

备份保留原字节，恢复过程不把内容写进日志。`compaction.json` 和项目 `mcp.json` 不走这套恢复：前者读失败按没有检查点处理，后者作为警告，其他配置照常使用。会话账本和 `sessions.db` 也不在这里修复。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `home` | 解析根、`sessions/`、`scratch/` 和会话内相对路径 |
| `secure_fs` | 有界读取、不落盘的锁、相对句柄的原子替换和改名隔离；不含文档 schema |
| `authority` | 文档 revision |
| `document_repair` | 把损坏或不可用的文档挪到 `.broken-*`，抢救后发布 |
| `settings` | `settings.json` 及下面各设置族 |
| `secrets` | `secrets.json`，每个 id 一把 key |
| `ui_state` | `ui.json`，只用于界面恢复 |
| `project_mcp` | 读 `<项目>/.mycode/mcp.json` |
| `compaction` | 每个会话的压缩检查点 |
| `subagents` | 内置角色、家目录和项目角色发现 |
| `resources` | `AGENTS.md` / `MYCODE.md` 与 `.agents` 技能发现 |
| `mcp_import` | 解析粘贴的 MCP JSON，抽取密钥 |

## 设置

`AppSettings` 是设置页的唯一文档。默认值：无提供商、三个内置网页后端均未启用、外观 `dark` / `auto` / `m`、无子代理和技能记录。

- **外观**：`appearance.theme` 为 `dark` 或 `light`；`language` 为 `auto`、`en`、`zh`；`fontSize` 为 `s`、`m`、`l`、`xl`，对应正文 12 / 14 / 16 / 18 像素，缺省 `m`。界面字体固定为操作系统界面字体。
- **代理**：`proxy` 为空表示跟随系统（环境变量或系统代理设置；系统没有就直连，不探测本机端口）。也可写 `http` / `https` / `socks5` / `socks5h` 代理 URL；只写 `host:port` 按 `http` 处理。`auto`、`direct`、`system` 也按跟随系统读。设置页的测试请求 `https://www.google.com/generate_204`，显示线路（直连 / 系统代理 / 手动代理）、状态码和耗时。环回、`.local` 和私网地址始终直连。
- **提供商**：`id` 是 1–128 字节的小写 portable id；`kind` 只有 `anthropic-messages`、`openai-completions`、`openai-responses`；`baseUrl` 必须是 `https://`；`models` 1–128 个，第一个是默认模型；`enabled` 控制模型选择器。非零的 `contextLimit` 优先于目录里的上下文长度；非零的 `maxOutput` 优先于目录的 `limit.output`，两者都没有时用 32000。`hiddenModels` 最多 2048 个，不出现在对话模型切换、最近使用推荐和命令面板，设置页仍会列出。密钥不写在这里。
- **推理强度**：`reasoningEffort` 可省略，可取 `off`、`none`、`on`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`；`default` 不允许。会话里选的模型和强度不写这里，见 `ui.json`。
- **Shell**：没有 shell 设置。解释器按操作系统自动选择，见 [tools.md](tools.md)。`MYCODE_SHELL` 为非 `auto` 的值时提示一次并忽略。
- **网页**：后端 `kind` 只有 `querit`、`anysearch`、`exa`，`endpoint` 必须是 `https://`，最多一个 `enabled`。默认三行均未启用。密钥先读环境变量 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY` / `EXA_API_KEY`，再读 `secrets.json` 的 `web-<id>`；Querit 和 Exa 必须有密钥，AnySearch 可以匿名。启用开关会先解析密钥，再对该行做一次真实搜索，成功才落为启用。没有启用的后端时，网页工具退到第一个有密钥的后端。
- **MCP**：`transport` 为 `stdio`（`command` 是单个可执行文件，不接受 shell 字符串；最多 64 个 `args`，最多 32 个 `env`）或 `http`（`endpoint` 为 `https://`，`keyHeader` 只能是 `bearer` 或 `x-api-key`）。最多 64 台服务器。密钥用 `mcp-<id>`。内置目录有 context7 和 GitHub（`https://api.githubcopilot.com/mcp`，bearer）；添加的行默认禁用。启用开关对声明了 `keyHeader` 的行要求先有已存密钥，再做一次 `tools/list` 探测，成功才启用。
- **子代理**：记录每个角色是否启用、provider/model 路由和思考强度；没写的角色默认启用并继承会话模型。路由必须同时给 provider 和 model，provider 必须已配置；`thinking` 除模型档位外还接受 `default`。每个父回合固定最多 8 个并发，不是设置项；子代理没有超时，取消回合、停止子代理或删除对话时结束。

粘贴导入由 `mcp_import` 处理：接受 `mcpServers`、`servers`、单个服务器对象或数组，允许 JSONC 注释、尾随逗号和单个片段；id 会被规范化，`Authorization` / `x-api-key` 头会抽成密钥并去掉 `Bearer` 前缀。项目 `mcp.json` 复用同一解析器，但只取服务器行：上限 256 KiB，只加入启用的行；与用户设置同 id 的行会被跳过并给出警告，项目服务器不会读取也不会继承 `secrets.json` 里的密钥。

## 数量上限

| 范围 | 上限 |
| --- | --- |
| 提供商 | 64 个；每提供商模型 128 个、隐藏模型 2048 个 |
| MCP | 64 台服务器；stdio 每条命令 64 个参数、32 个环境变量 |
| 网页 | 16 个后端 |
| 子代理 | 设置里 32 个角色；发现结果含内置最多 32 个；每个角色目录最多读 32 个文件 |
| 密钥 | 64 个 provider，单把 key 16 KiB |
| 技能 | 关闭列表 256 个 slug，每个 128 字节；system prompt 里最多 32 个 |
| 提示资源 | 每份 64 KiB，最多 16 份，合计最多 96K 字符 |
| 自由文本 | 代理、模型 id、MCP 命令/参数/环境变量、角色路由 model 最多 8 KiB；URL 最多 2 KiB |
| `ui.json` | 最近项目 16；会话→项目 256；每会话模型钉选 128；最近模型 8（选择器推荐其中前 3 个）；窗口每边 16384 像素 |

## 密钥

`secrets.json` 的 `providerKeys` 是一个 JSON 对象，值必须是字符串。空字符串等于删除该 id。id 必须是小写 portable id；最多 64 个；单把 key 最多 16 KiB，不能含 NUL 或换行。`Debug` 输出只列 id。整份替换走 revision CAS；`update_provider_key` 是锁内单键读改写。

## 界面状态

`ui.json` 只用于恢复界面，不是产品配置的真相。字段包括：

- `recentProjects`（最多 16）、`lastProject`、`autoUpdate`（默认开）、`selectedProvider`、`selectedModel`。
- `sessionProjects`（最多 256 对会话 id 和绝对路径），用于恢复每个对话的工具工作目录。
- `workspaceRoots`：扁平的文件夹列表，最新的在前。文档校验最多 128 条（兼容旧命名工作区合并）；新装时桌面只保留最近 8 个，旧文档合并出的更长列表不会被新增操作截断；模型每回合最多看到 8 个。
- `recentModels`：最多 8 条，选择器只推荐其中仍可用、未隐藏的前 3 条。
- `sessionModels`：每个会话一条 `SessionModelPin`（provider、model、可选 `reasoning`），最多 128 条，新的在前；同一会话不能重复。切换会话时恢复各自的钉选。
- `selectedReasoning`：作曲栏最后一次选的强度，新会话从它起步，并按模型公布的档位向下收敛。
- `thinkingCollapsed`（默认展开）、`inspectorOpen`、`inspectorPinned`（默认关闭）。
- `windowSize`：上次窗口宽高（逻辑像素，非零且每边不超过 16384）。启动时按它计算窗口并收缩到屏幕可用区、居中；最大化记录可还原尺寸，全屏不写；拖动结束并防抖后写回。见 [desktop.md](desktop.md)。

`recentProjects`、`lastProject`、`workspaceRoots` 必须是绝对路径，单条最多 1024 字节；会话 id 最多 64 字节；模型钉选的 provider/model 每段最多 256 字符。旧文档里的 `workspaces` 会在读取时把文件夹并进 `workspaceRoots`（抢救时最多保留 16 个），`sessionWorkspaces`（抢救时最多保留 512 对）和 `activeWorkspace` 会被清空；其他未知字段仍然按损坏处理。删除会话时一并清掉它的项目、工作区和模型钉选。

## 压缩检查点

每个会话一份 `compaction.json`，放在会话目录里（由 `locate_session_dir` 找到；目录不存在时读取返回空、写入失败），记录 `sessionId`、`branchId`、`coveredHead`、`coveredMessages`、`summary`、`model`、`createdAtUnix`。`summary` 最多 24000 字符；应用层在写检查点前截断，config 层对超长文档直接校验失败。各 id 最多 128 字符。读取时只采用 `branchId` 与当前分支一致的检查点。检查点只缩小发给模型的请求，不改写会话账本。

## 角色与提示资源

内置角色编进二进制：

| 角色 | 职责 |
| --- | --- |
| scout | 只读研究。隔离 `shared`，思考强度 `low`，工具 `read`、`grep`、`find`、`web_search`、`fetch_content`，同一条回复里可以并行；项目覆盖也加不进写工具 |
| artisan | 实现 brief 里的结果，检查与改动相称，不提交、不推送。隔离 `worktree`，思考强度 `high` |

发现顺序是内置 → `<MYCODE_HOME>/agents/*.md` → `<项目>/.mycode/agents/*.md`，同名时后者覆盖前者。每份文件最多 32 KiB；每个目录最多读 32 个 `.md`；解析后的目录（含内置）最多 32 个角色。文件名必须等于 frontmatter 里的 `name`。frontmatter 只认 `name`、`description`、`isolation`（`shared` 或 `worktree`）、`thinking`、`tools` 五个键；未知键、缺字段、空正文、非 UTF-8 或超限文件会被跳过并打一行 stderr，不影响其他角色。启用状态、模型路由和思考覆盖写在设置里，不写进角色文件。

提示资源按固定位置发现：工作区的 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`，然后是 `MYCODE_HOME/AGENTS.md`，最后是 OS 用户主目录（`USERPROFILE`，否则 `HOME`）的 `.agents/AGENTS.md`。重复路径合并；每份最多 64 KiB，最多 16 份，合计进入 prompt 最多 96K 字符；读不了或超限的文件跳过。

技能从 `.agents` 和 `.agents/skills` 下发现：平铺的 `*.md`（扩展名不分大小写）或 `<目录>/SKILL.md`。工作区和 OS 用户主目录各扫一遍（会话 prompt 另扫用户添加的工作区文件夹，不含临时目录），同名 slug 工作区优先；发现不设上限，进入 system prompt 时最多 32 个。目录里列 slug、标题、描述和路径；描述取自 frontmatter，最多 300 字符（超出截断）。模型匹配后用 `read` 按路径读正文。OS 用户主目录的 `.agents` 和 `.agents/skills` 存在时是只读锚定根：`read` 可以按绝对路径读取，`edit` / `write` 拒绝。子代理（含 scout）拿到同一份技能目录。设置里的 `skills.disabled` 记录关闭的 slug（最多 256 个，不能重复）；关闭的技能先被过滤，不占 32 个名额；列表为空时不写入 `settings.json`。

## 不在这里

- models.dev 的下载、校验和 `catalog-cache.json` 写入在 `mycode-providers`：缓存 32 MiB、版本 3，24 小时内直接使用，过期后条件请求刷新。
- 跑回合、连接 MCP、导出/导入配置在 `mycode-app`。导出包包含 `settings`、`ui.json` 和一份明文密钥映射；`secrets.json` 本身仍然独立。
- 会话账本和 `sessions.db` 归 `mycode-agent`，不走本文的文档恢复流程；整体结构见 [architecture.md](architecture.md)。
