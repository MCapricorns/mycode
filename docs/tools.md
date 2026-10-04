# mycode-tools

模型能调用的工具。这里定义 trait、注册表，以及文件、搜索和进程这组内置实现。`ask_user`、`task`、网页和 MCP 的宿主在 `mycode-app`，它们实现同一个 trait，在每个回合注册进去。

```text
模型的 tool_call
    → 按名字取出 ToolDyn
    → 用该工具的 JSON Schema 校验参数
    → 执行
    → ToolResult
         content  回到模型
         details  只给界面（diff、字节数），不进上下文
```

## 设计思路

一份 schema 两处用。`schemars` 从参数结构体生成 JSON Schema，既放进 `ToolSpec` 给模型，也在 `execute_dyn` 里校验。模型看见的和运行时接受的是同一份。

注册表按名字最后写入者生效，规格按名字排序后发给模型，保证请求体稳定。已经注册且参数合法的调用直接执行。未知工具、参数错误、取消和执行错误都变成工具结果里的 `is_error`，循环不因此停掉。没有「先问用户准不准」的回调。

文件发现和内容搜索在进程内完成，不依赖外部的 `fd` 或 `rg`。这样工具机器上没有这些二进制也能用，也少一次进程启动。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `tool` | `Tool` / `ToolDyn`、`ToolResult`、`ToolError` |
| `registry` | 名字 → 工具。`prompt_entries` 给 system prompt 列名字和一句说明 |
| `ctx` | 一次调用的上下文：取消令牌、会话工作目录、额外根、进度流 |
| `roots` | 把模型给的路径锚到工作目录，拒绝逃出根的相对路径 |
| `stream` | 进度事件，然后恰好一个终止事件。多个克隆里只有一个能写下终止 |
| `builtin` | 下面这七个内置工具 |

## 内置工具

| 工具 | 行为 |
| --- | --- |
| `read` | 带行号读文件，输出有字节上限 |
| `write` | 创建或整文件替换 |
| `edit` | 精确字符串替换。小幅漂移用模糊匹配；`ast` 用 gpui-kit 注册的 Tree-sitter 语法对一下 |
| `find` | 按 glob 找文件，限制在搜索根内 |
| `grep` | 进程内的内容搜索，支持 include / exclude |
| `exec` | 显式参数运行一个可加载映像（PE / ELF / Mach-O）。不经过 shell |
| `shell` | 交给用户的 shell，用于管道、重定向和脚本 |

`shell` 与 `exec` 共用启动路径：钉住程序身份、限制参数、截断约 50 KiB 输出、超时和取消时终止并回收整棵进程树。丢掉 future 也会把清理交出去，避免留下孤儿进程。非零退出码是 `is_error` 结果，不是循环故障。

Windows 上 PATH 搜索会跳过打不开或 0 字节的商店执行别名。shell 侦查顺序是 PowerShell 7（`pwsh`），否则 Git bash，不用 Windows PowerShell 5.1 和 `cmd`。

`exec` 的实现按平台拆开（Windows x64、Linux x86_64 glibc、macOS Apple Silicon），摘要复查和参数组装共用。Windows ARM64 没有单独的启动实现。其它 Unix 目标不支持启动。

搜索有独立的预算：扫描字节、截止时间、错误样本条数。到顶就停并说明停因，而不是把目录树读完。

## 宿主工具（定义在本 crate，装配在 app）

| 工具 | 宿主提供什么 |
| --- | --- |
| `ask_user` | 把问题送到界面，等用户答完再继续 |
| `task` | 按角色再跑一个有白名单的子循环 |
| `web_search` / `fetch_content` | 有界 HTTP。没钥匙时调用失败，并提示去设置页粘贴 |

MCP 不把远端工具名注册进这张表。应用层注册 `search_tool` 和 `use_tool`：先取 schema，再按 schema 调用。这样远端工具不会盖住 `read` 或 `shell`。

## 和文件快照的交界

本 crate 在改文件前调用 `mycode-config` 的 `checkpoint_file`。快照失败就跳过，工具仍会写。回滚由应用层按会话清单把每个路径恢复到最早一份快照。

## 不放在这里的东西

本 crate 不知道哪个会话正在打开，也不保存密钥。工作目录、额外文件夹、网页钥匙和 MCP 连接都由 `ToolCtx` 和宿主在回合开始时放进来。
