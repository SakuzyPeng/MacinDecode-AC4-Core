#!/usr/bin/env python3
"""用规范 PDF 反向核对 A-JOC 参数、去相关器与 ducker 表。

    ./scripts/check_ajoc_tables.py
    ./scripts/check_ajoc_tables.py --allow-missing

核对四组数值：

  * P2 表 28（`5.7.3.1`）与本地生成的 `SB_TO_PB` 逐格相等；
  * P2 表 28 的 15/12/9/7 四列与 P1 表 197（`5.7.7.2`）逐格相等。
  * P2 表 29–32（`5.7.3.3`）与本地生成的四组精确有理数逐项相等
    （允许 PDF 十进制排印本身的末位舍入）。
  * P1 表 198–201（`5.7.7.4`）与本地生成的三个子带区域及
    48 个 all-pass 系数逐项相等。
  * P2 `5.7.3.5` 的七路循环与本地生成结果逐项相等。

**为什么必须有这个脚本。** `bands.rs` 的单元判据只能验结构：每列单调非降、
自 0 起、末值为 `num_bands - 1`、逐频带满射，外加十三个手抄锚点。这些挡不住
落在锚点之外、又不破坏结构的数值抄错——实测把 `30-34` 那行的 12 列由 10 改成
11，四条单元判据**全部沉默**（单调、步进、满射、末值都仍然成立）。规范不随附
第二份 A-JOC 频带表，因此唯一的独立来源就是 PDF 本身。这与 `5.7.6.3.1.4` 的
patch 表是同一类缺口，见 `docs/SPEC_TRACEABILITY.md` 5.29。

**第二组核对不是冗余。** 表 28 的四列与表 197 相等，是 `5.7.3.5` 的 transient
ducker 借用 A-CPL 15 频带划分（P1 `5.7.7.4.3`）的前提；`bands.rs` 据此不另存
一张表 197。相等一旦被推翻，ducker 必须改取独立的表，所以这条要一直核着。

依赖规范 PDF 与 pdfplumber；缺少时默认失败。只有显式传入 `--allow-missing`
才允许跳过，以免自动化把「未核对」误报成成功。
"""

from __future__ import annotations

import argparse
import re
import sys
from decimal import Decimal
from pathlib import Path

from _pdf_tables import Failure

REPO_ROOT = Path(__file__).resolve().parent.parent
PDF_PART1 = REPO_ROOT / "spec" / "ts_10319001v010401p.pdf"
PDF_PART2 = REPO_ROOT / "spec" / "ts_10319002v010301p.pdf"
GENERATED_RS = REPO_ROOT / "spec" / "generated" / "ts103190_pdf_tables.rs"

NUM_QMF_SUBBANDS = 64
# 表 28 的列序，与 `AJOC_BAND_COUNTS` 相同。
BAND_COUNTS = (23, 15, 12, 9, 7, 5, 3, 1)
# 表 197 的列序。
ACPL_BAND_COUNTS = (15, 12, 9, 7)

ROW_RE = re.compile(r"^\s*(\d+)(?:\s*-\s*(\d+))?\s+((?:\d+\s+)*\d+)\s*$")
DEQUANT_ROW_RE = re.compile(r"^\s*(\d+)\s+(-?\d+(?:,\d+)?)\s*$")

# PDF 表名、生成行名、行数。次序也固定四种 public 选择的语义。
DEQUANT_TABLES = (
    ("Table 29", "DRY_COARSE", 51),
    ("Table 30", "DRY_FINE", 101),
    ("Table 31", "WET_COARSE", 21),
    ("Table 32", "WET_FINE", 41),
)

# P1 的 PDF 表名、生成常量名、系数行数；每行按 D0/D1/D2 排列。
DECORRELATOR_TABLES = (
    ("Table 199", "TABLE_199", 8),
    ("Table 200", "TABLE_200", 5),
    ("Table 201", "TABLE_201", 3),
)

DECORRELATOR_REGION_RE = re.compile(
    r"^\s*k(\d+)\s+(\d+)\s*-\s*(\d+)\s+(\d+)\s+(\d+)\s*$"
)
DECORRELATOR_COEFFICIENT_RE = re.compile(
    r"^\s*(\d+)\s+(-?\d+,\d+)\s+(-?\d+,\d+)\s+(-?\d+,\d+)\s*$"
)


def _rows_after(text: str, marker: str, width: int) -> list[tuple[int, int, list[int]]]:
    """抽取 `marker` 之后所有恰好 `width` 列的数值行。

    列头（`23 15 12 9 7 5 3 1`）本身也是一行数字，靠列数过滤掉：它有 8 个数
    而数据行有 1 或 2 个子带号加 8 个值。
    """
    segment = text[text.find(marker) :]
    if not segment:
        raise Failure(f"PDF 中找不到 {marker}")
    rows: list[tuple[int, int, list[int]]] = []
    for line in segment.splitlines():
        match = ROW_RE.match(line)
        if not match:
            continue
        values = [int(v) for v in match.group(3).split()]
        if len(values) != width:
            continue
        low = int(match.group(1))
        high = int(match.group(2)) if match.group(2) else low
        rows.append((low, high, values))
    return rows


def _expand(rows: list[tuple[int, int, list[int]]], width: int, label: str) -> list[list[int]]:
    """把区间行展开成 `[列][子带]`，并要求恰好覆盖 64 个子带。"""
    grid: list[list[int | None]] = [[None] * NUM_QMF_SUBBANDS for _ in range(width)]
    for low, high, values in rows:
        if not 0 <= low <= high < NUM_QMF_SUBBANDS:
            raise Failure(f"{label} 的行区间 {low}-{high} 越出 0..{NUM_QMF_SUBBANDS - 1}")
        for subband in range(low, high + 1):
            for column in range(width):
                if grid[column][subband] is not None:
                    raise Failure(f"{label} 的子带 {subband} 被多行覆盖")
                grid[column][subband] = values[column]
    missing = [sb for sb in range(NUM_QMF_SUBBANDS) if grid[0][sb] is None]
    if missing:
        raise Failure(f"{label} 未覆盖子带 {missing}")
    return [[v for v in column] for column in grid]  # type: ignore[misc]


def _extract_dequant_tables(text: str) -> dict[str, list[tuple[Decimal, int]]]:
    """抽取表 29–32，并保留每项排印的小数位数供舍入容差使用。"""
    tables: dict[str, list[tuple[Decimal, int]]] = {}
    for index, (marker, source_name, levels) in enumerate(DEQUANT_TABLES):
        start = text.find(marker)
        if start < 0:
            raise Failure(f"P2 中找不到 {marker}")
        if index + 1 < len(DEQUANT_TABLES):
            next_marker = DEQUANT_TABLES[index + 1][0]
            end = text.find(next_marker, start + len(marker))
            if end < 0:
                raise Failure(f"{marker} 后找不到 {next_marker}")
        else:
            next_marker = "5.7.3.4"
            end = text.find(next_marker, start + len(marker))
            if end < 0:
                raise Failure(f"{marker} 后找不到 {next_marker}")

        rows: dict[int, tuple[Decimal, int]] = {}
        for line in text[start:end].splitlines():
            match = DEQUANT_ROW_RE.match(line)
            if not match:
                continue
            q = int(match.group(1))
            printed = match.group(2).replace(",", ".")
            decimals = len(printed.partition(".")[2])
            if q in rows:
                raise Failure(f"{marker} 的 q={q} 重复")
            rows[q] = (Decimal(printed), decimals)

        expected = list(range(levels))
        if sorted(rows) != expected:
            missing = sorted(set(expected) - rows.keys())
            extra = sorted(rows.keys() - set(expected))
            raise Failure(f"{marker} 行号不完整：缺 {missing}，多 {extra}")
        tables[source_name] = [rows[q] for q in expected]
    return tables


def _extract_decorrelator_tables(
    text: str,
) -> tuple[list[tuple[int, int, int, int]], dict[str, list[list[Decimal]]]]:
    """抽取 P1 表 198 的区域和表 199–201 的 D0/D1/D2 系数。"""
    region_start = text.find("Table 198")
    region_end = text.find("Table 199", region_start + len("Table 198"))
    if region_start < 0 or region_end < 0:
        raise Failure("P1 中找不到完整的表 198–199")

    indexed_regions: dict[int, tuple[int, int, int, int]] = {}
    for line in text[region_start:region_end].splitlines():
        match = DECORRELATOR_REGION_RE.match(line)
        if not match:
            continue
        region, first, last, delay, order = (int(value) for value in match.groups())
        if region in indexed_regions:
            raise Failure(f"表 198 的区域 k{region} 重复")
        indexed_regions[region] = (first, last, delay, order)
    if sorted(indexed_regions) != [0, 1, 2]:
        raise Failure(f"表 198 区域不完整：{sorted(indexed_regions)}")

    tables: dict[str, list[list[Decimal]]] = {}
    for index, (marker, source_name, row_count) in enumerate(DECORRELATOR_TABLES):
        start = text.find(marker)
        if start < 0:
            raise Failure(f"P1 中找不到 {marker}")
        next_marker = (
            DECORRELATOR_TABLES[index + 1][0]
            if index + 1 < len(DECORRELATOR_TABLES)
            else "5.7.7.4.3"
        )
        end = text.find(next_marker, start + len(marker))
        if end < 0:
            raise Failure(f"{marker} 后找不到 {next_marker}")

        rows: dict[int, list[Decimal]] = {}
        for line in text[start:end].splitlines():
            match = DECORRELATOR_COEFFICIENT_RE.match(line)
            if not match:
                continue
            coefficient_index = int(match.group(1))
            if coefficient_index in rows:
                raise Failure(f"{marker} 的 i={coefficient_index} 重复")
            rows[coefficient_index] = [
                Decimal(value.replace(",", ".")) for value in match.groups()[1:]
            ]
        expected = list(range(row_count))
        if sorted(rows) != expected:
            missing = sorted(set(expected) - rows.keys())
            extra = sorted(rows.keys() - set(expected))
            raise Failure(f"{marker} 行号不完整：缺 {missing}，多 {extra}")
        tables[source_name] = [rows[row] for row in expected]

    return [indexed_regions[index] for index in range(3)], tables


def _extract_ajoc_decorrelator_cycle(text: str) -> list[int]:
    """抽取 P2 `5.7.3.5` 明列的七路去相关器循环。"""
    match = re.search(
        r"used in a cyclic way:\s*([012]\s*,\s*[012]\s*,\s*[012]\s*,\s*"
        r"[012]\s*,\s*[012]\s*,\s*[012]\s*,\s*[012])",
        text,
    )
    if not match:
        raise Failure("P2 5.7.3.5 中找不到七路 decorrelator 循环")
    return [int(value) for value in re.findall(r"[012]", match.group(1))]


def extract_pdf() -> tuple[
    list[list[int]],
    list[list[int]],
    dict[str, list[tuple[Decimal, int]]],
    list[tuple[int, int, int, int]],
    dict[str, list[list[Decimal]]],
    list[int],
]:
    import pdfplumber

    with pdfplumber.open(PDF_PART2) as pdf:
        page_texts = [page.extract_text() or "" for page in pdf.pages]
        part2_text = "\n".join(page_texts)
        dequant_tables = _extract_dequant_tables(part2_text)
        ajoc_decorrelator_cycle = _extract_ajoc_decorrelator_cycle(part2_text)
        for page_text in page_texts:
            if "Table 28: A-JOC parameter band" not in page_text:
                continue
            rows = _rows_after(page_text, "Table 28", len(BAND_COUNTS))
            table_28 = _expand(rows, len(BAND_COUNTS), "表 28")
            break
        else:
            raise Failure("P2 中找不到表 28")

    with pdfplumber.open(PDF_PART1) as pdf:
        page_texts = [page.extract_text() or "" for page in pdf.pages]
        part1_text = "\n".join(page_texts)
        decorrelator_regions, decorrelator_tables = _extract_decorrelator_tables(part1_text)
        for page_text in page_texts:
            if "Table 197: Mapping of parameter bands" not in page_text:
                continue
            rows = _rows_after(page_text, "Table 197", len(ACPL_BAND_COUNTS))
            table_197 = _expand(rows, len(ACPL_BAND_COUNTS), "表 197")
            break
        else:
            raise Failure("P1 中找不到表 197")

    return (
        table_28,
        table_197,
        dequant_tables,
        decorrelator_regions,
        decorrelator_tables,
        ajoc_decorrelator_cycle,
    )


def _generated_source() -> str:
    if not GENERATED_RS.exists():
        raise Failure("缺少本地生成表；先运行 scripts/generate_spec_tables.py")
    return GENERATED_RS.read_text(encoding="utf-8")


def _generated_array(source: str, name: str) -> str:
    match = re.search(rf"const\s+{name}\s*:[^=]+?=\s*\[(.*?)\];", source, re.S)
    if match is None:
        raise Failure(f"{GENERATED_RS.name} 中找不到 {name}")
    return match.group(1)


def parse_rust() -> list[list[int]]:
    """从本地生成文件读出 `[列][子带]`。"""
    source = _generated_source()
    counts = [int(value) for value in re.findall(r"\d+", _generated_array(source, "BAND_COUNTS"))]
    if tuple(counts) != BAND_COUNTS:
        raise Failure(f"生成表的 BAND_COUNTS {counts} 与解析列序 {BAND_COUNTS} 不一致")
    values = [int(value) for value in re.findall(r"\d+", _generated_array(source, "SB_TO_PB"))]
    expected = len(BAND_COUNTS) * NUM_QMF_SUBBANDS
    if len(values) != expected:
        raise Failure(f"SB_TO_PB 应有 {expected} 项，实际 {len(values)}")
    return [
        values[start : start + NUM_QMF_SUBBANDS]
        for start in range(0, len(values), NUM_QMF_SUBBANDS)
    ]


def parse_dequant_rust() -> dict[str, tuple[int, int, int]]:
    """从本地生成文件读取四张表的 `(levels, midpoint, step_numerator)`。"""
    source = _generated_source()
    values = [int(value) for value in re.findall(r"\d+", _generated_array(source, "QUANTIZER_ROWS"))]
    if len(values) != len(DEQUANT_TABLES) * 3:
        raise Failure(f"QUANTIZER_ROWS 应有 {len(DEQUANT_TABLES) * 3} 项，实际 {len(values)}")
    rows = [tuple(values[index : index + 3]) for index in range(0, len(values), 3)]
    return {
        source_name: rows[index]  # type: ignore[dict-item]
        for index, (_, source_name, _) in enumerate(DEQUANT_TABLES)
    }


def parse_decorrelator_rust() -> tuple[
    list[tuple[int, int, int, int]], dict[str, list[list[Decimal]]], list[int]
]:
    """从本地生成文件读取表 198–201 与 A-JOC 七路循环。"""
    source = _generated_source()
    region_values = [
        int(value)
        for value in re.findall(r"\d+", _generated_array(source, "DECORRELATOR_REGIONS"))
    ]
    if len(region_values) != 12:
        raise Failure(f"DECORRELATOR_REGIONS 应有 12 项，实际 {len(region_values)}")
    regions = [tuple(region_values[index : index + 4]) for index in range(0, 12, 4)]

    tables: dict[str, list[list[Decimal]]] = {}
    for _, source_name, row_count in DECORRELATOR_TABLES:
        values = [
            Decimal(value.replace("_", ""))
            for value in re.findall(
                r"[+\-]?\d+(?:\.\d+)?", _generated_array(source, source_name)
            )
        ]
        if len(values) != row_count * 3:
            raise Failure(f"{source_name} 应有 {row_count * 3} 项，实际 {len(values)}")
        tables[source_name] = [values[index : index + 3] for index in range(0, len(values), 3)]

    cycle = [
        int(value)
        for value in re.findall(r"\d+", _generated_array(source, "DECORRELATOR_CYCLE"))
    ]
    if len(cycle) != 7:
        raise Failure(f"AJOC_DECORRELATOR_CYCLE 有 {len(cycle)} 项，应为 7 项")
    return regions, tables, cycle


def compare(table_28: list[list[int]], table_197: list[list[int]], rust: list[list[int]]) -> list[str]:
    problems: list[str] = []

    for index, count in enumerate(BAND_COUNTS):
        mismatches = [
            (sb, rust[index][sb], table_28[index][sb])
            for sb in range(NUM_QMF_SUBBANDS)
            if rust[index][sb] != table_28[index][sb]
        ]
        if mismatches:
            detail = ", ".join(f"sb {sb}: 生成值 {a} != PDF {b}" for sb, a, b in mismatches[:6])
            more = "" if len(mismatches) <= 6 else f"（另有 {len(mismatches) - 6} 处）"
            problems.append(f"TABLE_28 的 {count} 列与表 28 不符：{detail}{more}")

    for acpl_index, count in enumerate(ACPL_BAND_COUNTS):
        ajoc_index = BAND_COUNTS.index(count)
        mismatches = [
            (sb, table_28[ajoc_index][sb], table_197[acpl_index][sb])
            for sb in range(NUM_QMF_SUBBANDS)
            if table_28[ajoc_index][sb] != table_197[acpl_index][sb]
        ]
        if mismatches:
            detail = ", ".join(f"sb {sb}: 表28 {a} != 表197 {b}" for sb, a, b in mismatches[:6])
            problems.append(
                f"表 28 的 {count} 列与表 197 不再逐格相同：{detail}；"
                "ducker 复用 A-CPL 频带划分的前提被推翻，见 ajoc/bands.rs 文件头"
            )

    return problems


def compare_dequant(
    pdf_tables: dict[str, list[tuple[Decimal, int]]],
    rust: dict[str, tuple[int, int, int]],
) -> list[str]:
    """逐项比较精确有理数与 PDF；容差只覆盖排印末位的四舍五入。"""
    problems: list[str] = []
    for marker, source_name, expected_levels in DEQUANT_TABLES:
        levels, midpoint, step_numerator = rust[source_name]
        if levels != expected_levels:
            problems.append(f"{source_name} 声明 {levels} 档，{marker} 有 {expected_levels} 档")
            continue
        if midpoint * 2 + 1 != levels:
            problems.append(f"{source_name} 的 midpoint={midpoint} 未落在 {levels} 档正中")
            continue

        for q, (printed, decimals) in enumerate(pdf_tables[source_name]):
            exact = Decimal((q - midpoint) * step_numerator) / Decimal(2_048)
            tolerance = Decimal(0) if decimals == 0 else Decimal(5).scaleb(-decimals - 1)
            if abs(exact - printed) > tolerance:
                problems.append(
                    f"{marker} q={q}：生成公式 {exact} 与 PDF {printed} 不符"
                )
                if len(problems) >= 12:
                    return problems
    return problems


def compare_decorrelator(
    pdf_regions: list[tuple[int, int, int, int]],
    pdf_tables: dict[str, list[list[Decimal]]],
    pdf_cycle: list[int],
    rust_regions: list[tuple[int, int, int, int]],
    rust_tables: dict[str, list[list[Decimal]]],
    rust_cycle: list[int],
) -> list[str]:
    """逐项比较去相关器区域、系数和 A-JOC 路序。"""
    problems: list[str] = []
    if rust_regions != pdf_regions:
        problems.append(f"TABLE_198 生成值 {rust_regions} 与 PDF {pdf_regions} 不符")

    for marker, source_name, _ in DECORRELATOR_TABLES:
        for row, (rust_values, pdf_values) in enumerate(
            zip(rust_tables[source_name], pdf_tables[source_name])
        ):
            for column, (rust_value, pdf_value) in enumerate(
                zip(rust_values, pdf_values)
            ):
                if rust_value != pdf_value:
                    problems.append(
                        f"{source_name} ({marker}) i={row}, D{column}："
                        f"生成值 {rust_value} != PDF {pdf_value}"
                    )
    if rust_cycle != pdf_cycle:
        problems.append(f"A-JOC decorrelator 循环生成值 {rust_cycle} 与 P2 {pdf_cycle} 不符")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--allow-missing",
        action="store_true",
        help="规范 PDF 或 pdfplumber 缺失时跳过而不失败",
    )
    args = parser.parse_args()

    for pdf in (PDF_PART1, PDF_PART2):
        if not pdf.exists():
            message = f"缺少规范 PDF：{pdf}（先运行 scripts/fetch_specs.py）"
            if args.allow_missing:
                print(f"{message}\n  按 --allow-missing 跳过", file=sys.stderr)
                return 0
            print(message, file=sys.stderr)
            return 1

    try:
        import pdfplumber  # noqa: F401
    except ImportError:
        message = "缺少 pdfplumber"
        if args.allow_missing:
            print(f"{message}\n  按 --allow-missing 跳过", file=sys.stderr)
            return 0
        print(message, file=sys.stderr)
        return 1

    try:
        (
            table_28,
            table_197,
            dequant_tables,
            decorrelator_regions,
            decorrelator_tables,
            decorrelator_cycle,
        ) = extract_pdf()
        rust = parse_rust()
        dequant_rust = parse_dequant_rust()
        rust_regions, rust_decorrelator_tables, rust_cycle = parse_decorrelator_rust()
    except Failure as error:
        print(f"提取失败：{error}", file=sys.stderr)
        return 1

    problems = compare(table_28, table_197, rust)
    problems.extend(compare_dequant(dequant_tables, dequant_rust))
    problems.extend(
        compare_decorrelator(
            decorrelator_regions,
            decorrelator_tables,
            decorrelator_cycle,
            rust_regions,
            rust_decorrelator_tables,
            rust_cycle,
        )
    )
    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1

    print(
        f"A-JOC 频带表核对通过：表 28 的 {len(BAND_COUNTS)} 列 × {NUM_QMF_SUBBANDS} 子带"
        f"与本地生成值逐格相同；其中 {'/'.join(str(c) for c in ACPL_BAND_COUNTS)} 四列"
        "与 P1 表 197 逐格相同；表 29–32 共 214 个反量化值与精确公式相符；"
        "P1 表 198–201 的 3 个区域和 48 个系数、P2 七路循环均与生成值相同"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
