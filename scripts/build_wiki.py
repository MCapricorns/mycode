#!/usr/bin/env python3
"""Build wiki/index.html from wiki/graph.json and scripts/wiki_shell.html.

The page embeds the source of every file a node points at, so it opens from
disk with no network and always shows the code that shipped with it. Re-run
this after touching graph.json or any listed file.

  python scripts/build_wiki.py           # write wiki/index.html
  python scripts/build_wiki.py --check   # fail when the page is stale
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GRAPH = ROOT / "wiki" / "graph.json"
SHELL = ROOT / "scripts" / "wiki_shell.html"
OUT = ROOT / "wiki" / "index.html"
GRAPH_TOKEN = "__GRAPH_JSON__"
FILE_TOKEN = "__FILE_JSON__"


def load_graph() -> dict:
    return json.loads(GRAPH.read_text(encoding="utf-8"))


def collect_files(graph: dict) -> tuple[dict[str, str], list[str]]:
    files: dict[str, str] = {}
    missing: list[str] = []
    for node in graph["nodes"]:
        for path in node.get("files", []):
            if path in files:
                continue
            target = ROOT / path
            if not target.is_file():
                missing.append(path)
                continue
            files[path] = target.read_text(encoding="utf-8")
    return files, missing


def payload(value: object) -> str:
    # A literal "</" inside a script element closes it early.
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")


def render(graph: dict, files: dict[str, str]) -> str:
    shell = SHELL.read_text(encoding="utf-8")
    for token in (GRAPH_TOKEN, FILE_TOKEN):
        if token not in shell:
            raise SystemExit(f"{SHELL.name} is missing {token}")
    graph_payload = payload({"lanes": graph["lanes"], "nodes": graph["nodes"]})
    html = shell.replace(GRAPH_TOKEN, graph_payload).replace(FILE_TOKEN, payload(files))
    for token in (GRAPH_TOKEN, FILE_TOKEN):
        if token in html:
            raise SystemExit(f"{OUT.name} still contains {token}")
    return html


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="exit 1 when wiki/index.html is stale")
    args = parser.parse_args()

    graph = load_graph()
    files, missing = collect_files(graph)
    if missing:
        for path in missing:
            print(f"graph.json points at a missing file: {path}", file=sys.stderr)
        return 1

    html = render(graph, files)
    if args.check:
        current = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if current != html:
            print("wiki/index.html is stale; run python scripts/build_wiki.py", file=sys.stderr)
            return 1
        print("wiki/index.html is up to date")
        return 0

    OUT.write_text(html, encoding="utf-8", newline="\n")
    kb = len(html.encode("utf-8")) // 1024
    print(f"wrote {OUT.relative_to(ROOT)} ({kb} KB, {len(files)} files, {len(graph['nodes'])} nodes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
