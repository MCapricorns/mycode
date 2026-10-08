# 文档

这里说明 mycode 怎么分成模块、每个模块为什么这样切、以及它具体干什么。

## 先读哪篇

| 文档 | 回答的问题 |
| --- | --- |
| [架构](architecture.md) | 七个 crate 怎么分工，一条用户消息怎么走完，数据放在哪，快 / 稳 / 省各靠什么 |
| [mycode-core](core.md) | 大家共用的对话类型和模型端口 |
| [mycode-config](config.md) | 家目录、设置、密钥、会话旁路文档怎么落盘 |
| [mycode-providers](providers.md) | 三种协议、目录、OAuth、流式传输 |
| [mycode-tools](tools.md) | 工具契约，以及文件、搜索、进程工具怎么做 |
| [mycode-agent](agent.md) | 回合循环，以及可崩溃恢复的会话账本 |
| [mycode-app](app.md) | 无界面的应用核：桥接、回合装配、MCP、网页、更新 |
| [mycode-desktop](desktop.md) | GPUI 窗口：纯状态机、渲染、和工作区 |

crate 的模块注释指向对应文档。改行为时先改代码，再改这里。
