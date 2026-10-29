#!/usr/bin/env python3
"""Build the offline wiki from graph.json, architecture Markdown and the shell.

The page embeds the source of every file a node points at, plus rendered
reference documents, so it opens from disk with no network. Re-run after
changing a document, graph, template or embedded source file.

  python scripts/build_wiki.py           # write wiki/index.html
  python scripts/build_wiki.py --check   # fail when the page is stale
"""

from __future__ import annotations

import argparse
import html
import json
import re
import sys
from urllib.parse import urljoin
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GRAPH = ROOT / "wiki" / "graph.json"
SHELL = ROOT / "scripts" / "wiki_shell.html"
OUT = ROOT / "wiki" / "index.html"
ICON = ROOT / "crates/mycode-desktop/assets/icon.svg"
GRAPH_TOKEN = "__GRAPH_JSON__"
FILE_TOKEN = "__FILE_JSON__"
DOC_TOKEN = "__DOC_JSON__"
DOCS = {
    "architecture": ROOT / "wiki/architecture/runtime.md",
    "code-mode": ROOT / "wiki/architecture/code-mode-engine.md",
}
REPO = "https://github.com/MCapricorns/mycode/blob/main/"


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


def validate_graph(graph: dict) -> None:
    """Reject broken navigation before publishing the snapshot."""
    nodes = graph["nodes"]
    ids = [node["id"] for node in nodes]
    if not nodes or len(ids) != len(set(ids)):
        raise SystemExit("graph.json needs non-empty nodes with unique ids")
    for node in nodes:
        if not 0 <= node["lane"] < len(graph["lanes"]):
            raise SystemExit(f"invalid lane for {node['id']}")
        for target in node.get("links", []):
            if target not in ids:
                raise SystemExit(f"unknown graph link: {node['id']} -> {target}")


def doc_link(target: str, source: Path) -> str:
    """Resolve wiki documents locally and repository source links on GitHub."""
    if target.startswith(("https://", "http://", "#v=", "#n=", "#f=")):
        return target
    if target.startswith("#"):
        for key, doc in DOCS.items():
            if source.resolve() == doc.resolve():
                return f"#v={key}&s={target[1:]}"
        return target
    path, _, anchor = target.partition("#")
    resolved = (source.parent / path).resolve()
    for key, doc in DOCS.items():
        if resolved == doc.resolve():
            return f"#v={key}" + (f"&s={anchor}" if anchor else "")
    relative = resolved.relative_to(ROOT).as_posix()
    return urljoin(REPO, relative) + (f"#{anchor}" if anchor else "")


def inline(text: str, source: Path) -> str:
    """Render the deliberately small inline vocabulary used by wiki docs."""
    pattern = r"`([^`]+)`|\[([^\]]+)\]\(([^)]+)\)|\*\*([^*]+)\*\*"
    parts = []
    previous = 0
    for match in re.finditer(pattern, text):
        parts.append(html.escape(text[previous:match.start()]))
        code, label, target, strong = match.groups()
        if code is not None:
            parts.append(f"<code>{html.escape(code)}</code>")
        elif label is not None:
            href = html.escape(doc_link(target, source), quote=True)
            parts.append(f'<a href="{href}">{html.escape(label)}</a>')
        else:
            parts.append(f"<strong>{html.escape(strong)}</strong>")
        previous = match.end()
    parts.append(html.escape(text[previous:]))
    return "".join(parts)


def render_doc(source: Path) -> dict:
    """Render headings, paragraphs, fenced code, flat lists and GFM tables.

    Raw HTML is escaped. Keeping this subset explicit avoids a runtime or
    third-party Markdown dependency in the offline wiki.
    """
    lines = source.read_text(encoding="utf-8").splitlines()
    output, headings = [], []
    ids = set()
    title = ""
    i = 0
    while i < len(lines):
        line = lines[i]
        if not line.strip():
            i += 1
            continue
        # Preserve published section links while visible headings are translated.
        anchor = re.fullmatch(r'<a id="([a-z][a-z0-9-]*)"></a>', line)
        if anchor:
            if anchor[1] in ids:
                raise SystemExit(f"duplicate document anchor {anchor[1]} in {source}")
            ids.add(anchor[1])
            output.append(line)
            i += 1
            continue
        if line.startswith("```"):
            language = line[3:].strip()
            code = []
            i += 1
            while i < len(lines) and not lines[i].startswith("```"):
                code.append(lines[i])
                i += 1
            if i == len(lines):
                raise SystemExit(f"unclosed code fence in {source}")
            output.append('<pre><code data-language="' + html.escape(language, quote=True) + '">' + html.escape("\n".join(code)) + "</code></pre>")
            i += 1
            continue
        heading = re.match(r"^(#{1,3}) (.+)$", line)
        if heading:
            level, label = len(heading[1]), heading[2]
            slug = re.sub(r"[^\w -]", "", label.lower())
            slug = re.sub(r"^\d+\s+", "", slug).strip().replace(" ", "-")
            if slug in ids:
                raise SystemExit(f"duplicate document anchor {slug} in {source}")
            ids.add(slug)
            if level == 1:
                title = label
            else:
                headings.append({"id": slug, "label": label})
            output.append(f'<h{level} id="{slug}">{inline(label, source)}</h{level}>')
            i += 1
            continue
        if line.startswith("|"):
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                cells = [cell.strip() for cell in lines[i].strip().strip("|").split("|")]
                if not all(re.fullmatch(r":?-+:?", cell) for cell in cells):
                    rows.append(cells)
                i += 1
            table = ['<div class="table-scroll" tabindex="0" role="region" aria-label="参考表格"><table>']
            for index, cells in enumerate(rows):
                tag = "th" if index == 0 else "td"
                table.append("<tr>" + "".join(f"<{tag}>{inline(cell, source)}</{tag}>" for cell in cells) + "</tr>")
            output.append("".join(table) + "</table></div>")
            continue
        if line.startswith("- "):
            items = []
            while i < len(lines) and lines[i].startswith("- "):
                items.append("<li>" + inline(lines[i][2:], source) + "</li>")
                i += 1
            output.append("<ul>" + "".join(items) + "</ul>")
            continue
        paragraph = [line]
        i += 1
        while i < len(lines) and lines[i].strip() and not lines[i].startswith(("#", "```", "|", "- ", '<a id="')):
            paragraph.append(lines[i])
            i += 1
        output.append("<p>" + inline(" ".join(paragraph), source) + "</p>")
    if not title:
        raise SystemExit(f"missing document title in {source}")
    return {"title": title, "html": "\n".join(output), "headings": headings,
            "source": source.relative_to(ROOT).as_posix()}


def render(graph: dict, files: dict[str, str]) -> str:
    shell = SHELL.read_text(encoding="utf-8")
    for token in (GRAPH_TOKEN, FILE_TOKEN, DOC_TOKEN):
        if token not in shell:
            raise SystemExit(f"{SHELL.name} is missing {token}")
    graph_payload = payload({"lanes": graph["lanes"], "nodes": graph["nodes"]})
    html = shell.replace(GRAPH_TOKEN, graph_payload).replace(FILE_TOKEN, payload(files))
    html = html.replace(DOC_TOKEN, payload({key: render_doc(path) for key, path in DOCS.items()}))
    for token in (GRAPH_TOKEN, FILE_TOKEN, DOC_TOKEN):
        if token in html:
            raise SystemExit(f"{OUT.name} still contains {token}")
    return html


def brand_assets() -> dict[Path, str]:
    """Reuse the native mark verbatim; never maintain a second M polygon."""
    icon = ICON.read_text(encoding="utf-8")
    mark = icon.split(">", 1)[1].rsplit("</svg>", 1)[0]
    banner = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="360" viewBox="0 0 1280 360" role="img" aria-label="MYCode — Rust 原生编码代理">
  <rect width="1280" height="360" rx="20" fill="#F3EDE2"/>
  <rect x="12" y="12" width="1256" height="336" rx="12" fill="#FAF6EE"/>
  <svg x="64" y="90" width="180" height="180" viewBox="0 0 256 256">{mark}</svg>
  <g font-family="Segoe UI, PingFang SC, Microsoft YaHei, sans-serif">
    <text x="284" y="160" font-size="68" font-weight="650" letter-spacing="-2" fill="#14131B">MYCode</text>
    <text x="288" y="212" font-size="26" fill="#14131B">每次改动，有迹可循。</text>
    <text x="288" y="260" font-size="18" fill="#625D58">原生桌面编码代理 · Rust + GPUI</text>
  </g>
</svg>
'''
    return {ROOT / "wiki/images/logo.svg": icon, ROOT / "wiki/images/banner.svg": banner}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="exit 1 when wiki/index.html is stale")
    args = parser.parse_args()

    graph = load_graph()
    validate_graph(graph)
    files, missing = collect_files(graph)
    if missing:
        for path in missing:
            print(f"graph.json points at a missing file: {path}", file=sys.stderr)
        return 1

    html = render(graph, files)
    outputs = {OUT: html, **brand_assets()}
    if args.check:
        stale = [path for path, text in outputs.items()
                 if not path.exists() or path.read_text(encoding="utf-8") != text]
        if stale:
            for path in stale:
                print(f"{path.relative_to(ROOT).as_posix()} is stale; run python scripts/build_wiki.py", file=sys.stderr)
            return 1
        print("wiki/index.html, wiki/images/logo.svg and wiki/images/banner.svg are up to date")
        return 0

    for path, text in outputs.items():
        path.write_text(text, encoding="utf-8", newline="\n")
    kb = len(html.encode("utf-8")) // 1024
    print(f"wrote {OUT.relative_to(ROOT)} ({kb} KB, {len(files)} files, {len(graph['nodes'])} nodes)")
    print("wrote wiki/images/logo.svg and wiki/images/banner.svg from the native icon")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
