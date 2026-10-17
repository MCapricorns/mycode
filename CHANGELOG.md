# Changelog

日期为发布日（UTC）。更早版本的条目已清理，历史可在 git 记录中查看。

`main` 上三个平台构建成功后都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64 `x86_64-pc-windows-msvc`、Windows ARM64 `aarch64-pc-windows-msvc`、macOS Apple Silicon `aarch64-apple-darwin`，压缩包名形如 `mycode-desktop-vX.Y.Z-aarch64-pc-windows-msvc.zip`；没有 Linux 包）。pull request 不发版，也不跑发布构建：PR CI 只在 Windows 上执行 `cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings`（dev/test profile，没有 fat LTO）。推到 `main` 时三个平台才做 fat LTO 发布构建；同一份检查通过且三个构建都成功后才打标签。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。压缩包未签名。

## [Unreleased]

### Added

- 会话标题自动生成：会话首条消息发出时，用当前模型另发一次不带系统提示和工具的旁路请求，把首条消息概括成短标题写入索引并刷新侧边栏；失败时保留首条消息作标题。
- 子代理运行计时：对话里的 agent 行、上下文面板的子代理卡片和详情窗口显示已运行时长，运行中每秒刷新，结束后保留最终耗时。
- 折叠的工具批次行列出每次调用的目标（文件、查询等），不展开也能看出动了哪些文件。
- 输入区芯片悬停提示：模型芯片显示「服务商 · 完整模型名」，目录芯片显示完整路径；上下文芯片平时只显示用量比例，缓存命中与本轮入/出 token 移到悬停提示。
- grep 在没设 `is_regex`、模式里含 `|` 或 `.*` 且零匹配时，提示这次是按字面匹配的。

### Changed

- 提问卡（`ask_user`）从全屏模态改为停靠在输入框上方，一次一题：单选点选即进入下一题，多选或自由文本点「下一题」，最后一题提交；可回到上一题，再点已选项可取消，输入文字会清掉已选项。
- 回合进行中回车改为排队（不再追加引导），Ctrl+Enter 打断当前回合并立即发送；回合运行时 Esc 不再停止回合或撤回队列，只有停止按钮能结束回合。
- 手动 `/compact` 期间锁定输入框并在上方显示等待卡：回车排队，Ctrl+Enter 取消；重复 `/compact` 会提示等待。完成后界面采用新的分支头，下一次压缩或发送不再撞上「头已移动」检查，排队消息随后自动发送。
- 去掉命名工作区：侧边栏不再有工作区切换、新建、重命名、删除，目录变成一个扁平列表，所有会话都列出；旧 `ui.json` 里各工作区的目录合并进 `workspaceRoots`。点击目录打开它最新的会话（没有就新建）；删除目录会删除绑定该目录的全部会话。
- 用户消息按 Markdown 渲染；悬停操作只保留「撤回」（回退到该消息之前并把原文放回输入框，附提示），去掉单独的「编辑」。
- 输入框固定三行高，长文本在框内滚动；排队列表、提问卡、压缩卡统一为磨砂面板。
- 回合已结束但工具调用没完成（被中断）时标红；子代理行区分「子代理失败」与「子代理中断」。
- 更新下载中再点「检查更新」会打开更新对话框，按钮显示「下载中…」；更新对话框不再响应 Esc。
- 工具参数校验错误不再把整个参数值（如 edit 的多行内容）塞进错误信息：`oneOf`/`anyOf` 失败时列出允许的 `type` 取值或指出缺少 `type`，其余错误截断到 320 字节。

### Fixed

- 回退（rewind）后重开会话或重启应用会落回旧分支、显示回退前的对话：索引现在记录会话最后写入的分支，重开时打开该分支。

## [0.11.29] - 2026-10-17

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.11.28] - 2026-10-17

### Fixed

- 思考档位重启后丢失：`ui.json` 的 `selectedReasoning` 只在新会话建立时播种，桌面输入框（无会话）重启后芯片一直显示「Thinking」，且首个新会话会把服务商默认档位写进 pin。现在加载 `ui.json`、重载设置文档（OAuth 登录完成、导入配置）、会话离开屏幕时按「会话 pin > 作曲栏上次所选 > 文档默认」重敷实时档位，目录未加载时芯片也能显示记住的档位。

## [0.11.27] - 2026-10-17

### Added

- 模型库列表新增 OpenDesign 行：和其它服务商一样点「添加」打开预设表单、粘贴 API key 即可，不再走单独的二级页。
- 新增 `docs/model-catalog-updates.md`：手工更新 Codex / OpenDesign 模型与思考档位的维护指南。

### Changed

- 模型目录改为缓存优先：启动后读家目录 `catalog-cache.json`，缓存超过 24 小时或不存在才下载 models.dev（带 `If-None-Match` 条件请求），失败退回上次缓存；不再把整份 models.dev 快照编进二进制。GitHub Copilot 的模型行完全来自 models.dev，不再硬编码。
- 离线保底只剩两行手工维护数据：OpenAI Codex 预设（`openai_codex_preset`）与 OpenDesign 实测表（`CURATED`），每份目录文档解析后都会注入这两行。
- 目录刷新时机：启动首帧后静默刷一次（24 小时内不发请求，失败静默）；应用更新下载成功后强制刷一次，让新版本读到的缓存是新的；关于页手动「刷新」照旧。
- 设置页添加服务商收敛为三个按钮：「从模型库添加」、「OAuth 登录」、「添加自定义端点」。GitHub Copilot 与 OpenAI Codex 共用一页 OAuth 登录，两张卡片各自发码；签入绑定目录行模型，之后在服务商详情里裁剪。
- 更新检查和目录刷新的所有结果都只弹右上角气泡（已是最新、有新版本、下载中、失败、目录刷新成功或失败），关于页不再重复铺状态行和大段错误文字。

### Removed

- 去掉 Codex / OpenDesign 的实时模型拉取：签入后不再从 `chatgpt.com/backend-api/codex/models` 拉列表缓存到 `codex-models.json`，OpenDesign 不再有「从网关加载模型」按钮和 `opendesign-models.json`——实时列表拿不到思考档位，模型与档位改在代码里手工维护随版本发布。
- 删掉每周 models.dev 快照流水线：`models-snapshot.yml`、`scripts/generate_catalog.py`、`catalog_snapshot_diff.py` 与编进二进制的 `catalog/snapshot.json` 一并移除，发布构建不再拉取云端目录。

### Fixed

- 更新安装成功后清理 `mycode-desktop.exe.mycode-previous` 备份与暂存目录残留，不再在更新后遗留旧二进制。

## [0.11.26] - 2026-10-17

### Added

- 排队消息每行带三个操作：✎ 撤回到输入框编辑、× 删除、↵ 打断当前回合并立即发送这条。
- 内置 MCP 目录新增 GitHub 服务器（`https://api.githubcopilot.com/mcp`，Bearer 密钥）。
- 窗口尺寸持久化：启动恢复上次窗口大小（最大化记还原尺寸），拖动边缘调整后防抖动写回 `ui.json`。
- Codex 模型列表改由后端实时提供：签入完成时从 `chatgpt.com/backend-api/codex/models` 拉取并缓存到 `codex-models.json`（models.dev 不含 Codex 行），已签入的账户每次启动在首帧后静默刷新；拉取失败或过期令牌刷新失败都回退到预设列表。
- GitHub Copilot 与 OpenAI Codex 设备码登录在设置页各有独立入口页（与 OpenDesign 相同层级），不再混在 models.dev 目录列表里；目录列表不再显示 `device-code` 服务商。
- 损坏的 `settings.json` / `secrets.json` / `ui.json` 重置时逐字段抢救（salvage）：能解析成 JSON 的文档里每个成员单独解码校验，通过的保留，只有通不过的成员被丢弃，避免一次重置让用户全部重配。

### Fixed

- 对话已在最新位置时，再向下滚动（滚轮或触控板惯性）不再闪出「回到底部」按钮：滚动偏移在钳制前会短暂越过底部，此前被误判为离开尾部。
- 会话分支提交中途失败时遗留半截 `new_log` 文件，现在在复制失败路径上清理。
- 工作区 `.env` 行解析此前先把逗号替换成空格，引号内的逗号（`KEY="a,b"`）会被误拆；改为按引号感知分隔。

### Changed

- 依赖整体更新到兼容版本（gpui-kit 0.7.3、serde_json、tokio、zip 等）；会话 id 生成复用 uuid，移除 getrandom 依赖；设置页按钮统一为描边样式。
- 启动路径减负：目录缓存只校验信封、文档保留原始字节，模型选择器等目录界面首次打开才在阻塞池里解析；更新检查与目录刷新推迟到首帧之后；`spawn_blocking` 线程池上限收为 32；全局分配器换为 mimalloc，Windows 目标用 VC-LTL 把 CRT 链到系统自带的 ucrtbase/msvcrt，去掉运行时 DLL 依赖。
- MCP 与网页搜索后端的启用开关改为先解析密钥、再做真实探测（MCP 走工具探测，网页走一次搜索），成功才落为启用；从内置目录添加的 MCP 服务器默认禁用，不能再绕过探测。
- 移除自定义（`custom`）网页搜索后端：设置页只保留 querit、anysearch、exa 三个厂商；旧配置文件里的 `custom` 行在抢救时直接丢弃，此前的自定义行开关也顺带堵上了绕过「密钥+探测」门的口子。
- `openai-codex` 预设与实时列表都只保留 `gpt-5.6` / `gpt-6` / `gpt-6.1` 系列（8 个预设），`o3`、5.3/5.4/5.5 等旧族不再进目录。
- 出站 User-Agent 改为应用固定的 `MYCode-Agent/<版本> (<平台> <内核>; <架构>; <shell>)`，不再伪装 pi，Codex 请求的 `originator` 同步改为 `mycode`；设置里的自定义 UA 输入框与 `userAgent` 字段一并移除。
- 配置文档改为严格解码：未知字段、旧字段（`userAgent`、`usage`、`appearance.palette`/`fontFamily`、`tools.*`、`subagents.maxConcurrent`/`timeoutSecs`、网页 `custom` 后端、`ui.json` 的 `workspaceRoots`/`trustedProjects` 等）一律视为文档损坏，备份后重置 + salvage，不再做逐字段兼容合并；`json_recover` 尾逗号修复同步移除。
- 设置页模型区的按钮行拉开间距，不再挤在一起。

## [0.11.25] - 2026-10-17

### Changed

- 运行中子代理卡片的渲染不再整拷贝列表；输入区工具控件去掉静态底色与描边，选中态仅用悬停底色区分。
- 请求与流式事件的 JSON 字节上限校验改用零分配字节计数；回放历史窗口中的助手消息只做一次反序列化；命令面板与工作区菜单共用预构建的会话绑定索引；模型显示名、上下文窗口与目录短标签统一收敛到 `view_model`。

### Fixed

- 网页正文按目标提取小节时，多行段落首行以 `Version ` 开头不再被误判为二级标题。
- 工作区切换菜单在会话绑定到已删除工作区时，首工作区的会话计数与侧边栏保持一致。
- 自定义子代理角色仅声明 `agent`、`ask_user` 或未知工具名时不再被误判为可写角色。

## [0.11.24] - 2026-10-16

### Added

- 网页搜索后端新增 Exa：设置页列出 Querit、AnySearch、Exa 三个厂商，点进各自详情页粘贴密钥（密钥只存凭据库，也支持 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY` / `EXA_API_KEY` 环境变量）。搜索走 `POST /search`（带 highlights 作摘要），取页面走 `POST /contents`，沿用同样的条数、字节和取消上限。

### Changed

- 网页搜索设置不再一进来就是密钥输入框和自定义后端表单：改为厂商列表 + 详情页，和模型服务商页的交互一致。旧设置文件里的 `custom` 后端仍能读取、显示、停用和删除，但界面不再新增。
- 技能设置页一行一个技能：命令名、标题、作用域、路径压进单行，启用开关和「使用」按钮保留，不再每项占一张卡片。
- 设置页渲染不再每次整克隆一份设置投影；排队消息和追加引导的渲染也不再整拷贝列表；厂商后端合并逻辑收敛到配置层一处。

## [0.11.23] - 2026-10-16

### Changed

- 欢迎页：空白桌面的欢迎内容居中，标题字标改为暖色，去掉「新建任务」按钮。
- 子代理取消挪到对话里：输入框上方不再出现「运行中」列表，正在运行的子代理那一行末尾有一个 ×，点它只停止这个子代理；点这一行打开子代理窗口。
- 子代理运行时，「工作中」状态行只显示「等待代理结束」（多个时显示数量），不再重复对话里代理行已有的任务和步骤；状态行下方重复的那一行也去掉了。
- 子代理窗口自动滚到最新一步；手动往上翻时不会被拉回底部，打开另一个子代理时从底部开始。
- 侧边栏会话列表每帧只建一次索引、只复制可见行，会话多时滚动和输入更顺；流式输出时工具调用的名称和参数复用缓冲区；多处字符串拼接不再先生成临时字符串。
- 代码整理：`turn.rs` 和其余超过 1000 行的文件按职责拆分；删除 Linux 专用代码（macOS 是唯一的 Unix 目标，`cfg(unix)` 代码保留给 macOS）；PR CI 的检查改在 Windows 上运行。

### Fixed

- 回合进行中新建的对话不再一直显示「发送中」。
- 工具结果编码失败时报告错误，而不是静默丢掉。
- 「全部撤回」排队消息一次完成，不再逐条撤回。
- 技能目录里扩展名大写的单文件技能（如 `NOTES.MD`）也能被发现。
- Windows 上 `TEMP` 是 8.3 短路径（用户名较长或带空格时常见）时，文件工具拒绝「短父目录 + 长子目录名」写法的路径（报 path escapes the session cwd）；现在会按磁盘上的长路径再匹配一次。

### Security

- 所有出站请求只走 https：服务商、MCP、OAuth、模型目录的请求在每一跳都要求 https，重定向不能降级到 http（设置里本来就只接受 https 地址）。
- 重定向跳到另一个源（协议、主机或端口不同）时不再带上 `Authorization`、`x-api-key`、MCP 自定义密钥头等凭据，只保留 `accept` 和 `content-type`。
- 项目目录里的 `.mycode/mcp.json` 不能再用已配置的服务 id 顶替用户自己的 MCP 服务，项目里的服务也拿不到用户保存的密钥；之前恶意仓库可以借同名服务把用户的密钥发到它指定的地址。
- 更新只从本仓库的 GitHub Release 下载；压缩包里带盘符前缀的条目（如 `C:x.exe`）会被拒绝，不能写出暂存目录。
- OAuth 登录打开浏览器不再经过 `cmd /c start`（服务器返回的地址里的 `&`、`|` 会被当成命令解析），且只打开 https 地址。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.11.29...HEAD
[0.11.29]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.29
[0.11.28]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.28
[0.11.27]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.27
[0.11.26]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.26
[0.11.24]: https://github.com/MCapricorns/mycode/releases/tag/v0.11.24
