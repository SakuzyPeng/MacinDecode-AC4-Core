#!/usr/bin/env python3
"""检查标量 A-SPX 游标在同一 QMF 时间轴的不同区间划分下是否连续。

TS103190-1:v1.4.1:5.7.6.4.3–5.7.6.4.4，Pseudocode 102–105。
构造恒定包络，只改变跨帧区间边界；不读取媒体或噪声表，不调用生产解码器。
该检查区分时间单位，不能裁决 QMF 相位约定或音调初始索引应为 0 还是 1。
"""

from __future__ import annotations

import json


def render(factor: int, carry_ats: int, convert_origin: bool) -> dict:
    subbands = 20
    frame_slots = 16 * factor
    intervals = [(0, 16 + carry_ats), (carry_ats, 16), (0, 16)]
    noise_previous = 0
    tone_previous = 0
    values = {}
    for frame, (first, last) in enumerate(intervals):
        noise_base = noise_previous
        tone_base = 1 if frame == 0 else tone_previous + 1
        origin = first * factor if convert_origin else first
        for ts in range(first * factor, last * factor):
            global_slot = frame * frame_slots + ts
            tone = (tone_base + ts - origin) % 4
            for sb in range(subbands):
                noise = (noise_base + subbands * (ts - origin) + sb + 1) % 512
                key = (global_slot, sb)
                assert key not in values, "不能重复生成上一帧已携带的 QMF 时隙"
                values[key] = (noise, tone)
            noise_previous = noise
            tone_previous = tone
    assert len(values) == 3 * frame_slots * subbands

    differences = []
    for (slot, sb), actual in sorted(values.items()):
        # 独立定义一条全局连续序列，不重用区间起点或上一帧游标。
        expected = ((slot * subbands + sb + 1) % 512, (1 + slot) % 4)
        if actual != expected:
            differences.append({
                "qmf_slot": slot,
                "subband": sb,
                "actual_noise": actual[0],
                "expected_noise": expected[0],
                "actual_tone": actual[1],
                "expected_tone": expected[1],
            })
    return {
        "factor": factor,
        "carry_ats": carry_ats,
        "converted_origin": convert_origin,
        "qmf_slots": 3 * frame_slots,
        "time_band_entries": len(values),
        "differing_entries": len(differences),
        "first_difference": differences[0] if differences else None,
        "noise_final": noise_previous,
        "tone_final": tone_previous,
    }


def main() -> None:
    cases = []
    for factor in (1, 2):
        for carry in range(4):
            literal = render(factor, carry, False)
            converted = render(factor, carry, True)
            assert converted["differing_entries"] == 0
            if factor == 1 or carry == 0:
                assert literal["differing_entries"] == 0
            else:
                assert literal["differing_entries"] > 0
                first = literal["first_difference"]
                assert (first["actual_noise"] - first["expected_noise"]) % 512 == 20 * carry
                assert (first["actual_tone"] - first["expected_tone"]) % 4 == carry
            cases.append({"literal": literal, "converted": converted})
    print(json.dumps({
        "schema": "macinac4.aspx-cursor-timeline-check",
        "version": 1,
        "assumption": "已采用的标量 previous 游标；无配置复位；恒定包络；实际生成区间连续覆盖 QMF 时间轴",
        "tone_initial_index": 1,
        "cases": cases,
        "production_implementation_checked": False,
        "conformance": "not_assessed",
    }, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
