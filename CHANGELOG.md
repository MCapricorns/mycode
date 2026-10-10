# Changelog

0.9.25 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

## [0.9.25] - 2026-10-10

### Added

- init 第一版：mycode 首次公开发布。
- 四个平台的发布包：Windows x64（`x86_64-pc-windows-msvc`）、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`，zip 与 `.dmg`）、Linux x86_64（`x86_64-unknown-linux-gnu`），均附 `.sha256`。
- Windows 上 `shell` 的脚本模式优先 PowerShell 7（`pwsh`），其次 Git bash，不侦查 Windows PowerShell 5.1。两者都没有时，运行时才退到 `cmd.exe`，并且不把这次退路写进设置。`pwsh` 以 UTF-16LE 的 `-EncodedCommand` 启动，并尽量把管道编码设为 UTF-8。标准输出和标准错误里的 CLIXML、`_xHHHH_` 转义和 ANSI 颜色会收成可读文本。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.9.25...HEAD
[0.9.25]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.25
