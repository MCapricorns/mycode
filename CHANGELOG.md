# Changelog

0.9.10 是第一次公开发布。下面更早的条目只作开发记录。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

### 修复

- 模型流中途失败、异常结束或连接中断时，已经到达的思考和正文会留在对话里，并附带一行可见错误。回合不再像没发生过一样被清掉。用户取消仍然丢弃半截回复。
- GLM、DeepSeek、Kimi 会在后续请求里带回思考内容。GLM 的思考深度会带上 `reasoning_effort`；glm-5 不再发送会被拒绝的 `thinking.type = disabled`。MiniMax 思考开启时，正常多轮的请求形状保持不变。
- `ask_user` 可以多选。点选若干选项后提交，模型会收到全部值。单选仍是点一下即提交。
- 详情面板点置顶不再被背后的遮罩立刻关掉。置顶状态会留下，面板保持可见，不必再点一次标题栏开关。
- 输入框选择的思考强度会写入设置。保存进行中再选一次也不会丢掉。自定义模型或只标明开关的模型可以记住低、中、高。
- 本机没有 git 不是思考突然消失的原因。变更面板仍提示未安装 git；需要工作树的子代理失败时，只返回该次工具错误。

## [0.9.16] - 2026-10-07

### 产品

- 左侧栏分成四块：工作区切换器、操作条（新建任务、搜索会话）、目录、会话。目录和会话不再跟操作按钮挤在同一条列表里。
- 输入框占位改为「向代理提问 / Ask the agent」。进行中的回合仍是「追加引导,不打断当前任务」。
- 标题栏、侧栏、检查器、菜单和对话框改为半透明毛玻璃。窗口使用系统模糊。对话和设置的阅读区仍接近实色。

## [0.9.15] - 2026-10-07

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.9.14] - 2026-10-07

### 产品

- 会话索引的读取不再抢写锁。一次存储失败不再把会话服务停掉，之后的新建、打开和发送不会一直报 “the session service is unavailable”。
- 输入 `/` 会列出命令、技能和已启用的 MCP。回车接受当前行，不必再用鼠标点。`/new`、`/settings`、技能和 MCP 服务或工具都可以这样选。
- 模型选择里的「最近」按两行文字留出高度，不再压住下一行。
- 设置色板按多行换行：七列，色块 28px，格子 52px，间距 6px。不再把十三个颜色排成一行撑破设置面板。
- 置顶的详情是一张内缩卡片；未置顶时抽屉从标题栏下面开始，不再挡住最小化、最大化和关闭，双击标题栏也不会点到详情。
- 切换语言后，设置搜索、会话搜索和输入框占位会跟着变成当前语言。

## [0.9.13] - 2026-10-07

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.9.12] - 2026-10-06

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.9.11] - 2026-10-06

### 产品

- 设置顶栏去掉重复的「设置 / Settings」标题，高度改为 44px。左侧只保留返回和 Esc，右侧保留搜索。整个设置页不再提供「保存更改」。
- 外观字段顺序为语言、色板、界面字体、字号、User-Agent。语言在第一屏。色块 28px，格子 52px，间距 6px，仍是 5 列。色块底色用同一份 Spec 的页面背景，角上是强调色，选中环也用强调色。
- 选择色板、语言、字体或字号后立即写入 `settings.json`。User-Agent 在失焦或停止输入约 400ms 后写入。

## [0.9.10] - 2026-10-05

第一次公开发布。安装包状态栏为 v0.9.10。

### 产品

- Linux x86_64 进入发布包：在 `ubuntu-latest` 上构建 `x86_64-unknown-linux-gnu`，与 Windows x64、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`）一起发出，各带 `.sha256`。
- 全平台使用客户端绘制的标题栏（CSD）。系统标题栏隐藏；最小化、最大化、关闭在右侧。Linux 上窗口装饰由客户端绘制，不再叠一层系统标题栏。
- 没有打开工作区时，空工作台只有「打开目录 / Open folder」。打开文件夹之后才出现「新建任务 / New task」。
- 外观包括十三色色板、界面字体（Interface font）和界面字号 S / M / L / XL。色板、字体和字号立即生效并写入设置。
- 外观绘制（#42）：色板下面的界面字体和 S–XL 画在第一屏里，不再被色板行高挤出视口。
- 设置滚动：色块放在固定宽度的格子里，色板行按内容高度排列，避免百分比高度把设置滚动区撑开、把字体控件顶出第一屏。

## [0.9.9] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.8] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.7] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.6] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.5] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.4] - 2026-10-05

### Changed

- 三个平台构建通过的 `main` 推送自动发布。

## [0.9.3] - 2026-10-05

### 产品

- 桌面更安静：侧栏更窄，对话居中，详情默认收起，宽窗口可以钉在旁边。设置里的界面字号（S / M / L / XL）立即生效并写入 settings.json。

## [0.9.2] - 2026-10-05

### 产品

- 桌面更安静：侧栏更窄，对话居中，详情默认收起，宽窗口可以钉在旁边。设置里的界面字号（S / M / L / XL）立即生效并写入 settings.json。

## [0.9.1] - 2026-10-05

### Changed

- 质量门通过的 `main` 推送自动发布。

## [0.9.0] - 2026-10-05

mycode 是跑在本机的桌面编码代理。对话、工具和会话都在你的电脑上。你自己提供 API 密钥。这一版先给真机验证用。

### 产品

- 发布包是三个平台的 zip：Windows x64、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`），各带 `.sha256`。不提供 Linux 发布包。
- 进程工具只有 `shell`。`mode` `script` 走平台 shell，可以用 Python heredoc 或短脚本改文件；`mode` `program` 直接启动钉住的程序映像。`write` 和 `edit` 仍然可用。没有名为 `exec` 的工具。
- 委派工具是 `agent`。内置角色只有 scout 和 artisan。没有 `task` 别名。
- 不保存文件快照，也没有工作区撤销。对话里的编辑和撤回只截断对话；编辑会把原文填回输入框。
- 子代理没有墙钟超时，长任务不会在大约 10 分钟时被杀掉。取消仍然会停掉子代理。并发默认 4 个。设置里的 `0` 表示使用这个默认值，不是零个子代理。
- 界面为深色。设置里可选色板：石板灰、海洋、森林、暮色、沙丘、玫瑰、墨色、苔原、余烬、冰川、梅紫、铜绿、极光。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.9.16...HEAD
[0.9.16]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.16
[0.9.15]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.15
[0.9.14]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.14
[0.9.13]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.13
[0.9.12]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.12
[0.9.11]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.11
[0.9.10]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.10
[0.9.9]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.9
[0.9.8]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.8
[0.9.7]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.7
[0.9.6]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.6
[0.9.5]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.5
[0.9.4]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.4
[0.9.3]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.3
[0.9.2]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.2
[0.9.1]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.1
[0.9.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.0
