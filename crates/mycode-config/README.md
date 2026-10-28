# mycode-config

MYCode 的 home 布局与严格配置文档。会话与压缩检查点都在 `sessions/` 下。

## Home

`MYCODE_HOME` 非空则整棵树迁走；否则 `HOME`（Windows 还可回退 `USERPROFILE`）下的 `.mycode`。

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ AGENTS.md
├─ mcp.json
├─ skills/
├─ agents/<role>.md
├─ sessions.db
├─ sessions/<项目键>/<sess-id>/{<br1-id>.jsonl,payloads/,compaction.json}
└─ scratch/
```

- `settings.json`：magpie 网关绑定（含插件写入的网关密钥）、MCP、subagents、appearance、skills 开关。未知字段或已退役字段（`providers`、`web`、`userAgent`、`usage`、`tools.*`、`subagents.maxConcurrent`/`timeoutSecs`、`appearance.palette`/`fontFamily`）均按文档损坏处理，备份后重置并逐项抢救（salvage）仍合法的字段。Windows 上同用户外部写入带来的安全 ACL 差异会原地收紧为 owner + SYSTEM，内容和 revision 不变，也不报告文档损坏；其他主体可写、owner 不符、link/hardlink 则拒绝读取和抢救。临时 I/O 错误直接返回，不移动健康文档。子代理并发固定为 `SUBAGENT_CONCURRENCY`（8）。
- `secrets.json`：MCP 服务器的 `mcp-<id>`。
- `ui.json`：最近项目、会话→项目路径、选中模型、窗口尺寸。last-writer-wins，无 revision CAS。
- `AGENTS.md` / `mcp.json` / `skills/`：magpie Library 写入的规则、MCP 服务器和技能（也可手写）。
- `agents/`：用户级 subagent 角色；项目级角色在 `<project>/.mycode/agents/`。
- 系统提示规则先加载 `<MYCODE_HOME>/AGENTS.md`，再按 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md` 的顺序加载 workspace 文件。每段标注来源路径；项目规则覆盖全局默认，同目录后面的规则覆盖前面的冲突项，用户消息优先于规则和技能。不会读取 OS user home 的 `.agents` 规则；每次构造提示词重新读文件，magpie Library 更新下一轮即可生效。技能来自 `<MYCODE_HOME>/skills/`。
- `project_dir_key(project)` 把项目路径拼成文件夹名（无项目为 `no-project`），`new_session_dir(project, id)` 生成新会话目录 `sessions/<项目键>/<id>`，`locate_session_dir(home, id)` 扫描 `sessions/*/` 找到已有会话的目录。
- 未绑定项目时工具 cwd 是 `scratch/`，不是按会话 id 命名的目录。

路径不合法时拒绝读取；文档校验失败时按损坏处理，备份后重置并逐项抢救。
