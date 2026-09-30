# MYCode 架构

## 1. 产品形态

MYCode 是 GPUI 桌面应用（Windows / macOS）。配置（Provider、Web、MCP、Agents、主题）全部在设置页完成；密钥只进 `secrets.json`。

```text
mycode-desktop (GPUI Desk)
  ├─ view_model reducer（无 GPU，可测）
  ├─ 渲染：侧栏 / 时间线 / inspector
  └─ CoreBridge（专用线程 + current-thread tokio）
       mycode-app
         ├─ mycode-agent     会话 ledger + agent loop + hooks
         ├─ mycode-providers 三协议 + catalog + SSE transport
         ├─ mycode-tools     内置工具 + task / web_search
         └─ mycode-config    settings / secrets / sessions 文档
```

Workspace 成员只有这 7 个 crate：`mycode-core`、`mycode-config`、`mycode-providers`、`mycode-tools`、`mycode-agent`、`mycode-app`、`mycode-desktop`。

## 2. 关键边界

- **UI 不碰文件、密钥、网络**：一律经 `BridgeCommand` / `BridgeReply` / `BridgeEvent`。
- **持久化走严格文档**：owned-file 事务 + revision CAS。
- **模型回合 = durable turn**：事件落 `sessions/<id>/`；`write`/`edit` 前 checkpoint。
- **Provider 是数据**：`kind` 只有三个 wire；新厂商加配置即可。
- **工具同一 registry**：builtins、`task`、`web_search`/`fetch_content`、已连接 MCP 都注册后写入 `build_system_prompt`。
- **项目 cwd**：绑定真实项目路径；未绑定使用 `scratch/`，不按会话 id 造文件夹。

## 3. Home 布局

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ catalog-cache.json
├─ sessions/<ses1-id>/     # manifest、branches、pending、compaction
├─ checkpoints/<ses1-id>/
└─ scratch/
```

会话 ledger 与压缩检查点共处同一会话目录，不再拆到 `plugins/session/…` 与 `workspace/ses1-…`。

## 4. 相关文档

| 文档 | 内容 |
| --- | --- |
| [01-agent-core.md](01-agent-core.md) | loop、压缩、ledger |
| [02-tools-permissions.md](02-tools-permissions.md) | 工具与权限 |
| [09-desktop-ui.md](09-desktop-ui.md) | 桌面与桥接 |
