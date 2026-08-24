#!/usr/bin/env python3
"""冻结真实 A-JOC 媒体的矩阵侧信息覆盖与 M6 full-path 支持凭证。

    ./scripts/ajoc_census.py              # 校验全部已登记和本地发现的媒体
    ./scripts/ajoc_census.py FILE [...]   # 校验指定媒体（仍要求它已登记）
    ./scripts/ajoc_census.py --update     # 原子重建完整基线

基线记录 `ajoc()` 的数据点、渐变、参数带、量化、稀疏矩阵、差分方向、
去相关器和原始码值范围。它冻结真实语料覆盖，不把未出现的分支解释为受支持。
每条 A-JOC substream 还必须取得 Rust 侧唯一的 full-path 支持凭证；任一凭证
缺失都会使门禁失败。channel-based IMS 只能按基线中的具名 scene path 跳过。

编码媒体被版本控制排除，因此这是 fail-closed 的本地门禁，不在缺少素材的
环境里对偶然存在的子集报告通过。需要 audio-decode feature，故需先运行
`scripts/fetch_specs.py`。
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
BASELINE = VECTORS / "ajoc_syntax_baseline.json"
ALLOWED_NAMED_SKIP_PATHS = frozenset({"channel_based"})

COMMENT = [
    "A-JOC 矩阵侧信息覆盖基线，由 scripts/ajoc_census.py 维护。",
    "逐媒体冻结数据点、起点/渐变、参数带、粗细量化、稀疏矩阵、频率/时间差分、去相关器和原始码值范围。",
    "full_support 是进入 M6 full DSP 的唯一支持凭证计数；任一未支持 substream 都使门禁失败。",
    "基线只证明现有语料覆盖未意外改变；未出现的分支不因此成为受支持路径。",
    "两条 IMS channel-based 媒体必须按名称和 scene_path 精确跳过，不能以通配规则放行。",
    "编码媒体不入库；默认校验基线声明与本地发现集合的并集，缺失媒体时不运行子集。",
]


def path_for_key(name: str) -> Path:
    """把 `案例名/文件名` 安全还原为被忽略的 encoded 媒体。"""
    parts = PurePosixPath(name).parts
    if len(parts) != 2 or any(
        part in ("", ".", "..") or "\\" in part for part in parts
    ):
        raise ValueError(f"非法基线键：{name!r}")
    return VECTORS / parts[0] / "encoded" / parts[1]


def key_for(path: Path) -> str:
    """生成与绝对 checkout 路径无关的 `案例名/文件名` 键。"""
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


def default_inputs(entries: dict, skips: dict) -> list[Path]:
    """覆盖登记媒体、具名跳过以及任何尚未登记的本地新增媒体。"""
    paths = {path_for_key(name) for name in entries}
    paths.update(path_for_key(name) for name in skips)
    paths.update(discovered_inputs())
    return sorted(paths, key=lambda path: path.as_posix())


def _integer(value, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{label} 必须是非负整数")
    return value


def inspect(path: Path) -> tuple[str, dict | None]:
    """运行一次 trace，返回 `(scene_path, A-JOC census 或 None)`。"""
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
                "--features",
                "macindecode-ac4-cli/audio-decode",
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
        validation = envelope["result"]["validation"]
        scene_path = validation["topology"]["configuration"]["scene_path"]
        if not isinstance(scene_path, str) or not scene_path:
            raise ValueError("scene_path 缺失")
        coverage = validation["ajoc"]["coverage"]
        census = validation["ajoc"]["observations"]["ajoc_matrix"]
        frames = _integer(coverage["frames"], "coverage.frames")
        parsed = _integer(coverage["parsed"], "coverage.parsed")
        substreams = _integer(coverage["substreams"], "coverage.substreams")
        parsed_substreams = _integer(
            coverage["parsed_substreams"], "coverage.parsed_substreams"
        )
        failures = _integer(coverage["failures"], "coverage.failures")
    except (json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"trace 输出无效：{error}") from error

    if scene_path != "ajoc":
        if any((frames, parsed, substreams, parsed_substreams, failures)):
            raise RuntimeError(
                f"非 A-JOC 路径 {scene_path!r} 却报告了 A-JOC coverage"
            )
        return scene_path, None

    try:
        census_substreams = _integer(census["substreams"], "census.substreams")
        support = census["full_support"]
        supported = _integer(support["supported"], "full_support.supported")
        unsupported = _integer(support["unsupported"], "full_support.unsupported")
        first_unsupported = support["first_unsupported"]
    except (KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"A-JOC census 输出无效：{error}") from error

    problems = []
    if failures != 0 or parsed != frames:
        problems.append(f"帧解析为 {parsed}/{frames}，failures={failures}")
    if parsed_substreams != substreams:
        problems.append(f"substream 解析为 {parsed_substreams}/{substreams}")
    if census_substreams != parsed_substreams:
        problems.append(
            f"census substream={census_substreams}，解析={parsed_substreams}"
        )
    if supported != parsed_substreams or unsupported != 0:
        problems.append(
            "full 支持凭证为 "
            f"{supported}/{parsed_substreams}，unsupported={unsupported}"
        )
    if first_unsupported is not None:
        problems.append(f"首个未支持分支：{first_unsupported}")
    if frames == 0:
        problems.append("没有 A-JOC codec frame")
    if problems:
        raise RuntimeError("；".join(problems))

    return scene_path, {
        "codec_frames": frames,
        "substreams": parsed_substreams,
        "census": census,
    }


def load_baseline(update: bool) -> dict:
    if BASELINE.exists():
        try:
            baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"无法读取基线 {BASELINE}：{error}") from error
    elif update:
        baseline = {"comment": COMMENT, "entries": {}, "skips": {}}
    else:
        raise ValueError(f"找不到基线 {BASELINE}，先运行 --update")
    if not isinstance(baseline, dict):
        raise ValueError("基线顶层必须是对象")
    entries = baseline.get("entries")
    skips = baseline.get("skips")
    if not isinstance(entries, dict) or not isinstance(skips, dict):
        raise ValueError("基线顶层必须包含 entries 与 skips 对象")
    if set(entries).intersection(skips):
        raise ValueError("同一媒体不能同时出现在 entries 与 skips")
    invalid_skips = {
        name: value
        for name, value in skips.items()
        if value not in ALLOWED_NAMED_SKIP_PATHS
    }
    if invalid_skips:
        raise ValueError(
            "skips 只能登记已批准的 scene_path："
            + ", ".join(sorted(ALLOWED_NAMED_SKIP_PATHS))
        )
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
        skips = baseline["skips"]
        inputs = args.inputs or default_inputs(entries, skips)
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

    # 先检查全体存在性，避免缺一个媒体却对已存在的子集运行并输出似是而非的
    # 部分成功结果。
    missing = [(name, path) for name, path in work_items if not path.is_file()]
    for name, _ in missing:
        print(f"{name}：找不到输入", file=sys.stderr)
    if missing or failed:
        return 1

    updated_entries = {}
    updated_skips = {}
    decoded = 0
    skipped = 0
    codec_frames = 0
    for name, path in work_items:
        try:
            scene_path, actual = inspect(path)
        except RuntimeError as error:
            print(f"{name}：census 失败：{error}", file=sys.stderr)
            failed = True
            continue

        if actual is None:
            if scene_path not in ALLOWED_NAMED_SKIP_PATHS:
                print(
                    f"{name}：scene_path={scene_path} 不允许作为 M6 具名跳过",
                    file=sys.stderr,
                )
                failed = True
            elif args.update:
                skipped += 1
                updated_skips[name] = scene_path
                print(f"{name}：具名跳过 scene_path={scene_path}")
            elif name in entries:
                print(
                    f"{name}：已冻结 A-JOC 媒体变为 {scene_path}", file=sys.stderr
                )
                failed = True
            elif skips.get(name) != scene_path:
                print(
                    f"{name}：未登记跳过或 scene_path 不匹配（实际 {scene_path}）",
                    file=sys.stderr,
                )
                failed = True
            else:
                skipped += 1
                print(f"{name}：按名称跳过 scene_path={scene_path}")
            continue

        decoded += 1
        codec_frames += actual["codec_frames"]
        if args.update:
            updated_entries[name] = actual
            print(
                f"{name}：{actual['codec_frames']} codec frame，"
                f"{actual['substreams']} 个 full 支持凭证"
            )
            continue
        if name in skips:
            print(f"{name}：具名跳过媒体现在变为 A-JOC", file=sys.stderr)
            failed = True
            continue
        expected = entries.get(name)
        if expected is None:
            print(f"{name}：基线中没有该输入，先运行 --update", file=sys.stderr)
            failed = True
        elif expected != actual:
            print(f"{name}：A-JOC census 与基线不一致", file=sys.stderr)
            failed = True
        else:
            print(
                f"{name}：{actual['codec_frames']} codec frame 侧信息逐项一致"
            )

    if decoded == 0:
        print("没有任何 A-JOC 输入被检查，全部跳过或失败", file=sys.stderr)
        failed = True

    if args.update:
        if failed:
            print("A-JOC census 更新未完成，旧文件保持不变", file=sys.stderr)
            return 1
        replacement = {
            "comment": COMMENT,
            "entries": dict(sorted(updated_entries.items())),
            "skips": dict(sorted(updated_skips.items())),
        }
        try:
            write_baseline(replacement)
        except OSError as error:
            print(f"写入基线失败：{error}", file=sys.stderr)
            return 1
        print(f"已写入 {BASELINE.relative_to(REPO_ROOT)}")
    elif failed:
        print("A-JOC 侧信息基线未通过", file=sys.stderr)
        return 1

    print(
        f"A-JOC 侧信息基线通过：{decoded} 条媒体，{codec_frames} 个 codec frame；"
        f"具名跳过 {skipped} 条"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
