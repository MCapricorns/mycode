# Changelog

0.9.25 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明用本文件里该版本的条目，不从提交记录生成。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

## [0.9.27] - 2026-10-10

### Added

- 系统提示词新增 `<environment>` 块：OS 与架构、会话 cwd、当前解析到的 shell（`script_shell_line()`，与脚本模式同一条解析链：设置 → 检测 → Windows 的 cmd 兜底）。子代理提示词同样注入一份，cwd 是各自的运行目录。模型不再需要靠报错猜平台或 shell 方言。

### Changed

- shell 为 PowerShell 且 bash 单行命令被翻译成 cmdlet 时，结果文本第一行注明 `[bash command translated to PowerShell: …]`，细节里新增 `translated_command`：改写对模型可见，不再静默成功让 bash 习惯看起来直接可用。
- `shell` 工具描述删去跨平台 shell 枚举句，改为指向系统提示词的 `<environment>` 块；翻译说明同步注明结果会标注改写。

## [0.9.26] - 2026-10-10

### Fixed

- 会话里超长的运行状态行（如整条 shell 命令）不再横向溢出对话列，改为单行内截断。
- 对话列右下角的「顶部/底部」胶囊按钮替换为悬停显示的居中圆形箭头：靠近标题栏的向上箭头与输入框上方的向下箭头，仅在鼠标悬停对话区域且对应方向可滚动时出现。
- 详情面板与输入框下方的上下文统计口径对齐：上下文行补上缓存命中率（与输入框逐项一致），输入/输出/缓存/轮次归入「会话累计」分组，不再和最新一次提示的缓存数字混淆。
- 切换模型不再丢失已选的思考强度：当前模型不支持该档位时显示「思考 · 默认」，切回支持的模型自动恢复；请求只携带当前模型支持的档位。
- 手动 `/compact` 对很短的会话也会生成摘要检查点，不再提示没有可压缩的上下文。

### Added

- Windows 上 shell 为 PowerShell 时，常见 bash 单行命令（`ls -la`、`rm -rf`、`cp -r`、`mkdir -p`、`touch`、`which`、`head`/`tail`、`wc -l`、`grep`、`find -name`）自动转换为对应 cmdlet，`2>/dev/null` 转为 `2> $null`；无法识别的命令原样执行。

### Changed

- 修复平台差异导致的测试失败（Windows ACL 测试夹具、绝对路径断言），并清理全部编译警告。
- 发版前的全仓代码审计清理：更新下载的全部磁盘阶段移出单线程核心运行时（下载期间不再卡住界面与对话）；一轮以工具步骤收尾时补记用量统计；Windows 更新器复归正常返回结构；ask 面板的自由文本在提交后清空；悬空的工作区绑定回退到第一个工作区；目录选择器的截断标记与长文件名、Toast 长文本不再溢出；PowerShell 序言语法识别大小写（`Param($x)` 不再插入失败）；Anthropic 网关缺 `content_block_start` 的文本增量不再在最终消息中丢失；清理死代码（七个未发布的调色板规格、无调用的 HTTP 客户端与搜索包装、调试遗留语句）、合并 shell/program 两路执行的四组重复 helper 与跨模块重复函数，并修正十余处过期注释与错误文案。

## [0.9.25] - 2026-10-10

### Added

- init 第一版：mycode 首次公开发布。
- 四个平台的发布包：Windows x64（`x86_64-pc-windows-msvc`）、Windows ARM64（`aarch64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`，zip 与 `.dmg`）、Linux x86_64（`x86_64-unknown-linux-gnu`），均附 `.sha256`。
- Windows 上 `shell` 的脚本模式优先 PowerShell 7（`pwsh`），其次 Git bash，不侦查 Windows PowerShell 5.1。两者都没有时，运行时才退到 `cmd.exe`，并且不把这次退路写进设置。`pwsh` 以 UTF-16LE 的 `-EncodedCommand` 启动，并尽量把管道编码设为 UTF-8。标准输出和标准错误里的 CLIXML、`_xHHHH_` 转义和 ANSI 颜色会收成可读文本。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.9.27...HEAD
[0.9.27]: https://github.com/MCapricorns/mycode/compare/v0.9.26...v0.9.27
[0.9.26]: https://github.com/MCapricorns/mycode/compare/v0.9.25...v0.9.26
[0.9.25]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.25
