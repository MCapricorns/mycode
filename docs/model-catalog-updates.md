# 模型目录更新指南

改 Copilot / Codex / OpenDesign 的模型列表或思考档位时照这份文档做。

## 三个来源

| 服务商 | 模型列表 | 思考档位 | 更新方式 |
| --- | --- | --- | --- |
| GitHub Copilot | models.dev 云端目录 | models.dev 的 `reasoning_options` | 不用改代码。客户端读 `catalog-cache.json`，超过 24 小时自动重新验证；设置页「关于 → 刷新」可强制刷新 |
| OpenAI Codex | `crates/mycode-providers/src/catalog.rs` 的 `openai_codex_preset()` | 同一函数的 effort 列表 | 改代码后发版 |
| OpenDesign | `crates/mycode-providers/src/catalog/opendesign.rs` 的 `CURATED` | 每行的 `efforts` | 改代码后发版 |

models.dev 不含 Codex 和 OpenDesign，这两个服务商没有实时拉取或“从网关加载模型”的入口。

## 运行时顺序

`crates/mycode-providers/src/catalog/store.rs`：

1. 启动时 `current_with_repair` 只校验 `catalog-cache.json` 的信封（kind、`formatVersion`、ETag、整体 JSON）；模型列表在第一次 `document()` 时才解析。
2. `fetched_at > 0` 且年龄小于 `DEFAULT_MAX_AGE_SECS`（24h）时直接使用，不发请求。
3. 过期或 `force` 时请求 `https://models.dev/api.json`，带上已有 ETag 作为 `If-None-Match`。首包和块间空闲上限 30s，整体 300s，文档上限 32 MiB。
4. 304：只改写信封里的 `fetchedAt` 和 ETag，不解析模型。200：解析并归一化，且必须至少有一个非注入 provider，否则视为失败（防止把门户 HTML 写进缓存）。
5. 任何失败都保留旧缓存；没有缓存则用 `bundled()`，即 Codex + OpenDesign 两行基线。
6. 下载结果、缓存物化和基线都会跑 `codex::attach` + `opendesign::attach`，这两个 provider 始终存在，云端数据覆盖不到。

Copilot 只来自 models.dev。首次离线且没有缓存时，目录里没有 Copilot 行。

## 更新 Codex

`crates/mycode-providers/src/catalog.rs` → `openai_codex_preset()`：

- `models` 是 `(id, name, efforts)` 数组。每行固定 `reasoning: true`、`tool_call: true`，`context` 和 `output` 保持默认 0：输出上限因此回退 32000，上下文为 0 时压缩阈值固定 48000 token。
- `efforts` 写后端实际接受的 token：`none`、`low`、`medium`、`high`、`xhigh`、`max`。`none` 在 UI 里显示为 Off。
- 当前只保留 GPT-5.6、GPT-6、GPT-6.1：`gpt-5.6-luna`、`gpt-5.6-sol`、`gpt-5.6-sol-pro`、`gpt-5.6-terra`、`gpt-6-astra`、`gpt-6-luna`、`gpt-6-sol`、`gpt-6.1-sol`。Astra 和 6.1 Sol 没有 `none`。
- `codex::usable_family()` 是 `#[cfg(test)]` 断言，不在运行时过滤。新增系列时同步改 `crates/mycode-providers/src/catalog/codex.rs` 的前缀列表，并更新 `codex::tests::usable_family_covers_the_current_series_only` 和 `catalog::tests::codex_preset_carries_only_the_current_model_families`。
- `codex::attach` 会用预设整行替换同 id 的 provider。

## 更新 OpenDesign

`crates/mycode-providers/src/catalog/opendesign.rs` → `CURATED`：

- 每行 `Curated { id, context, output, image, efforts }`。`context` 取网关的 `context_limit`；`output` 取网关 `context_budget` 和厂商公布上限中较小的那个；`image` 决定是否支持图片；`efforts` 只写实测通过网关的 token。
- `curated_model()` 固定 `reasoning: true`、`reasoning_toggle: false`、`tool_call: true`，价格为空。
- `clamp_output` 在 `context != 0` 时把输出压到 `context - PROMPT_RESERVE_TOKENS`（2048）以内，下限 1024。当前 `kimi-k2.7-code` 的 262000 窗口因此发 259952。
- 共享档位常量：

| 常量 | token |
| --- | --- |
| `OFF_AND_ALL` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| `GLM_53` | `low`, `high`, `max` |
| `GPT_6_LUNA` | `none`, `low`, `medium`, `high`, `xhigh`, `max` |
| `GPT_61_SOL` | `low`, `medium`, `high`, `xhigh`, `max` |
| `KIMI_K27_CODE` | `minimal`, `low`, `medium`, `high`, `xhigh` |

- 只有实测 `reasoning_effort: "none"` 真正关闭思考的模型才加 `none`（DeepSeek、GPT-6 Luna、MiMo）；GLM-5.3 没有 Off，K2.7 Code 没有 Off 也没有 `max`。
- 数据测量方法和日期写在上方注释里（当前为 2026-10-01，`pi-opendesign-bridge` 0.2.9）。改行时同步更新日期。
- `opendesign::attach` 会用 `CURATED` 整行替换同 id 的 provider；provider id 固定 `opendesign`，base URL 固定 `https://amr-link.open-design.ai/v1`。

## models.dev 数据

不需要改代码。刷新后新模型和服务商直接进设置页。解析时会丢掉带 `status` 的模型和 `npm` 含 `bedrock` / `vertex` / `azure` / `google` 的服务商；每个服务商最多 512 个模型，字符串字段最多 8 KiB，effort 最多 16 个。

## 验证

```bash
cargo test -p mycode-providers
cargo test -p mycode-frontend
cargo check -p mycode-app
cargo check -p mycode-desktop
python .github/scripts/release_plan.py --self-test
```

重点测试：

- `catalog::tests::codex_preset_carries_only_the_current_model_families`
- `catalog::codex::tests::usable_family_covers_the_current_series_only`
- `catalog::codex::tests::attach_replaces_the_row_with_the_preset`、`catalog::opendesign::tests::curated_levels_follow_the_measured_gateway_behaviour`
- `catalog::opendesign::tests::a_ceiling_equal_to_the_window_keeps_room_for_the_prompt`
- `catalog::opendesign::tests::attach_replaces_the_row_and_keeps_providers_sorted`
- `catalog::store::tests::a_cloud_body_without_providers_is_not_a_catalog`
- `family::tests::kimi_effort_prefers_the_active_catalog_and_falls_back_when_absent`
- `view_model::reduce::models::tests::the_opendesign_preset_form_opens_on_the_configured_models`

Codex 或 OpenDesign 的模型改动需要发版：push 到 main 后，CI 的 release-plan 会在标签已存在时 bump 补丁号。models.dev 数据更新不需要发版。

## 已删除的旧机制

- 编译进二进制的 `catalog/snapshot.json`，以及每周快照 PR 管线（`.github/workflows/models-snapshot.yml`、`scripts/generate_catalog.py`、`.github/scripts/catalog_snapshot_diff.py`）。
- `BridgeCommand::RefreshOpenDesignModels`、`RefreshOAuthModels`、`opendesign_loading`，以及设置页的 OpenDesign 专页和「从网关加载模型」按钮。
- 启动后的后台刷新保留：缓存不足 24 小时不发请求；过期或缺失才条件请求或下载；失败静默，继续用缓存或基线。应用更新下载成功后仍会强制刷新一次。
