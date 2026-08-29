#!/usr/bin/env python3
"""审计 `macindecode-ac4-bitstream` 内部的层依赖方向（ADR-0011）。

    ./scripts/check_layers.py
    ./scripts/check_layers.py --list

ADR-0011 把长期依赖方向定为 syntax -> decode/engine -> scene。物理拆包之前，
这个方向在同一个 crate 内**没有任何东西强制**：Rust 允许模块之间互相引用，而
`audio-decode` 只在解码层整体缺席时才顺带挡住一部分越界——`asf::dequant` 与
`ajoc::{MAX_*, MatrixKind}` 在默认配置下无门控可见，语法层引用它们不会被任何
配置抓到。本脚本补上那道门禁。

规则只有一条：**语法层不得引用解码层**。反向（解码层引用语法层）是既定的正确
方向，不报。基础层（bit reader、实数函数、Huffman 码本机制）两边都可依赖，但它
自己不得反过来依赖上层。

`huffman` 归入基础层而不是解码层，因为 DRC gains 与 dialogue enhancement 是
**Huffman 编码的元数据**：`presentation_substream/drc_gains.rs` 与
`audio_substream/dialog_enhancement.rs` 都要用 `huffman::tables`，它跟解码层
一起走会造出环。

只用标准库，不需要规范 PDF，也不需要任何 feature。由 CI 的 quality 检查运行。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CRATE_SRC = REPO_ROOT / "crates/macindecode-ac4-bitstream/src"

PRIMITIVE = "primitive"
SYNTAX = "syntax"
DECODE = "decode"

# 层归属的单一声明点。新增顶层模块必须在此登记，否则审计失败——漏登记不得退化
# 成默许。
LAYERS: dict[str, str] = {
    # 基础层：不解释任何 AC-4 语义，两侧共用。
    "reader": PRIMITIVE,
    "math": PRIMITIVE,
    "huffman": PRIMITIVE,
    # 语法与元数据层：有界解析、拓扑、原始/量化 OAMD 与不解释 datatype 的
    # opaque metadata。
    "syncframe": SYNTAX,
    "toc": SYNTAX,
    "presentation": SYNTAX,
    "presentation_substream": SYNTAX,
    "audio_substream": SYNTAX,
    "emdf": SYNTAX,
    "oamd": SYNTAX,
    "substream": SYNTAX,
    "topology": SYNTAX,
    # 解码/engine 层：数值重建、QMF、表 188 对齐与统一 Full A-JOC engine。
    "asf": DECODE,
    "aspx": DECODE,
    "ajoc": DECODE,
    "full_ajoc": DECODE,
    "channel": DECODE,
    "var_element": DECODE,
    "audio_data": DECODE,
    "element_drive": DECODE,
    "substream_audio": DECODE,
    "frame_alignment": DECODE,
    # 生成的 PDF 表。目前是 lib.rs 里的 `pub(crate) mod spec_tables`，但十个消费者
    # （asf/aspx/ajoc）无一在语法层，因此按解码层记；物理拆包时随 decode crate 走。
    "spec_tables": DECODE,
}

# 允许该层引用的层。缺省即禁止。
ALLOWED: dict[str, frozenset[str]] = {
    PRIMITIVE: frozenset({PRIMITIVE}),
    SYNTAX: frozenset({PRIMITIVE, SYNTAX}),
    DECODE: frozenset({PRIMITIVE, SYNTAX, DECODE}),
}

# 不参与审计：crate 根是各层的装配点，testutil 只在测试配置下存在。
EXEMPT_FILES = {"lib.rs", "testutil.rs"}

CFG_TEST = re.compile(r"^#\[cfg\((?:test|all\(\s*test\b)")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


class LayerError(Exception):
    """审计无法继续——缺声明或源码树不符合预期。"""


def strip_test_items(text: str) -> str:
    """去掉 `#[cfg(test)]` 标注的条目，只留生产路径。

    仓库强制 `cargo fmt`，因此顶层条目以第 0 列的 `}` 收尾，缩进 N 的条目以第
    N 列的 `}` 收尾。据此按缩进跳过整个条目，而不是从第一处 `#[cfg(test)]` 一刀
    切到文件尾：全 crate 有 18 个文件在首处 `#[cfg(test)]` 之后仍有生产代码，
    最多的 `full_ajoc/decoder.rs` 有 84 427 个字符。

    但要把这条实现的**实际收益说准**：那 18 个文件当前**全部在解码层**，没有一个
    语法层文件在首处 `#[cfg(test)]` 之后还有生产代码。因此对「语法层不得引用解码
    层」这条唯一的规则而言，一刀切与本实现在当前源码树上给出完全相同的 87 条边，
    本实现多出来的覆盖**现在观察不到**。它是纵深防御，防的是将来某条越界恰好落在
    首处 `#[cfg(test)]` 之后——不是在修一个已观察到的漏报。构造不出能区分两者的
    注入用例，这一点不应被读成「已验证穷尽」。
    """
    lines = text.splitlines()
    kept: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        if not CFG_TEST.match(line.lstrip()):
            kept.append(line)
            index += 1
            continue

        indent = len(line) - len(line.lstrip())
        closing = " " * indent + "}"
        index += 1
        # 跳过该条目自身余下的属性与文档行。属性可以跨多行——`oamd/mod.rs` 的
        # `#[cfg(test)]` 后面跟着一个四行的 `#[expect(...)]`——所以按方括号配平
        # 前进，不能逐行匹配 `^#\[`：那样会停在属性的续行上，把整个测试模块当成
        # 生产代码留下来。
        while index < len(lines):
            stripped = lines[index].strip()
            if stripped.startswith(("///", "//!", "//")):
                index += 1
                continue
            if stripped.startswith("#["):
                depth = 0
                while index < len(lines):
                    depth += lines[index].count("[") - lines[index].count("]")
                    index += 1
                    if depth <= 0:
                        break
                continue
            break
        if index >= len(lines):
            break
        # 无花括号的单行条目（`#[cfg(test)] use ...;`）只跳这一行；带花括号的
        # 条目跳到同缩进的收尾行。
        if "{" not in lines[index]:
            while index < len(lines) and not lines[index].rstrip().endswith(";"):
                index += 1
            index += 1
            continue
        while index < len(lines):
            stripped = lines[index].rstrip()
            if stripped in (closing, closing + ";"):
                index += 1
                break
            index += 1
    return "\n".join(kept)


def brace_group_heads(text: str, start: int) -> tuple[list[str], int]:
    """解析 `crate::{a, b::c, d::{e, f}}` 的顶层名称，返回名称与右括号位置。"""
    heads: list[str] = []
    depth = 0
    position = start
    expect_head = True
    while position < len(text):
        char = text[position]
        if char == "{":
            depth += 1
            position += 1
            continue
        if char == "}":
            depth -= 1
            if depth == 0:
                return heads, position
            position += 1
            continue
        if depth == 1:
            if char == ",":
                expect_head = True
                position += 1
                continue
            if expect_head:
                match = IDENT.match(text, position)
                if match:
                    heads.append(match.group())
                    expect_head = False
                    position = match.end()
                    continue
        position += 1
    raise LayerError("`crate::{...}` 括号不闭合")


def referenced_modules(text: str) -> set[str]:
    """收集本文件引用到的顶层模块名。

    `super::` 一律忽略：它指向同一顶层模块内的父模块，跨不出本模块。
    """
    found: set[str] = set()
    for match in re.finditer(r"\bcrate::", text):
        position = match.end()
        while position < len(text) and text[position].isspace():
            position += 1
        if position >= len(text):
            continue
        if text[position] == "{":
            heads, _ = brace_group_heads(text, position)
            found.update(heads)
            continue
        ident = IDENT.match(text, position)
        if ident:
            found.add(ident.group())
    return found


def owning_module(path: Path) -> str:
    relative = path.relative_to(CRATE_SRC)
    return relative.parts[0][:-3] if len(relative.parts) == 1 else relative.parts[0]


def declared_layer(module: str) -> str:
    layer = LAYERS.get(module)
    if layer is None:
        raise LayerError(
            f"顶层模块 `{module}` 未在 scripts/check_layers.py 的 LAYERS 中登记；"
            "新增模块必须显式声明所属层"
        )
    return layer


def collect_edges() -> dict[tuple[str, str], set[str]]:
    """返回 (源模块, 目标模块) -> 出现该边的文件集合。"""
    if not CRATE_SRC.is_dir():
        raise LayerError(f"找不到 crate 源码目录：{CRATE_SRC}")
    edges: dict[tuple[str, str], set[str]] = {}
    seen_modules = False
    for path in sorted(CRATE_SRC.rglob("*.rs")):
        if path.name in EXEMPT_FILES and path.parent == CRATE_SRC:
            continue
        source = owning_module(path)
        declared_layer(source)
        seen_modules = True
        production = strip_test_items(path.read_text(encoding="utf-8"))
        for target in referenced_modules(production):
            if target == source:
                continue
            declared_layer(target)
            key = (source, target)
            try:
                shown = str(path.relative_to(REPO_ROOT))
            except ValueError:  # 测试可把 CRATE_SRC 指到仓库之外
                shown = str(path)
            edges.setdefault(key, set()).add(shown)
    if not seen_modules:
        raise LayerError("没有扫描到任何模块，源码树布局与预期不符")
    return edges


def violations(edges: dict[tuple[str, str], set[str]]) -> list[tuple[str, str, set[str]]]:
    found = []
    for (source, target), files in sorted(edges.items()):
        if declared_layer(target) not in ALLOWED[declared_layer(source)]:
            found.append((source, target, files))
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="打印完整的模块依赖图而不只是违规项",
    )
    args = parser.parse_args()

    try:
        edges = collect_edges()
        bad = violations(edges)
    except LayerError as error:
        print(f"层依赖审计无法完成：{error}", file=sys.stderr)
        return 1

    if args.list:
        for layer in (PRIMITIVE, SYNTAX, DECODE):
            print(f"[{layer}]")
            for module in sorted(m for m, l in LAYERS.items() if l == layer):
                targets = sorted(t for (s, t) in edges if s == module)
                print(f"  {module:24s} -> {', '.join(targets) or '(无)'}")

    if bad:
        for source, target, files in bad:
            print(
                f"  违规  {declared_layer(source)}:{source}"
                f" -> {declared_layer(target)}:{target}"
                f"（{', '.join(sorted(files))}）",
                file=sys.stderr,
            )
        print(
            f"{len(bad)} 条边违反 ADR-0011 的层依赖方向",
            file=sys.stderr,
        )
        return 1

    print(
        f"层依赖审计通过：{len(LAYERS)} 个模块、{len(edges)} 条边，"
        "语法层没有引用解码层"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
