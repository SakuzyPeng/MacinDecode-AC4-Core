# A-JOC 去相关与 ducker：完整历史、同输入与独立递推

日期：2026-10-22–23；起点 `c67d540`。机器记录见
[`ajoc_decorrelator_comparison.json`](experiments/ajoc_decorrelator_comparison.json)。

## 结论

本地 all-pass、15 带输入能量、逐时隙 ducker、跨帧状态和最终 Y 通过独立核对，没有发现
需要修改生产 DSP 的独立反例。参考三个真实窗口的有效 IIR／ducker 历史已恢复，同状态
重放逐位复现实际输出及结束状态。

固定同一输入后，参考与本地仍有可解释的不同：参考使用量化后的滤波系数及合并乘加系数，
能量在其接口量级上限为 1，ducker 比值不含本地 P114 的 EPSILON。这些观察不构成修改
默认算法的依据。本轮只提交测试、检查器、文档和汇总证据。

## 实际生产边界与本地模型

观察入口全部位于 `cfg(test)`。在真正的 `process_timeslot` 内记录输入 u、原 all-pass
结果、能量、增益、ducker 前后状态及输出 Y；重建入口另标记实例下标、启用状态和时隙。
每个调用的次序和数量必须与真实帧尺寸一致。

所有重建时隙保存 u/Y 和实际 decorrelator kind；仅选定窗口保存详细中间量与帧首／末
完整滤波状态。没有把未采样的详细值写成已捕获。完整递归从第一重建帧的零状态推进，
不以有限预热区间代替 IIR 历史。

| 输入 | 连续重建帧 | 连续时隙 | 详细窗口 |
| --- | ---: | ---: | --- |
| 强纯音 143 AU | 142 | 4,544 | AU 7、8、9 |
| 独立音乐 400 AU | 399 | 12,768 | AU 32–34、99–101 |
| bed 256K 49 AU | 48 | 1,536 | AU 2、3、4 |
| 8 条自有七路链 | 48 | 1,376 | 全部 48 帧 |

真实流使用三路去相关器；自有链覆盖完整七路循环、D0/D1/D2、全部 64 子带与三个区域边界，
包含静音、实／虚脉冲、突发、载波、固定种子噪声、低能量及阈值激励。每条链按
24／30／32／24／30／32 时隙分帧。既有普通回归继续检查拆帧等价、reset、非法输入保护，
以及非零 wet 禁用／重新启用时的历史保持。

合计 **8,470,528 个输出实／虚分量**与独立 canonical 数值重放逐位一致；详细窗口的能量、
增益、ducker 状态及滤波状态同样逐位一致。canonical 重放用于核对存储与运算边界，
不能独自承担算法正确性的证明。

另一模型按 P111 的独立输入／输出历史直接递推，系数和频带映射直接从锁定 PDF 抽取，
不读取生产生成表来证明自身。再用 80 位 Decimal 连续核对每路的 0／6／7／22／23／47／
48／63 子带、实／虚两分量，共 1,040 条序列、1,058,816 个采样值。误差界由系数 companion
矩阵的稳定块范数、输入幅度和运算舍入传播预先确定；最大高精度误差／界约 0.003168。
完整直接递推与 canonical 的最大绝对差约 1.61e−10，属于内部 QMF 单位。

## 参考捕获与恢复可信度

参考仍为 DRP 4.3.0.19350 arm64。强纯音 AU 8、音乐 AU 100／33 各在两个独立进程完成
捕获，全部预期 AU、成功返回码、参数设置、有限 PCM 和未插桩完整 PCM 摘要均通过门禁。
两次不完整探索尝试已排除，不能据此推断码流不受支持。

只在合法实际调用中保存初始状态、逐时隙结束状态与 Y，并核对实际输出缓冲区身份。
参考 pre 输入在 decoder 关闭后，使用实际捕获的 X 和 pre 系数调用原 pre 原语重建；
它不是直接插在滤波前读取的 u。随后恢复完整有效 IIR／ducker 历史进行独立重放：每个
窗口的 32×3 次调用均逐位复现实际 Y、延迟历史、递归历史、游标和 ducker 状态。
三窗口每轮共 288 次，两个进程全部重复。

参考只确认子带 0–47 为有效输出。更高工作区不作为输出 oracle；本地完整 64 带的检查
由上一节独立模型承担。原生布局、地址、适配器、系数快照及媒体只留在本地。

## 固定同一物理输入后的差异分解

另在 decoder 关闭后，用自有零状态单独调用参考去相关原语。输入包含九条自有配方和
来自三份完整流的连续 u。真实来源先按已冻结的 `R64 / 2^23` 换算，再限定有效 48 带；
本地重放同一份公共表示 u 时固定乘 `2^23`，输出除回。该二次幂尺度不拟合，也不能
忘记它对 EPSILON 量级的影响。

这十二组重放均从零开始，包含全部输入历史，并在两个参考进程重复。本地也实际运行
生产去相关函数。此实验隔离了矩阵插值、QMF 前端和去相关输入的差异，**不是原节目
完整对象 PCM 的对比**。

参考实际 ducker 状态由“输入能量上限 1、逐时隙更新、融合乘加、无 EPSILON 的比值”模型
逐位预测，包括三个有非零初值的真实窗口。上限在自有阈值案例的 1,932 个带／时隙触发，
在三个真实来源重放中没有触发。各实例状态仍独立，未观察到整帧只更新一次的行为。

参考只读滤波及合并系数落在 `2^-14` 网格上，与锁定 PDF 的印刷值和精确 all-pass 关系
存在细微差异。检查器从这些已捕获系数推导传递函数，再用另一套直接递推验证参考输出；
没有搜索系数或用输出残差拟合它们。

| 固定输入 | 本地生产与参考 SNR | 使用参考传递函数、保留本地 gain | 使用参考传递函数及参考 gain |
| --- | ---: | ---: | ---: |
| 强纯音来源 | 88.08 dB | 145.89 dB | 145.51 dB |
| 音乐来源 | 74.81 dB | 138.79 dB | 138.75 dB |
| bed 来源 | 77.31 dB | 138.74 dB | 138.55 dB |
| 高能量／阈值构造 | 8.72 dB | 8.72 dB | 142.22 dB |
| 极低能量构造 | 23.95 dB | 23.95 dB | 142.30 dB |

表中后两列都是独立诊断模型，未写入生产。只替换 gain 的诊断仍保留前三份真实来源
约 75–88 dB 的差异，支持这些输入的残差主要来自滤波系数；高能量和极低能量构造则能
分别暴露上限与 EPSILON。系数最大差或 SNR 不代表可听影响，也不是端到端精度提升。
规范印刷精度与参考内部表的关系仍须分别评价。

## 故障注入、门禁与复现

七类实际生产故障注入均被捕获：提前执行新 ramp、错误延迟、错误频带、交换实例、以
all-pass 输出计算能量、将 ducker 更新限制到帧首、清除跨帧历史。等价乘法换序及恢复后的
时间／去相关基线通过。故障注入后所有生产文件已恢复，本轮不保留实验算法。

新增 21 项无媒体 Python 判据测试接入 CI，覆盖严格阈值及相邻浮点数、低能量、时钟、
因果延迟、实例独立、缺失／错误表、截断、非有限值和整数边界。默认／audio-decode／
spec-tables 分别 516／1,331／107 项测试通过，相关 Python 套件共 157 项通过；三套
Clippy、两套 no_std、格式、分层、规范来源及表、数学／变换常量、工作流语法检查通过。

使用同一构建缓存重新构建 release 验证产物，Core／A-SPX／objects 各 12 条基线全部通过。
三份完整对象流的 25,460,736 个 PCM 样本与前轮正式导出逐位一致。验证产物留在外盘，
既有正式二进制恢复原摘要；三份入库 PCM 基线始终未改写。

```bash
MACINAC4_AJOC_MATRIX=1 MACINAC4_AJOC_DECORR=1 \
MACINAC4_AJOC_INPUT_STREAM=/absolute/path/input.ac4 \
MACINAC4_AJOC_INPUT_OUTPUT=/absolute/path/new-local \
MACINAC4_AJOC_INPUT_AUS=32,33,34,99,100,101 \
  cargo test -p macindecode-ac4-decode --features audio-decode \
  capture_ajoc_input_boundary -- --ignored
python3 scripts/check_ajoc_decorrelator.py --extract-tables /absolute/path/tables.json
python3 scripts/check_ajoc_decorrelator.py --prepare /absolute/path/new-synthetic
MACINAC4_DECORR_REPLAY=/absolute/path/new-synthetic/burst \
  cargo test -p macindecode-ac4-decode --features audio-decode \
  replay_ajoc_decorrelator -- --ignored
python3 scripts/check_ajoc_decorrelator.py /absolute/path/manifest.json \
  --output /absolute/path/new-comparison.json
python3 -m unittest scripts/test_check_ajoc_decorrelator.py scripts/test_check_ajoc_timing.py
```

下一步如继续扩大验证，应补非零起点／非 32 ramp 的真实参考时间激励，并把已解释的
去相关系数、ducker 条件与前端残差分开记账。当前结果不关闭全部 AC-4 判读，不改变
参考有效带宽之外、非本轮拓扑或不同运行时版本的覆盖声明。
