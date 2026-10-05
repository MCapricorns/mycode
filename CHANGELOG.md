# Changelog

显著变化从 `0.4.5` 记起。更早的发布记录已作废，不再保留。日期为发布日（UTC）。
质量门通过的 `main` 推送都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，才快进 `main` 并创建标签。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

### Added

- 模型可以用 `shell` 里的 Python 改文件：POSIX shell 用引号 heredoc 或短脚本，PowerShell 用 here-string 管道给 `python`。`write` 和 `edit` 仍然可用。

### Changed

- 版本号需要递增时，先把版本提交推到 `ci/release-<version>-<run id>`。Windows x64、Windows ARM64 和 macOS Apple Silicon 从这次提交构建。三个构建成功之后才快进 `main` 并创建标签，二进制里的版本与标签一致。
- `shell` 是唯一的进程工具。`mode` `script` 走平台 shell，也可以用 Python heredoc 或短脚本改文件；`mode` `program` 保留原来的无 shell 直接启动（钉住 PE / ELF / Mach-O）。提示词和 JSON Schema 只出现 `shell`。
- 委派工具从 `task` 硬改名为 `agent`。进度协议前缀改为 `agent|`。不保留旧名字，旧会话里的 `task` 调用不会按新名字重放。

### Removed

- 不再在改文件之前保存快照，也没有工作区回滚。编辑和撤回只截断对话，编辑会把原文填回输入框，不会恢复或删除工作区文件。`shell`、`write` 和 `edit` 的改动都不会被撤销。删除会话时仍会清掉旧版本留下的 `checkpoints/<id>/`。
- 去掉独立工具 `exec`。没有别名。
- 内置子代理只保留 `scout` 和 `artisan`。去掉 `steward` 和 `sentinel`。
- `scout` 与 `artisan` 的提示改成短的何时使用、边界和完成标准。父提示只在工作能独立并行、边界清楚、并且确实能降低成本或提高完成质量时派 `agent`；同一时刻尽量只跑一个 `artisan`。

## [0.7.5] - 2026-10-05

### Removed

- 发布包不再提供 Linux x86_64。GitHub Release 只包含 Windows x64、Windows ARM64（`aarch64-pc-windows-msvc`）和 macOS Apple Silicon（`aarch64-apple-darwin`）。
- 质量门不再包含 Ubuntu。格式、clippy 和 `exec` 烟测只在 Windows x64、Windows ARM64 与 macOS Apple Silicon 上跑。发布计划自测改在 macOS 的 `core` 作业里。

## [0.7.4] - 2026-10-04

### Changed

- 质量门通过的 `main` 推送都会发布新的 GitHub Release 和新标签。工作区版本的标签已存在时，发布计划把补丁号加一，不再因为该版本的四个平台压缩包已经齐就跳过。

## [0.7.3] - 2026-10-04

### Added

- 发布包增加 Windows ARM64（`aarch64-pc-windows-msvc`）。该平台的 `exec` 与 `shell` 通过 `CreateProcessW` 启动 PE 程序。应用内更新按该平台选择对应压缩包。

## [0.7.2] - 2026-10-04

### Added

- 发布包增加 Linux x86_64（glibc，`x86_64-unknown-linux-gnu`）。应用内更新按该平台选择对应压缩包。
- 对话里的围栏代码块使用 gpui-kit 自带的 Tree-sitter 高亮，覆盖 kit 提供的全部语言。`edit` 的 `ast` 操作改为读取这份注册表，不再自带语法包。

### Removed

- 去掉浅色模式。界面固定为深色；已保存的 `appearance.theme: light` 在读取时改成 `dark` 并写回。

## [0.7.1] - 2026-09-30

### Fixed

- 含有旧待办事件（账本 tag 4）的会话可以再次打开。这些事件不进入模型上下文，也不会显示成用量行。新的待办事件不再写入。
- 压缩检查点只按账本回放的下标复用。已经摘要过的历史如果再次超过阈值，只在内存里再压一次，避免用错误下标切掉尾部。
- MCP 有服务器没连上时不缓存连接池，下一回合会重试。连接中途断开后，下一回合整池重连。
- 0.7.0 之前导出的数据包仍能导入；顶层待办字段会被忽略。
- `@` 文件搜索在提及已经改成命令之后，不会再发出一次过期的文件搜索。
- 工具规格缓存和注册表写在同一把锁里，注册和读取交错时不会把旧规格留在缓存里。

## [0.7.0] - 2026-09-30

### Removed

- 去掉已经过时的待办：`todo_write`、输入框上方的待办条，以及会话目录里的 `todos.json`。导出包不再携带待办。子代理也不能再调用这个工具。

### Changed

- 打开会话和重建模型历史时，一页事件的载荷在同一次会话读取里取回，不再对每条事件各走一轮。
- MCP 连接在设置不变时跨回合复用；连接断开后下一回合重连。
- 同一回合里，历史已经压过且检查点仍覆盖当前 head 时，不再重复调用摘要模型。token 估算直接数字符，不再为每条消息拼一份全文。
- `@` 文件提及在输入停顿后再搜索。工具 schema 注册后缓存，并在每一轮模型请求之间共享。对话消息用 `Arc` 共享，压缩只替换前缀，尾部不再整份深拷贝。
- 删掉未接入的 pack 权威类型、没人读取的角色发现问题列表、空转的 hook，以及从未参与调度的工具并发标记。

## [0.6.0] - 2026-09-24

### Added

- 命名工作区：侧边栏头部现在是工作区切换器，可以新建、重命名、删除工作区；每个工作区各自维护若干目录（原"工作区目录"成为当前工作区的目录列表），会话显式归属到所属工作区并在其下列出，不再按目录绑定推断分组，也没有"其他"分组。首次升级会把原有的目录列表迁移为名为"默认"的工作区，历史会话归入其中。
- 会话列表现在会显示数据损坏（无法读取 manifest）的会话行，可直接删除清理，而不是整个列表因单个坏目录而报"stored data failed validation"。

### Fixed

- 修复删除会话与在途写入的竞态：删除现在先取消该会话的运行中 turn 与子代理、并让会话服务逐出内存账本再删盘上数据，不再出现"删除后残留半个会话目录 → 列表永久报错"、"the session service is unavailable"（actor 致命停机）的连锁故障。
- 删除会话时同步清理 ui.json 中残留的会话-目录/会话-工作区绑定，不再累积已不存在的会话 id。

## [0.5.1] - 2026-09-24

### Changed

- Windows 平台 shell 只支持 PowerShell 7（pwsh）与 Git bash：侦查顺序为 pwsh、回退 Git bash，不再使用 Windows PowerShell 5.1 与 cmd；设置页类型下拉只剩 pwsh/bash，浏览选择其它可执行文件会被拒绝；旧设置里存的 powershell/cmd 会在启动时自动丢弃并重新侦查，不会导致加载失败。
- 改动面板的 git status 轮询改用只读方式（`--no-optional-locks`），不再抢占 `.git/index.lock` 或回写索引；快照连续不变时轮询间隔从 2 秒退避到 4/8 秒，一有改动或切换目录立即回到 2 秒。

## [0.5.0] - 2026-09-24

### Added

- 中英双语界面：新增 `appearance.language` 设置（auto/en/zh），通用页可切换，全 UI 即时切换；`auto` 跟随系统语言。
- 完整改动抽屉：右侧面板的文件列表收敛为预览（前 6 个），“查看全部”打开全高抽屉，浏览全部改动文件并查看所选文件的 diff。
- 自更新对话框：发现新版本后自动下载并校验，完成后弹出对话框确认“重启并安装”；右上角状态片（可用/下载中/待安装）点击即打开该对话框。

### Changed

- 子代理进度只保留两处：底部状态行与右侧面板卡片；对话内不再重复显示，子代理结束后其卡片像已完成的待办一样自动消失，面板卡片可直接取消该子代理。
- 更新失败的错误文案压缩为单行摘要，不再把整段 HTTP 错误链拉满横幅。

### Fixed

- 修复模型用量统计：会话回放重建的行（裸模型名）与实时记录的行（provider/model）现在合并为同一行，输入/输出与轮次会持续刷新而不是冻结在旧行上；缓存占比在重建后不再丢失；面板选中模型无匹配行时回退显示实际运行的模型，不再出现“本会话暂无用量统计”的误报。

## [0.4.7] - 2026-09-23

### Changed

- 删除全部测试套件与仅被测试引用的代码（测试钩子、注入缝隙、dev 依赖），共约 1.3 万行；测试将在之后按需重写。
- 工作区内 21 个 `mod.rs` 全部改为现代模块文件布局，并以工作区级 `clippy::mod_module_files` 强制不再出现。
- 删除只有测试在用的接口：agent 的 steer/follow-up 队列、`AgentHandle` 与队列模式（消息排队由应用层调度负责，`TurnOutcome` 只剩 Completed / Aborted），桌面的 `MentionDismissed` / `UnboundSessionsAssigned` / `DismissError` 动作。
- 编译提速：HTTP 客户端改用 OS TLS 并去掉 http2 特性；`base64` 对齐到 0.22；移除未使用的 `syn` 依赖。
- 发布构建打开 fat LTO、O3 与单 codegen unit，追求最佳运行性能。
- CI 收敛为单个 workflow：push 只做 fmt + clippy 质量门禁（不再跑测试）；发版改为手动触发，发布说明手写、不由 changelog 或提交记录生成。
- 超过 1000 行的桌面端源文件按职责拆分；`exec` 的三个平台实现共享同一份摘要复查与 C 字符串组装助手。
- 设置页头部不再显示 revision 版本号。

### Fixed

- Querit 网页检索的 `crawlTimeout` 改为按秒发送。
- 深浅主题下选中的文本保持可读。

## 0.4.5 - 2026-09-23

### Changed

- 窗口启动时落在主屏幕可用区域的正中。屏幕比默认尺寸小的时候，窗口会先缩小再居中。
- 模型菜单和思考菜单从右侧按钮旁边打开。
- 已经保存的网页搜索密钥不再显示在输入框里，只留一把锁。点锁可以换一把新钥匙，旧密钥不会被填回来。
- MCP 的添加收进二级页：从目录添加、导入 JSON、自定义服务器。已保存的密钥同样只显示锁。
- 思考按钮写出 Thinking / Thinking off / Thinking on，不再只写 On。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.7.5...HEAD
[0.7.5]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.5
[0.7.4]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.4
[0.7.3]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.3
[0.7.2]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.2
[0.7.1]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.1
[0.7.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.0
[0.5.1]: https://github.com/MCapricorns/mycode/releases/tag/v0.5.1
[0.5.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.5.0
[0.4.7]: https://github.com/MCapricorns/mycode/releases/tag/v0.4.7
