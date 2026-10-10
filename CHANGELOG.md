# Changelog

0.9.25 是第一次公开发布（init 第一版）。日期为发布日（UTC）。

`main` 上四个平台构建成功后都会发布新的 GitHub Release 和新标签（四个平台的 zip 与 `.sha256`：Windows x64、Windows ARM64、macOS Apple Silicon、Linux x86_64）。说明先用本文件里该版本的条目，再附上自上一个标签以来合并的 pull request、这些 PR 关闭的 issue，以及比较链接。写回 main 使用 GITHUB_TOKEN，release-plan 会跳过 github-actions[bot]，避免同一次发布把补丁号再加一次。工作区版本如果已经有标签，发布计划会把补丁号加一，把 `## [Unreleased]` 的内容写到新版本下（没有内容就写一句固定说明），并更新 `Cargo.toml` 与 `Cargo.lock`。这次提交先放在临时引用上，四个平台都从该提交构建成功后，先创建标签并上传压缩包，再把版本写回 `main`（能快进就快进，否则把版本提交重放到当前 `main` 上，不强制推送）。不会因为旧版本的压缩包已经齐就跳过。pull request 不发版。压缩包未签名。

## [Unreleased]

### Changed

- 进程工具不再读取 `MYCODE_SHELL`，也不再使用设置里的 `tools.shell`。解释器在进程启动时自动选定，工具名、参数说明和系统提示在整个会话里保持一致；子代理用同一个工具。旧的 `tools.shell` 仍能被读入，启动时忽略并在写回设置时去掉。若环境变量 `MYCODE_SHELL` 有值，进程提示一次后忽略。
- Linux / macOS 使用 `$SHELL` 里的 bash、zsh 或 sh。否则 macOS 依次试 zsh、bash、sh，其它 Unix 依次试 bash、zsh、sh。这些系统不选择 pwsh 或 cmd。工具名是 `bash`、`zsh` 或 `sh`，路径分隔符是 `/`。
- Windows 只自动选择 PowerShell：先是 PowerShell 7（`pwsh`，常规安装优先于 `WindowsApps` 目录下的执行别名），然后是 Windows PowerShell 5.1，最后才是 `cmd.exe`。不再把 Git Bash、MSYS2、Cygwin、`WindowsApps\bash.exe` 或 WSL 的 `System32\bash.exe` 当成 shell。商店别名按路径形状识别，不再靠 64 字节大小。

### Fixed

- 取消或超时会清掉整棵进程树。`setsid` 换了会话的子进程，以及子 shell 退出后被重新挂接、但仍留在原进程组里的后台进程，都会被结束。Linux 上 shell 会成为子进程回收者，这样脱离会话的孙子进程仍留在这棵树上。Windows 仍用不允许脱离的 Job Object。
- 后台子进程不再把工具拖到管道关闭。`sleep 20 & echo bg` 在 shell 退出时返回输出，而不是等 `sleep` 结束。
- `mycode-desktop --version` 和 `--help` 打印后退出，不再打开窗口。Linux 上 `DISPLAY` 和 `WAYLAND_DISPLAY` 都未设置时，或窗口表面创建失败（`Failed to create surface`）时，打印说明并以状态码 1 退出，不再以 101 崩溃。

## [0.10.2] - 2026-10-10

### Changed

- GitHub Release 的说明自动包含本文件里该版本的条目，以及自上一个标签以来合并的 pull request、这些 PR 关闭的 issue 和比较链接。每次发版生成，不用手写。
- 写回 `main` 的版本提交使用 `GITHUB_TOKEN`，`release-plan` 忽略 `github-actions[bot]` 的推送，避免一次发布把补丁号连加两次。pull request 和 `main` 仍共用 `ci.yml`。

### Fixed

- macOS 的 `Build release binary` 在编译前会先访问 `index.crates.io`，并且只对域名解析、下载和 checksum 失败重试，编译错误不重试。`797dfde` 那次发布失败是因为 cargo 在大约 80 秒内耗尽对 `index.crates.io` 的解析重试（`Could not resolve host` / `download of config.json failed`），构建步骤没有外层重试，macOS 一失败就跳过了 Release。

## [0.10.1] - 2026-10-10

### Changed

- 进程工具按启动时解析到的解释器只注册一个：Windows 上优先 PowerShell 7（`pwsh`），其次 Windows PowerShell 5.1（`powershell.exe`），工具名是 `powershell`，描述和参数按该版本的 PowerShell 来写。Git Bash 仅在两种 PowerShell 都没有，或设置 / `MYCODE_SHELL` 指定时使用，工具名是 `bash`。都不在时运行时才用 `cmd`，且不写入设置。不自动选择 WSL。Linux / macOS 仍是 `bash`。命令按该解释器原样执行，不再把 bash 翻译成 PowerShell。
- 系统提示词的 `<environment>` / `<shell>` 写明当前 OS 和这一个 shell 的语法（PowerShell 5.1 不用 `&&` / `||`，路径分隔符和引号跟解释器走）。子代理使用同一个工具和同一套说明；白名单里的 `shell`、`bash`、`powershell`、`cmd` 不会再变成第二个 shell。scout 仍然不注册进程工具。
- 四个平台的构建在 crates.io 域名解析或下载失败时会重试，编译错误不重试。macOS 上一次发布（`797dfde`）就是卡在 `index.crates.io` 解析失败。pull request 和 `main` 共用 `ci.yml`：都会构建四个平台，只有推到 `main` 且版本还没有标签时才打标签并上传 Release。`release.yml` 已删除。

## [0.9.28] - 2026-10-10

### Changed

- 四个平台构建通过的 `main` 推送自动发布。

## [0.9.27] - 2026-10-10

### Added

- 系统提示词新增 `<environment>` 块：OS 与架构、会话 cwd、当前解析到的 shell（`script_shell_line()`，与脚本模式同一条解析链：设置 → 检测 → Windows 的 cmd 兜底）。子代理提示词同样注入一份，cwd 是各自的运行目录。模型不再需要靠报错猜平台或 shell 方言。

### Changed

- 系统提示词不再出现两句身份声明：工具清单前的「You are MYCode Agent…」改为纯粹的工具契约（「Complete the task with the tools listed below. Do not invent tools.」），身份由会话或子代理提示词开头各声明一次。
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

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.10.2...HEAD
[0.10.2]: https://github.com/MCapricorns/mycode/releases/tag/v0.10.2
[0.10.1]: https://github.com/MCapricorns/mycode/compare/v0.9.28...v0.10.1
[0.9.28]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.28
[0.9.27]: https://github.com/MCapricorns/mycode/compare/v0.9.26...v0.9.27
[0.9.26]: https://github.com/MCapricorns/mycode/compare/v0.9.25...v0.9.26
[0.9.25]: https://github.com/MCapricorns/mycode/releases/tag/v0.9.25
