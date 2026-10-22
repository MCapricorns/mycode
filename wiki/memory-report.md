# 内存占用研究报告

> 2026-02 静态审查与构建基准。范围：八个 crate、约 115k 行 Rust；方法：代码审查 + `cargo build` 产物测量，不含运行时 RSS 采样。

## 结论摘要

1. **分配器已全覆盖。** mimalloc 的 `#[global_allocator]` 只在 `mycode-core` 声明一次（整个链接图只允许一处）；线性链保证每个 crate 都传递链接 `mycode-core`，因此全部二进制与全部测试目标统一走 mimalloc。VC-LTL 同理，`mycode-config` 里重复的声明已删除。验证：`cargo tree -i mimalloc` 从 core 一路覆盖到 desktop 八层。
2. **没有无界增长点。** 缓存、队列、载荷全部有常量上限（见下表）；驻留会话数、MCP 工具数、流式字符数等都封顶。
3. **热路径无深拷贝。** 对话历史走 `SharedMessage = Arc<Message>`；审查发现的 `message.clone()` 均为每回合一次的所有权转移（返回值需要所有权），不在逐 delta 路径上。
4. **基准：** release 二进制 38.11 MB（fat LTO、单 codegen unit），dev 69.78 MB；依赖图去重 427 个包；release 全量构建 6 分 40 秒。

## 分配器与链接策略

| 项 | 声明位置 | 生效范围 |
| --- | --- | --- |
| `#[global_allocator]` mimalloc | `mycode-core/src/lib.rs` | 链上全部二进制与测试目标 |
| VC-LTL（CRT 重定向） | `mycode-core`（Windows target） | 同上；`mycode-config` 的重复声明已删 |

mimalloc 的收益场景与本工作负载匹配：大量短生命周期小字符串（消息块、SSE 增量、JSON 序列化），页对齐段与每线程 free list 能压住默认分配器逐步累积的驻留 RSS。

## 内存上限清单（既有防护）

| 位置 | 上限 |
| --- | --- |
| agent session 驻留会话 | `MAX_RESIDENT_SESSIONS = 8` |
| 会话事件单条载荷 | 8 MiB（usage 64 KiB） |
| 会话落盘总量 | 512 MiB / 会话 |
| MCP 消息 / 工具数 | 1 MiB / 128 个 |
| 流式回复字符 | 256 KiB |
| 请求编码体积（core） | 8 MiB |
| 附件（core） | 图片 4 张 / 3.75 MB，单图 base64 5 MiB，文件 8 个 / 256 KiB |
| 搜索（@ 提及） | 访问 8192 / 收集 64 / 匹配 8 |
| 导出捆绑 | 48 MiB |
| 更新资产 / 二进制 | 512 MiB |
| 压缩转录 | 80k token / 24k 字符摘要 |
| 运行时准入 | live 资源 4096 / 未决操作 1024 |

## 热路径审查

- `agent/src/turn.rs`：`StreamEvent::Done` 把 `AssistantMessage` 克隆一次包进 `Arc`（返回值需要所有权），每回合一次，非逐 delta；错误路径同理。消除它需要把返回类型改成 `SharedMessage`，波及调用方，收益小，暂缓。
- `providers` SSE：传输层错误体读取有硬上限（`an_oversized_error_body_stops_reading_at_the_cap` 测试锁定）；增量用事件直通，不做全文缓冲拼接。
- `app/src/ledger.rs` 历史读取为一次性 `Vec`（分页由上层限制），无累积缓存。
- 全库未发现无上界 `HashMap`/`Vec` 缓存；缓存类结构（catalog、代理探测结果）均为有界或按请求重建。

## 基准数据

| 指标 | 值 |
| --- | --- |
| 二进制（release，fat LTO） | 38.11 MB |
| 二进制（dev） | 69.78 MB |
| release 全量构建 | 6 分 40 秒（含 LTO） |
| 依赖图去重包数 | 427 |
| 工作区代码量 | 115,487 行（316 个 .rs 文件） |

release 产物分 crate（`.rlib`）：app 13.72 MB、desktop 10.14 MB、tools 9.21 MB、config 4.29 MB、agent 3.63 MB、frontend 3.27 MB、providers 2.76 MB、core 2.36 MB。

## 本次已实施

- 线性链重构使 `mycode-config`（原唯一不链接 core 的 crate）及其测试目标进入 mimalloc 覆盖。
- 删除三份重复的 `unix_now` 包装（config / tools / agent），统一走 core 的实现。
- 删除 `mycode-config` 重复的 VC-LTL 声明。

## 后续可选

- `Agent::prompt` 返回 `SharedMessage` 以消除每回合一次的 `AssistantMessage` 深拷贝。
- 启动后 RSS 采样脚本（本轮未含运行时测量）。
