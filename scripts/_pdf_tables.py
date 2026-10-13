"""从规范 PDF 抽取表格行的共用工具。

`pdftotext -layout` 在这些表上有根本性歧义：千位分隔用的是空格，`2 048`
既可能是「2 048」一个数，也可能是相邻两列的 `2` 和 `048`，纯文本无法区分。
故一律改用字形 x 坐标：先按 y 坐标聚行，再按 x 间距把同一单元格内被千位
空格拆开的数字段合回去。

供 `check_sfb_tables.py` 与 `check_aspx_tables.py` 使用。
"""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
from pathlib import Path


MANIFEST = Path(__file__).resolve().parent.parent / "spec" / "MANIFEST.json"
# 页码是 PDF 的物理页码（从 1 开始），只适用于以下已验证的来源。
# 只记录定位信息，不保存或分发规范正文与表值。
PAGE_SOURCES = {
    "ts_10319001v010401p.pdf": "25c868dae73bec515dd6be2522c87312e051cec62613948a3e32ce851e16297f",
    "ts_10319002v010301p.pdf": "0f6deedf99febab50aac23b4624992cd813ce27875199b6375b743b723641cf1",
}
PAGE_SECTIONS = {
    "sfb": ("ts_10319001v010401p.pdf", tuple(range(283, 298)),
            ("Annex B (normative):", "Annex C (normative):")),
    "aspx": ("ts_10319001v010401p.pdf", (97, 194, 203, 204, 208, 211),
             ("sbg_template_lowres", "sbg_template_highres", "Table 189:",
              "Table 190:", "Table 191:", "Table 192:", "Table 194:")),
    "asf": ("ts_10319001v010401p.pdf", (84, 85, 88, 89, 189, 273, 274, 275),
            ("Table 99:", "Table 100:", "Table 103:", "Table 106:",
             "Table 109:", "Table 110:", "Table 186:", "Table A.2:", "Table A.15:")),
    "ajoc_part1": ("ts_10319001v010401p.pdf", (233, 235, 236, 237),
                   ("Table 197:", "Table 198:", "Table 199:", "Table 200:",
                    "Table 201:", "5.7.7.4.3")),
    "ajoc_part2": ("ts_10319002v010301p.pdf", tuple(range(82, 90)),
                   ("Table 28:", "Table 29:", "Table 30:", "Table 31:",
                    "Table 32:", "5.7.3.4", "used in a cyclic way:")),
}


class Failure(Exception):
    """抽取失败；调用方负责转成非零退出码。"""


def verify_pdf(path: Path) -> None:
    """每次打开前校验输入；页码索引与清单须同时认可同一份 PDF。"""
    try:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        document = next(doc for doc in manifest["documents"] if doc["filename"] == path.name)
        payload = path.read_bytes()
    except (OSError, ValueError, KeyError, StopIteration) as error:
        raise Failure(f"无法验证规范 PDF {path.name}：{error}") from error
    actual = hashlib.sha256(payload).hexdigest()
    if actual != document.get("sha256") or actual != PAGE_SOURCES.get(path.name):
        raise Failure(f"{path.name} 摘要与清单或页码索引不一致")
    if len(payload) != document.get("size"):
        raise Failure(f"{path.name} 大小与清单不一致")


@contextmanager
def open_pdf_section(path: Path, section: str, *, full_scan: bool = False):
    """快速页码路径与原始全量扫描共用提取逻辑，不缓存规范内容。"""
    import pdfplumber

    verify_pdf(path)
    filename, numbers, markers = PAGE_SECTIONS[section]
    if path.name != filename:
        raise Failure(f"{section} 不属于 {path.name}")
    with pdfplumber.open(path, pages=None if full_scan else list(numbers)) as pdf:
        if not full_scan:
            if tuple(page.page_number for page in pdf.pages) != numbers:
                raise Failure(f"{section} 的 PDF 页码不完整")
            text = "\n".join(page.extract_text() or "" for page in pdf.pages)
            for marker in markers:
                if marker not in text:
                    raise Failure(f"{section} 的索引页中缺少 {marker}")
        yield pdf


def words_by_line(page, tol: float = 2.0) -> list[list[dict]]:
    """把一页的字形按 y 坐标聚成行，行内按 x 排序。"""
    words = page.extract_words(x_tolerance=1.2, y_tolerance=1.5)
    lines: list[list[dict]] = []
    for word in sorted(words, key=lambda w: (round(w["top"], 1), w["x0"])):
        if lines and abs(lines[-1][0]["top"] - word["top"]) <= tol:
            lines[-1].append(word)
        else:
            lines.append([word])
    for line in lines:
        line.sort(key=lambda w: w["x0"])
    return lines


def merge_cells(line: list[dict], gap: float = 6.0) -> list[str]:
    """把同一单元格内被千位空格拆开的数字段合回去。"""
    cells: list[list[dict]] = []
    for word in line:
        if cells and word["x0"] - cells[-1][-1]["x1"] < gap:
            cells[-1].append(word)
        else:
            cells.append([word])
    return ["".join(w["text"] for w in cell) for cell in cells]
