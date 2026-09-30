# Agent Core

Core 拥有会话 ledger 与 turn loop。它不拥有 UI、密钥文件或网络 socket；那些由 `mycode-app` 经注入的 provider / tool host 接入。

## 1. Agent loop

```text
loop {
    request = hooks.prepare_request(build_request(state, tools))
    state.messages = request.messages          # 压缩结果写回
    assistant = stream(provider, request)
    state.push(assistant)
    for call in tool_calls(assistant) {
        hooks.observe_before_tool(call)
        state.push(dispatch(call))
    }
}
```

`HookRunner::with_before_request` 在每一次 provider 请求前运行（含工具结果之后的 mid-turn）。生产路径用它做自动压缩。

## 2. Session ledger

每个会话一个目录 `sessions/<ses1-id>/`：

- `manifest.json` — 分支头、committed 长度（CAS 权威）
- `branches/<br1-id>.events` — 追加日志
- `pending/<evt1-id>.payload` — 提交前暂存
- `compaction.json` — 与 ledger 同目录，不再另开 workspace 树

`ses1-` id 只是内部身份。界面标题取会话首条用户消息；空标题时用绑定项目的文件夹名（如 `MCode`），不用 `ses1-…`。

## 3. 自动压缩（Codex 风格）

实现：`mycode-app/src/compaction.rs`。

- 可用窗口 = 模型 context 的 95%；触发阈值 = 可用窗口的 90%。目录/设置没有 context 时回退 48k tokens。
- 尾部保留约 20k tokens，且不切断 tool_use / tool_result 对。
- 头部交给同一 provider 做 handoff 摘要（`COMPACTION SUMMARY`），写入 `compaction.json`。
- ledger 不改写；失败则继续发送完整历史。

## 4. System prompt

Turn 组装：身份说明 + 文件/web/task/MCP 用法 + 资源文件 + `build_system_prompt(registry)` + subagent 委派表。registry 里有什么，模型就能看见什么。
