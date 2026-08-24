#!/usr/bin/env python3
"""从锁定的 ETSI PDF 生成本地 Rust 规范表。

    ./scripts/fetch_specs.py
    ./scripts/generate_spec_tables.py
    ./scripts/generate_spec_tables.py --output /tmp/spec_tables.rs

输出默认写入 ``spec/generated/ts103190_pdf_tables.rs``。该文件及其来源 PDF
不进入版本控制或 crate 包；构建脚本只消费生成结果，不联网也不调用 Python。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from decimal import Decimal
from pathlib import Path

from _pdf_tables import Failure, merge_cells, words_by_line
from check_ajoc_tables import (
    BAND_COUNTS,
    DECORRELATOR_TABLES,
    DEQUANT_TABLES,
    extract_pdf as extract_ajoc,
)
from check_aspx_tables import extract as extract_aspx
from check_sfb_tables import extract as extract_sfb

REPO_ROOT = Path(__file__).resolve().parent.parent
SPEC_DIR = REPO_ROOT / "spec"
MANIFEST = SPEC_DIR / "MANIFEST.json"
PDF_PART1 = SPEC_DIR / "ts_10319001v010401p.pdf"
DEFAULT_OUTPUT = SPEC_DIR / "generated" / "ts103190_pdf_tables.rs"


def _verify_pdf_inputs() -> None:
    """按清单复核生成器实际读取的两份 PDF。"""
    try:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise Failure(f"读取 {MANIFEST} 失败：{error}") from error

    documents = {
        document.get("filename"): document
        for document in manifest.get("documents", [])
    }
    for path in (PDF_PART1, SPEC_DIR / "ts_10319002v010301p.pdf"):
        document = documents.get(path.name)
        if document is None:
            raise Failure(f"{MANIFEST} 未锁定 {path.name}")
        if not path.exists():
            raise Failure(f"缺少 {path}；先运行 scripts/fetch_specs.py")
        try:
            payload = path.read_bytes()
        except OSError as error:
            raise Failure(f"读取 {path} 失败：{error}") from error
        actual = hashlib.sha256(payload).hexdigest()
        expected = document.get("sha256")
        if actual != expected:
            raise Failure(f"{path.name} 哈希不匹配：期望 {expected}，实际 {actual}")
        expected_size = document.get("size")
        if expected_size is not None and len(payload) != expected_size:
            raise Failure(
                f"{path.name} 大小为 {len(payload)}，清单记为 {expected_size}"
            )


def _segment(text: str, marker: str, next_marker: str) -> str:
    start = text.find(marker)
    if start < 0:
        raise Failure(f"PDF 中找不到 {marker}")
    end = text.find(next_marker, start + len(marker))
    if end < 0:
        raise Failure(f"{marker} 后找不到 {next_marker}")
    return text[start:end]


def _extract_asf_auxiliary() -> dict[str, object]:
    import pdfplumber

    with pdfplumber.open(PDF_PART1) as pdf:
        pages = list(pdf.pages)
        page_texts = [page.extract_text() or "" for page in pages]
        text = "\n".join(page_texts)

        def page_rows(marker: str) -> list[list[str]]:
            for page, page_text in zip(pages, page_texts):
                if marker not in page_text:
                    continue
                return [merge_cells(line) for line in words_by_line(page)]
            raise Failure(f"PDF 中找不到 {marker}")

        def table_rows(marker: str, next_marker: str | None = None) -> list[list[str]]:
            rows = page_rows(marker)
            compact_marker = marker.replace(" ", "")
            start = next(
                (index for index, row in enumerate(rows) if compact_marker in "".join(row).replace(" ", "")),
                None,
            )
            if start is None:
                raise Failure(f"表格行中找不到 {marker}")
            end = len(rows)
            if next_marker is not None:
                compact_next = next_marker.replace(" ", "")
                end = next(
                    (
                        index
                        for index, row in enumerate(rows[start + 1 :], start + 1)
                        if compact_next in "".join(row).replace(" ", "")
                    ),
                    len(rows),
                )
            return rows[start:end]

    frame_bases = [2048, 1920, 1536, 1024, 960, 768, 512, 384]
    long_rows = [
        row
        for row in page_rows("Table 99:")
        if len(row) == 4 and row[0].isdigit() and int(row[0]) in frame_bases
    ]
    if [int(row[0]) for row in long_rows] != frame_bases:
        raise Failure(f"表 99 的 48 kHz 帧长不完整：{long_rows}")

    partial = [
        row
        for row in table_rows("Table 100:", "Table 101:")
        if len(row) == 5 and row[0].isdigit() and int(row[0]) in {2048, 1920, 1536}
    ]
    if len(partial) != 3:
        raise Failure(f"表 100 应有 3 行，实际 {partial}")

    short = [
        row
        for row in table_rows("Table 103:", "Table 104:")
        if len(row) == 5 and row[0].isdigit() and int(row[0]) in frame_bases
    ]
    if len(short) != 5:
        raise Failure(f"表 103 应有 5 行，实际 {short}")

    bit_rows = [
        row
        for row in table_rows("Table 106:", "Table 107:")
        if len(row) == 4 and row[0].isdigit()
    ]
    expected_lengths = [2048, 1920, 1536, 1024, 960, 768, 512, 480, 384, 256, 240, 192, 128, 120, 96]
    if [int(row[0]) for row in bit_rows] != expected_lengths:
        raise Failure(f"表 106 的变换长度不完整：{bit_rows}")

    group_long: list[list[int]] = []
    for row in table_rows("Table 109:", "Table 110:"):
        numeric = [int(value) for value in row if value.isdigit()]
        if len(numeric) >= 3 and numeric[-3] <= 3 and numeric[-2] <= 3:
            group_long.append(numeric[-3:])
    if len(group_long) != 16:
        raise Failure(f"表 109 应有 16 行，实际 {group_long}")

    group_short: list[tuple[int, int, int]] = []
    current_group = -1
    for row in table_rows("Table 110:", "4.3.6.2.5"):
        numeric = [int(value) for value in row if value.isdigit()]
        if any(value in {"1024,960,768", "1024,960,768-"} for value in row):
            current_group = 0
        elif any(value in {"512,384", "512,384-"} for value in row):
            current_group = 1
        if current_group < 0 or len(numeric) < 2:
            continue
        index, bits = numeric[-2], numeric[-1]
        if index > 3 or bits > 7:
            continue
        group_short.append((current_group, index, bits))
    if group_short != [
        (0, 0, 7), (0, 1, 3), (0, 2, 1), (0, 3, 0),
        (1, 0, 3), (1, 1, 1), (1, 2, 0),
    ]:
        raise Failure(f"表 110 抽取结果异常：{group_short}")

    dimensions_match = re.search(
        r"Table A\.14: CB_DIM.*?CB_DIM\s+((?:\d+\s+){10}\d+)", text, re.S
    )
    unsigned_match = re.search(
        r"Table A\.15: UNSIGNED_CB.*?UNSIGNED_CB\s+"
        r"((?:(?:true|false)\s+){10}(?:true|false))",
        text,
        re.S,
    )
    if dimensions_match is None or unsigned_match is None:
        raise Failure("未抽到表 A.14/A.15")
    dimensions = [int(value) for value in dimensions_match.group(1).split()]
    unsigned = [value == "true" for value in unsigned_match.group(1).split()]

    codebook_rows: list[tuple[int, bool, int, int, int]] = [(0, False, 0, 0, 0)]
    for number in range(1, 12):
        marker = f"Table A.{number + 1}:"
        next_marker = f"Table A.{number + 2}:"
        body = _segment(text, marker, next_marker)
        length_match = re.search(r"codebook_length\s+(\d+)", body)
        modulus_match = re.search(r"cb_mod\s+(\d+)", body)
        offset_match = re.search(r"cb_off\s+(\d+)", body)
        if length_match is None or modulus_match is None or offset_match is None:
            raise Failure(f"{marker} 的元数据不完整")
        codebook_rows.append(
            (
                dimensions[number - 1],
                unsigned[number - 1],
                int(modulus_match.group(1)),
                int(offset_match.group(1)),
                int(length_match.group(1)),
            )
        )

    alpha_by_length: dict[int, int] = {}
    for row in table_rows("Table 186:", "The KBD windows are defined"):
        values = row[-10:]
        if len(values) != 10 or not all(value.isdigit() for value in values[:9]):
            continue
        alpha_halves = int(Decimal(values[9].replace(",", ".")) * 2)
        for length in (int(values[0]), int(values[1]), int(values[2])):
            alpha_by_length[length] = alpha_halves
    alpha_halves = [alpha_by_length.get(length) for length in expected_lengths]
    if any(value is None for value in alpha_halves):
        raise Failure(f"表 186 的 48 kHz alpha 不完整：{alpha_halves}")

    return {
        "frame_bases": frame_bases,
        "partial": [[int(value) for value in row] for row in partial],
        "short": [
            [int(row[0]), *[None if value == "×" else int(value) for value in row[1:]]]
            for row in short
        ],
        "bit_rows": [
            [int(row[0]), int(row[1]), int(row[2]), None if row[3] == "N/A" else int(row[3])]
            for row in bit_rows
        ],
        "group_long": group_long,
        "group_short": group_short,
        "codebooks": codebook_rows,
        "alpha_halves": alpha_halves,
    }


def _rust_array(values: list[object] | tuple[object, ...]) -> str:
    def render(value: object) -> str:
        if value is None:
            return "None"
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, list):
            return _rust_array(value)
        if isinstance(value, tuple):
            return "(" + ", ".join(render(item) for item in value) + ")"
        return str(value)

    return "[" + ", ".join(render(value) for value in values) + "]"


def _rust_option(value: object) -> str:
    return "None" if value is None else f"Some({value})"


def _derive_quantizers(
    tables: dict[str, list[tuple[Decimal, int]]],
) -> list[tuple[int, int, int]]:
    rows: list[tuple[int, int, int]] = []
    for _, source_name, expected_levels in DEQUANT_TABLES:
        values = tables[source_name]
        levels = len(values)
        midpoint = (levels - 1) // 2
        if levels != expected_levels or levels != midpoint * 2 + 1:
            raise Failure(f"{source_name} 的行数无法形成中心对称量化器")
        numerator = int((values[-1][0] * Decimal(2048) / Decimal(midpoint)).to_integral_value())
        for q, (printed, decimals) in enumerate(values):
            exact = Decimal((q - midpoint) * numerator) / Decimal(2048)
            tolerance = Decimal(0) if decimals == 0 else Decimal(5).scaleb(-decimals - 1)
            if abs(exact - printed) > tolerance:
                raise Failure(f"{source_name} q={q} 不能由统一精确步长还原")
        rows.append((levels, midpoint, numerator))
    return rows


def generate() -> str:
    counts, offsets = extract_sfb()
    aspx = extract_aspx()
    (
        table_28,
        table_197,
        dequant_tables,
        regions,
        decorrelator_tables,
        cycle,
    ) = extract_ajoc()
    auxiliary = _extract_asf_auxiliary()

    lengths = sorted(counts, reverse=True)
    if lengths != list(auxiliary["bit_rows"][index][0] for index in range(15)):
        raise Failure("附录 B 与表 106 的变换长度顺序不一致")
    for column, count in enumerate(BAND_COUNTS[1:5], start=1):
        if table_28[column] != table_197[column - 1]:
            raise Failure(f"表 28 的 {count} 带列与表 197 不一致")

    lines = [
        "// @generated by scripts/generate_spec_tables.py; do not commit or redistribute.",
        "// Sources are pinned by spec/MANIFEST.json.",
        "",
        "pub(crate) mod asf {",
        f"    pub(crate) const TRANSFORM_LENGTHS_48: [u16; {len(lengths)}] = {_rust_array(lengths)};",
        f"    pub(crate) const NUM_SFB_48: [u8; {len(lengths)}] = {_rust_array([counts[length] for length in lengths])};",
    ]
    for length in lengths:
        lines.append(
            f"    const SFB_OFFSET_{length}: [u16; {len(offsets[length])}] = "
            f"{_rust_array(offsets[length])};"
        )
    lines.extend(
        [
            f"    pub(crate) const SFB_OFFSETS_48: [&[u16]; {len(lengths)}] = "
            + _rust_array([f"&SFB_OFFSET_{length}" for length in lengths])
            + ";",
            "    pub(crate) type SpectrumCodebookRow = (u8, bool, u16, i16, u16);",
            f"    pub(crate) const SPECTRUM_CODEBOOK_ROWS: [SpectrumCodebookRow; 12] = {_rust_array(auxiliary['codebooks'])};",
            f"    pub(crate) const N_MSFB_BITS_48: [u8; 15] = {_rust_array([row[1] for row in auxiliary['bit_rows']])};",
            f"    pub(crate) const N_SIDE_BITS_48: [u8; 15] = {_rust_array([row[2] for row in auxiliary['bit_rows']])};",
            "    pub(crate) const N_MSFBL_BITS_48: [Option<u8>; 15] = "
            + _rust_array([_rust_option(row[3]) for row in auxiliary["bit_rows"]])
            + ";",
            "    pub(crate) const PARTIAL_TRANSFORM_48: [(u16, [u16; 4]); 3] = "
            + _rust_array([(row[0], row[1:]) for row in auxiliary["partial"]])
            + ";",
            "    pub(crate) const SHORT_BASE_TRANSFORM_48: [(u16, [Option<u16>; 4]); 5] = "
            + _rust_array(
                [(row[0], [_rust_option(value) for value in row[1:]]) for row in auxiliary["short"]]
            )
            + ";",
            f"    pub(crate) const FRAME_LEN_BASES_48: [u16; 8] = {_rust_array(auxiliary['frame_bases'])};",
            "    pub(crate) const N_GRP_BITS_LONG_BASE: [[u8; 4]; 4] = "
            + _rust_array([[row[2] for row in auxiliary["group_long"] if row[0] == first] for first in range(4)])
            + ";",
            "    pub(crate) const N_GRP_BITS_SHORT_BASE: [[Option<u8>; 4]; 2] = "
            + _rust_array(
                [
                    [
                        _rust_option(
                            next(
                                (
                                    bits
                                    for row_group, row_index, bits in auxiliary["group_short"]
                                    if row_group == group_id and row_index == target_index
                                ),
                                None,
                            )
                        )
                        for target_index in range(4)
                    ]
                    for group_id in range(2)
                ]
            )
            + ";",
            f"    pub(crate) const KBD_ALPHA_HALVES_48: [u8; 15] = {_rust_array(auxiliary['alpha_halves'])};",
            "}",
            "",
            "pub(crate) mod aspx {",
            f"    pub(crate) const SBG_TEMPLATE_LOWRES: [u8; {len(aspx['templates']['lowres'])}] = {_rust_array(aspx['templates']['lowres'])};",
            f"    pub(crate) const SBG_TEMPLATE_HIGHRES: [u8; {len(aspx['templates']['highres'])}] = {_rust_array(aspx['templates']['highres'])};",
            f"    pub(crate) const NUM_TS_IN_ATS: [(u16, u8, u8); 8] = {_rust_array(aspx['table_192'])};",
            "    pub(crate) type TabBorderRow = (u8, [u8; 2], [u8; 3], [u8; 5]);",
            "    pub(crate) const TAB_BORDER: [TabBorderRow; 5] = "
            + _rust_array(aspx["table_194"])
            + ";",
            "}",
            "",
            "pub(crate) mod ajoc {",
            f"    pub(crate) const BAND_COUNTS: [u8; 8] = {_rust_array(BAND_COUNTS)};",
            f"    pub(crate) const SB_TO_PB: [[u8; 64]; 8] = {_rust_array(table_28)};",
            f"    pub(crate) const QUANTIZER_ROWS: [(i16, i16, i16); 4] = {_rust_array(_derive_quantizers(dequant_tables))};",
            f"    pub(crate) const DECORRELATOR_REGIONS: [(u8, u8, u8, u8); 3] = {_rust_array(regions)};",
        ]
    )
    for _, source_name, row_count in DECORRELATOR_TABLES:
        rows = decorrelator_tables[source_name]
        rendered_rows = "[" + ", ".join(
            "[" + ", ".join(str(value) for value in row) + "]" for row in rows
        ) + "]"
        lines.append(
            f"    pub(crate) const {source_name}: [[f64; 3]; {row_count}] = {rendered_rows};"
        )
    lines.extend(
        [
            f"    pub(crate) const DECORRELATOR_CYCLE: [u8; 7] = {_rust_array(cycle)};",
            "}",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()

    try:
        _verify_pdf_inputs()
        generated = generate().encode("utf-8")
    except (Failure, ImportError) as error:
        print(f"生成规范表失败：{error}", file=sys.stderr)
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(generated)
    digest = hashlib.sha256(generated).hexdigest()
    print(f"已生成 {args.output}（{len(generated)} 字节，sha256 {digest}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
