# 模型目录更新指南

下次让代理（或自己）更新 Copilot / Codex / OpenDesign 的模型和思考强度时，照这个文件做。

## 三个来源

| 服务商 | 模型列表来源 | 思考强度来源 | 更新方式 |
| ------ | ------------ | ------------ | -------- |
| GitHub Copilot | models.dev 云端目录 | models.dev 发布的 `reasoning_options` | **不用改代码**。客户端启动时读 `catalog-cache.json`，缓存超过 24 小时自动重新下载 models.dev；设置页「关于 → 服务商目录 → 刷新」可手动刷新 |
| OpenAI Codex | `crates/mycode-providers/src/catalog.rs` 的 `openai_codex_preset()` | 同一函数的 `reasoning_efforts` 列表 | 改代码后发版 |
| OpenDesign | `crates/mycode-providers/src/catalog/opendesign.rs` 的 `CURATED` 表 | 同表每行的 `efforts`（共享梯子常量） | 改代码后发版 |

运行时没有任何「拉最新模型」的按钮或 live 端点：models.dev 不含 Codex 和 OpenDesign 的行，这两个 provider 完全手工维护。

## 目录解析顺序（为什么保底是安全的）

`crates/mycode-providers/src/catalog/store.rs`：

1. `catalog-cache.json` 在 owned home，未过期（`DEFAULT_MAX_AGE_SECS = 24h`）直接用，无网络流量。
2. 过期或手动刷新 → 带 `If-None-Match` 条件请求 `https://models.dev/api.json`，成功则重写缓存。
3. 下载失败、缓存损坏、首次离线 → `bundled()`，即 `attach_subscription_presets()` 注入的 **Codex + OpenDesign 两行硬编码保底**。
4. 下载到的文档与缓存文档在物化时同样跑 `codex::attach` + `opendesign::attach`，所以这两个 provider 永远存在，云端数据覆盖不到它们。

Copilot 只从 models.dev 来：没有缓存也没有网络时，Copilot 行不存在 —— 能下载客户端就有网，这是接受的取舍。

## 更新 Codex 模型 / 强度

`crates/mycode-providers/src/catalog.rs` → `openai_codex_preset()`：

- 在 `(id, name, efforts)` 数组里增删模型。
- `efforts` 是该模型在 Codex 后端实际接受的推理档位 token（`none` 表示接受 `reasoning_effort: "none"`，即 UI 里的 Off）。档位集合：`none / low / medium / high / xhigh / max`。
- `codex::usable_family()` 把预设限制在 `gpt-5.6-` / `gpt-6-` / `gpt-6.1-` 家族 —— 加新系列时同步扩展前缀列表和它的测试（`codex.rs` 底部、`catalog.rs` 底部 `codex_preset_carries_only_the_current_model_families`）。

## 更新 OpenDesign 模型 / 强度

`crates/mycode-providers/src/catalog/opendesign.rs` → `CURATED` 表：

- `Curated { id, context, output, image, efforts }`：
  - `context` = 网关报告的 `context_limit`；
  - `output` = min(网关 `context_budget`, 厂商发布上限)；`clamp_output` 会再留 `PROMPT_RESERVE_TOKENS`；
  - `efforts` 用共享梯子常量（`OFF_AND_ALL`、`GLM_53`、`GPT_6_LUNA`、`GPT_61_SOL`、`KIMI_K27_CODE`）或新写一个 —— 只写实测能过的 token，`none` 只在 `reasoning_effort:"none"` 真正关思考时才加。
- 数据来自对网关的实测（历史来源 `@lokeraar/pi-opendesign-bridge`），改完顺手更新 `CURATED` 上方的测量日期注释。

## 验证

```bash
cargo test -p mycode-providers          # 目录解析/保底/注入、kimi fixture、wire 行为
cargo test -p mycode-desktop          # 设置页 view-model 归约测试
cargo check -p mycode-app             # 核心侧无新引用
python .github/scripts/release_plan.py --self-test
```

Codex/Opendesign 模型改动只需要一个版本发布（push 到 main，CI 的 release-plan 自动 bump 补丁号）；models.dev 数据更新不需要发版，客户端每天自动拿新数据。

## 已删除的旧机制（勿复活）

- `catalog/snapshot.json` 编译进二进制 —— 已删除；保底只有两行硬编码。
- `.github/workflows/models-snapshot.yml`、`scripts/generate_catalog.py`、`catalog_snapshot_diff.py` —— 每周快照 PR 管线，已删除。
- `BridgeCommand::RefreshOpenDesignModels`、`RefreshOAuthModels`、`opendesign_loading`、设置页里的 OpenDesign 专页和「从网关加载模型」按钮 —— 已删除。
- 启动首帧后后台静默刷一次目录：缓存不到 24 小时不发请求，过期或缺失才下载 / 条件请求；失败完全静默，继续用旧缓存或保底行。About 页手动「刷新」和应用更新下载成功后各再强制刷一次。
