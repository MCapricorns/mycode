# 性能与资源记录

本页区分结构变化、实测结果和源码中的局部上限。合并 package 不自动等于降低内存占用；只有在相同构建配置或输入下测量的结果才能用于比较。

## 2026-11-02 重构记录

基线为 fe60708。重构将八个内部 package 合并为 mycode 与 mycode-desktop，使用模块保留职责，去掉逐层通配转导出。外部依赖的名称、版本、来源和 checksum 与基线完全一致，共 896 个 registry/git package 记录。

| 结构指标 | 基线 | 重构后 |
| --- | --- | --- |
| workspace package | 8 | 2 |
| 内部 package 依赖边 | 7 | 1 |
| package manifest 依赖声明，含 target/dev/build | 98 | 51 |

依赖声明减少反映维护成本，不能解释为 Cargo 原来把同一个依赖复制链接了 47 次。Cargo 会统一可兼容的包版本与 features；仍然存在的不同版本来自实际外部依赖图。

## Windows x64 release 体积

同一台 Windows x64 机器，rustc 1.99.1，使用 lockfile 和工作区 release profile：opt-level=3、fat LTO、codegen-units=1。未使用 UPX、额外 strip 或功能裁剪。基线来自独立 worktree，两侧都执行 cargo build --release -p mycode-desktop --locked。

| 产物 | 字节 |
| --- | --- |
| fe60708 的 mycode-desktop.exe | 43,622,400 |
| 两包结构的 mycode-desktop.exe | 42,347,008 |
| 减少 | 1,275,392，约 2.92% |

这个结果只代表该工具链与 Windows x64 release binary，不能外推到 macOS、Windows ARM64、安装包压缩率、启动时间或 RSS。若修改编译选项或外部依赖，应重新建立可比基线。

## 流式帧选择

旧实现每帧为追赶阈值统计全部 delta 的 Unicode 字符，再在选择帧时重复计数。新实现只扫描到阈值，短片段仍遵循原来的 80 字符 / 16 delta 预算；较大积压仍追赶，失败和取消仍立即刷新。

基准位于 crates/mycode/examples/stream_frames.rs。它直接调用生产 split_stream_frame，反复重用同一队列，测量包括帧选择与重组，不包括网络、模型延迟或 GPUI 绘制。两侧使用同一 fixture、同一 release profile，预热后运行五轮。

| 输入 | 基线中位数 | 重构后中位数 |
| --- | --- | --- |
| 32 个小 delta | 531 ns / 次 | 550 ns / 次 |
| 单个 64 KiB ASCII delta | 4,405 ns / 次 | 112 ns / 次 |
| 单个约 1 MiB UTF-8 delta | 74,174 ns / 次 | 202 ns / 次 |

小片段本次中位数增加 19 ns，未测得改善。优化针对大段文本的重复扫描；所有轮次选择的事件数一致。

```sh
cargo run -p mycode --release --example stream_frames --locked
```

微基准只用于检验这段字符扫描的成本；短文本结果容易受调度与分配器噪声影响，不据此宣称整个桌面界面快了同样倍数。Unicode 阈值、工具起止顺序、取消刷新和积压追赶由回归测试验证。

## 分配与缓存

mycode/src/lib.rs 唯一声明 mimalloc 全局分配器，桌面与测试目标复用。Markdown cache 命中时共享 Rc block 数组，更新文本使解析缓存失效。附件路径读取与剪贴板路径读取复用同一个格式和大小检查。

这些变更减少了重复实现或特定路径的工作量。本次没有进程级 RSS、长会话峰值内存、启动时间或 UI 帧率对比，因此不作相关改善结论。

## 局部资源上限

| 资源 | 当前限制 | 实现 |
| --- | --- | --- |
| 用户文本附件 | 8 个；单文件 256 KiB；合计 512 KiB | core/attachments.rs |
| 用户图片附件 | 4 张；单图原始 3,750,000 字节；base64 合计 6 MiB | core/attachments.rs |
| 请求 JSON 编码 | 8 MiB | core/provider.rs |
| 单条 provider stream event | 1 MiB；channel 容量 16 | core/provider/stream.rs |
| CodeMode | 堆 32 MiB；栈 256 KiB；源码 4 MiB；256 次 call | tools/code_mode/engine.rs、cell.rs |
| CodeMode 保留表 | 进程内 32 个 cell | tools/code_mode/cell.rs |
| 子代理记录 | 每会话 128 个 run、32 个 workflow | tools/builtin/agent/runtime.rs |
| 子代理摘要答复 | 24,000 字符 | tools/builtin/agent.rs |
| MCP 消息与目录 | 单条 1 MiB；每服务器最多 128 项工具 | app/mcp_client.rs |
| MCP 内联结果 | 20 KiB 截取预算，完整正文尝试存产物文件 | app/mcp_tools/results.rs |
| HTTP 错误响应体 | 最多 64 KiB | providers/transport.rs |
| 提示规则资源 | 每文件 64 KiB；总贡献 96 Ki 字符 | config/resources.rs |

表内源码路径均相对 crates/mycode/src。上限约束不同层次；应用同时可能持有工具结果、provider 缓冲、会话投影、Markdown、图片、GPUI 与系统资源，不能把这些限额相加当作完整内存模型。

## 后续测量方法

测试长期增长时，固定模型回复、项目、主题和窗口大小，分别记录空载、长文本、工具输出、图片、会话切换及取消后的进程私有字节和工作集。对比相同任务序列，等待后台任务收尾，再重复多轮。

若发现增长，先区分活跃任务、可回收缓存、保留会话和泄漏。Windows handle/Job 与受管文件代码存在看似重复但安全策略不同的分支，不能为了降低重复率而合并。
