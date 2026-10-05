# 架构

mycode 是跑在本机的桌面编码代理（Windows x64、Windows ARM64、macOS Apple Silicon）。对话、工具调用和会话都在你的机器上；模型请求发到你自己的 API。界面不读密钥、不写文件、不发网络请求。发布包只有这三个平台；Ubuntu 仍参与编译检查。

## 七个 crate

依赖只向下。上面的 crate 可以调用下面的，下面的不知道上面的存在。

```text
mycode-desktop     GPUI 窗口。渲染，并把动作交给桥
        │
mycode-app         应用核。会话、回合、工具宿主、MCP、网页、更新
        │
        ├── mycode-agent        回合循环 + 会话账本
        ├── mycode-tools        工具 trait、注册表、内置文件/搜索/进程工具
        └── mycode-providers    三协议适配、模型目录、OAuth
                │
                └── mycode-config   家目录与严格 JSON 文档
                        │
                        └── mycode-core   消息、事件、工具规格、Provider 端口
```

| Crate | 干什么 | 设计思路 |
| --- | --- | --- |
| `mycode-core` | 共享词汇 | 叶子 crate。类型能序列化，能原样穿过事件和请求。没有协议、没有磁盘、没有策略 |
| `mycode-config` | 拥有的文件 | 路径只在词法上拼接，不跟随符号链接。文档有版本、种类、大小上限和 revision CAS |
| `mycode-providers` | 模型出口 | 厂商差异是 `settings.json` 里的数据。密钥由调用方传入，不进设置 |
| `mycode-tools` | 工具实现 | 一份 JSON Schema 同时给模型和运行时。搜索和读文件在进程内完成 |
| `mycode-agent` | 循环和账本 | 循环可以没有磁盘。账本是另一层，用期望头把一次写入做成可恢复的提交 |
| `mycode-app` | 把上面拼成产品 | 前端只看见命令、回复和事件。回合在这里装配 provider、工具和压缩 |
| `mycode-desktop` | 窗口 | 视图模型是纯函数。GPUI 只画状态、收集点击 |

工作区成员只有这七个。配置、密钥、界面状态、会话和角色都在 `mycode-config` 与 `mycode-agent` 里，没有独立的 web / mcp crate。

## 一条消息怎么走完

```text
输入框
  → DesktopAction（纯 reducer，先改界面状态）
  → BridgeCommand::SendMessage（先把用户消息写入账本）
  → BridgeCommand::ChatTurn
       读 settings + secrets，解析 endpoint
       注册本回合工具，拼 system prompt
       Agent::prompt
         压缩钩子（失败则仍发全文）
         provider 流式事件 → BridgeEvent → 界面轮询
         工具调用 → 结果写回历史，直到模型停止
       完成的助手消息追加进账本
```

取消只作用于这一回合的子 token。流到一半的助手消息不进历史。工具失败变成 `is_error` 结果，循环继续，模型能看见失败原因。

## 快、稳、省

这三条是实现时的取舍，不是三套互相独立的子系统。

**快。** 热路径少绕路。`grep` / `find` 在进程内搜索，不启动外部 `rg` 或 `fd`。HTTP 用操作系统 TLS。界面在事件到达时被唤醒，核心线程和界面互不阻塞。改动面板跟着文件夹的文件系统事件读 `git status`，用只读方式，不按固定间隔轮询。发布构建用 fat LTO 和单个 codegen unit，换的是链接时间。

**稳。** 能弄丢用户数据的写都走同一套约束：拥有的路径、不跟随链接、带锁的原子替换、revision 比较交换。会话以 `manifest.json` 为提交权威；日志尾部超出已提交长度的部分在恢复时丢掉，已提交前缀里的摘要或结构错误则整段拒绝。删除会话先取消回合和子代理，再赶出内存账本，然后删目录。单个坏会话出现在列表里供删除，不让整个侧栏失败。压缩写检查点失败时继续发送完整历史。`write` / `edit` 之前对文件做内容寻址快照，回滚恢复到会话第一次改动之前。

**省。** 上限写在类型旁边，而不是靠调用方记得。设置、密钥、单条事件、工具输出、压缩摘要、目录缓存都有字节或条数上限。压缩只缩短发往模型的历史，不重写账本，失败也不多打一轮。模型目录把一份快照编进二进制，家目录里再缓存一份，刷新用条件请求。开发档只保留行号表，避免多 GiB 的调试信息。

## 家目录

`MYCODE_HOME` 有值时，它就是根。否则用 `~/.mycode`（Windows 在没有 `HOME` 时用 `USERPROFILE`）。解析不访问磁盘，也不按当前工作目录拼接。

```text
~/.mycode/
├─ settings.json          提供商、MCP、网页、外观、子代理。不含密钥
├─ secrets.json           API 密钥与网页 / MCP 凭证
├─ ui.json                工作区、文件夹、会话归属、最近项目。可丢弃
├─ catalog-cache.json     models.dev 目录缓存
├─ sessions/<id>/         账本、压缩检查点
├─ checkpoints/<id>/      write/edit 之前的文件快照
└─ scratch/               没有绑定文件夹时的工具工作目录
```

界面标题用会话第一条用户消息。空标题时用文件夹名。`ses1-…` 只是内部身份。

## 当前边界

这些是现在代码的真实边界，读模块文档时一起记住。

- 工具在当前用户权限下执行，没有沙箱，也没有每次调用前的许可弹窗。校验过的调用会直接跑。
- `shell` 和 `exec` 会钉住要启动的程序映像并回收进程树。环境变量过滤不是隔离。
- 持续集成跑 `rustfmt` 和 `clippy -D warnings`，不跑测试套件。
- 会话写入是单写者。generation fence 把正在提交和正在删除排开，提交用期望头 CAS。
- 回合钩子只有两个：请求前压缩，以及工具前观察。
