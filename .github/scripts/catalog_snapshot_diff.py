#!/usr/bin/env python3
"""Summarize added and removed providers and models between catalog snapshots.

Usage: catalog_snapshot_diff.py <old snapshot.json> <new snapshot.json> <body.md>

The body is the pull request description for a models.dev snapshot refresh.
It lists provider and model ids that appeared or disappeared, plus edits that
keep the same id. The vendored file is produced by scripts/generate_catalog.py.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path

# GitHub rejects a pull request body longer than 65536 characters.
BODY_LIMIT = 64_000
KIND = "mycode-catalog-snapshot"


@dataclass(frozen=True)
class ProviderView:
    name: str
    model_count: int
    meta: str
    model_ids: tuple[str, ...]
    models: dict[str, str]
    model_names: dict[str, str]


def canonical(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def one_line(value: str) -> str:
    return " ".join(value.split())


def load_snapshot(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"{path}: {error}") from error
    if not isinstance(data, dict):
        raise SystemExit(f"{path}: snapshot root is not an object")
    if data.get("kind") != KIND:
        raise SystemExit(f"{path}: expected kind {KIND}")
    providers = data.get("providers")
    if not isinstance(providers, list):
        raise SystemExit(f"{path}: providers is not a list")
    return data


def index_providers(doc: dict) -> dict[str, ProviderView]:
    indexed: dict[str, ProviderView] = {}
    for provider in doc["providers"]:
        if not isinstance(provider, dict) or not isinstance(provider.get("id"), str):
            raise SystemExit("snapshot provider is missing an id")
        provider_id = provider["id"]
        raw_models = provider.get("models")
        if not isinstance(raw_models, list):
            raise SystemExit(f"{provider_id}: models is not a list")
        models: dict[str, str] = {}
        names: dict[str, str] = {}
        order: list[str] = []
        for model in raw_models:
            if not isinstance(model, dict) or not isinstance(model.get("id"), str):
                raise SystemExit(f"{provider_id}: model is missing an id")
            model_id = model["id"]
            order.append(model_id)
            models[model_id] = canonical(model)
            name = model.get("name")
            names[model_id] = name if isinstance(name, str) else ""
        meta = {key: value for key, value in provider.items() if key != "models"}
        name = provider.get("name")
        indexed[provider_id] = ProviderView(
            name=name if isinstance(name, str) else "",
            model_count=len(raw_models),
            meta=canonical(meta),
            model_ids=tuple(order),
            models=models,
            model_names=names,
        )
    return indexed


def model_total(doc: dict) -> int:
    return sum(len(provider.get("models") or []) for provider in doc["providers"])


def count_label(count: int, singular: str, plural: str) -> str:
    noun = singular if count == 1 else plural
    return f"{count} {noun}"


def provider_line(provider_id: str, view: ProviderView) -> str:
    models = count_label(view.model_count, "model", "models")
    line = f"`{provider_id}` ({models})"
    name = one_line(view.name)
    if name and name != provider_id:
        line += f" — {name}"
    return line


def model_line(provider_id: str, model_id: str, name: str) -> str:
    line = f"`{provider_id}` / `{model_id}`"
    cleaned = one_line(name)
    if cleaned and cleaned != model_id:
        line += f" — {cleaned}"
    return line


def append_models(
    lines: list[str],
    provider_id: str,
    view: ProviderView,
    model_ids: list[str] | tuple[str, ...],
) -> None:
    listed: set[str] = set()
    for model_id in model_ids:
        if model_id in listed:
            continue
        listed.add(model_id)
        lines.append(model_line(provider_id, model_id, view.model_names[model_id]))


def bullet_list(lines: list[str], cap: int | None = None) -> str:
    if not lines:
        return "- none\n"
    if cap is not None and cap <= 0:
        omitted = count_label(len(lines), "entry", "entries")
        return f"- {omitted} omitted so the added and removed lists fit\n"
    if cap is None or cap >= len(lines):
        shown = lines
        extra = 0
    else:
        shown = lines[:cap]
        extra = len(lines) - cap
    body = "".join(f"- {line}\n" for line in shown)
    if extra:
        body += f"- and {extra} more\n"
    return body


def render(old: dict, new: dict) -> str:
    old_providers = index_providers(old)
    new_providers = index_providers(new)
    old_ids = set(old_providers)
    new_ids = set(new_providers)

    added_providers = [
        provider_line(provider_id, new_providers[provider_id])
        for provider_id in sorted(new_ids - old_ids)
    ]
    removed_providers = [
        provider_line(provider_id, old_providers[provider_id])
        for provider_id in sorted(old_ids - new_ids)
    ]
    updated_providers: list[str] = []
    reordered_providers: list[str] = []
    added_models: list[str] = []
    removed_models: list[str] = []
    updated_models: list[str] = []

    for provider_id in sorted(new_ids - old_ids):
        view = new_providers[provider_id]
        append_models(added_models, provider_id, view, view.model_ids)

    for provider_id in sorted(old_ids - new_ids):
        append_models(
            removed_models,
            provider_id,
            old_providers[provider_id],
            old_providers[provider_id].model_ids,
        )

    for provider_id in sorted(old_ids & new_ids):
        before = old_providers[provider_id]
        after = new_providers[provider_id]
        if before.meta != after.meta:
            updated_providers.append(provider_line(provider_id, after))
        if before.model_ids != after.model_ids and set(before.model_ids) == set(
            after.model_ids
        ):
            reordered_providers.append(f"`{provider_id}`")
        append_models(
            added_models,
            provider_id,
            after,
            sorted(set(after.models) - set(before.models)),
        )
        append_models(
            removed_models,
            provider_id,
            before,
            sorted(set(before.models) - set(after.models)),
        )
        for model_id in sorted(set(before.models) & set(after.models)):
            if before.models[model_id] != after.models[model_id]:
                updated_models.append(
                    model_line(provider_id, model_id, after.model_names[model_id])
                )

    def compose(caps: dict[str, int | None]) -> str:
        def listed(name: str, lines: list[str]) -> str:
            return bullet_list(lines, caps[name]).rstrip("\n")

        sections = [
            "Refreshes the vendored [models.dev](https://models.dev) catalog snapshot at `crates/mycode-providers/src/catalog/snapshot.json`.",
            "",
            "Generated by `python scripts/generate_catalog.py <models.dev api.json> crates/mycode-providers/src/catalog/snapshot.json` from `https://models.dev/api.json`.",
            "",
            "`cargo test -p mycode-providers` passed on `ubuntu-latest` before this pull request was opened.",
            "",
            "## Catalog diff",
            "",
            f"- Providers: {len(old['providers'])} → {len(new['providers'])} (added {len(added_providers)}, removed {len(removed_providers)})",
            f"- Models: {model_total(old)} → {model_total(new)} (added {len(added_models)}, removed {len(removed_models)})",
            f"- Updated models: {len(updated_models)}",
            f"- Reordered providers: {len(reordered_providers)}",
            f"- generatedAt: `{old.get('generatedAt')}` → `{new.get('generatedAt')}`",
            "",
            "### Added providers",
            "",
            listed("added_providers", added_providers),
            "",
            "### Removed providers",
            "",
            listed("removed_providers", removed_providers),
            "",
            "### Added models",
            "",
            listed("added_models", added_models),
            "",
            "### Removed models",
            "",
            listed("removed_models", removed_models),
            "",
            "### Updated providers",
            "",
            listed("updated_providers", updated_providers),
            "",
            "### Updated models",
            "",
            listed("updated_models", updated_models),
            "",
            "### Reordered providers",
            "",
            listed("reordered_providers", reordered_providers),
            "",
            "## Actions permissions",
            "",
            "The repository setting **Allow GitHub Actions to create and approve pull requests** must be enabled (Settings → Actions → General → Workflow permissions). Without it, this workflow cannot create the pull request.",
            "",
            "Pull requests opened with `GITHUB_TOKEN` do not trigger `ci.yml` automatically. `ci.yml` runs on `pull_request`, and GitHub does not start workflows from events produced by `GITHUB_TOKEN`.",
            "",
        ]
        return "\n".join(sections)

    # List every added and removed id when the description fits. Clip
    # edited rows first; those counts stay in the summary above.
    full = {name: None for name in (
        "added_providers",
        "removed_providers",
        "added_models",
        "removed_models",
        "updated_providers",
        "updated_models",
        "reordered_providers",
    )}
    body = compose(full)
    if len(body) <= BODY_LIMIT:
        return body
    clipped = dict(full)
    clipped["updated_models"] = 20
    body = compose(clipped)
    if len(body) <= BODY_LIMIT:
        return body
    clipped["updated_models"] = 0
    clipped["reordered_providers"] = 0
    body = compose(clipped)
    if len(body) <= BODY_LIMIT:
        return body
    cap = max(len(added_models), len(removed_models))
    best = body
    while cap >= 0 and len(best) > BODY_LIMIT:
        clipped["added_models"] = cap
        clipped["removed_models"] = cap
        best = compose(clipped)
        if len(best) <= BODY_LIMIT or cap == 0:
            break
        cap = max(0, cap - 40)
    return best


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("old", type=Path)
    parser.add_argument("new", type=Path)
    parser.add_argument("body", type=Path)
    args = parser.parse_args(argv)
    body = render(load_snapshot(args.old), load_snapshot(args.new))
    args.body.write_text(body, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
