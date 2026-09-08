#!/usr/bin/env python3
"""审计 bitstream 与 decode crate 内部的层依赖方向（ADR-0011、ADR-0013）。

    ./scripts/check_layers.py
    ./scripts/check_layers.py --list

ADR-0011 把长期依赖方向定为 syntax -> decode/engine -> scene；ADR-0013 已完成
bitstream/decode 物理拆包。Cargo 强制 crate 间方向，本脚本继续拒绝各 crate 内从低层到
高层的模块引用，并对未登记的新顶层模块失败关闭。decode 内的 Huffman metadata 层可依赖
Huffman primitive，但不得依赖 DSP。

只用标准库，不需要规范 PDF，也不需要任何 feature。由 CI 的 quality 检查运行。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

PRIMITIVE = "primitive"
SYNTAX = "syntax"
METADATA = "metadata"
DSP = "dsp"

# 层归属的单一声明点。新增顶层模块必须在此登记，否则审计失败——漏登记不得退化
# 成默许。
#
# 物理拆包后 syntax -> decode 的方向已由 Cargo 强制（bitstream 不依赖 decode），
# 本脚本因此转为守各 crate 内部的方向，以及「新模块必须登记」这条 fail-closed。
BITSTREAM_LAYERS: dict[str, str] = {
    # 基础层：不解释任何 AC-4 语义。
    "reader": PRIMITIVE,
    "math": PRIMITIVE,
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
}

DECODE_LAYERS: dict[str, str] = {
    # 码本机制：只在 BitReader 上走 trie，不解释任何 AC-4 语义。
    "huffman": PRIMITIVE,
    # Huffman 编码的**元数据**解码。它们按规范不是 DSP，放在本 crate 只因为消费
    # 同一批随附 C 表；不得反过来依赖 DSP。
    "drc_gains": METADATA,
    "dialog_enhancement": METADATA,
    # 数值重建、QMF、表 188 对齐与统一 Full A-JOC engine。
    "asf": DSP,
    "aspx": SYNTAX,
    "ajoc": SYNTAX,
    "audio_syntax": SYNTAX,
    "full_ajoc": DSP,
    "channel": SYNTAX,
    "channel::matrix": DSP,
    "var_element": SYNTAX,
    "audio_data": SYNTAX,
    "element_drive": DSP,
    "substream_audio": SYNTAX,
    "frame_alignment": DSP,
    # 生成的 PDF 表，lib.rs 里的 `pub(crate) mod spec_tables`。
    "spec_tables": PRIMITIVE,
    "asf::framing": SYNTAX,
    "asf::tables": SYNTAX,
    "asf::spectrum": SYNTAX,
    "asf::dequant": DSP,
    "asf::imdct": DSP,
    "asf::reconstruct": DSP,
    "aspx::bands": SYNTAX,
    "aspx::codebooks": SYNTAX,
    "aspx::frames": SYNTAX,
    "aspx::reach": SYNTAX,
    "aspx::syntax": SYNTAX,
    "aspx::tables": SYNTAX,
    "aspx::dequant": DSP,
    "aspx::envelope": DSP,
    "aspx::hfadjust": DSP,
    "aspx::hfassemble": DSP,
    "aspx::hfgain": DSP,
    "aspx::hfgen": DSP,
    "aspx::interleave": DSP,
    "aspx::limiter": DSP,
    "aspx::lowband": DSP,
    "aspx::noisegen": DSP,
    "aspx::patches": DSP,
    "aspx::pipeline": DSP,
    "aspx::preflatten": DSP,
    "aspx::qmf": DSP,
    "aspx::state": DSP,
    "aspx::tna": DSP,
    "aspx::tonegen": DSP,
    "aspx::workspace": DSP,
    "ajoc::bands": SYNTAX,
    "ajoc::de": SYNTAX,
    "ajoc::syntax": SYNTAX,
    "ajoc::decorrelator": DSP,
    "ajoc::dequant": DSP,
    "ajoc::diff": DSP,
    "ajoc::interp": DSP,
    "ajoc::reconstruction": DSP,
}

METADATA_LAYERS = {
    "input": SYNTAX, "selection": SYNTAX, "error": SYNTAX,
    "audio": METADATA, "presentation": METADATA, "group_oamd": METADATA,
    "state": METADATA, "session": METADATA, "extended": METADATA,
    "oamd": METADATA, "layout": METADATA,
}

CRATES: dict[str, dict[str, str]] = {
    "macindecode-ac4-bitstream": BITSTREAM_LAYERS,
    "macindecode-ac4-decode": DECODE_LAYERS,
    "macindecode-ac4-metadata": METADATA_LAYERS,
}

# 当前被审计的 crate。`main` 逐个 crate 重新绑定这两个全局量。
CRATE_SRC = REPO_ROOT / "crates/macindecode-ac4-bitstream/src"
LAYERS: dict[str, str] = BITSTREAM_LAYERS

# 允许该层引用的层。缺省即禁止。
ALLOWED: dict[str, frozenset[str]] = {
    PRIMITIVE: frozenset({PRIMITIVE}),
    SYNTAX: frozenset({PRIMITIVE, SYNTAX}),
    METADATA: frozenset({PRIMITIVE, SYNTAX, METADATA}),
    DSP: frozenset({PRIMITIVE, SYNTAX, METADATA, DSP}),
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
        # 条目跳到同缩进的收尾行。rustfmt 会把空函数保留成 `fn helper() {}`，
        # 这种条目在起始行已经闭合，不能继续吞掉后面的生产代码。
        if "{" not in lines[index]:
            while index < len(lines) and not lines[index].rstrip().endswith(";"):
                index += 1
            index += 1
            continue
        if lines[index].count("{") <= lines[index].count("}"):
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


def referenced_modules(
    text: str,
    *,
    module_depth: int | None = None,
    known_modules: set[str] | None = None,
) -> set[str]:
    """收集本文件引用到的顶层模块名。

    `super::` 是否跨顶层模块取决于当前文件的模块深度：顶层 `meta.rs` 中的
    `super::dsp` 等价于 `crate::dsp`，而 `meta/child.rs` 中的 `super::helper`
    仍留在 `meta` 内。内联模块无法只凭文件路径精确定位；跨过文件模块根时仅把
    已登记的顶层模块当作依赖，避免把 `tables { use super::Type; }` 中的类型名
    误判为模块。
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
    if module_depth is None:
        return found

    registered = set(LAYERS) if known_modules is None else known_modules
    for match in re.finditer(r"\bsuper(?:(?:::)[ \t\r\n]*super)*(?:::)", text):
        hops = match.group().count("super")
        if hops < module_depth:
            continue
        position = match.end()
        while position < len(text) and text[position].isspace():
            position += 1
        if position >= len(text):
            continue
        if text[position] == "{":
            heads, _ = brace_group_heads(text, position)
        else:
            ident = IDENT.match(text, position)
            heads = [] if ident is None else [ident.group()]
        found.update(head for head in heads if head in registered)
    return found


def module_parts(path: Path) -> tuple[str, ...]:
    """返回标准文件布局对应的模块路径，不含 crate 根。"""
    relative = path.relative_to(CRATE_SRC)
    directories = list(relative.parts[:-1])
    stem = relative.stem
    if stem != "mod":
        directories.append(stem)
    return tuple(directories)


def owning_module(path: Path) -> str:
    parts = module_parts(path)
    if not parts:
        raise LayerError(f"无法确定源码文件的顶层模块：{path}")
    # 混合 facade 的子模块单独登记，避免把 ASF/A-SPX 语法与 DSP 一起放行。
    if any(key.startswith(parts[0] + "::") for key in LAYERS) and len(parts) > 1:
        prefix = "::".join(parts[:2])
        declared_layer(prefix)
    for length in range(len(parts), 0, -1):
        candidate = "::".join(parts[:length])
        if candidate in LAYERS:
            return candidate
    return parts[0]


def declared_layer(module: str) -> str:
    layer = LAYERS.get(module)
    if layer is None:
        raise LayerError(
            f"顶层模块 `{module}` 未在 scripts/check_layers.py 的 LAYERS 中登记；"
            "新增模块必须显式声明所属层"
        )
    return layer


def use_paths(body: str) -> list[tuple[tuple[str, ...], str | None]]:
    """展开 Rust use 树，保留子模块与 as 别名；不把 facade 名当作实际目标。"""
    tokens = re.findall(r"[A-Za-z_][A-Za-z0-9_]*|::|[{},*]", body)
    position = 0
    paths = []

    def group(prefix=()):
        nonlocal position
        while position < len(tokens) and tokens[position] != "}":
            if tokens[position] == ",":
                position += 1
                continue
            path = list(prefix)
            alias = None
            while position < len(tokens):
                token = tokens[position]
                if token == "::":
                    position += 1
                    continue
                if token in ("{", "}", ",", "as"):
                    break
                path.append(token)
                position += 1
            if position < len(tokens) and tokens[position] == "{":
                position += 1
                group(tuple(path))
                if position >= len(tokens) or tokens[position] != "}":
                    raise LayerError("use 树缺少右花括号")
                position += 1
            else:
                if position < len(tokens) and tokens[position] == "as":
                    position += 1
                    if position < len(tokens):
                        alias = tokens[position]
                        position += 1
                paths.append((tuple(path), alias))
    group()
    return paths


def absolute_path(parts: tuple[str, ...], current: tuple[str, ...]):
    if not parts:
        return None
    if parts[0] == "crate":
        return parts[1:]
    if parts[0] in ("self", "super"):
        base = current
        while parts and parts[0] in ("self", "super"):
            if parts[0] == "super":
                base = base[:-1]
            parts = parts[1:]
        return base + parts
    if parts[0] in {name.split("::")[0] for name in LAYERS}:
        return parts
    # `use syntax::Type` 是当前模块内的 re-export。
    if "::".join(current + parts[:1]) in LAYERS:
        return current + parts
    return None  # 外部 crate 的方向由 Cargo 管理。


def qualified_target(parts, aliases, globs, symbols, visiting=frozenset()):
    if not parts or parts in visiting:
        return set()
    visiting = visiting | {parts}
    for length in range(len(parts), 0, -1):
        prefix = parts[:length]
        if prefix in aliases and aliases[prefix] != prefix:
            return qualified_target(aliases[prefix] + parts[length:], aliases, globs, symbols, visiting)
        if prefix in symbols:
            return {symbols[prefix]}
    found = set()
    for prefix, target in globs:
        if parts[:len(prefix)] != prefix:
            continue
        candidate = target + parts[len(prefix):]
        if any(candidate[:length] in symbols or candidate[:length] in aliases for length in range(1, len(candidate) + 1)):
            found.update(qualified_target(candidate, aliases, globs, symbols, visiting))
    if found:
        return found
    for length in range(len(parts), 0, -1):
        candidate = "::".join(parts[:length])
        if candidate in LAYERS:
            return {candidate}
    declared_layer(parts[0])  # 未登记的 crate 顶层路径失败关闭。
    return {parts[0]}


def collect_edges() -> dict[tuple[str, str], set[str]]:
    """返回 (源模块, 目标模块) -> 出现该边的文件集合。"""
    if not CRATE_SRC.is_dir():
        raise LayerError(f"找不到 crate 源码目录：{CRATE_SRC}")
    edges: dict[tuple[str, str], set[str]] = {}
    files = sorted(CRATE_SRC.rglob("*.rs"))
    aliases, symbols, globs, sources = {}, {}, [], {}
    use_pattern = re.compile(r"\buse\s+([^;]+);", re.S)
    for path in files:
        production = strip_test_items(path.read_text(encoding="utf-8"))
        production = re.sub(r"//[^\n]*", "", production)
        current = () if path.name == "lib.rs" and path.parent == CRATE_SRC else module_parts(path)
        sources[path] = (production, current)
        if path.name not in EXEMPT_FILES or path.parent != CRATE_SRC:
            owner = owning_module(path)
            declared_layer(owner)
            for match in re.finditer(r"^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|type|const|fn|trait)\s+(\w+)", production, re.M):
                symbols[current + (match.group(1),)] = owner
        for statement in use_pattern.finditer(production):
            for parts, alias in use_paths(statement.group(1)):
                target = absolute_path(parts, current)
                if not target:
                    continue
                if target[-1] == "*":
                    globs.append((current, target[:-1]))
                else:
                    aliases[current + (alias or target[-1],)] = target
    seen_modules = False
    for path in files:
        if path.name in EXEMPT_FILES and path.parent == CRATE_SRC:
            continue
        parts = module_parts(path)
        source = owning_module(path)
        declared_layer(source)
        seen_modules = True
        production, current = sources[path]
        targets = set()
        for statement in use_pattern.finditer(production):
            for target, _ in use_paths(statement.group(1)):
                absolute = absolute_path(target, current)
                if absolute:
                    targets.update(qualified_target(absolute, aliases, globs, symbols))
        outside_use = use_pattern.sub("", production)
        for match in re.finditer(r"\b(?:crate|super|self)(?:::\s*[A-Za-z_][A-Za-z0-9_]*)+", outside_use):
            absolute = absolute_path(tuple(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", match.group())), current)
            if absolute:
                targets.update(qualified_target(absolute, aliases, globs, symbols))
        for target in targets:
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
    global CRATE_SRC, LAYERS

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="打印完整的模块依赖图而不只是违规项",
    )
    args = parser.parse_args()

    total_modules = total_edges = 0
    failed = False
    for crate, layers in CRATES.items():
        CRATE_SRC = REPO_ROOT / "crates" / crate / "src"
        LAYERS = layers
        try:
            edges = collect_edges()
            bad = violations(edges)
        except LayerError as error:
            print(f"{crate} 层依赖审计无法完成：{error}", file=sys.stderr)
            return 1

        total_modules += len(layers)
        total_edges += len(edges)

        if args.list:
            print(f"=== {crate} ===")
            for layer in (PRIMITIVE, SYNTAX, METADATA, DSP):
                members = sorted(m for m, l in layers.items() if l == layer)
                if not members:
                    continue
                print(f"[{layer}]")
                for module in members:
                    targets = sorted(t for (s, t) in edges if s == module)
                    print(f"  {module:24s} -> {', '.join(targets) or '(无)'}")

        if bad:
            failed = True
            for source, target, files in bad:
                print(
                    f"  违规  {crate}  {declared_layer(source)}:{source}"
                    f" -> {declared_layer(target)}:{target}"
                    f"（{', '.join(sorted(files))}）",
                    file=sys.stderr,
                )

    if failed:
        print("存在违反 ADR-0011 / ADR-0013 层依赖方向的边", file=sys.stderr)
        return 1

    print(
        f"层依赖审计通过：{len(CRATES)} 个 crate、{total_modules} 个模块、"
        f"{total_edges} 条边"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
