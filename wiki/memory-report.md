# 性能记录

这里记录可重复的测量结果。不同版本、构建方式或任务的数字不能直接相减；历史结果保留原来的条件，本次没有重新测量的项目会明确标出。

## CodeMode 并发耗时

2026-11-02，在 Windows x64、rustc 1.99.1、debug 构建下，对比同一版本中的同步 call 和异步 callAsync。任务是 8 个互不依赖的工具调用，每个工具等待 50 ms 后返回。每种方式预热一次，再运行五次，计时包含脚本启动、调度、工具等待和结果返回。

| 调用方式 | 中位数 | 五次范围 |
| --- | --- | --- |
| 逐个同步 call | 498.08 ms | 492.70–501.88 ms |
| callAsync + Promise.all | 61.72 ms | 60.63–63.56 ms |

这组模拟 I/O 任务的耗时约为原来的 1/8.07。Windows 定时器和调度会让实际等待略长于配置的 50 ms。这个结果说明独立调用已能同时等待；真实收益取决于文件、服务和工具延迟，不代表模型生成、单个工具或整个应用都快了 8 倍。

复现命令：

```sh
cargo run -p mycode --example code_mode_latency --locked
```

[测量程序](../crates/mycode/examples/code_mode_latency.rs) 使用生产 RunCodeTool 和调度器，唯一的模拟部分是工具里的 50 ms 等待。写入、shell 和其他独占工具仍按顺序运行，不应期待同样的并发收益。

## 历史记录：程序体积

以下是之前两包重构时记录的数据，本次未重跑 release 体积测试。

基线 fe60708 的八个内部 package 合并成 mycode 与 mycode-desktop。外部依赖版本、来源和 checksum 保持一致，共 896 个 registry/git package 记录。

| 结构 | 重构前 | 重构后 |
| --- | --- | --- |
| workspace package | 8 | 2 |
| 内部 package 之间的依赖 | 7 | 1 |
| manifest 中的依赖声明，含 target/dev/build | 98 | 51 |

在同一台 Windows x64 机器上，使用 rustc 1.99.1、同一 lockfile 和 release profile：opt-level=3、fat LTO、codegen-units=1。两边都执行 cargo build --release -p mycode-desktop --locked，没有额外压缩或裁剪功能。

| mycode-desktop.exe | 字节 |
| --- | --- |
| fe60708 | 43,622,400 |
| 两包重构后 | 42,347,008 |
| 减少 | 1,275,392，约 2.92% |

依赖声明减少让项目更容易维护，不代表同一依赖原来被重复链接了很多次。上面的文件体积也不能换算成内存、启动速度或其他平台的改善。

## 历史记录：大段回复的显示处理

之前的实现每帧会反复统计整段文字的字符数。改动后只扫描到所需阈值，仍保留普通帧的 80 字符 / 16 个片段预算，并在积压、失败或取消时及时刷新。

下面是同一输入、release 构建、预热后五轮测量的中位数，本次未重跑。

| 输入 | 改动前 | 改动后 |
| --- | --- | --- |
| 32 个小片段 | 531 ns / 次 | 550 ns / 次 |
| 单个 64 KiB ASCII 片段 | 4,405 ns / 次 | 112 ns / 次 |
| 单个约 1 MiB UTF-8 片段 | 74,174 ns / 次 | 202 ns / 次 |

小片段没有测出改善。大段文字省去了重复扫描，但这只是帧选择与队列重组的耗时，不包含网络、模型生成或 GPUI 绘制。

```sh
cargo run -p mycode --release --example stream_frames --locked
```

## 内存限制各管哪一部分

| 内容 | 当前上限 | 源码，位于 crates/mycode/src |
| --- | --- | --- |
| 一条消息的文本附件 | 8 个；单文件 256 KiB；总计 512 KiB | core/attachments.rs |
| 一条消息的图片附件 | 4 张；单张原始数据 3,750,000 字节；base64 总计 6 MiB | core/attachments.rs |
| 编码后的模型请求 JSON | 8 MiB | core/provider.rs |
| 一条模型流事件 | 1 MiB；传递队列容量 16 | core/provider/stream.rs |
| 一个 CodeMode 脚本 | 堆 32 MiB；栈 256 KiB；源码 4 MiB；256 次工具调用；10,000 个 Promise 任务 | tools/code_mode/engine.rs、engine/promises.rs、cell.rs |
| CodeMode 执行记录 | 进程内 32 个 | tools/code_mode/cell.rs |
| 子代理运行记录 | 每会话 128 个 run、32 个 workflow | tools/builtin/agent/runtime.rs |
| 子代理摘要 | 24,000 字符 | tools/builtin/agent.rs |
| MCP 消息和工具目录 | 单条消息 1 MiB；每服务器最多 128 个工具 | app/mcp_client.rs |
| MCP 直接返回的文本 | 20 KiB 截取预算，完整内容尝试另存文件 | app/mcp_tools/results.rs |
| HTTP 错误正文 | 64 KiB | providers/transport.rs |
| 提示中的规则文件 | 单文件 64 KiB；总计 96 Ki 个字符 | config/resources.rs |

这些限制只约束各自的数据。应用还持有会话、图片、Markdown 缓存、窗口和系统资源；不能把表里的数字当成整个进程的内存上限。

当前使用 mimalloc。Markdown 缓存复用未变化的解析结果，文件附件与剪贴板图片共用格式检查。本次没有测量进程内存峰值、启动时间或帧率，因此没有这些指标的改善结论。

## 怎样继续测内存

固定项目、模型回复、主题和窗口大小，分别记录空闲、长回复、大量工具输出、图片、切换会话及取消后的进程内存。每组使用相同操作，等待后台任务收尾，再重复多轮。

发现增长时，先检查是否仍有任务运行、结果需要保留或缓存尚未释放，再判断是否泄漏。修改进程回收和文件访问代码时，也要运行对应的取消、权限和兼容性测试。
