# 工具系统

对应 crate：`mycode-tools`。单源 schema（schemars）同时服务 LLM spec 与运行时校验。

## 1. Registry

`ToolRegistry` 按名字注册 `Arc<dyn ToolDyn>`。内置文件工具名称固定。MCP 工具不得覆盖已有名字。

每个 turn 注册：

| 来源 | 工具 | 何时 |
| --- | --- | --- |
| builtins | `read` `write` `edit` `find` `grep` `shell` `exec` | 总是 |
| hosts | `ask_user` | 总是 |
| web | `web_search` `fetch_content` | 总是（无 key 时调用失败，提示去设置页粘贴） |
| subagent | `task` | 至少一个角色启用 |
| MCP | 各服务器 `tools/list` | 连接成功且不阴影内置名 |

`build_system_prompt` 列出 registry 中每一项及其 `prompt_snippet`。

## 2. Web

`mycode-app` 的 bounded 客户端：Querit / AnySearch / custom。Key 存 `secrets.json` 的 `web-<id>`，或环境变量 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY`。传输层 `bearer_auth`，用户只贴 key。

## 3. Subagent

`task` → `BridgeTaskHost`。角色目录：scout / artisan / steward / sentinel（可被 `~/.mycode` 或项目 `.mycode/agents/*.md` 覆盖）。设置页配置启用、模型路由、thinking。隔离与工具白名单见 `mycode-config::subagents`。

## 4. MCP

stdio 或 Streamable-HTTP。设置页可粘贴 JSON 配置解析。HTTP 凭证头 Bearer 或 `x-api-key`。连接失败跳过该服务器，不让 turn 失败。

## 5. 文件与进程

- `write`/`edit` 前 `checkpoint_file`。
- Windows：PATH 搜索跳过无法打开或 0 字节的 Store App Execution Alias（os error 1920）。
- `shell` 发现 pwsh 时同样跳过空镜像；侦查顺序为 pwsh，回退 Git bash，不使用 Windows PowerShell 5.1 与 cmd。
