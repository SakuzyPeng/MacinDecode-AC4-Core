# ADR-0009：QMF 分析的镜像子带成对调制

- 状态：Accepted
- 日期：2026-08-26
- 关系：服从 [ADR-0002](0002-numeric-format-for-reconstruction.md) 的逐位标量约束；使用 [ADR-0003](0003-trigonometric-tables-for-the-transform.md) 的冻结 QMF 相位表；与 [ADR-0008](0008-paired-qmf-synthesis-modulation.md) 的合成成对调制互相独立

## 背景

`TS103190-1:v1.4.1:Pseudocode 65` 的分析调制对每个 QMF 时隙执行 `64 × 128` 项直接求和：

```text
Q[sb] = Σ_n u[n] · exp(j·2π·(2sb+1)·(2n−1)/512)
```

此前保留的两声道入口只沿声道轴并排执行这棵加法树；每路仍分别计算全部 64 个子带。实现继续
要求 `no_std`、稳态零分配、safe Rust、禁止 FMA/fast-math，并保持 Core、A-SPX 与 A-JOC 对象
PCM 逐位不变。因此可以共享乘法，但不能横向归约子带或改变任一输出的 `n=0…127` 累加顺序。

## 决策

1. **分析子带按 `sb` 与 `63−sb` 成对计算。** 令 `q=2sb+1`、`m=2n−1`。镜像子带的奇数
   频率 `q'` 为 `128−q`，所以：

   ```text
   exp(j·q'·m·2π/512)
     = exp(j·mπ/2) · conj(exp(j·q·m·2π/512))
   ```

   当 `n` 为偶数时，`m ≡ 3 (mod 4)`，镜像相位为 `(-sin, -cos)`；当 `n` 为奇数时，
   `m ≡ 1 (mod 4)`，镜像相位为 `(sin, cos)`。
2. **两个子带共享一次相位加载和两次乘法。** 每个 `n` 只计算 `real=value·cos` 与
   `imaginary=value·sin`，分别馈入原子带与换位后的镜像子带。相位加载和乘法从每时隙
   8,192 组降为 4,096 组。
3. **偶数与奇数 `n` 相邻展开。** 这消除镜像符号的热循环分支，但每个实部/虚部累加器仍以
   `n, n+1` 的顺序更新；没有结合、交换或减少任何输出自身的 128 次加法。
4. **标量和既有两声道入口使用同一关系。** 两声道入口保留声道间独立的 2×f64 lane；镜像
   子带只共享相位和乘法结果，不做跨 lane 或跨子带归约。奇数尾声道继续走标量入口。
5. **保留逐子带定义式 oracle。** 测试核对全部 4,096 个镜像相位对，并以确定性非零值、有限
   浮点边界值、跨调用状态和真实媒体基线锁定逐位等价。

## 验证证据

### 正确性与构建约束

- 19 个确定性折叠时隙和 10 个有限边界时隙（含 `±0`、subnormal、最小 normal 与
  `±f32::MAX`）的 64 个输出均逐位等于定义式。
- 19 个完整 PCM 时隙拆成两次调用后，输出和全部分析滤波状态逐位等于拆分前字面实现。
- `scripts/decode_check.py --stage all` 的 Core、A-SPX 和 A-JOC 对象 PCM 基线全部通过；两条
  尚未实现的 channel-based IMS 向量仍按既有规则跳过。
- 默认路径没有 `std`、Rayon、平台 intrinsic、`unsafe`、FMA、fast-math 或新增工作区；
  allocation 构建对 24/24 项测得零 allocation、reallocation、deallocation 和 bytes。

### 性能

M4 Pro、macOS 27.0、Rust 1.96.0，普通 portable release。before 为已推送提交 `acb6db3`，
after 只增加本决策：

- before SHA-256：`92e6d65549f8d4261de3fe1c19c62a10e6a657b5aa98a0803f78298e7d7a913a`
- after SHA-256：`8c611d59fe36da7f4d256c31c28a46c2bcb25ee0f430e7c487ac7e315112e87e`

12 条真实媒体的 Core/Full 共 24 项执行三轮交替 A/B。每项先完整预热，再测至少 5 个完整 pass
且累计至少 2 秒；比较时统一换算为单 pass，并先取每项三轮中位数：

| 模式 | 汇总总时长改善 | 单项总时长改善范围 | p99 改善范围 |
| --- | ---: | ---: | ---: |
| Core | 20.99% | 20.23%–21.52% | 18.80%–21.45% |
| Full | 9.82% | 8.49%–10.53% | 8.15%–25.83% |

24/24 总时长改善、24/24 p99 改善，before 与 after 的六次全套运行均无 deadline miss。完整
汇总见 [M4 Pro QMF 分析镜像子带成对 A/B](../experiments/m4_pro_qmf_analysis_subband_pairs_ab.json)，
原始逐轮 JSON 只保留在 `target/perf/`。

ARM64 精确 after 二进制的两声道分析函数包含 2×f64 `fmul`/`fadd`，没有 FMA 或
`panic_bounds_check` 调用。当前版本中，两声道入口相对逐路标量的 release 微基准仍为
`1.056×`；其相对收益缩小是因为两条路径现在都共享镜像子带乘法，不是功能回退。

## 未采用的方向

本决策不改用 FFT、横向 SIMD 归约或近似三角函数。它们会重排浮点加法、改变冻结相位值或同时
扩大多个变量，无法直接继承本决策的逐位结论。未来若单独探索，必须以本实现和字面 oracle 为
基线重新通过正确性及完整 A/B 门禁。
