# 文档

这些文档按 crate 拆分，说明每层负责什么、边界在哪，以及行为改动后需要同步哪些地方。

## 从哪里开始

| 文档 | 回答的问题 |
| --- | --- |
| [architecture.md](architecture.md) | 九个 crate 怎么分层，一条消息怎么走完，数据放在哪，有哪些运行时边界 |
| [core.md](core.md) | 消息、内容块、事件、工具规格和 Provider 端口长什么样 |
| [config.md](config.md) | 家目录、设置、密钥、界面状态、角色和提示资源怎么读写 |
| [providers.md](providers.md) | 线协议、流式传输、思考强度、提示缓存、magpie 网关的模型列表 |
| [tools.md](tools.md) | 内置工具、搜索预算、进程工具、宿主工具和 MCP 的接入方式 |
| [agent.md](agent.md) | 回合循环、取消、工具分发和会话账本 |
| [app.md](app.md) | 桥和线程、回合装配、压缩、子代理、MCP、更新 |
| [desktop.md](desktop.md) | 桌面和终端界面、共享视图模型、和核心的交互节拍 |

`docs/images/` 只放随文档维护的 SVG（banner、logo、架构图、消息流程图），不存界面截图。改行为时先改代码，再同步这里的文档和图中文字。
