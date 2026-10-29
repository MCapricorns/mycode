"""Regression checks for the offline wiki generator (Python standard library)."""

from html.parser import HTMLParser
import importlib.util
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("build_wiki", ROOT / "scripts/build_wiki.py")
wiki = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wiki)


class DocumentLinks(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = []
        self.links = []

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if "id" in values:
            self.ids.append(values["id"])
        if tag == "a" and "href" in values:
            self.links.append(values["href"])


class WikiGeneratorTests(unittest.TestCase):
    def render_fixture(self, markdown):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "reference.md"
            source.write_text(markdown, encoding="utf-8")
            with patch.object(wiki, "ROOT", root):
                return wiki.render_doc(source)

    def test_graph_links_and_all_embedded_sources_resolve(self):
        graph = wiki.load_graph()
        wiki.validate_graph(graph)
        files, missing = wiki.collect_files(graph)
        self.assertEqual(missing, [])
        self.assertIn("crates/mycode-tools/src/code_mode/progress.rs", files)
        for path, content in files.items():
            self.assertEqual(content, (ROOT / path).read_text(encoding="utf-8"))

    def test_duplicate_ids_and_unknown_targets_fail(self):
        for graph in (
            {"lanes": ["A"], "nodes": [{"id": "a", "lane": 0}, {"id": "a", "lane": 0}]},
            {"lanes": ["A"], "nodes": [{"id": "a", "lane": 0, "links": ["missing"]}]},
            {"lanes": ["A"], "nodes": [{"id": "a", "lane": 1}]},
        ):
            with self.subTest(graph=graph), self.assertRaises(SystemExit):
                wiki.validate_graph(graph)

    def test_embedded_script_cannot_close_its_container(self):
        value = {"source": '</script><script>alert("x")</script>'}
        encoded = wiki.payload(value)
        self.assertNotIn("</", encoded)
        self.assertEqual(json.loads(encoded), value)

    def test_inline_code_and_raw_html_are_escaped(self):
        source = wiki.DOCS["code-mode"]
        rendered = wiki.inline('<script> `a < b` **bold**', source)
        self.assertEqual(rendered, '&lt;script&gt; <code>a &lt; b</code> <strong>bold</strong>')

    def test_document_links_stay_offline_and_source_links_use_repo(self):
        source = wiki.DOCS["architecture"]
        self.assertEqual(wiki.doc_link("code-mode-engine.md#progress-and-child-identity", source),
                         "#v=code-mode&s=progress-and-child-identity")
        self.assertEqual(wiki.doc_link("../../crates/mycode-app/src/turn.rs", source),
                         wiki.REPO + "crates/mycode-app/src/turn.rs")
        self.assertEqual(wiki.doc_link("#subagent-orchestration", source),
                         "#v=architecture&s=subagent-orchestration")
        self.assertEqual(wiki.doc_link("#n=agent", source), "#n=agent")

    def test_reference_tables_and_heading_anchors_are_generated(self):
        doc = wiki.render_doc(wiki.DOCS["code-mode"])
        parsed = DocumentLinks()
        parsed.feed(doc["html"])
        self.assertIn("progress-and-child-identity", parsed.ids)
        self.assertEqual(len(parsed.ids), len(set(parsed.ids)))
        self.assertRegex(doc["title"], r"[\u4e00-\u9fff]")
        self.assertIn("<table>", doc["html"])
        self.assertNotIn("<td>---</td>", doc["html"])
        self.assertIn("result.content[0].Text", doc["html"])
        self.assertEqual(len(doc["headings"]), len({h["id"] for h in doc["headings"]}))

    def test_legacy_anchors_survive_chinese_headings_without_allowing_arbitrary_html(self):
        doc = self.render_fixture(
            '# 中文文档\n\n<a id="old-section"></a>\n\n## 新标题\n\n'
            '<a id="unsafe" onclick="alert(1)"></a>\n\n<script>alert(1)</script>\n'
        )
        self.assertIn('<a id="old-section"></a>', doc["html"])
        self.assertIn('<h2 id="新标题">新标题</h2>', doc["html"])
        self.assertIn('&lt;a id=&quot;unsafe&quot; onclick=&quot;alert(1)&quot;&gt;', doc["html"])
        self.assertNotIn('<script>', doc["html"])
        with self.assertRaises(SystemExit):
            self.render_fixture('# 标题\n\n<a id="duplicate"></a>\n\n<a id="duplicate"></a>\n')

    def test_all_reference_section_links_resolve_in_the_offline_page(self):
        documents = {}
        for name, path in wiki.DOCS.items():
            parsed = DocumentLinks()
            parsed.feed(wiki.render_doc(path)["html"])
            documents[name] = parsed
        for parsed in documents.values():
            for href in parsed.links:
                if not href.startswith("#v="):
                    continue
                route = parse_qs(href[1:])
                target = route["v"][0]
                self.assertIn(target, documents)
                if "s" in route:
                    self.assertIn(route["s"][0], documents[target].ids, href)

    def test_brand_assets_share_the_native_vector_source(self):
        assets = wiki.brand_assets()
        icon = wiki.ICON.read_text(encoding="utf-8")
        self.assertEqual(assets[ROOT / "wiki/images/logo.svg"], icon)
        polygon = icon.split('<polygon', 1)[1].split('/>', 1)[0]
        self.assertIn(polygon, assets[ROOT / "wiki/images/banner.svg"])

    def test_render_embeds_all_three_payloads_without_tokens(self):
        graph = wiki.load_graph()
        files, _ = wiki.collect_files(graph)
        rendered = wiki.render(graph, files)
        for token in (wiki.GRAPH_TOKEN, wiki.FILE_TOKEN, wiki.DOC_TOKEN):
            self.assertNotIn(token, rendered)
        self.assertIn('id="doc-data"', rendered)
        self.assertIn('id="graph-data"', rendered)
        self.assertIn('id="file-data"', rendered)


if __name__ == "__main__":
    unittest.main()
