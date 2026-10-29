# MYCode Wiki

Open [index.html](index.html) for the architecture explorer, runtime reference and code mode documentation. The generated page embeds its graph, Markdown reference and source files, so graph navigation, search and source reading also work offline from `file://`.

The paper and ink palette follows the desktop: `#FAF6EE`, `#F3EDE2`, `#14131B` and `#C72E38`. The optional dark theme uses `#141316`, `#1B1A1E`, `#242328`, cream text and `#F46A6C`. The wiki reuses the native bracket-M red stamp from `crates/mycode-desktop/assets/icon.svg`. The wiki generator copies that source to `images/logo.svg` and uses the same shape in `images/banner.svg`; native ICO, PNG and ICNS generation remains separate.

## Source of truth

- `wiki/graph.json` defines lanes, nodes, relationships and embedded source paths.
- `wiki/architecture/runtime.md` describes crate boundaries and the turn pipeline.
- `wiki/architecture/code-mode-engine.md` documents the embedded JavaScript and MCP contract.
- `scripts/wiki_shell.html` owns the responsive shell, graph, search, source reader and themes.
- `scripts/build_wiki.py` renders the two Markdown references and embeds all payloads into `wiki/index.html`; it also generates `wiki/images/logo.svg` and `wiki/images/banner.svg` from the native SVG mark.

Edit these inputs and regenerate. Do not hand-edit `wiki/index.html`. Source links inside the reference resolve to GitHub; links between the two reference documents stay inside the offline page. The generator supports headings, paragraphs, inline code, bold, links, fenced code, flat unordered lists and tables; raw HTML is escaped.

## Build and validate

Run from the repository root, using Python's standard library only:

```sh
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

`--check` fails when graph data, a reference document, shell markup, an embedded source or either generated brand asset differs from the snapshot. Missing source paths, invalid node lanes and broken graph links fail generation. Regenerate once more after integrated backend changes before committing.

## Interaction and accessibility

The desktop opens the dependency graph with a selected node's details beside it. Tablet and phone widths default to a readable node list, with the graph still available. On a phone, choosing a node opens its details; the back control restores focus to that node.

Search matches node labels, descriptions and source paths, with a result count and a clear action for no matches. Press `/` to focus search. Graph nodes support Enter and Space. The source dialog uses native modal focus handling, Escape, copy and a GitHub link. Navigation folds at tablet widths, supports Escape and keeps Tab within its open controls. Theme preference is stored locally when storage is available.

Existing `#n=<node>&f=<path>` deep links remain supported. Reference pages use `#v=architecture` or `#v=code-mode`; section links add `&s=<heading-id>`.
