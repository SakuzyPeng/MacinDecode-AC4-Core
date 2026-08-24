"""从规范 PDF 抽取表格行的共用工具。

`pdftotext -layout` 在这些表上有根本性歧义：千位分隔用的是空格，`2 048`
既可能是「2 048」一个数，也可能是相邻两列的 `2` 和 `048`，纯文本无法区分。
故一律改用字形 x 坐标：先按 y 坐标聚行，再按 x 间距把同一单元格内被千位
空格拆开的数字段合回去。

供 `check_sfb_tables.py` 与 `check_aspx_tables.py` 使用。
"""

from __future__ import annotations


class Failure(Exception):
    """抽取失败；调用方负责转成非零退出码。"""


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
