#!/usr/bin/env python3
"""用独立最小二乘解核对 A-SPX 的两处数学解释，不裁决解码器 conformance。

TS103190-1:v1.4.1:5.7.6.4.1.2–5.7.6.4.1.4，Pseudocode 85–89。
只使用构造复数信号；无需 PDF、规范附表、参考库或媒体。
需要 scripts/requirements-pcm.txt 中的 NumPy。
"""

from __future__ import annotations

import json

import numpy as np


def prediction_case(signal: np.ndarray) -> dict:
    # P86 的同一条时序、间隔为 2 的三个抽头。
    times = np.arange(4, len(signal), 2)
    y, x1, x2 = (signal[times - lag] for lag in (0, 2, 4))
    c01 = np.vdot(x1, y)
    c02 = np.vdot(x2, y)
    c11 = float(np.vdot(x1, x1).real)
    c12 = np.vdot(x2, x1)
    c22 = float(np.vdot(x2, x2).real)
    epsilon = 2**-20
    denominator = c22 * c11 - abs(c12)**2 / (1 + epsilon)
    alpha1 = (c01 * c12 - c02 * c11) / denominator
    literal_alpha0 = (-c01 + alpha1 * c12.conjugate()) / c11
    normal_alpha0 = (-c01 - alpha1 * c12.conjugate()) / c11

    # 将 P87 的行列式正则项写成第二抽头的 ridge 项，再直接求增广最小二乘。
    # 这一路没有复写上面的两抽头闭式解。
    ridge = abs(c12)**2 / c11 * epsilon / (1 + epsilon)
    design = np.column_stack((x1, x2))
    augmented = np.vstack((design, [0.0, np.sqrt(ridge)]))
    target = np.concatenate((-y, [0.0]))
    independent, _, rank, singular = np.linalg.lstsq(augmented, target, rcond=None)
    assert rank == 2
    assert max(abs(literal_alpha0), abs(normal_alpha0), abs(alpha1)) < 4
    candidate = np.array([normal_alpha0, alpha1])
    assert np.max(abs(candidate - independent)) < 1e-10

    result = {
        "samples": len(times),
        "ridge_second_tap": float(ridge),
        "condition_number": float(singular[0] / singular[-1]),
        "normal_vs_independent_max_error": float(np.max(abs(candidate - independent))),
    }
    for name, alpha0 in (("literal_plus", literal_alpha0), ("normal_minus", normal_alpha0)):
        residual = y + alpha0 * x1 + alpha1 * x2
        first_equation = abs(np.vdot(x1, residual))
        result[name] = {
            "alpha0": [float(alpha0.real), float(alpha0.imag)],
            "alpha1": [float(alpha1.real), float(alpha1.imag)],
            "first_normal_equation_residual": float(first_equation),
            "residual_energy": float(np.vdot(residual, residual).real),
            "regularized_objective": float(np.vdot(residual, residual).real + ridge * abs(alpha1)**2),
        }
    assert result["literal_plus"]["first_normal_equation_residual"] > 1e-3
    assert result["normal_minus"]["first_normal_equation_residual"] < 1e-10
    assert result["normal_minus"]["regularized_objective"] < result["literal_plus"]["regularized_objective"]
    return result


def preflatten_case() -> dict:
    # 高能量、三阶 dB 包络使 P85 的 +1 地板影响可量化且很小。
    u = np.linspace(-1, 1, 36)
    original_db = 80 + 20*u + 2*u**2 - 3*u**3
    power = 10**(original_db / 10)
    envelope = 10*np.log10(power + 1)
    design = np.column_stack([u**k for k in range(4)])
    coefficients = np.linalg.lstsq(design, envelope, rcond=None)[0]
    slope = design @ coefficients
    gain = 10**((envelope.mean() - slope) / 20)
    literal_db = 10*np.log10(power / gain**2)
    multiply_db = 10*np.log10(power * gain**2)
    result = {
        "input_db_span": float(np.ptp(original_db)),
        "literal_divide_db_span": float(np.ptp(literal_db)),
        "multiply_db_span": float(np.ptp(multiply_db)),
        "scope": "只核对谱斜率方向；不是参考 runtime 采用乘法的证据",
    }
    assert result["literal_divide_db_span"] > 1.99 * result["input_db_span"]
    assert result["multiply_db_span"] < 1e-3
    return result


def main() -> None:
    t = np.arange(42)
    rng = np.random.Generator(np.random.PCG64(2101))
    signals = {
        "two_complex_tones": (1 + .25j)*np.exp(.23j*t) + (.75 - .5j)*np.exp(.81j*t),
        "tones_and_noise": np.exp(.17j*t) + .7*np.exp(.61j*t)
        + .2*(rng.standard_normal(len(t)) + 1j*rng.standard_normal(len(t))),
    }
    result = {
        "schema": "macinac4.aspx-math-check",
        "version": 1,
        "prediction": {name: prediction_case(signal) for name, signal in signals.items()},
        "preflatten": preflatten_case(),
        "conformance": "not_assessed",
    }
    print(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
