#!/usr/bin/env python3
"""Generate the vendored provider-catalog snapshot from a models.dev api.json.

Usage: python scripts/generate_catalog.py <models.dev api.json> <output snapshot.json>

The snapshot keeps only providers MYCode can serve with its three first-party
wire protocols (anthropic-messages, openai-completions) and normalizes every
entry to the compact camelCase schema the mycode-providers crate embeds.
"""
import json
import sys
from datetime import date, datetime, timezone

MAX_STRING_BYTES = 8 * 1024
MAX_MODELS_PER_PROVIDER = 512

# Known base URLs for first-party labs that models.dev lists without an `api`
# field because the AI SDK package embeds the endpoint.
ENDPOINT_FIXES = {
    "anthropic": ("anthropic-messages", "https://api.anthropic.com"),
    "openai": ("openai-completions", "https://api.openai.com/v1"),
    "groq": ("openai-completions", "https://api.groq.com/openai/v1"),
    "mistral": ("openai-completions", "https://api.mistral.ai/v1"),
    "xai": ("openai-completions", "https://api.x.ai/v1"),
    "cerebras": ("openai-completions", "https://api.cerebras.ai/v1"),
    "perplexity": ("openai-completions", "https://api.perplexity.ai"),
}

# Providers that authenticate with an OAuth device flow instead of a pasted
# API key; the settings UI renders a sign-in button for these.
DEVICE_CODE_PROVIDERS = {"github-copilot"}

# Providers excluded from presets: cloud consoles with non-portable auth
# (cloud SDKs) rather than a plain API key or a supported OAuth flow.
EXCLUDED_PROVIDERS = set()


def resolve_wire(provider_id: str, raw: dict) -> tuple[str, str] | None:
    """Returns (wire kind, base URL) for one models.dev provider entry."""
    if provider_id in EXCLUDED_PROVIDERS:
        return None
    npm = clean_text(raw.get("npm"))
    api = clean_text(raw.get("api"))
    if "anthropic" in npm:
        kind = "anthropic-messages"
    elif any(token in npm for token in ("bedrock", "vertex", "azure", "google")):
        return None
    else:
        kind = "openai-completions"
    if provider_id == "github-copilot":
        # The chat completions host; models.dev lists the console host.
        return (kind, "https://api.githubcopilot.com")
    if api.startswith("https://"):
        return (kind, api)
    fix = ENDPOINT_FIXES.get(provider_id)
    if fix is not None:
        return fix
    return None


def clean_text(value, fallback=""):
    if not isinstance(value, str):
        return fallback
    value = value.strip()
    if len(value.encode("utf-8")) > MAX_STRING_BYTES:
        return fallback
    return value


def build_model(model_id: str, raw: dict) -> dict | None:
    if not isinstance(raw, dict):
        return None
    if isinstance(raw.get("status"), str) and raw["status"].strip():
        # Deprecated or retired entries are not offered as presets.
        return None
    name = clean_text(raw.get("name"), model_id) or model_id
    limit = raw.get("limit") if isinstance(raw.get("limit"), dict) else {}
    cost = raw.get("cost") if isinstance(raw.get("cost"), dict) else {}

    def number(value):
        return value if isinstance(value, (int, float)) and value >= 0 else None

    model = {
        "id": model_id,
        "name": name,
        "reasoning": bool(raw.get("reasoning")),
        "toolCall": bool(raw.get("tool_call")),
        "attachment": bool(raw.get("attachment")),
        "context": int(number(limit.get("context")) or 0),
        "output": int(number(limit.get("output")) or 0),
    }
    toggle, efforts = reasoning_options(raw)
    if toggle:
        model["reasoningToggle"] = True
    if efforts:
        model["reasoningEfforts"] = efforts
    cost_in = number(cost.get("input"))
    cost_out = number(cost.get("output"))
    if cost_in is not None or cost_out is not None:
        model["costIn"] = cost_in
        model["costOut"] = cost_out
    return model


REASONING_TOKENS = {
    "off",
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "on",
    "default",
}


def reasoning_options(raw: dict) -> tuple[bool, list[str]]:
    """Extract models.dev toggle / effort rows. Unknown tokens are dropped."""
    options = raw.get("reasoning_options")
    if not isinstance(options, list):
        return False, []
    toggle = False
    efforts: list[str] = []
    for option in options:
        if not isinstance(option, dict):
            continue
        kind = option.get("type")
        if kind == "toggle":
            toggle = True
            continue
        if kind != "effort":
            continue
        values = option.get("values")
        if not isinstance(values, list):
            continue
        for value in values:
            token = clean_text(value).lower()
            if token not in REASONING_TOKENS or token in efforts or len(efforts) >= 8:
                continue
            efforts.append(token)
    return toggle, efforts


def build_provider(provider_id: str, raw: dict) -> dict | None:
    if not isinstance(raw, dict):
        return None
    wire = resolve_wire(provider_id, raw)
    if wire is None:
        return None
    kind, base_url = wire
    models = []
    seen = set()
    raw_models = raw.get("models") if isinstance(raw.get("models"), dict) else {}
    for model_id, model in raw_models.items():
        if len(models) >= MAX_MODELS_PER_PROVIDER:
            break
        if not isinstance(model_id, str) or not model_id or model_id in seen:
            continue
        built = build_model(model_id, model)
        if built is not None:
            seen.add(model_id)
            models.append(built)
    if not models:
        return None
    provider = {
        "id": provider_id,
        "name": clean_text(raw.get("name"), provider_id) or provider_id,
        "kind": kind,
        "baseUrl": base_url,
        "doc": clean_text(raw.get("doc")) or None,
        "models": models,
    }
    if provider_id in DEVICE_CODE_PROVIDERS:
        provider["auth"] = "device-code"
    return provider


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: generate_catalog.py <api.json> <snapshot.json>", file=sys.stderr)
        return 2
    with open(sys.argv[1], "r", encoding="utf-8") as handle:
        source = json.load(handle)

    providers = []
    for provider_id, raw in source.items():
        built = build_provider(provider_id, raw)
        if built is not None:
            providers.append(built)
    providers.sort(key=lambda provider: provider["id"])

    snapshot = {
        "formatVersion": 1,
        "kind": "mycode-catalog-snapshot",
        "source": "models.dev",
        "generatedAt": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "providers": providers,
    }
    payload = json.dumps(snapshot, ensure_ascii=False, separators=(",", ":"))
    with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as handle:
        handle.write(payload)
        handle.write("\n")

    total_models = sum(len(provider["models"]) for provider in providers)
    anthropic = sum(1 for p in providers if p["kind"] == "anthropic-messages")
    print(
        f"{len(providers)} providers ({anthropic} anthropic-messages), "
        f"{total_models} models, {len(payload) / 1024:.0f} KiB"
    )
    for provider in providers[:8]:
        print(f"  {provider['id']}: {len(provider['models'])} models ({provider['kind']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
