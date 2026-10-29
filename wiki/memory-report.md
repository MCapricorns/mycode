# 内存占用研究报告

本文件保留早期内存审查的线索，并以当前源码重新界定可证实的范围。原报告自述为“2026-02 静态审查与构建基准”，但没有记录可重现的修订号、工具链、机器和完整测量日志。本次仅核对实现与测试文本，没有重新构建或采集运行时 RSS；下文历史数字不能当作当前版本的性能结论。

## 结论摘要

当前可以确认：工作区通过 `mycode-core` 的单一 `#[global_allocator]` 使用 mimalloc，若干载荷、队列、执行单元及会话有局部上限。局部限额不等于进程总内存上限，也不足以证明“没有无界增长点”“全部缓存均封顶”或“热路径没有深拷贝”。这些旧结论撤回为待测问题。

CodeMode 的 JavaScript 堆、图片队列、Rust 侧执行日志、宿主请求、工具临时数据、子代理结果和进程资源具有不同生命周期，不能把 32 MiB 堆当成整个执行单元的内存预算。保留记录的数量限制也不限制已经写到磁盘的产物或保留工作树的总量。

## 分配器与链接策略

| 项目 | 当前源码依据 | 能说明的范围 |
| --- | --- | --- |
| mimalloc | `crates/mycode-core/src/lib.rs` 的单一全局分配器声明 | 链接该核心包的 Rust 目标使用此分配器；不能由此推断 RSS 一定下降 |
| 工作区依赖链 | 各包的 `Cargo.toml`：`desktop → frontend → app → agent → tools → providers → config → core` | 八个工作区包可达同一核心包；本次未逐一构建全部目标 |
| VC-LTL | `crates/mycode-core/Cargo.toml` 中的 Windows MSVC 目标平台依赖 | Windows 链接策略；不是内存泄漏或性能验证 |

分配器的实际效果受分配形状、线程、原生库及操作系统回收影响。要评价驻留内存，仍需在相同工作负载下测量 RSS、峰值和回落，而非比较二进制文件大小。

## 内存上限清单（既有防护）

以下是本次能从常量和准入逻辑核实的局部限制，单位按实际统计对象区分。它们不是完整的全库上限清单。

| 对象 | 当前限制 | 依据路径 |
| --- | --- | --- |
| 驻留会话账本 | 8 个 | `crates/mycode-agent/src/session/actor.rs` |
| 单条会话事件正文 | 8 MiB；用量事件（`usage`）为 64 KiB | `crates/mycode-agent/src/session/dto.rs` |
| 会话持久化计费量 | 512 MiB / 会话，非驻留内存预算 | `crates/mycode-agent/src/session/store.rs` |
| 会话运行时准入 | 存活资源 4,096；未决操作 1,024 | `crates/mycode-agent/src/session/runtime/admission.rs` |
| 核心包请求编码与流事件 | 请求编码 8 MiB；单事件编码 1 MiB；事件流容量 16 | `crates/mycode-core/src/provider.rs`、`crates/mycode-core/src/provider/stream.rs` |
| 用户图片附件 | 4 张；单张原始数据 3,750,000 字节；单张 base64 5 MiB；合计 base64 6 MiB | `crates/mycode-core/src/attachments.rs` |
| 用户文本附件 | 8 个；单文件 256 KiB；合计 512 KiB | `crates/mycode-core/src/attachments.rs` |
| CodeMode 求值 | 堆 32 MiB；原生栈 256 KiB；源码 4 MiB；256 次宿主调用；100 次中断轮询 | `crates/mycode-tools/src/code_mode/engine.rs`、`crates/mycode-tools/src/code_mode/cell.rs` |
| CodeMode 文本与图片 | 默认累计文本 32 KiB，可设 1 KiB–4 MiB；待交付图片 4 张 / 6 MiB base64 | `crates/mycode-tools/src/code_mode.rs`、`crates/mycode-tools/src/code_mode/cell.rs` |
| CodeMode 保留表与实时进度 | 全进程 32 个执行单元；每个执行单元的广播通道容量 256 | `crates/mycode-tools/src/code_mode/cell.rs` |
| 子代理保留记录 | 每个会话 128 条运行记录、32 个流程；摘要结果上限 24,000 个字符；每个流程最多 16 次显式返工 | `crates/mycode-tools/src/builtin/agent.rs`、`crates/mycode-tools/src/builtin/agent/runtime.rs`、`crates/mycode-tools/src/builtin/agent/rework.rs` |
| MCP 载荷与目录 | 单条消息 1 MiB；每服务器工具列表最多 128 项，并非全进程 128 项 | `crates/mycode-app/src/mcp_client.rs` |
| MCP 结果内联 | 以 20 KiB 为截取预算，另附存储或失败提示；完整文本尝试写文件 | `crates/mycode-app/src/mcp_tools/results.rs` |
| HTTP 错误响应体 | 最多读取 64 KiB | `crates/mycode-providers/src/transport.rs` |
| 提示资源 | 每文件 64 KiB；总贡献 96 Ki 个字符；技能元数据缓存 512 项 | `crates/mycode-config/src/resources.rs` |

## 热路径审查

`SharedMessage = Arc<Message>` 允许历史消息共享所有权，但结构中存在 `Arc` 不代表构造、投影、序列化和文本累积不复制数据。判断深拷贝成本需要沿实际调用路径分析并做分配或峰值测量。

CodeMode 的输出上限只约束累计输出。宿主请求使用 Rust 的无界通道，执行日志为增长的 `Vec`；`log` / `yield_control` 不消耗 256 次 `call` 预算。虚拟机轮询最终限制脚本求值，但没有给这些 Rust 容器各自设定同样的字节上限，因此不能声称所有中间缓冲都已封顶。

MCP 结果在内联截断之前已经形成文本，完整产物又有独立磁盘生命周期。每个会话的运行记录限额也不能推出 `CoreState.agent_runtimes` 所有会话的总体固定预算。当前前端的近期会话显示缓存与后端运行时表是不同结构。

相关回归文件覆盖取消、图片准入、输出截断与运行记录保留策略，能支持这些局部行为；本次未执行性能基准或全工作区测试，也没有得出“零泄漏”的结论。

## 基准数据

以下只转录旧报告的历史记录，不作为本次测量结果。原报告未提供对应修订号、机器、工具链与命令日志，无法判断它们是否可复现或适用于现在的 QuickJS-NG、流程和界面实现。

| 历史指标 | 旧报告记载 |
| --- | --- |
| 发布构建（`release`）二进制 | 38.11 MB，标注完整链接时优化（`fat LTO`） |
| 开发构建（`dev`）二进制 | 69.78 MB |
| 发布构建（`release`）全量构建 | 6 分 40 秒，标注含链接时优化（LTO） |
| 依赖去重包数 | 427 |
| 工作区代码量 | 115,487 行、316 个 Rust 文件 |
| 各包 `.rlib` | `app` 13.72 MB、`desktop` 10.14 MB、`tools` 9.21 MB、`config` 4.29 MB、`agent` 3.63 MB、`frontend` 3.27 MB、`providers` 2.76 MB、`core` 2.36 MB |

磁盘产物大小和构建耗时不能替代运行时 RSS、堆峰值、线程或子进程占用数据。不要把表中的旧数据转写为“当前版本已验证”。

## 本次已实施

本次只重写文档：纠正图片原始数据与 base64 编码和文本单文件/合计限额，补充 CodeMode 与子代理的局部预算，撤回绝对内存结论，并标记历史数字的来源缺口。没有修改分配器、后端或测试。

旧报告曾把依赖链调整、删除重复 `unix_now` 和重复 VC-LTL 声明列为“本次实施”。当前源码只能核对最终声明，不能据此确定这些改动的时间、提交或当时检查结果；此处不把它们继续表述为本次完成的工程工作。

## 后续可选

若需要当前性能结论，应先固定修订号、操作系统、CPU、工具链与构建配置，再记录启动空闲、长对话、MCP 连接、交还控制权与等待收集、取消及多轮流程的 RSS 和峰值。结束后继续采样，观察执行单元淘汰、子进程回收和保留记录的影响；磁盘产物、工作树与驻留内存分别统计。

可参考[代码编排的资源与取消约定](architecture/code-mode-engine.md#内部执行与资源上限)和[子代理生命周期](architecture/runtime.md#子代理编排)，选择可复现的操作序列。没有这些运行数据前，保留“局部限制已核对、整体内存表现未测”的结论。
