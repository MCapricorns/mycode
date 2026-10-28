"""Regression checks for the offline wiki generator (Python standard library)."""

import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("build_wiki", ROOT / "scripts/build_wiki.py")
wiki = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wiki)


class WikiGeneratorTests(unittest.TestCase):
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

    def test_reference_tables_and_heading_anchors_are_generated(self):
        doc = wiki.render_doc(wiki.DOCS["code-mode"])
        self.assertIn('<h2 id="progress-and-child-identity">', doc["html"])
        self.assertIn("<table>", doc["html"])
        self.assertNotIn("<td>---</td>", doc["html"])
        self.assertIn("result.content[0].Text", doc["html"])
        self.assertEqual(len(doc["headings"]), len({h["id"] for h in doc["headings"]}))

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
