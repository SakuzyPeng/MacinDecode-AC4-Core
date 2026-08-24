#!/usr/bin/env python3
"""用规范 PDF 反向核对用户本地生成的 A-SPX 静态表。

    ./scripts/check_aspx_tables.py            核对
    ./scripts/check_aspx_tables.py --allow-missing

核对五组数值：

  * `5.7.6.3.1.1` 的两张模板子带组表（正文文本行）；
  * 表 190/191 的 `aspx_start_freq`/`aspx_stop_freq` 到 QMF 子带的映射；
  * 表 189 的 `num_qmf_timeslots`；
  * 表 192 的 `num_ts_in_ats` 与 `ts_offset_hfgen`；
  * 表 194 的 `tab_border`。

表 193 的 `noise_mid_border` 是文字表达式而非数值表，不在核对范围内。

模板表的**奇数下标项没有任何规范内的第二来源**，这正是本脚本存在的理由。
Rust 单元测试能校验模板表严格递增、长度与组数吻合，并用表
190/191 反查**偶数**下标项——因为 `Pseudocode 67` 以 `2 * aspx_start_freq`
索引模板表，表 190/191 只覆盖得到偶数位置。把 `sbg_template_highres` 的
第 17 项由 47 改成 45，全部单元测试依旧通过；改第 18 项（偶数下标）则会被
起止频率测试抓住。本脚本补上前者缺失的那一路核对。

依赖规范 PDF 与 pdfplumber；缺少时默认失败。只有显式传入 `--allow-missing`
才允许跳过，以免自动化把「未核对」误报成成功。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

from _pdf_tables import Failure, merge_cells, words_by_line

REPO_ROOT = Path(__file__).resolve().parent.parent
PDF = REPO_ROOT / "spec" / "ts_10319001v010401p.pdf"
GENERATED_RS = REPO_ROOT / "spec" / "generated" / "ts103190_pdf_tables.rs"


def extract() -> dict[str, object]:
    import pdfplumber

    templates: dict[str, list[int]] = {}
    start_stop: dict[str, dict[str, list[int]]] = {}
    table_189: list[tuple[int, int]] = []
    table_192: list[tuple[int, int, int]] = []
    table_194: list[tuple[int, list[int], list[int], list[int]]] = []

    with pdfplumber.open(PDF) as pdf:
        for page in pdf.pages:
            text = page.extract_text() or ""

            for scale in ("lowres", "highres"):
                match = re.search(
                    rf"sbg_template_{scale}\s*=\s*\[([0-9,\s]+)\]", text
                )
                if match is not None and scale not in templates:
                    templates[scale] = [
                        int(tok) for tok in re.findall(r"\d+", match.group(1))
                    ]

            for number, scale in (("190", "lowres"), ("191", "highres")):
                if f"Table {number}:" not in text:
                    continue
                rows = _numeric_rows(page)
                starts = [row[1] for row in rows if len(row) >= 2]
                stops = [row[4] for row in rows if len(row) >= 6]
                if len(starts) == 8 and len(stops) == 4:
                    start_stop[scale] = {"start": starts, "stop": stops}

            if "Table 189:" in text and not table_189:
                table_189 = _table_189(page)
            if "Table 192:" in text and not table_192:
                table_192 = [
                    (row[0], row[1], row[2]) for row in _numeric_rows(page) if len(row) >= 3
                ]
            if "Table 194:" in text and not table_194:
                table_194 = _table_194(text)

    for scale in ("lowres", "highres"):
        if scale not in templates:
            raise Failure(f"未抽到 sbg_template_{scale}")
        if scale not in start_stop:
            raise Failure(f"未抽到 {scale} 的起止频率表")
    if len(table_189) != 8:
        raise Failure(f"表 189 应有 8 列，实际 {len(table_189)} 列")
    if len(table_192) != 8:
        raise Failure(f"表 192 应有 8 行，实际 {len(table_192)} 行")
    if len(table_194) != 5:
        raise Failure(f"表 194 应有 5 行，实际 {len(table_194)} 行")

    return {
        "templates": templates,
        "start_stop": start_stop,
        "table_189": table_189,
        "table_192": table_192,
        "table_194": table_194,
    }


def _table_194(text: str) -> list[tuple[int, list[int], list[int], list[int]]]:
    """表 194 的每行形如 `6 {0, 6} {0, 3, 6} {0, 2, 3, 4, 6}`。"""
    rows: list[tuple[int, list[int], list[int], list[int]]] = []
    for line in text.splitlines():
        match = re.match(r"\s*(\d+)\s+(\{.*\})\s*$", line)
        if match is None:
            continue
        groups = re.findall(r"\{([0-9,\s]*)\}", match.group(2))
        if len(groups) != 3:
            continue
        borders = [[int(tok) for tok in re.findall(r"\d+", g)] for g in groups]
        rows.append((int(match.group(1)), borders[0], borders[1], borders[2]))
    return rows


def _numeric_rows(page) -> list[list[int]]:
    """抽出整页中以数字开头、且前若干格全为数字的行。"""
    rows: list[list[int]] = []
    for line in words_by_line(page):
        cells = merge_cells(line)
        if not cells or not cells[0].isdigit():
            continue
        values: list[int] = []
        for cell in cells:
            if cell.isdigit():
                values.append(int(cell))
            else:
                break
        if values:
            rows.append(values)
    return rows


def _table_189(page) -> list[tuple[int, int]]:
    """表 189 是横排的：一行 frame_length，一行 num_qmf_timeslots。"""
    lengths: list[int] = []
    slots: list[int] = []
    for line in words_by_line(page):
        cells = merge_cells(line)
        if not cells:
            continue
        head, rest = cells[0], cells[1:]
        if not all(cell.isdigit() for cell in rest) or not rest:
            continue
        if head == "frame_length":
            lengths = [int(cell) for cell in rest]
        elif head == "num_qmf_timeslots":
            slots = [int(cell) for cell in rest]
    if len(lengths) != len(slots):
        raise Failure("表 189 的两行长度不一致")
    return list(zip(lengths, slots))


def parse_rust() -> dict[str, object]:
    if not GENERATED_RS.exists():
        raise Failure("缺少本地生成表；先运行 scripts/generate_spec_tables.py")
    text = GENERATED_RS.read_text(encoding="utf-8")

    def array(name: str) -> list[int]:
        match = re.search(rf"const\s+{name}\s*:[^=]+?=\s*\[(.*?)\];", text, re.S)
        if match is None:
            raise Failure(f"{GENERATED_RS.name} 中找不到 {name}")
        return [int(tok) for tok in re.findall(r"\d+", match.group(1))]

    def triples(name: str) -> list[tuple[int, int, int]]:
        values = array(name)
        if len(values) % 3 != 0:
            raise Failure(f"{name} 的项数不是三的倍数")
        return [tuple(values[i : i + 3]) for i in range(0, len(values), 3)]

    def tab_border() -> list[tuple[int, list[int], list[int], list[int]]]:
        values = array("TAB_BORDER")
        width = 1 + 2 + 3 + 5
        if len(values) != 5 * width:
            raise Failure(f"TAB_BORDER 应有 {5 * width} 个整数，实际 {len(values)}")
        rows: list[tuple[int, list[int], list[int], list[int]]] = []
        for start in range(0, len(values), width):
            row = values[start : start + width]
            rows.append((row[0], row[1:3], row[3:6], row[6:11]))
        return rows

    templates = {
        "lowres": array("SBG_TEMPLATE_LOWRES"),
        "highres": array("SBG_TEMPLATE_HIGHRES"),
    }
    table_192 = triples("NUM_TS_IN_ATS")

    # 表 190/191 是模板的偶数下标投影；表 189 则由帧长除以 64 得到。
    start_stop = {}
    for scale, template in templates.items():
        start_stop[scale] = {
            "start": template[0:16:2],
            "stop": list(reversed(template[-7::2])),
        }

    return {
        "table_194": tab_border(),
        "templates": templates,
        "start_stop": start_stop,
        "table_189": [(length, length // 64) for length, _, _ in table_192],
        "table_192": table_192,
    }


def compare(pdf: dict[str, object], rust: dict[str, object]) -> list[str]:
    problems: list[str] = []

    def diff(label: str, expected: list, actual: list) -> None:
        if expected == actual:
            return
        if len(expected) != len(actual):
            problems.append(f"{label}: 项数 PDF {len(expected)} vs Rust {len(actual)}")
            return
        for index, (a, b) in enumerate(zip(expected, actual)):
            if a != b:
                problems.append(f"{label}: 第 {index} 项 PDF {a} vs Rust {b}")

    for scale in ("lowres", "highres"):
        diff(
            f"sbg_template_{scale}",
            pdf["templates"][scale],
            rust["templates"][scale],
        )
        for which in ("start", "stop"):
            diff(
                f"{scale} 的 aspx_{which}_freq 子带",
                pdf["start_stop"][scale][which],
                rust["start_stop"][scale][which],
            )

    diff("表 189", pdf["table_189"], rust["table_189"])
    diff("表 192", pdf["table_192"], rust["table_192"])
    diff("表 194", pdf["table_194"], rust["table_194"])

    # 表 192 第三列换算到 A-SPX 时隙后必须恒为 3，即 aspx_var_bord_* 的
    # 2 位字段上限；这条把 5.7.6.3.3.1 的正文与表 53 的字段宽度串起来。
    for length, factor, offset in pdf["table_192"]:
        if factor == 0 or offset // factor != 3:
            problems.append(
                f"表 192: frame_length {length} 的 ts_offset_hfgen {offset} "
                f"除以 num_ts_in_ats {factor} 不等于 3"
            )

    # 起止频率列必须落在模板表的偶数下标上，这一条把两份来源真正串起来。
    for scale in ("lowres", "highres"):
        template = pdf["templates"][scale]
        starts = pdf["start_stop"][scale]["start"]
        stops = pdf["start_stop"][scale]["stop"]
        for index, subband in enumerate(starts):
            if template[2 * index] != subband:
                problems.append(
                    f"{scale}: aspx_start_freq={index} 指向 {subband}，"
                    f"模板表第 {2 * index} 项却是 {template[2 * index]}"
                )
        last = len(template) - 1
        for index, subband in enumerate(stops):
            if template[last - 2 * index] != subband:
                problems.append(
                    f"{scale}: aspx_stop_freq={index} 指向 {subband}，"
                    f"模板表第 {last - 2 * index} 项却是 {template[last - 2 * index]}"
                )

    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--allow-missing",
        action="store_true",
        help="缺少规范 PDF 或 pdfplumber 时允许跳过",
    )
    args = parser.parse_args()

    try:
        import pdfplumber  # noqa: F401
    except ImportError:
        print("未安装 pdfplumber，无法核对 A-SPX 表", file=sys.stderr)
        print(
            "  python3 -m pip install --cert /etc/ssl/cert.pem pdfplumber",
            file=sys.stderr,
        )
        if args.allow_missing:
            print("  按 --allow-missing 跳过", file=sys.stderr)
            return 0
        return 1

    if not PDF.exists():
        print(
            f"缺少 {PDF.name}，无法核对 A-SPX 表；先运行 scripts/fetch_specs.py",
            file=sys.stderr,
        )
        if args.allow_missing:
            print("  按 --allow-missing 跳过", file=sys.stderr)
            return 0
        return 1

    try:
        pdf_data = extract()
    except Failure as error:
        print(f"从 PDF 抽取失败：{error}", file=sys.stderr)
        return 1

    try:
        rust_data = parse_rust()
    except Failure as error:
        print(f"解析 {GENERATED_RS.name} 失败：{error}", file=sys.stderr)
        return 1

    problems = compare(pdf_data, rust_data)
    if problems:
        print(f"A-SPX 表与本地生成表不一致 {len(problems)} 处：")
        for line in problems:
            print(f"  {line}")
        return 1

    borders = sum(len(v) for v in pdf_data["templates"].values())
    tab_border_values = sum(
        len(row[1]) + len(row[2]) + len(row[3]) for row in pdf_data["table_194"]
    )
    print(
        f"A-SPX 表核对通过：{borders} 个模板边界、{2 * (8 + 4)} 个起止子带、"
        f"表 189 与表 192 各 8 行、表 194 的 {tab_border_values} 个边界与 PDF 一致"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
