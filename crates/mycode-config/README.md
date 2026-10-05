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
├─ sessions/<ses1-id>/{manifest.json,compaction.json,…}
└─ scratch/
```

- `settings.json`：providers、web backends、MCP、agents、appearance。无密钥。
- `secrets.json`：`provider-id`、`web-<id>`、`mcp-<id>`。
- `ui.json`：最近项目、会话→项目路径、选中模型。
- `agents/`：用户级 subagent 角色；项目级角色在 `<project>/.mycode/agents/`。
- 系统提示资源来自 workspace 与 home 的 `AGENTS.md`/`MYCODE.md`，技能来自 `.agents` 树。
- `session_relative(id, file)` 生成 `sessions/<id>/<file>`。
- 未绑定项目时工具 cwd 是 `scratch/`，不是按会话 id 命名的目录。

路径不合法或文档校验失败时拒绝读取。
