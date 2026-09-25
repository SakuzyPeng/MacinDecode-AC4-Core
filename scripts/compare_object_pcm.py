#!/usr/bin/env python3
"""按显式轨道身份、整数延迟和固定尺度比较外部对象 PCM。

输入为本地 manifest，格式见 docs/OBJECT_PCM_REFERENCE.md。脚本不启动参考解码器，
不推测对象排列，不自动寻找延迟，也不把最小二乘增益用于通过判定。
默认仅产生 observation；--max-abs-error 才启用逐样本误差门禁。
退出码 0 表示报告生成成功（或显式门禁通过），1 表示门禁不通过，2 表示输入无效。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
import sys
from pathlib import Path

import numpy as np

BLOCK = 4096
FLOAT_GUID = bytes.fromhex("0300000000001000800000aa00389b71")


def integer(value, name: str, minimum: int | None = None) -> int:
    if type(value) is not int or (minimum is not None and value < minimum):
        raise ValueError(f"{name} 必须为整数" + (f"且 >= {minimum}" if minimum is not None else ""))
    return value


def positive(value, name: str) -> float:
    if type(value) not in (int, float) or not math.isfinite(value) or value <= 0:
        raise ValueError(f"{name} 必须为有限正数")
    return float(value)


def description(value, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"缺少 {name} 的证据说明")
    return value


def checked_file(root: Path, spec: dict) -> tuple[Path, str]:
    path = root / spec["path"]
    expected = spec["sha256"]
    if not isinstance(expected, str) or len(expected) != 64:
        raise ValueError("sha256 必须为 64 位十六进制摘要")
    try:
        int(expected, 16)
    except ValueError as error:
        raise ValueError("sha256 不是十六进制摘要") from error
    with path.open("rb") as reader:
        digest = hashlib.file_digest(reader, "sha256").hexdigest()
    if digest != expected.lower():
        raise ValueError(f"{path.name}: SHA-256 不符")
    return path, digest


def float_wave(path: Path) -> tuple[np.ndarray, int]:
    """只接受有界 RIFF Float32 WAVE；不让 wave 模块把 float 当整数读取。"""
    size = path.stat().st_size
    with path.open("rb") as reader:
        header = reader.read(12)
        if len(header) != 12 or header[:4] != b"RIFF" or header[8:] != b"WAVE":
            raise ValueError("本地 PCM 必须是 RIFF WAVE")
        if struct.unpack_from("<I", header, 4)[0] + 8 != size:
            raise ValueError("RIFF 长度与文件大小不符")
        fmt = None
        data = None
        while reader.tell() < size:
            chunk = reader.read(8)
            if len(chunk) != 8:
                raise ValueError("WAVE chunk 头截断")
            tag, length = struct.unpack("<4sI", chunk)
            offset = reader.tell()
            end = offset + length + (length & 1)
            if end > size:
                raise ValueError("WAVE chunk 越过文件边界")
            if tag == b"fmt ":
                if fmt is not None or length < 16:
                    raise ValueError("重复或过短的 fmt chunk")
                # 只读固定头，避免由恶意长度驱动整块分配。
                fmt = reader.read(min(length, 40))
            elif tag == b"data":
                if data is not None:
                    raise ValueError("重复 data chunk")
                data = (offset, length)
            reader.seek(end)
    if fmt is None or data is None:
        raise ValueError("缺少 fmt/data chunk")
    code, channels, rate, byte_rate, align, bits = struct.unpack_from("<HHIIHH", fmt)
    if code == 0xFFFE:
        if (len(fmt) < 40 or struct.unpack_from("<H", fmt, 16)[0] != 22
                or struct.unpack_from("<H", fmt, 18)[0] != 32 or fmt[24:40] != FLOAT_GUID):
            raise ValueError("WAVE extensible 子格式不是有效的 Float32")
    elif code != 3:
        raise ValueError("WAVE 不是 IEEE Float32")
    if (not channels or not rate or bits != 32 or align != channels * 4
            or byte_rate != rate * align or not data[1] or data[1] % align):
        raise ValueError("WAVE PCM 形状无效")
    array = np.memmap(path, dtype="<f4", mode="r", offset=data[0],
                      shape=(data[1] // align, channels))
    return array, rate


def track_ids(tracks: list[dict], channels: int) -> list[str]:
    if not isinstance(tracks, list) or len(tracks) != channels:
        raise ValueError("轨道描述数量与 PCM 声道数不符")
    by_output = {}
    for track in tracks:
        output = integer(track["output_channel"], "output_channel", 0)
        if output >= channels or output in by_output:
            raise ValueError("轨道 output_channel 重复或越界")
        if track["role"] == "lfe":
            identity = "lfe"
        elif track["role"] == "ajoc_object":
            identity = f"object:{integer(track['ajoc_object'], 'ajoc_object', 0)}"
        else:
            raise ValueError("仅支持 Full A-JOC 对象/LFE 身份")
        by_output[output] = identity
    ids = [by_output[index] for index in range(channels)]
    if len(set(ids)) != channels:
        raise ValueError("轨道身份重复；不得按出现顺序猜测对应关系")
    return ids


def finite_pcm(array: np.ndarray, name: str) -> None:
    for start in range(0, len(array), BLOCK):
        if not np.isfinite(array[start:start + BLOCK]).all():
            raise ValueError(f"{name} 包含非有限 PCM，包括比较区间之外的样本")


def db_ratio(numerator: float, denominator: float) -> float | None:
    if numerator <= 0 or denominator <= 0:
        return None
    return 10.0 * (math.log10(numerator) - math.log10(denominator))


def statistics(xx: float, yy: float, xy: float, error: float,
               fitted_error: float, count: float) -> dict:
    gain = xy / xx if xx else None
    activity = ("both_zero" if xx == yy == 0 else "reference_only" if xx == 0
                else "local_only" if yy == 0 else "both_nonzero")
    return {
        "activity": activity,
        "local_rms": math.sqrt(xx / count),
        "reference_rms": math.sqrt(yy / count),
        "correlation": min(1.0, max(-1.0, xy / math.sqrt(xx * yy))) if xx and yy else None,
        "rmse": math.sqrt(error / count),
        "snr_db": db_ratio(yy, error),
        "zero_error": error == 0,
        "gain_fit_diagnostic_only": {
            "reference_over_local": gain,
            "gain_db": 20 * math.log10(abs(gain)) if gain else None,
            "inverted_polarity": gain < 0 if gain is not None else None,
            "residual_rmse": math.sqrt(fitted_error / count),
            "residual_snr_db": db_ratio(yy, fitted_error),
            "zero_residual": fitted_error == 0,
        },
    }


def compare_track(local: np.ndarray, reference: np.ndarray, scale: float, rate: int) -> dict:
    # 按单轨分配 float64 缓冲，不复制整份多声道媒体。
    x = np.asarray(local, dtype=np.float64) * scale
    y = np.asarray(reference, dtype=np.float64)
    xx, yy, xy = float(x @ x), float(y @ y), float(x @ y)
    delta = y - x
    gain = xy / xx if xx else 0.0
    fitted = y - gain * x
    result = statistics(xx, yy, xy, float(delta @ delta), float(fitted @ fitted), len(x))
    result.update({
        "max_abs_error": float(np.max(np.abs(delta))),
        "local_peak": float(np.max(np.abs(x))),
        "reference_peak": float(np.max(np.abs(y))),
    })
    # Hann STFT 仅诊断频带差异；>=18 kHz 不用于裁决 DRP 与本实现谁正确。
    # 逐样本门禁仍覆盖整个比较区间，不能借频带排除或拟合增益隐藏误差。
    window = np.hanning(BLOCK)
    bins = np.fft.rfftfreq(BLOCK, 1 / rate)
    weights = np.full(len(bins), 2.0)
    weights[0] = weights[-1] = 1.0
    full_blocks = len(x) // BLOCK
    sums = np.zeros((len(bins), 4), dtype=np.float64)
    for start in range(0, full_blocks * BLOCK, BLOCK):
        a = np.fft.rfft(x[start:start + BLOCK] * window)
        b = np.fft.rfft(y[start:start + BLOCK] * window)
        sums[:, 0] += np.abs(a) ** 2 * weights
        sums[:, 1] += np.abs(b) ** 2 * weights
        sums[:, 2] += (a.conj() * b).real * weights
        sums[:, 3] += np.abs(b - a) ** 2 * weights
    bands = []
    if full_blocks:
        normalizer = full_blocks * BLOCK * float(window @ window)
        for low, high in [(0, 12000), (12000, 18000), (18000, rate / 2)]:
            if high <= low:
                continue
            mask = (bins >= low) & ((bins <= high) if high == rate / 2 else (bins < high))
            u, v, cross, error = sums[mask].sum(axis=0)
            fitted_error = max(0.0, v - cross * cross / u) if u else v
            bands.append({
                "hz": [low, high],
                "usable_as_drp_reference": high <= 18000,
                **statistics(float(u), float(v), float(cross), float(error),
                             float(fitted_error), normalizer),
            })
    result["bands"] = bands
    return result


def compare_manifest(manifest: dict, root: Path, max_abs_error: float | None = None) -> dict:
    if not isinstance(manifest, dict) or type(manifest.get("version")) is not int or manifest["version"] != 1:
        raise ValueError("不支持的 manifest version")
    if max_abs_error is not None and (
            type(max_abs_error) not in (int, float) or not math.isfinite(max_abs_error)
            or max_abs_error < 0):
        raise ValueError("max_abs_error 必须为有限非负数")
    case = description(manifest["case"], "case")
    _, source_hash = checked_file(root, manifest["input"])
    local_path, local_hash = checked_file(root, manifest["local_pcm"])
    export_path, export_hash = checked_file(root, manifest["local_export"])
    reference_spec = manifest["reference_pcm"]
    reference_path, reference_hash = checked_file(root, reference_spec)
    if local_path.resolve() == reference_path.resolve():
        raise ValueError("本地与外部参考不能是同一路径")
    export = json.loads(export_path.read_text(encoding="utf-8"))
    if (not isinstance(export, dict) or export.get("schema") != "macinac4.cli-result"
            or type(export.get("version")) is not int or export["version"] != 1
            or export.get("command") != "export-objects-pcm"):
        raise ValueError("local_export 必须来自 export-objects-pcm JSON v1")
    result = export["result"]
    if result["scale"] != "±32768" or result["channel_order"] != "ajoc_objects_with_lfe_reinserted":
        raise ValueError("本地导出尺度或轨序契约不符")
    local, rate = float_wave(local_path)
    audio = result["audio"]
    if (integer(audio["sample_rate_hz"], "local sample rate", 1) != rate
            or integer(audio["channels"], "local channels", 1) != local.shape[1]
            or integer(audio["frames"], "local frames", 1) != len(local)
            or integer(audio["bit_depth"], "local bit depth", 1) != 32
            or audio["format"] != "wave_extensible_ieee_float32"):
        raise ValueError("local_export 与 WAVE 形状不符")
    substreams = {integer(track["substream"], "substream", 0) for track in result["tracks"]}
    if len(substreams) != 1:
        raise ValueError("当前比较契约只接受单一物理 substream")
    local_ids = track_ids(result["tracks"], local.shape[1])
    if (reference_spec["format"] != "interleaved_f32le"
            or integer(reference_spec["sample_rate_hz"], "reference sample rate", 1) != rate):
        raise ValueError("参考必须是同采样率的 interleaved_f32le")
    if rate != 48000:
        raise ValueError("当前频带比较协议限定 48 kHz")
    channels = integer(reference_spec["channels"], "reference channels", 1)
    reference_ids = track_ids(reference_spec["tracks"], channels)
    if set(local_ids) != set(reference_ids):
        raise ValueError("两侧轨道身份集合不一致")
    raw_bytes = reference_path.stat().st_size
    if not raw_bytes or raw_bytes % (channels * 4):
        raise ValueError("参考 PCM 为空或末尾不是完整采样帧")
    reference = np.memmap(reference_path, dtype="<f4", mode="r",
                          shape=(raw_bytes // (channels * 4), channels))
    finite_pcm(local, "local")
    finite_pcm(reference, "reference")
    alignment = manifest["alignment"]
    lag = integer(alignment["reference_lag_samples"], "reference_lag_samples")
    alignment_evidence = description(alignment["evidence"], "alignment")
    scale = positive(manifest["local_scale"], "local_scale")
    scale_evidence = description(manifest["scale_evidence"], "scale")
    identity_evidence = description(reference_spec["identity_evidence"], "identity")
    runtime = description(reference_spec["runtime"], "reference runtime")
    processing = reference_spec["processing"]
    if not isinstance(processing, dict) or not processing:
        raise ValueError("必须记录参考端的 processing 设置")
    start_x, start_y = max(0, -lag), max(0, lag)
    count = min(len(local) - start_x, len(reference) - start_y)
    if count <= 0:
        raise ValueError("显式延迟下没有共同样本")
    tracks = []
    for output, identity in enumerate(local_ids):
        ref_output = reference_ids.index(identity)
        tracks.append({
            "identity": identity, "local_channel": output, "reference_channel": ref_output,
            **compare_track(local[start_x:start_x + count, output],
                            reference[start_y:start_y + count, ref_output], scale, rate),
        })
    excited = sum(track["activity"] != "both_zero" for track in tracks)
    maximum = max(track["max_abs_error"] for track in tracks)
    verdict = "observation_only"
    if max_abs_error is not None:
        verdict = "insufficient_excitation" if not excited else "pass" if maximum <= max_abs_error else "fail"
    return {
        "schema": "macinac4.object-pcm-reference", "version": 1, "case": case,
        "numpy_version": np.__version__,
        "provenance": {
            "input_sha256": source_hash, "local_pcm_sha256": local_hash,
            "local_export_sha256": export_hash, "reference_pcm_sha256": reference_hash,
            "reference_runtime": runtime, "reference_processing": processing,
            "identity_evidence": identity_evidence,
        },
        "alignment": {
            "reference_lag_samples": lag, "evidence": alignment_evidence,
            "local_frames": len(local), "reference_frames": len(reference),
            "local_start": start_x, "reference_start": start_y, "compared_frames": count,
            "uncompared_local_prefix": start_x, "uncompared_reference_prefix": start_y,
            "uncompared_local_suffix": len(local) - start_x - count,
            "uncompared_reference_suffix": len(reference) - start_y - count,
        },
        "scale": {"local_multiplier": scale, "reference_multiplier": 1.0,
                  "evidence": scale_evidence, "gain_fit_applied": False},
        "spectral_method": {"fft_samples": BLOCK, "hop_samples": BLOCK, "window": "numpy.hanning",
                            "discarded_tail_samples": count % BLOCK,
                            "drp_reference_upper_hz_exclusive": 18000},
        "verdict": verdict, "max_abs_error_limit": max_abs_error,
        "excited_tracks": excited, "max_abs_error": maximum, "tracks": tracks,
        "limitations": [
            "静音轨的零差异不验证该轨重建算法；整体全静音不能通过显式门禁。",
            "增益拟合只诊断差异，既不改变 PCM，也不参与通过判定。",
            "整数延迟与对象身份来自显式证据，不由相关性最大化替代。",
            "DRP >=18 kHz 的输出不是本实现的规范目标；低频一致也不证明未激活分支。",
            "成功生成 observation 不等于数值验证通过，更不等于 AC-4 conformance。",
        ],
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path, help="新 JSON 文件；不覆盖已有报告")
    parser.add_argument("--max-abs-error", type=float, help="显式逐样本门禁；不允许增益拟合掩盖差异")
    args = parser.parse_args(argv)
    try:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        report = compare_manifest(manifest, args.manifest.resolve().parent, args.max_abs_error)
        text = json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
        if args.output:
            with args.output.open("x", encoding="utf-8") as writer:
                writer.write(text)
        else:
            print(text, end="")
        return 0 if report["verdict"] in ("pass", "observation_only") else 1
    except (OSError, ValueError, KeyError, TypeError, struct.error) as error:
        print(f"对象 PCM 比较失败: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
