# mycode-config

MYCode 的 home 布局与严格配置文档。会话与压缩检查点都在 `sessions/` 下。

## Home

`MYCODE_HOME` 非空则整棵树迁走；否则 `HOME`（Windows 还可回退 `USERPROFILE`）下的 `.mycode`。

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ catalog-cache.json
├─ agents/<role>.md
├─ sessions.db
├─ sessions/<项目键>/<sess-id>/{<br1-id>.jsonl,payloads/,compaction.json}
└─ scratch/
```

- `settings.json`：providers、web backends、MCP、subagents、appearance。无密钥。未知字段或已退役字段（`userAgent`、`usage`、`tools.*`、`subagents.maxConcurrent`/`timeoutSecs`、`appearance.palette`/`fontFamily`、网页 `custom` 后端）均按文档损坏处理，备份后重置并逐项抢救（salvage）仍合法的字段。子代理并发固定为 `SUBAGENT_CONCURRENCY`（8）。
- `secrets.json`：`provider-id`、`web-<id>`、`mcp-<id>`。
- `ui.json`：最近项目、会话→项目路径、选中模型、窗口尺寸。last-writer-wins，无 revision CAS。
- `agents/`：用户级 subagent 角色；项目级角色在 `<project>/.mycode/agents/`。
- 系统提示资源来自 workspace 与 home 的 `AGENTS.md`/`MYCODE.md`，技能来自 `.agents` 树。
- `project_dir_key(project)` 把项目路径拼成文件夹名（无项目为 `no-project`），`new_session_dir(project, id)` 生成新会话目录 `sessions/<项目键>/<id>`，`locate_session_dir(home, id)` 扫描 `sessions/*/` 找到已有会话的目录。
- 未绑定项目时工具 cwd 是 `scratch/`，不是按会话 id 命名的目录。

路径不合法时拒绝读取；文档校验失败时按损坏处理，备份后重置并逐项抢救。
