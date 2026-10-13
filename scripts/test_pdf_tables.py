"""无需规范文件或 pdfplumber 的页码快速路径负向测试。"""
from contextlib import nullcontext
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from scripts import _pdf_tables as pdf


class PdfInputTests(unittest.TestCase):
    def test_source_hash_manifest_and_index_must_agree(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = root / "source.pdf"
            source.write_bytes(b"synthetic PDF input")
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            manifest = root / "MANIFEST.json"
            document = {"filename": source.name, "sha256": digest, "size": source.stat().st_size}
            manifest.write_text(json.dumps({"documents": [document]}))
            with patch.object(pdf, "MANIFEST", manifest), patch.object(pdf, "PAGE_SOURCES", {source.name: digest}):
                pdf.verify_pdf(source)
                source.write_bytes(b"corrupted")
                with self.assertRaises(pdf.Failure):
                    pdf.verify_pdf(source)
                source.write_bytes(b"synthetic PDF input")
                document["size"] += 1
                manifest.write_text(json.dumps({"documents": [document]}))
                with self.assertRaises(pdf.Failure):
                    pdf.verify_pdf(source)
            with patch.object(pdf, "MANIFEST", manifest), patch.object(pdf, "PAGE_SOURCES", {source.name: "0" * 64}):
                with self.assertRaises(pdf.Failure):
                    pdf.verify_pdf(source)

    def test_missing_input_or_manifest_fails(self):
        with tempfile.TemporaryDirectory() as folder:
            with patch.object(pdf, "MANIFEST", Path(folder) / "missing.json"):
                with self.assertRaises(pdf.Failure):
                    pdf.verify_pdf(Path(folder) / "missing.pdf")


class PageSelectionTests(unittest.TestCase):
    def open(self, numbers=(2, 3), text="Table X: end", full_scan=False):
        pages = [SimpleNamespace(page_number=n, extract_text=lambda: text) for n in numbers]
        opener = Mock(return_value=nullcontext(SimpleNamespace(pages=pages)))
        with patch.dict("sys.modules", {"pdfplumber": SimpleNamespace(open=opener)}), \
             patch.object(pdf, "verify_pdf"), \
             patch.object(pdf, "PAGE_SECTIONS", {"fixture": ("fixture.pdf", (2, 3), ("Table X:", "end"))}):
            with pdf.open_pdf_section(Path("fixture.pdf"), "fixture", full_scan=full_scan):
                pass
        return opener

    def test_fast_path_selects_only_declared_pages(self):
        self.open().assert_called_once_with(Path("fixture.pdf"), pages=[2, 3])

    def test_full_scan_bypasses_page_index(self):
        self.open(numbers=(1, 2, 3, 4), text="", full_scan=True).assert_called_once_with(Path("fixture.pdf"), pages=None)

    def test_missing_wrong_or_duplicate_pages_fail(self):
        for numbers in [(2,), (1, 3), (2, 2), (3, 2)]:
            with self.subTest(numbers=numbers), self.assertRaises(pdf.Failure):
                self.open(numbers=numbers)

    def test_missing_title_or_end_marker_fails(self):
        for text in ["", "Table X:", "end"]:
            with self.subTest(text=text), self.assertRaises(pdf.Failure):
                self.open(text=text)


if __name__ == "__main__":
    unittest.main()
