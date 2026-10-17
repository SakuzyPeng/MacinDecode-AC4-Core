# ADR-0018：可选保留 A-SPX limiter 关闭时的能量补偿

日期：2026-10-04。状态：已采用，默认关闭。

## 背景

`TS103190-1:v1.4.1:4.3.10.1.7` 表 122 定义 limiter 开／关，但
`5.7.6.4.2.2` 的 P95–101 未明确关闭标志对整个 boost 链的作用范围。本地原实现
在 `aspx_limiter=0` 时直接使用 P95/P94 的初始信号增益与噪声／正弦电平。

[内部参考检查点记录](../experiments/aspx_reference_checkpoints.json)
确认，已测 DRP 路径在关闭 limiter 后仍执行补偿并保留有限增益约束。用户要求将这项
行为差异做成可配置开关，以便独立比较，不强制改变既有输出。

## 决策

新增 `aspx_limiter_off_boost`，默认 `false`：

| 码流 `aspx_limiter` | 新选项 | 行为 |
| --- | --- | --- |
| 1 | 任意 | 沿用 P95–101 的原有路径 |
| 0 | false | 沿用 P95/P94 初始结果，boost 为 1 |
| 0 | true | 跳过 P96–98，以初始结果执行 P99–101 的按组能量补偿 |

补偿使用同源的 limiter 分组表，沿用 P99 的噪声／正弦能量条件以及 P100 的约 4 dB
boost 上限。选项不启用 `MAX_SIG_GAIN` 硬上限，也不更改预平坦化、QMF 相位、游标、
码流语法或传输标志。因此它是单独的补偿策略，不声明与 DRP 的完整关闭分支逐位兼容。

Scene API 通过 `Ac4DecoderConfig::with_aspx_limiter_off_boost(bool)` 选择；低层 Full
engine 通过 `FullAjocDecoder::with_aspx_limiter_off_boost(bool)` 创建空 decoder。
两者的策略在实例创建时固定，reset、discontinuity 和逐 substream reset 保留选择。
直接使用元素／A-SPX 通路的调用方通过 `ElementParams::aspx_limiter_off_boost` 或
`HfAdjustParams::limiter_off_boost` 传入；低层调用方负责维持策略的时间线一致性。

CLI 在 `export-aspx-pcm`、`export-objects-pcm`、`export-core-caf`、`export-full-damf`
和 `export-full-adm-bwf` 提供 `--aspx-limiter-off-boost`。这五类成功响应的
`result.aspx_limiter_off_boost` 始终为实际选项的布尔值；它表示请求策略，不代表每个 AU
都触发该分支。pre-A-SPX 核心带和合成噪声探针命令不提供此选项。

## 验证与边界

数值判据覆盖补偿后的能量闭合、约 4 dB 上限、信号／噪声／正弦共同缩放，以及不引入
信号增益硬上限。错误分组来源仍在状态提交前拒绝。通路判据要求 limiter=1 不受新选项
影响，Session 判据要求配置实际传到 engine 且跨 reset 保留。

默认三层 PCM 基线不更新；真实 limiter=0 流用于验证显式选项确实改变输出，而原始
limiter=1 流用于验证选项无副作用。该选项的测量与其它相位、预平坦化诊断分开记录。

实现验证：默认构建 516 项、`audio-decode` 构建 1,283 项测试通过，两套 Clippy、格式和层
审计通过；三层共 36 条既有 PCM 基线逐位不变。真实 limiter=0 流的 A-SPX／对象出口在启用
选项后改变且保持有限，limiter=1 流的两类出口逐位不变；四个默认出口也分别与改动前二进制
比对一致。摘要与形状见[验证记录](../experiments/aspx_limiter_off_boost_option.json)。

未来若要把补偿改为默认或采用参考端的其它增益约束，须另作决策并重新验证，不能将其
悄悄包含在这个布尔选项内。

2026-10-16 的[独立对照](../ASPX_LIMITER_OFF_INVESTIGATION.md)量化了单项效果：强纯音的
12–18 kHz 误差增大约 1.11 dB，音乐基本不变。参考关闭分支还保留有限分组约束、补偿前硬上限
和额外能量地板，因此本选项仍只表示独立的补偿策略，不承诺普遍降低参考差异；默认值不变。
