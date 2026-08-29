#!/usr/bin/env python3
"""冻结真实媒体中 presentation 级 EMDF 路由与 opaque payload 签名。

    ./scripts/emdf_census.py              # 校验全部已登记和本地发现的媒体
    ./scripts/emdf_census.py FILE [...]   # 校验指定媒体
    ./scripts/emdf_census.py --update     # 原子重建完整基线

基线只登记实际出现 presentation EMDF 路由的媒体。零路由媒体仍会被逐条 trace，
但不写入基线；因此新增非空（或空 payload）路由必须显式审查并更新，而普通
A-JOC/IMS 的零路由结果不会制造重复条目。编码媒体不入库，默认校验基线声明
与本地发现集合的并集，缺失任一已登记媒体时会在 trace 前失败。
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath

REPO_ROOT = Path(__file__).resolve().parent.parent
VECTORS = REPO_ROOT / "vectors"
BASELINE = VECTORS / "emdf_baseline.json"

COMMENT = [
    "Presentation EMDF 路由与 opaque payload 签名基线，由 scripts/emdf_census.py 维护。",
    "只登记存在 EMDF payload substream 路由的媒体；本地零路由媒体仍会逐条检查。",
    "payload 只冻结完整配置、大小、FNV-1a 64 指纹和最多 16 字节前缀，不解释未知语义。",
    "基线只证明现有语料覆盖未意外改变；未出现的 ID、配置与 payload 不因此成为受支持路径。",
    "编码媒体不入库；默认校验基线声明与本地发现集合的并集，缺失媒体时不运行子集。",
]


def path_for_key(name: str) -> Path:
    """把 ``案例名/文件名`` 安全还原为被忽略的 encoded 媒体。"""
    relative = PurePosixPath(name)
    parts = relative.parts
    if relative.is_absolute() or len(parts) != 2 or any(
        part in ("", ".", "..") or "\\" in part for part in parts
    ):
        raise ValueError(f"非法基线键：{name!r}")
    encoded = (VECTORS / parts[0] / "encoded").resolve()
    target = VECTORS / parts[0] / "encoded" / parts[1]
    try:
        target.resolve().relative_to(encoded)
    except ValueError as error:
        raise ValueError(f"非法基线键：{name!r}") from error
    return target


def key_for(path: Path) -> str:
    """生成与 checkout 绝对路径无关的 ``案例名/文件名`` 键。"""
    try:
        relative = path.resolve().relative_to(VECTORS.resolve())
    except ValueError:
        return path.name
    parts = relative.parts
    if len(parts) >= 3 and parts[1] == "encoded":
        return f"{parts[0]}/{parts[-1]}"
    return "/".join(parts)


def discovered_inputs() -> list[Path]:
    return sorted(VECTORS.glob("*/encoded/*.m4a"))


def default_inputs(entries: dict) -> list[Path]:
    """覆盖已登记媒体以及任何尚未登记的本地新增媒体。"""
    paths = {path_for_key(name) for name in entries}
    paths.update(discovered_inputs())
    return sorted(paths, key=lambda path: path.as_posix())


def _integer(value, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{label} 必须是非负整数")
    return value


def _list(value, label: str) -> list:
    if not isinstance(value, list):
        raise ValueError(f"{label} 必须是数组")
    return value


def inspect(path: Path) -> dict | None:
    """运行一次 trace；零路由返回 ``None``，有路由则返回可冻结 census。"""
    try:
        result = subprocess.run(
            [
                "cargo",
                "run",
                "-q",
                "--release",
                "--locked",
                "--manifest-path",
                str(REPO_ROOT / "Cargo.toml"),
                "--bin",
                "macinac4",
                "--",
                "trace",
                str(path),
            ],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        raise RuntimeError(f"无法启动 trace：{error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip()
        raise RuntimeError(detail or "trace 失败")

    try:
        envelope = json.loads(result.stdout)
        if envelope.get("schema") != "macinac4.cli-result":
            raise ValueError("success envelope schema 不匹配")
        topology = envelope["result"]["validation"]["topology"]
        coverage = topology["coverage"]
        census = topology["observations"]["emdf"]
        codec_frames = _integer(coverage["frames_parsed"], "frames_parsed")
        parse_failures = _integer(coverage["parse_failures"], "parse_failures")
        infos = _integer(census["infos"], "emdf.infos")
        routed_infos = _integer(census["routed_infos"], "emdf.routed_infos")
        routed_frames = _integer(census["routed_frames"], "emdf.routed_frames")
        located = _integer(
            census["located_substreams"], "emdf.located_substreams"
        )
        parsed = _integer(census["parsed_substreams"], "emdf.parsed_substreams")
        nonempty = _integer(
            census["nonempty_substreams"], "emdf.nonempty_substreams"
        )
        empty = _integer(census["empty_substreams"], "emdf.empty_substreams")
        payloads = _integer(census["payloads"], "emdf.payloads")
        payload_bytes = _integer(census["payload_bytes"], "emdf.payload_bytes")
        _integer(census["max_payload_bytes"], "emdf.max_payload_bytes")
        failures = _integer(census["failures"], "emdf.failures")
        routes = _list(census["routes"], "emdf.routes")
        signatures = _list(census["signatures"], "emdf.signatures")
        first_error = census["first_error"]
        first_detail = census["first_detail"]
    except (json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"trace 输出无效：{error}") from error

    problems = []
    if codec_frames == 0:
        problems.append("没有 codec frame")
    if parse_failures != 0:
        problems.append(f"topology parse_failures={parse_failures}")
    if failures != 0 or first_error is not None:
        problems.append(f"EMDF failures={failures}，first_error={first_error!r}")
    if routed_frames > codec_frames:
        problems.append(f"路由帧 {routed_frames} 超过 codec frame {codec_frames}")
    if located != parsed:
        problems.append(f"已定位/已解析 substream 为 {located}/{parsed}")
    if nonempty + empty != parsed:
        problems.append(
            f"非空+空 substream={nonempty + empty}，已解析={parsed}"
        )
    try:
        route_count = sum(
            _integer(route["count"], f"emdf.routes[{index}].count")
            for index, route in enumerate(routes)
        )
        signature_count = sum(
            _integer(signature["count"], f"emdf.signatures[{index}].count")
            for index, signature in enumerate(signatures)
        )
    except (KeyError, TypeError, ValueError) as error:
        problems.append(str(error))
    else:
        if route_count != routed_infos:
            problems.append(
                f"route count={route_count}，routed_infos={routed_infos}"
            )
        if signature_count != payloads:
            problems.append(
                f"signature count={signature_count}，payloads={payloads}"
            )
    if routed_infos == 0:
        zero_only = (
            routed_frames,
            located,
            parsed,
            nonempty,
            empty,
            payloads,
            payload_bytes,
            len(routes),
            len(signatures),
        )
        if any(zero_only) or first_detail is not None:
            problems.append("零路由媒体报告了 EMDF payload 活动")
    elif located == 0:
        problems.append("存在 EMDF 路由但没有定位到 payload substream")
    if problems:
        raise RuntimeError("；".join(problems))

    if routed_infos == 0:
        return None
    return {
        "codec_frames": codec_frames,
        "census": census,
    }


def load_baseline(update: bool) -> dict:
    if BASELINE.exists():
        try:
            baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"无法读取基线 {BASELINE}：{error}") from error
    elif update:
        baseline = {"comment": COMMENT, "entries": {}}
    else:
        raise ValueError(f"找不到基线 {BASELINE}，先运行 --update")
    if not isinstance(baseline, dict):
        raise ValueError("基线顶层必须是对象")
    entries = baseline.get("entries")
    if not isinstance(entries, dict):
        raise ValueError("基线顶层必须包含 entries 对象")
    if not update and baseline.get("comment") != COMMENT:
        raise ValueError("基线 comment 与检查器不一致，请审查后运行 --update")
    return baseline


def write_baseline(baseline: dict) -> None:
    """在同目录原子替换，任何失败均保留旧基线。"""
    BASELINE.parent.mkdir(parents=True, exist_ok=True)
    try:
        target_mode = BASELINE.stat().st_mode & 0o777
    except FileNotFoundError:
        target_mode = 0o644
    handle = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=BASELINE.parent,
        prefix=f".{BASELINE.name}.tmp-",
        delete=False,
    )
    temp = Path(handle.name)
    try:
        with handle:
            handle.write(json.dumps(baseline, ensure_ascii=False, indent=2) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp, target_mode)
        os.replace(temp, BASELINE)
    except Exception:
        temp.unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="*", type=Path)
    parser.add_argument(
        "--update", action="store_true", help="用全部已登记和本地媒体重建基线"
    )
    args = parser.parse_args()
    if args.update and args.inputs:
        print("--update 不接受部分输入；必须原子重建完整基线", file=sys.stderr)
        return 1

    try:
        baseline = load_baseline(args.update)
        entries = baseline["entries"]
        inputs = args.inputs or default_inputs(entries)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    if not inputs:
        print("没有可校验的输入", file=sys.stderr)
        return 1

    names: dict[str, Path] = {}
    work_items = []
    failed = False
    for path in inputs:
        name = key_for(path)
        previous = names.get(name)
        if previous is not None:
            if previous.resolve() != path.resolve():
                print(f"{name}：输入键冲突：{previous} 与 {path}", file=sys.stderr)
                failed = True
            continue
        names[name] = path
        work_items.append((name, path))

    missing = [(name, path) for name, path in work_items if not path.is_file()]
    for name, _ in missing:
        print(f"{name}：找不到输入", file=sys.stderr)
    if missing or failed:
        return 1

    updated_entries = {}
    routed_media = 0
    zero_media = 0
    codec_frames = 0
    for name, path in work_items:
        try:
            actual = inspect(path)
        except RuntimeError as error:
            print(f"{name}：census 失败：{error}", file=sys.stderr)
            failed = True
            continue

        if actual is None:
            zero_media += 1
            if name in entries:
                print(f"{name}：已冻结 EMDF 媒体变为零路由", file=sys.stderr)
                failed = True
            else:
                print(f"{name}：零 EMDF 路由")
            continue

        routed_media += 1
        codec_frames += actual["codec_frames"]
        routed = actual["census"]["routed_infos"]
        if args.update:
            updated_entries[name] = actual
            print(
                f"{name}：{actual['codec_frames']} codec frame，"
                f"{routed} 个 EMDF 路由"
            )
            continue
        expected = entries.get(name)
        if expected is None:
            print(f"{name}：基线中没有该非零输入，先运行 --update", file=sys.stderr)
            failed = True
        elif expected != actual:
            print(f"{name}：EMDF census 与基线不一致", file=sys.stderr)
            failed = True
        else:
            print(
                f"{name}：{actual['codec_frames']} codec frame，"
                f"{routed} 个 EMDF 路由逐项一致"
            )

    if routed_media == 0:
        print("没有任何带 EMDF 路由的输入被检查", file=sys.stderr)
        failed = True

    if args.update:
        if failed:
            print("EMDF census 更新未完成，旧文件保持不变", file=sys.stderr)
            return 1
        replacement = {
            "comment": COMMENT,
            "entries": dict(sorted(updated_entries.items())),
        }
        try:
            write_baseline(replacement)
        except OSError as error:
            print(f"写入基线失败：{error}", file=sys.stderr)
            return 1
        print(f"已写入 {BASELINE.relative_to(REPO_ROOT)}")
    elif failed:
        print("EMDF 路由基线未通过", file=sys.stderr)
        return 1

    print(
        f"EMDF 路由基线通过：{routed_media} 条非零媒体，"
        f"{codec_frames} 个 codec frame；零路由 {zero_media} 条"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
