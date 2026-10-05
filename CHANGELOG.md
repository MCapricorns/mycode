# Changelog

0.9.0 是真机验证用的基线。更早的版本记录不再写在这里。日期为发布日（UTC）。

`main` 上三个平台构建成功后都会发布新的 GitHub Release 和新标签（三个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，三个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。不提供 Linux 发布包。

## [Unreleased]

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

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.9.9...HEAD
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
