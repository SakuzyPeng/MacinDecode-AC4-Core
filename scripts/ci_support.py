#!/usr/bin/env python3
"""CI 的失败汇总、工具链契约、规范准备与精简一致性入口；仅使用标准库。"""
from __future__ import annotations

import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parent.parent
CONSISTENCY_TESTS = {
    "synthetic_pcm_matches_frozen_bits",
    "cli_pcm_matches_engine_and_tracks",
    "fixture_registry_is_complete",
    "history_is_exercised_and_reset_is_exact",
    "truncated_frame_invalidates_history_and_iframe_recovers",
    "fingerprints_reject_pcm_bit_order_and_length_changes",
    "cli_rejects_missing_empty_and_truncated_inputs",
}


def require_success(needs: dict, expected: list[str]) -> None:
    if not isinstance(needs, dict) or not expected or set(needs) != set(expected):
        raise ValueError("门禁依赖集合为空或与声明不一致")
    if any(not isinstance(needs[name], dict) for name in expected):
        raise ValueError("门禁依赖状态格式不正确")
    failed = {name: needs[name].get("result") for name in expected
              if needs[name].get("result") != "success"}
    if failed:
        raise ValueError(f"必需检查未全部成功：{failed}")


def check_toolchain(root: Path, rustc_version: str) -> None:
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    pinned = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    msrv = workspace["package"]["rust-version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", pinned) or pinned != msrv + ".0":
        raise ValueError(f"固定工具链 {pinned} 必须与 MSRV {msrv}.0 一致")
    if rustc_version.split()[1] != pinned:
        raise ValueError(f"实际编译器与固定工具链不一致：{rustc_version}")
    for member in workspace["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text())["package"]
        if package.get("rust-version") != {"workspace": True}:
            raise ValueError(f"{member} 必须继承 workspace MSRV")


@contextlib.contextmanager
def measured(label: str):
    start = time.perf_counter()
    try:
        yield
    finally:
        seconds = time.perf_counter() - start
        print(f"{label}: {seconds:.3f}s", flush=True)
        if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(summary, "a", encoding="utf-8") as stream:
                stream.write(f"- {label}: {seconds:.3f} s\n")


def run(args: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(args, cwd=ROOT, check=False, text=True, encoding="utf-8",
                            stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        if result.stdout:
            print(result.stdout, file=sys.stderr)
        raise ValueError(f"命令失败 ({result.returncode})：{' '.join(args)}")
    return result.stdout or ""


def prepare_spec() -> None:
    with measured("spec dependencies"):
        run([sys.executable, "-m", "pip", "install", "--disable-pip-version-check",
             "-r", "scripts/requirements-spec.txt"])
    with measured("spec download and verification"):
        run([sys.executable, "scripts/fetch_specs.py"])
    with measured("spec fast generation and distribution audit"):
        run([sys.executable, "scripts/generate_spec_tables.py"])
        run([sys.executable, "scripts/check_spec_distribution.py", "--generated"])


def check_test_inventory(output: str) -> None:
    names = [line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")]
    if len(names) != len(CONSISTENCY_TESTS) or set(names) != CONSISTENCY_TESTS:
        raise ValueError(f"一致性测试集合不完整：{names}")


def check_test_result(output: str) -> None:
    expected = (f"test result: ok. {len(CONSISTENCY_TESTS)} passed; 0 failed; "
                "0 ignored; 0 measured; 0 filtered out;")
    if expected not in output:
        raise ValueError("一致性测试未全部执行成功（不允许 ignored 或 filtered）")


def check_reused_artifacts(messages: list[dict]) -> None:
    expected = {"macindecode_ac4_decode", "macinac4"}
    reused = {message["target"]["name"] for message in messages
              if message.get("reason") == "compiler-artifact" and message.get("fresh") is True}
    if not expected <= reused:
        raise ValueError(f"发布测试未复用已构建产物：{sorted(expected - reused)}；检查规范目录环境、profile 和 feature 是否一致")


def consistency(target: str | None, require_reuse: bool = False) -> None:
    command = ["cargo", "test", "--locked", "--release", "-p", "macindecode-ac4-cli",
               "--features", "audio-decode", "--test", "platform_consistency",
               "--no-run", "--message-format=json"]
    if target:
        command.extend(["--target", target])
    with measured("consistency release build"):
        messages = [json.loads(line) for line in run(command, capture=True).splitlines()]
    if require_reuse:
        check_reused_artifacts(messages)
    binaries = [message["executable"] for message in messages
                if message.get("reason") == "compiler-artifact"
                and message["target"]["name"] == "platform_consistency"
                and message.get("executable")]
    if len(binaries) != 1:
        raise ValueError(f"应生成恰好一个一致性测试程序：{binaries}")
    binary = binaries[0]
    check_test_inventory(run([binary, "--list", "--format=terse"], capture=True))
    with measured("consistency runtime"):
        output = run([binary, "--test-threads=1"], capture=True)
        print(output, flush=True)
        check_test_result(output)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("prepare-spec")
    commands.add_parser("toolchain")
    gate = commands.add_parser("gate")
    gate.add_argument("jobs", nargs="+")
    tests = commands.add_parser("consistency")
    tests.add_argument("--target")
    tests.add_argument("--require-reuse", action="store_true", help="要求复用前一步已构建的解码库与 CLI")
    args = parser.parse_args()
    try:
        if args.command == "gate":
            require_success(json.loads(os.environ["CI_NEEDS_JSON"]), args.jobs)
        elif args.command == "toolchain":
            check_toolchain(ROOT, run(["rustc", "--version"], capture=True))
        elif args.command == "prepare-spec":
            prepare_spec()
        else:
            consistency(args.target, args.require_reuse)
    except (OSError, ValueError, KeyError, IndexError) as error:
        print(f"CI 检查失败：{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
