# Changelog

0.9.23 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

### Fixed

- 打断正在生成的回合后再发送时，已经收到的助手正文和已完成的工具调用会写入会话，并标成被用户打断。下一条请求仍带着此前的历史和同一条 prompt cache key，输入框下的上下文计量不会被清成新会话。
- 回合进行中发送的文字会注入当前回合（steer），不打断正在跑的子代理。排队发送和「打断并发送」仍保留完整历史。
- 按停止后再撤回，不再因为界面上的会话头落后于账本而报「the session moved on; reopen it」。
- 当前对话已经是空白时，再点新建任务会留在这一条上，不再多出一份空会话。
- Windows PowerShell 的 stderr 不再把 ANSI 颜色和 `_x001B_` 转义原样显示出来。
- 过长的 shell 命令和输出在对话列里换行，不再把窗口横向撑开。
- 思考内容和正文分开显示，可以折叠。正在生成的思考默认展开，已经写入记录的默认收起。收起后仍是一行标题（思考 · 摘要 ▸），正文始终单独显示，不会和思考一起收成空行。
- 对话可以跳到顶部和底部。
- 输入框的上下文计量旁边显示这一次提示的缓存命中百分比。

### Changed

- 直接依赖升到当前稳定版，包括跨大版本的 base64 0.23.1、jsonschema 0.58.6、rusqlite 0.40.2、zip 9.0.0、gpui-kit 0.7.1、notify 8.2.0。`tree-sitter` 仍钉在 0.26.13，因为 gpui-component 0.7.1 要求 `^0.26.13`，0.27 不能和它链在一起。zip 9 的条目名现在是 `Result`，解压更新包时按错误处理。

## [0.9.23] - 2026-10-09

### Added

- init 第一版：mycode 首次公开发布。
- 四个平台的发布包：Windows x64（`x86_64-pc-windows-msvc`）、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`，zip 与 `.dmg`）、Linux x86_64（`x86_64-unknown-linux-gnu`），均附 `.sha256`。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.9.23...HEAD
[0.9.23]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.23
