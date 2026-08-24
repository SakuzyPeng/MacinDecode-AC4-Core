#!/usr/bin/env python3
"""用规范 PDF 反向核对用户本地生成的附录 B Rust 表。

    ./scripts/check_sfb_tables.py            核对
    ./scripts/check_sfb_tables.py --emit     打印可直接粘贴的 Rust 数组
    ./scripts/check_sfb_tables.py --allow-missing

附录 B 只存在于规范正文，没有随附的机器可读版本。数值由
`scripts/generate_spec_tables.py` 写入被忽略的 `spec/generated/`；本脚本重新从
PDF 抽取并逐值核对生成结果。Rust 单元测试只校验表的自洽性，不能替代此审核。

`pdftotext -layout` 在这张表上有根本性歧义：`47 896` 既可能是「sfb 47，值
896」，也可能是千位分隔的 `47896`，纯文本无法区分。故改用字形 x 坐标。

依赖规范 PDF 与 pdfplumber；缺少时默认失败。只有显式传入 `--allow-missing`
才允许跳过，以免自动化把「未核对」误报成成功。
"""

from __future__ import annotations

import argparse
import re
import sys
import textwrap
from pathlib import Path

from _pdf_tables import Failure, merge_cells, words_by_line

REPO_ROOT = Path(__file__).resolve().parent.parent
PDF = REPO_ROOT / "spec" / "ts_10319001v010401p.pdf"
GENERATED_RS = REPO_ROOT / "spec" / "generated" / "ts103190_pdf_tables.rs"

# 附录 B 表 B.4 至 B.7 的列所对应的变换长度（44,1 kHz 或 48 kHz 列）。
COLUMNS = {
    "B.4": [2048, 1920, 1536],
    "B.5": [1024, 960, 768],
    "B.6": [512, 480, 384],
    "B.7": [256, 240, 192, 128, 120, 96],
}


def store_row(rows: dict[int, list[str]], cells: list[str]) -> None:
    """一行可能含左右两栏，各自以 sfb 开头，中间以 '-' 分隔。

    表头（含 '@'）与页眉页脚（含 'ETSI'、版本号）都不是数据行：要求全部
    单元格为纯数字或 '-'，即可把它们排除。
    """
    if not cells or not all(cell.isdigit() or cell == "-" for cell in cells):
        return
    chunks: list[list[str]] = [[]]
    for cell in cells:
        if cell == "-" and chunks[-1]:
            chunks.append([])
        else:
            chunks[-1].append(cell)
    for chunk in chunks:
        if len(chunk) >= 2 and chunk[0].isdigit():
            rows[int(chunk[0])] = chunk[1:]


def extract() -> tuple[dict[int, int], dict[int, list[int]]]:
    import pdfplumber

    with pdfplumber.open(PDF) as pdf:
        pages = pdf.pages
        # 目录页含同样字样，故从后往前找正文标题。
        first = last = None
        for index in range(len(pages) - 1, -1, -1):
            text = pages[index].extract_text() or ""
            if last is None and "Annex C (normative):" in text:
                last = index
            if last is not None and "Annex B (normative):" in text:
                first = index
                break
        if first is None or last is None:
            raise Failure("未在 PDF 中定位到附录 B")

        raw: dict[str, dict[int, list[str]]] = {}
        current: str | None = None
        for page in pages[first:last]:
            for line in words_by_line(page):
                text = " ".join(w["text"] for w in line)
                if text.startswith("Table B."):
                    current = text.split(":")[0].replace("Table ", "").strip()
                    continue
                if current is not None:
                    store_row(raw.setdefault(current, {}), merge_cells(line))

    num_sfb = {
        key: int(values[0])
        for key, values in raw.get("B.1", {}).items()
        if len(values) == 1 and values[0].isdigit()
    }
    if len(num_sfb) != 15:
        raise Failure(f"表 B.1 应有 15 行，实际 {len(num_sfb)} 行")

    offsets: dict[int, list[int]] = {}
    for name, lengths in COLUMNS.items():
        rows = raw.get(name)
        if rows is None:
            raise Failure(f"未抽到表 {name}")
        keys = sorted(rows)
        if keys != list(range(len(keys))):
            raise Failure(f"表 {name} 的 sfb 不连续")
        for col, length in enumerate(lengths):
            take = num_sfb[length] + 1
            column = []
            for key in keys[:take]:
                cells = rows[key]
                if col >= len(cells) or not cells[col].isdigit():
                    raise Failure(f"表 {name} sfb {key} 第 {col} 列不是数值")
                column.append(int(cells[col]))
            offsets[length] = column
    return num_sfb, offsets


def parse_rust() -> tuple[dict[int, int], dict[int, list[int]]]:
    if not GENERATED_RS.exists():
        raise Failure("缺少本地生成表；先运行 scripts/generate_spec_tables.py")
    text = GENERATED_RS.read_text(encoding="utf-8")

    def array(name: str) -> list[int]:
        match = re.search(rf"{name}(?:\s*:\s*\[[^\]]*\])?\s*=\s*\[(.*?)\];", text, re.S)
        if match is None:
            raise Failure(f"{GENERATED_RS.name} 中找不到 {name}")
        return [int(tok) for tok in re.findall(r"\d+", match.group(1))]

    lengths = array("TRANSFORM_LENGTHS_48")
    counts = array("NUM_SFB_48")
    if len(lengths) != len(counts):
        raise Failure("TRANSFORM_LENGTHS_48 与 NUM_SFB_48 行数不一致")
    offsets = {length: array(f"SFB_OFFSET_{length}") for length in lengths}
    return dict(zip(lengths, counts)), offsets


def emit(offsets: dict[int, list[int]]) -> None:
    table_of = {
        length: name for name, lengths in COLUMNS.items() for length in lengths
    }
    for length in sorted(offsets, reverse=True):
        values = offsets[length]
        body = textwrap.fill(
            ", ".join(str(v) for v in values),
            width=92,
            initial_indent="    ",
            subsequent_indent="    ",
        )
        print(f"/// 变换长度 {length} 的 `sfb_offset`，附录 B 表 {table_of[length]}。")
        print(f"const SFB_OFFSET_{length}: [u16; {len(values)}] = [")
        print(body)
        print("];\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--emit", action="store_true", help="打印 Rust 数组而非核对")
    parser.add_argument(
        "--allow-missing",
        action="store_true",
        help="缺少规范 PDF 或 pdfplumber 时允许跳过",
    )
    args = parser.parse_args()

    try:
        import pdfplumber  # noqa: F401
    except ImportError:
        print("未安装 pdfplumber，无法核对附录 B", file=sys.stderr)
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
            f"缺少 {PDF.name}，无法核对附录 B；先运行 scripts/fetch_specs.py",
            file=sys.stderr,
        )
        if args.allow_missing:
            print("  按 --allow-missing 跳过", file=sys.stderr)
            return 0
        return 1

    try:
        pdf_counts, pdf_offsets = extract()
    except Failure as error:
        print(f"从 PDF 抽取失败：{error}", file=sys.stderr)
        return 1

    if args.emit:
        emit(pdf_offsets)
        return 0

    try:
        rust_counts, rust_offsets = parse_rust()
    except Failure as error:
        print(f"解析 {GENERATED_RS.name} 失败：{error}", file=sys.stderr)
        return 1

    problems: list[str] = []
    if set(pdf_counts) != set(rust_counts):
        problems.append(
            f"变换长度集合不一致：PDF {sorted(pdf_counts)}，Rust {sorted(rust_counts)}"
        )
    for length in sorted(set(pdf_counts) & set(rust_counts), reverse=True):
        if pdf_counts[length] != rust_counts[length]:
            problems.append(
                f"{length}: num_sfb PDF {pdf_counts[length]}，Rust {rust_counts[length]}"
            )
        expected, actual = pdf_offsets[length], rust_offsets.get(length, [])
        if expected != actual:
            diff = next(
                (
                    f"第 {i} 项 PDF {a} vs Rust {b}"
                    for i, (a, b) in enumerate(zip(expected, actual))
                    if a != b
                ),
                f"项数 PDF {len(expected)} vs Rust {len(actual)}",
            )
            problems.append(f"{length}: sfb_offset 不一致（{diff}）")

    if problems:
        print(f"附录 B 与本地生成表不一致 {len(problems)} 处：")
        for line in problems:
            print(f"  {line}")
        return 1

    total = sum(len(v) for v in pdf_offsets.values())
    print(f"附录 B 核对通过：{len(pdf_offsets)} 个变换长度，{total} 个偏移值与 PDF 一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
