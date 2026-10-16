# Changelog

日期为发布日（UTC）。更早版本的条目已清理，历史可在 git 记录中查看。

`main` 上三个平台构建成功后都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64 `x86_64-pc-windows-msvc`、Windows ARM64 `aarch64-pc-windows-msvc`、macOS Apple Silicon `aarch64-apple-darwin`，压缩包名形如 `mycode-desktop-vX.Y.Z-aarch64-pc-windows-msvc.zip`；没有 Linux 包）。pull request 不发版，也不跑发布构建：PR CI 只在 Windows 上执行 `cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings`（dev/test profile，没有 fat LTO）。推到 `main` 时三个平台才做 fat LTO 发布构建；同一份检查通过且三个构建都成功后才打标签。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。压缩包未签名。

## [Unreleased]

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

### Security

- 所有出站请求只走 https：服务商、MCP、OAuth、模型目录的请求在每一跳都要求 https，重定向不能降级到 http（设置里本来就只接受 https 地址）。
- 重定向跳到另一个源（协议、主机或端口不同）时不再带上 `Authorization`、`x-api-key`、MCP 自定义密钥头等凭据，只保留 `accept` 和 `content-type`。
- 项目目录里的 `.mycode/mcp.json` 不能再用已配置的服务 id 顶替用户自己的 MCP 服务，项目里的服务也拿不到用户保存的密钥；之前恶意仓库可以借同名服务把用户的密钥发到它指定的地址。
- 更新只从本仓库的 GitHub Release 下载；压缩包里带盘符前缀的条目（如 `C:x.exe`）会被拒绝，不能写出暂存目录。
- OAuth 登录打开浏览器不再经过 `cmd /c start`（服务器返回的地址里的 `&`、`|` 会被当成命令解析），且只打开 https 地址。
