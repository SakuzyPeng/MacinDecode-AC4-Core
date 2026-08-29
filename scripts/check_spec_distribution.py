#!/usr/bin/env python3
"""审计规范输入、生成表与 crates.io 包的分发边界。

默认检查版本控制内容、构建脚本内置摘要和全部 crate 的打包清单；传入
``--generated`` 时还核对用户本地生成文件的摘要与忽略规则。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
MANIFEST = REPO_ROOT / "spec" / "MANIFEST.json"
SPEC_LOCK = (
    REPO_ROOT
    / "crates"
    / "macindecode-ac4-bitstream"
    / "build_support"
    / "spec_lock.rs"
)

FORBIDDEN_TRACKED_SUFFIXES = {".pdf", ".zip", ".m4a", ".wav", ".caf"}
FORBIDDEN_PACKAGE_SUFFIXES = FORBIDDEN_TRACKED_SUFFIXES | {".c"}
FORBIDDEN_RUST_TABLES = {
    "crates/macindecode-ac4-bitstream/src/asf/tables.rs": (
        r"const\s+SFB_OFFSET_\d+\s*:",
        r"const\s+SPECTRUM_CODEBOOKS\s*:\s*\[[^=]+?=\s*\[",
        r"const\s+N_MSFB_BITS_48\s*:[^=]+?=\s*\[",
    ),
    "crates/macindecode-ac4-bitstream/src/asf/imdct.rs": (
        r"const\s+KBD_ALPHA_HALVES_48\s*:[^=]+?=\s*\[",
    ),
    "crates/macindecode-ac4-bitstream/src/aspx/tables.rs": (
        r"const\s+SBG_TEMPLATE_(?:LOW|HIGH)RES\s*:",
        r"const\s+NUM_TS_IN_ATS\s*:[^=]+?=\s*\[",
    ),
    "crates/macindecode-ac4-bitstream/src/aspx/frames.rs": (
        r"const\s+TAB_BORDER\s*:[^=]+?=\s*\[",
    ),
    "crates/macindecode-ac4-bitstream/src/ajoc/bands.rs": (
        r"const\s+TABLE_28\s*:[^=]+?=\s*\[",
        r"static\s+SB_TO_PB\s*:[^=]+?=\s*\[",
    ),
    "crates/macindecode-ac4-bitstream/src/ajoc/dequant.rs": (
        r"const\s+(?:DRY|WET)_(?:COARSE|FINE)\s*:\s*Quantizer\s*=",
    ),
    "crates/macindecode-ac4-bitstream/src/ajoc/decorrelator.rs": (
        r"const\s+TABLE_(?:198|199|200|201)\s*:[^=]+?=\s*\[",
        r"AJOC_DECORRELATOR_CYCLE\s*:[^=]+?=\s*\[",
    ),
}


def command(*args: str) -> str:
    result = subprocess.run(
        args,
        cwd=REPO_ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"{' '.join(args)} 失败：{detail}")
    return result.stdout


def tracked_files() -> list[str]:
    return [path for path in command("git", "ls-files").splitlines() if path]


def audit_tracked(paths: list[str]) -> list[str]:
    problems: list[str] = []
    for relative in paths:
        path = Path(relative)
        if relative.startswith("spec/") and relative != "spec/MANIFEST.json":
            problems.append(f"规范目录中存在被跟踪文件：{relative}")
        if path.suffix.lower() in FORBIDDEN_TRACKED_SUFFIXES:
            problems.append(f"版本控制中存在媒体或规范制品：{relative}")
        if path.name in {".env.local", "ts_103190_tables.c", "ts_103190_tables_part2.c"}:
            problems.append(f"版本控制中存在本地或规范输入：{relative}")

    for relative, patterns in FORBIDDEN_RUST_TABLES.items():
        source = (REPO_ROOT / relative).read_text(encoding="utf-8")
        for pattern in patterns:
            if re.search(pattern, source, re.S):
                problems.append(f"{relative} 重新内嵌了应由用户本地生成的规范表（{pattern}）")
    return problems


def manifest_hashes() -> tuple[dict[str, str], str, Path]:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    members = {
        document["attachment"]["member"]: document["attachment"]["member_sha256"]
        for document in data["documents"]
    }
    artifacts = data.get("generated_artifacts", [])
    if len(artifacts) != 1:
        raise RuntimeError("spec/MANIFEST.json 必须恰好锁定一个 PDF Rust 生成物")
    artifact = artifacts[0]
    if artifact.get("distribution") != "local-only":
        raise RuntimeError("PDF Rust 生成物必须标记为 local-only")
    return members, artifact["sha256"], REPO_ROOT / "spec" / artifact["filename"]


def audit_spec_lock() -> list[str]:
    problems: list[str] = []
    members, generated_hash, _ = manifest_hashes()
    source = SPEC_LOCK.read_text(encoding="utf-8")
    expected = {
        "PART1_TABLES_C_SHA256": members["ts_103190_tables.c"],
        "PART2_TABLES_C_SHA256": members["ts_103190_tables_part2.c"],
        "PDF_TABLES_RS_SHA256": generated_hash,
    }
    for name, digest in expected.items():
        match = re.search(rf"{name}:\s*&str\s*=\s*\"([0-9a-f]{{64}})\"", source)
        if match is None:
            problems.append(f"spec_lock.rs 缺少 {name}")
        elif match.group(1) != digest:
            problems.append(f"spec_lock.rs 的 {name} 与 MANIFEST.json 不一致")
    return problems


def crate_names() -> list[str]:
    names: list[str] = []
    for manifest in sorted((REPO_ROOT / "crates").glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        match = re.search(r'^name\s*=\s*"([^"]+)"', text, re.M)
        if match is None:
            raise RuntimeError(f"{manifest} 缺少 package name")
        names.append(match.group(1))
    return names


def audit_packages() -> list[str]:
    problems: list[str] = []
    for crate in crate_names():
        entries = command("cargo", "package", "--list", "--allow-dirty", "-p", crate).splitlines()
        for entry in entries:
            path = Path(entry)
            lowered = entry.lower()
            if path.suffix.lower() in FORBIDDEN_PACKAGE_SUFFIXES:
                problems.append(f"{crate} 包含禁止制品：{entry}")
            if "spec/generated" in lowered or "ts103190_pdf_tables" in lowered:
                problems.append(f"{crate} 包含本地生成规范表：{entry}")
            if path.name in {".env.local", "ts_103190_tables.c", "ts_103190_tables_part2.c"}:
                problems.append(f"{crate} 包含本地或规范输入：{entry}")
    return problems


def audit_generated() -> list[str]:
    problems: list[str] = []
    _, expected, generated = manifest_hashes()
    if not generated.exists():
        return [f"缺少 {generated}；先运行 scripts/generate_spec_tables.py"]
    actual = hashlib.sha256(generated.read_bytes()).hexdigest()
    if actual != expected:
        problems.append(f"{generated} 摘要 {actual} 与 MANIFEST.json 的 {expected} 不一致")
    ignored = subprocess.run(
        ["git", "check-ignore", "--quiet", str(generated.relative_to(REPO_ROOT))],
        cwd=REPO_ROOT,
        check=False,
    )
    if ignored.returncode != 0:
        problems.append(f"{generated.relative_to(REPO_ROOT)} 未被 .gitignore 覆盖")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generated", action="store_true", help="同时核对本地生成文件")
    args = parser.parse_args()

    try:
        paths = tracked_files()
        problems = audit_tracked(paths)
        problems.extend(audit_spec_lock())
        problems.extend(audit_packages())
        if args.generated:
            problems.extend(audit_generated())
    except (KeyError, OSError, RuntimeError, ValueError) as error:
        print(f"分发审计失败：{error}", file=sys.stderr)
        return 1

    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1
    suffix = "，本地生成摘要一致" if args.generated else ""
    print(f"规范分发审计通过：{len(paths)} 个跟踪路径、{len(crate_names())} 个 crate 包清单{suffix}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
