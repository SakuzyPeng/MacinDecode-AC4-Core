#!/usr/bin/env python3
"""核对 A-SPX 诊断相位基底的代数契约；不加载生产滤波器或参考 PCM。"""

from __future__ import annotations

import cmath
import json
import math


def rotation(subband: int) -> complex:
    return -cmath.exp(1j * math.pi * (subband + 0.5) / 128)


def main() -> None:
    sample = complex(0.7, -0.25)
    injected = complex(0.3, -0.8)
    patches = []
    for source, target, expected in [(28, 36, -11.25), (24, 44, -28.125),
                                     (22, 34, -16.875), (14, 44, -42.1875)]:
        before = rotation(source) * sample
        unchanged_copy = before / rotation(target)
        # 同一物理算子改用新基底时，交叉子带系数也必须变换。
        transformed_operator = rotation(target) / rotation(source)
        same_physical_output = transformed_operator * before / rotation(target)
        assert abs(same_physical_output - sample) < 1e-14
        phase = math.degrees(cmath.phase(unchanged_copy / sample))
        assert abs(phase - expected) < 1e-12
        assert abs(unchanged_copy - sample) > 0.05
        patches.append({"source": source, "target": target,
                        "unchanged_copy_phase_deg": phase,
                        "transformed_operator_error": abs(same_physical_output - sample)})

    direct_error = injection_error = sign_gauge_error = 0.0
    for subband in range(64):
        r = rotation(subband)
        direct_error = max(direct_error, abs((r * sample) / r - sample))
        injection_error = max(injection_error, abs((r * injected) / r - injected))
        # R 与 -R 的选择可由生成分量同时反相吸收，黑盒 PCM 不能唯一定位负号。
        sign_gauge_error = max(sign_gauge_error, abs((-injected) / (-r) - injected / r))
    assert max(direct_error, injection_error, sign_gauge_error) < 1e-14
    print(json.dumps({
        "schema": "macinac4.aspx-phase-basis-check",
        "version": 1,
        "patches": patches,
        "direct_roundtrip_error": direct_error,
        "transformed_injection_error": injection_error,
        "sign_gauge_error": sign_gauge_error,
        "scope": "诊断基底代数；没有验证生产实现、参考内部实现或 conformance",
    }, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
