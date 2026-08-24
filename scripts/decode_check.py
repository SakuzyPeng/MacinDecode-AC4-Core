#!/usr/bin/env python3
"""重建 PCM 的逐位回归基线，核心带、带宽扩展与对象输出各一份。

    ./scripts/decode_check.py                    # 三段都校验基线中的全部向量
    ./scripts/decode_check.py --stage aspx       # 只校验带宽扩展那一段
    ./scripts/decode_check.py --stage objects    # 只校验对象输出那一段
    ./scripts/decode_check.py <file.m4a> [...]   # 校验指定输入
    ./scripts/decode_check.py --update           # 重新生成基线

现有门禁都是结构性或统计性的：落点等式、非有限值计数、峰值是否有限、样本数
守恒。它们抓不到**数值本身的变化**——改掉 IMDCT 的一个归一化常数、把某张冻结
表换成另一份同样自洽的表、或者调整一处舍入，全部门禁照样通过，而每个样本都变
了。单元测试确实钉住了数值，但只在人造输入上；真实码流这一侧此前完全没有基线。

三段各自冻结，**基线文件也分开**：核心带那份的价值正在于「不因上层改动而变」，
共用一个文件迟早会因为一次 `--update` 把三层一起重冻。

本脚本用三个 PCM 命令导出 32 位浮点 WAVE；`data` 块是 `f32::to_bits()` 的直接转写
（不缩放、不取整），再对整个文件取 SHA-256。**三份都只证明「没有意外改变」，
不单独证明「正确」**。参考解码器已经可用，但 core/A-SPX 中间层与对象 oracle
层级不可比；对象层的外部逐路差分验证留待下一轮。任何一次基线变动都必须在提交
信息里解释清楚是哪一步的哪个改动导致的。

除摘要外还记录形状（采样率、声道数、帧数、声道来源）。形状先对比，因为它变了
的话摘要必然也变，而形状差异一眼能看出是声道少了还是长度变了。带宽扩展那段的
声道来源含 `role`，因此 LFE 被错标成一个 A-JOC 输入下标会直接顶出基线——那条
接缝在单元测试里够不到，只有真实码流上的逐路自述能钉住它。

编码媒体被版本控制排除，因此数值比对是本地门禁，不在 GitHub Actions 中伪装成
可运行检查。默认模式由基线条目驱动：缺少任一媒体都会失败，不允许对偶然存在的
子集报告通过。需要 audio-decode feature，故需先运行 scripts/fetch_specs.py。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath

REPO_ROOT = Path(__file__).resolve().parent.parent
VECTORS = REPO_ROOT / "vectors"

SHARED_COMMENT = [
    "data 块直接写 f32::to_bits()，整个文件的摘要覆盖呈现 PCM 与容器形状。",
    "MP4 输入已应用 edit list；编码媒体不入库，因此这是 fail-closed 的本地门禁。",
    "它证明「没有意外改变」，不证明「正确」；参考解码器不暴露 A-JOC 前的中间 PCM，输出层级不可比。",
]

OBJECT_SHARED_COMMENT = [
    "data 块直接写 f32::to_bits()，整个文件的摘要覆盖呈现 PCM 与容器形状。",
    "MP4 输入已应用 edit list；编码媒体不入库，因此这是 fail-closed 的本地门禁。",
    "它证明「没有意外改变」，不单独证明「正确」。",
]


class Stage:
    """一段导出：命令、基线文件与逐路来源的写法。

    三段共用全部 fail-closed 规则，只有这三样不同。基线文件必须分开，理由见
    模块文档。
    """

    def __init__(self, name, command, baseline, label, comment, track_of):
        self.name = name
        self.command = command
        self.baseline = baseline
        self.label = label
        self.comment = comment
        self.track_of = track_of


def core_track(item: dict) -> str:
    return "{}:{}:{}".format(item["substream"], item["element"], item["channel"])


def aspx_track(item: dict) -> str:
    """带宽扩展的逐路来源必须带 `role`。

    这一段的下标语义是 `Pseudocode 14a` 的 A-JOC 输入顺序，LFE 不进入 A-JOC、
    单独排在最后。只记整数下标的话，LFE 被标成 `ajoc_input` 时摘要与形状都不
    变，基线会静默接受一份语义已经错了的导出。
    """
    role = item["role"]
    if role == "lfe":
        return "{}:lfe".format(item["substream"])
    return "{}:{}:{}".format(item["substream"], role, item["ajoc_input"])


def objects_track(item: dict) -> str:
    """对象下标与 `Pseudocode 15` 后的输出位置都必须进入形状基线。"""
    role = item["role"]
    output = item["output_channel"]
    if role == "lfe":
        return "{}:lfe:{}".format(item["substream"], output)
    return "{}:{}:{}:{}".format(
        item["substream"], role, item["ajoc_object"], output
    )


def stages() -> dict:
    """按当前 `VECTORS` 现算，测试改写向量根时基线随之改道。"""
    return {
        "core": Stage(
            "core",
            "export-core-pcm",
            VECTORS / "decode_baseline.json",
            "核心带 PCM",
            [
                "核心带 PCM 的逐位回归基线，由 scripts/decode_check.py 维护。",
                "摘要取自 export-core-pcm 写出的 WAVE_FORMAT_EXTENSIBLE 32 位浮点 WAVE；",
                "内容是 A-JOC 下混信号的核心带重建，不含 A-SPX 带宽扩展。",
                "逐路来源是传输侧的 substream:element:channel。",
                "多 presentation 向量必须由 presentation_overrides 显式选择，禁止依赖默认第一项。",
                *SHARED_COMMENT,
            ],
            core_track,
        ),
        "aspx": Stage(
            "aspx",
            "export-aspx-pcm",
            VECTORS / "aspx_baseline.json",
            "带宽扩展 PCM",
            [
                "带宽扩展 PCM 的逐位回归基线，由 scripts/decode_check.py 维护。",
                "摘要取自 export-aspx-pcm 写出的 WAVE_FORMAT_EXTENSIBLE 32 位浮点 WAVE；",
                "内容是补上 A-SPX 高频后的下混信号，尚未执行 A-JOC 上混。",
                "Core PCM 的 2 倍接口因子在 QMF 分析后撤销、终端合成后补回。",
                "逐路顺序是 Pseudocode 14a 的 A-JOC 输入，LFE 带 role=lfe 单独排在最后；",
                "来源串因此含 role，LFE 被错标成 A-JOC 输入下标会直接顶出基线。",
                "PCM 已执行 P1 5.6 表 188 的 d_pcm，QMF 控制按同表 d_ctrl 延后到对应信号；",
                "与核心带那份各自冻结：共用一个文件会让一次 --update 把两层一起重冻。",
                "多 presentation 向量必须由 presentation_overrides 显式选择，禁止依赖默认第一项。",
                *SHARED_COMMENT,
            ],
            aspx_track,
        ),
        "objects": Stage(
            "objects",
            "export-objects-pcm",
            VECTORS / "objects_baseline.json",
            "A-JOC 对象 PCM",
            [
                "A-JOC 对象 PCM 的逐位回归基线，由 scripts/decode_check.py 维护。",
                "摘要取自 export-objects-pcm 写出的 WAVE_FORMAT_EXTENSIBLE 32 位浮点 WAVE；",
                "内容已执行 A-SPX、full A-JOC 矩阵、LFE 插回与终端 QMF 合成。",
                "Core PCM 的 2 倍接口因子在 QMF 分析后撤销、终端合成后补回。",
                "逐路来源同时记录 ajoc_object 与 output_channel；LFE 只记录插回后的 output_channel。",
                "外部参考解码器对象 PCM 差分验证留待下一轮；本基线不替代正确性 oracle。",
                "与 core/aspx 两份各自冻结；--stage objects --update 只改这一份。",
                "多 presentation 向量必须由 presentation_overrides 显式选择，禁止依赖默认第一项。",
                *OBJECT_SHARED_COMMENT,
            ],
            objects_track,
        ),
    }


def path_for_key(name: str) -> Path:
    """把 `案例名/文件名` 还原为被忽略的 encoded 媒体路径。"""
    parts = PurePosixPath(name).parts
    if len(parts) != 2 or any(
        part in ("", ".", "..") or "\\" in part for part in parts
    ):
        raise ValueError(f"非法基线键：{name!r}")
    return VECTORS / parts[0] / "encoded" / parts[1]


def discovered_inputs() -> list[Path]:
    return sorted(VECTORS.glob("*/encoded/*.m4a"))


def default_inputs(entries: dict) -> list[Path]:
    """既覆盖基线声明的全部输入，也暴露本地新增但未登记的向量。"""
    paths = {path_for_key(name) for name in entries}
    paths.update(discovered_inputs())
    return sorted(paths, key=lambda path: path.as_posix())


def key_for(path: Path) -> str:
    """基线的键：`案例名/文件名`，与向量目录布局一致且与绝对路径无关。"""
    try:
        relative = path.resolve().relative_to(VECTORS)
    except ValueError:
        return path.name
    parts = relative.parts
    if len(parts) >= 3 and parts[1] == "encoded":
        return f"{parts[0]}/{parts[-1]}"
    return "/".join(parts)


def decode(path: Path, stage: Stage, presentation: int | None = None) -> dict:
    """按该段的命令导出一次，返回摘要与形状。"""
    with tempfile.TemporaryDirectory() as work:
        output = Path(work) / "export.wav"
        command = [
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
            stage.command,
        ]
        if presentation is not None:
            command.extend(["--presentation", str(presentation)])
        command.extend(["-o", str(output), str(path)])
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
            )
        except OSError as error:
            raise RuntimeError(f"无法启动 {stage.command}：{error}") from error
        if result.returncode != 0:
            detail = result.stderr.strip()
            raise DecodeFailed(detail or f"{stage.command} 失败", unsupported_path(detail))
        try:
            envelope = json.loads(result.stdout)
            if envelope.get("schema") != "macinac4.cli-result":
                raise ValueError("success envelope schema 不匹配")
            report = envelope["result"]
            digest = hashlib.sha256(output.read_bytes()).hexdigest()
            tracks = [stage.track_of(item) for item in report["tracks"]]
            shape = {
                "sample_rate": report["audio"]["sample_rate_hz"],
                "channels": report["audio"]["channels"],
                "frames": report["audio"]["frames"],
            }
        except (OSError, json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
            raise RuntimeError(f"{stage.command} 输出无效：{error}") from error

    return {
        "sha256": digest,
        **shape,
        "tracks": tracks,
    }


class DecodeFailed(RuntimeError):
    """解码失败。`path` 非空时说明失败原因是「该编码路径尚未实现」。"""

    def __init__(self, message: str, path: str | None = None) -> None:
        super().__init__(message)
        self.path = path


def unsupported_path(stderr: str) -> str | None:
    """从诊断里取出「尚未实现的编码路径」，其余失败一律返回 None。

    只认 `unsupported.coding_path` 这一个 code，不认消息文本——文本会变，而按
    文本匹配会让别的失败也被当成「暂不支持」跳过去。
    """
    for line in stderr.splitlines():
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            isinstance(payload, dict)
            and payload.get("schema") == "macinac4.cli-diagnostic"
            and payload.get("code") == "unsupported.coding_path"
        ):
            context = payload.get("context")
            if isinstance(context, dict):
                path = context.get("scene_path")
                if isinstance(path, str) and path:
                    return path
            return "unknown"
    return None


def compare(expected: dict, actual: dict) -> list[str]:
    problems = []
    for field in ("sample_rate", "channels", "frames"):
        if expected.get(field) != actual[field]:
            problems.append(
                "{} 从 {} 变为 {}".format(field, expected.get(field), actual[field])
            )
    if expected.get("tracks") != actual["tracks"]:
        problems.append(
            "声道来源从 {} 变为 {}".format(
                ",".join(expected.get("tracks", [])), ",".join(actual["tracks"])
            )
        )
    if expected.get("sha256") != actual["sha256"]:
        problems.append(
            "摘要从 {} 变为 {}".format(
                (expected.get("sha256") or "缺失")[:16], actual["sha256"][:16]
            )
        )
    return problems


def load_baseline(stage: Stage, update: bool) -> dict:
    if stage.baseline.exists():
        try:
            baseline = json.loads(stage.baseline.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"无法读取基线 {stage.baseline}：{error}") from error
    elif update:
        baseline = {"comment": stage.comment, "entries": {}}
    else:
        raise ValueError(f"找不到基线 {stage.baseline}，先运行 --update")
    if not isinstance(baseline, dict) or not isinstance(baseline.get("entries"), dict):
        raise ValueError("基线顶层必须包含 entries 对象")
    return baseline


def presentation_overrides(stage: Stage, baseline: dict) -> dict[str, int]:
    """读取 Scene PCM 阶段的显式 presentation 选择，不允许静默选第一项。"""
    overrides = baseline.get("presentation_overrides", {})
    if not isinstance(overrides, dict):
        raise ValueError("presentation_overrides 必须是对象")
    if overrides and stage.name not in ("core", "aspx", "objects"):
        raise ValueError("只有 core/aspx/objects 阶段可以声明 presentation_overrides")
    checked = {}
    for name, index in overrides.items():
        if not isinstance(name, str):
            raise ValueError("presentation_overrides 的键必须是向量路径")
        path_for_key(name)
        if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index <= 0xFFFF_FFFF:
            raise ValueError(f"{name} 的 presentation 下标必须是 u32")
        checked[name] = index
    return checked


def write_baseline(stage: Stage, baseline: dict) -> None:
    """同目录原子替换，失败时保留旧基线。"""
    stage.baseline.parent.mkdir(parents=True, exist_ok=True)
    try:
        target_mode = stage.baseline.stat().st_mode & 0o777
    except FileNotFoundError:
        target_mode = 0o644
    handle = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=stage.baseline.parent,
        prefix=f".{stage.baseline.name}.tmp-",
        delete=False,
    )
    temp = Path(handle.name)
    try:
        with handle:
            handle.write(json.dumps(baseline, ensure_ascii=False, indent=2) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp, target_mode)
        os.replace(temp, stage.baseline)
    except Exception:
        temp.unlink(missing_ok=True)
        raise


def run_stage(stage: Stage, requested: list[Path], update: bool) -> bool:
    """跑完一段，返回是否失败。三段共用全部 fail-closed 规则。"""
    print(f"[{stage.label}]")
    try:
        baseline = load_baseline(stage, update)
        entries = baseline["entries"]
        presentation_by_name = presentation_overrides(stage, baseline)
        inputs = requested or default_inputs(entries)
    except ValueError as error:
        print(error, file=sys.stderr)
        return True
    if not inputs:
        print("  没有可校验的输入", file=sys.stderr)
        return True

    failed = False
    work_items = []
    names: dict[str, Path] = {}
    for path in inputs:
        name = key_for(path)
        previous = names.get(name)
        if previous is not None:
            if previous.resolve() != path.resolve():
                print(f"  {name}：输入键冲突：{previous} 与 {path}", file=sys.stderr)
                failed = True
            continue
        names[name] = path
        work_items.append((name, path))

    updated = dict(entries)
    skipped: list[tuple[str, str]] = []
    decoded = 0
    for name, path in work_items:
        if not path.is_file():
            print(f"  {name}：找不到输入", file=sys.stderr)
            failed = True
            continue
        try:
            presentation = presentation_by_name.get(name)
            actual = (
                decode(path, stage)
                if presentation is None
                else decode(path, stage, presentation)
            )
        except DecodeFailed as error:
            # **已进基线的输入永远不许跳过。** 跳过只对基线里没有的输入开放，
            # 因此任何已冻结条目的回归都跳不掉；而一旦该路径实现了，解码会成功、
            # 不再走这里，条目自然靠 `--update` 归队。
            if error.path is not None and name not in entries:
                skipped.append((name, error.path))
                print(f"  {name}：跳过，编码路径 {error.path} 尚未实现")
                continue
            print(f"  {name}：解码失败：{error}", file=sys.stderr)
            failed = True
            continue
        except RuntimeError as error:
            print(f"  {name}：解码失败：{error}", file=sys.stderr)
            failed = True
            continue

        decoded += 1
        if update:
            updated[name] = actual
            print(
                "  {}：{} 声道 × {} 帧，摘要 {}".format(
                    name, actual["channels"], actual["frames"], actual["sha256"][:16]
                )
            )
            continue

        expected = entries.get(name)
        if expected is None:
            print(f"  {name}：基线中没有该输入，先运行 --update", file=sys.stderr)
            failed = True
            continue
        problems = compare(expected, actual)
        if problems:
            print("  {}：{}".format(name, "；".join(problems)), file=sys.stderr)
            failed = True
        else:
            print(
                "  {}：{} 声道 × {} 帧逐位一致（{}）".format(
                    name, actual["channels"], actual["frames"], actual["sha256"][:16]
                )
            )

    # 跳过是有条件的放行，不是免检。一条都没真正解出来时判失败——否则「全部
    # 输入都报尚未实现」会让门禁静默变绿，那正是 fail-closed 要防的。
    if decoded == 0:
        print("  没有任何输入被解码，全部跳过或失败", file=sys.stderr)
        failed = True
    if skipped:
        print(
            "  已跳过 {} 个尚未实现的编码路径：{}".format(
                len(skipped),
                "，".join(f"{name}（{path}）" for name, path in skipped),
            )
        )

    if update:
        if failed:
            print(f"  {stage.label} 基线更新未完成，旧文件保持不变", file=sys.stderr)
            return True
        baseline["comment"] = stage.comment
        baseline["entries"] = dict(sorted(updated.items()))
        try:
            write_baseline(stage, baseline)
        except OSError as error:
            print(f"  写入基线失败：{error}", file=sys.stderr)
            return True
        print(f"  已写入 {stage.baseline.relative_to(REPO_ROOT)}")
        return False

    if failed:
        print(f"  {stage.label} 基线未通过", file=sys.stderr)
        return True
    print(f"  {stage.label} 基线通过")
    return False


def main() -> int:
    available = stages()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="*", type=Path)
    parser.add_argument(
        "--stage",
        choices=[*available, "all"],
        default="all",
        help="只跑其中一段；默认三段都跑",
    )
    parser.add_argument(
        "--update", action="store_true", help="用当前解码结果重新生成基线"
    )
    args = parser.parse_args()

    selected = list(available.values()) if args.stage == "all" else [available[args.stage]]
    # 一段失败不跳过后一段：三份基线各管各的，同时报出来才看得见是哪一层动了。
    failed = False
    for stage in selected:
        failed |= run_stage(stage, args.inputs, args.update)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
