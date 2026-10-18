# Changelog

日期为发布日（UTC）。0.12.0 是第一个正式版本；之前的预览版本及其 Release 已移除，历史可在 git 记录中查看。

`main` 上三个平台构建成功后都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64 `x86_64-pc-windows-msvc`、Windows ARM64 `aarch64-pc-windows-msvc`、macOS Apple Silicon `aarch64-apple-darwin`，压缩包名按可执行文件名分前缀、形如 `{可执行文件名}-vX.Y.Z-{target}.zip`，例如桌面版 `mycode-desktop-vX.Y.Z-aarch64-pc-windows-msvc.zip` 与 CLI `mycode-cli-vX.Y.Z-x86_64-pc-windows-msvc.zip`；没有 Linux 包）。pull request 不发版，也不跑发布构建：PR CI 只在 Windows 上执行 `cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings`（dev/test profile，没有 fat LTO）。推到 `main` 时三个平台才做 fat LTO 发布构建；同一份检查通过且三个构建都成功后才打标签。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。压缩包未签名。

## [Unreleased]

## [0.12.0] - 2026-10-18

### Changed

- 破坏性改名：终端可执行文件 `mycode` 更名 `mycode-cli`，发布资产前缀同步改为 `mycode-cli-v*`；旧 `mycode` 二进制仍走 CLI 系列更新，装上后落在 `mycode-cli`。
- 自安装：CLI 首次运行把自己复制进 `~/.mycode/bin` 并把该目录写入用户 PATH（Windows 注册表 `HKCU\Environment`，macOS `~/.zprofile` / `~/.bash_profile` 等），随后在终端里提示安装位置、按 Enter 继续，之后新开的终端可直接敲 `mycode-cli`；Windows 桌面版首次运行即安装——复制为 `~/.mycode/bin/mycode-desktop.exe`、带原参数启动安装好的副本、删除原来那个 exe 并新建桌面快捷方式。macOS 桌面版只用 dmg 安装、不做自安装，从「应用程序」原地自动更新（Release 里的 macOS 桌面 zip 只给更新器用）。清理下载只删文件，不会删名为 `mycode` 的文件夹。`MYCODE_NO_INSTALL=1` 或 debug 构建跳过全部自安装。
- 终端标题显示 `MYCode Cli <版本>`；Windows 版 `mycode-cli.exe` 带螃蟹图标与文件说明/版本资源。
- 代理只剩两种模式：「跟随系统」（环境变量或系统代理设置；没设就直连，不再偷偷探测本机 Clash / v2rayN 端口兜底）和「手动填写」（`http` / `https` / `socks5` / `socks5h`）。旧配置里的 `auto`、`direct` 都按跟随系统处理。
- `write` 工具的说明和报错写明：替换已有文件必须带该文件最新的 `[revision …]` 或 `overwrite=true`，报错里带上文件路径，模型不用再整份重发一次才知道怎么改。

### Added

- 代理「测试」：桌面通用页和 CLI `/settings` 都能按当前设置请求 google.com（`generate_204`），显示走的线路（直连 / 系统代理地址 / 手动地址）、状态码和耗时。
- CLI `/settings` 设置页：模型（服务商 → 模型两级选择器，带搜索与最近使用）、思考强度、主题/语言/字号、代理、自动更新、服务商与 MCP/网页开关、`a` 填 API 密钥、`u` 改端点、版本与更新行。粘贴会进当前输入框（API 密钥、端点、代理），`Ctrl+C` 先取消正在跑的回合。
- CLI 欢迎区显示 Ferris 螃蟹（hi ferris）；转录里中断/错误行（`[error] …`）红色加粗，思考改为品红斜体，与正文区分开。
- 回合进行中显示已用时间：CLI 每秒刷新并显示最新一行思考，桌面状态行同样带耗时。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/MCapricorns/mycode/releases/tag/v0.12.0
