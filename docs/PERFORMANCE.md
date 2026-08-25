# AC-4 解码性能基线

本报告首先记录 2026-08-25 在 Apple M4 Pro ARM64 上完成的 Core/Full 解码性能基线，并在后文
追加同日完成的 QMF 优化实验。基线范围是
`vectors/objects_baseline.json` 中 12 条真实 A-JOC 媒体及其 `presentation_overrides`，共 24 个
“媒体 × 模式”组合。基线轮只建立测量工具和人工分析数据；后续优化不改变 PCM 位模式、公共 API，
也没有引入 SIMD 和自动性能门禁。原始基线数字不按优化后结果重写。

原始结果与实验汇总：

- [timing JSON](experiments/m4_pro_decode_timing.json)
- [allocation JSON](experiments/m4_pro_decode_allocations.json)
- [Core QMF 分段 JSON](experiments/m4_pro_qmf_split_core.json)
- [Full QMF 分段 JSON](experiments/m4_pro_qmf_split_full.json)
- [QMF 成对调制 A/B JSON](experiments/m4_pro_qmf_paired_ab.json)

## 结论

- 24/24 组合均成功完成，无 decode error、`WaitingForRandomAccess`、空输入或非有限指标。
- Core 为 `11.64x`–`23.10x` 实时，Full 为 `5.22x`–`7.22x` 实时；当前正式运行均无
  deadline miss。
- 最差单 AU 预算占用为 Core `19.07%`、Full `48.09%`。上一版基线曾捕获一次 Full
  56.114 ms 极值，但本轮没有复现；后续 A/B 重复测量继续保留并定位这类样本。
- 完整预热并复用 `Ac4DecoderSession` 容量后，24/24 组合的 allocation、reallocation 和
  deallocation 都为零。
- Core 和 Full 的第一热点都是 QMF；Full 的前三大 top-of-stack 符号依次为 QMF 合成、
  A-JOC 重建和 QMF 分析。因此后续若单独立项优化，首先应验证 QMF 分析/合成，再评估
  A-JOC 重建。
- 首个 QMF 候选“融合合成尾段的加窗与相位求和”通过逐位 PCM 门禁，但五轮交替 A/B 在
  Full 标准 1500K 上仅提升 `0.186%`，未达到预先约定的 `5%` 保留线，代码已撤销。
- 后续符号级拆分确认，合成调制占 `synthesise` 的 Core `96.57%`、Full `96.48%`；窗尾仅占
  `2.24%` / `2.08%`。
- 最终保留的成对调制利用输出行的负共轭关系共享一半乘法和相位加载，逐位 PCM 与稳态零分配
  均不变。24/24 项总时长与 p99 都改善，Core 汇总提升 `20.94%`、Full 提升 `24.35%`。

## 测量环境

| 项目 | 值 |
| --- | --- |
| CPU / 架构 | Apple M4 Pro / ARM64 (`aarch64`) |
| 内存 | 51,539,607,552 bytes（约 48 GiB） |
| 系统 | macOS 27.0 |
| Rust | 1.96.0 (`rustc 1.96.0 (ac68faa20 2026-05-25)`) |
| Cargo | 1.96.0 |
| 构建 | 普通 `--release`，`target_cpu = portable-default` |
| timing 生成时间 | 2026-08-25 04:29:03 UTC |
| allocation 生成时间 | 2026-08-25 01:40:33 UTC |
| 被测提交 | `404ceded6e4f5cfa610cf360d070f6362332da7b`；工作树包含本次性能观测改动 |

## 测量边界与方法

`macindecode-ac4-perf` 是 `publish = false` 的内部 workspace crate。它先通过
`macindecode-ac4-mp4` 使用的同一组 MP4 primitive 读取 sample table，把整个文件和 AU 保存为
已检查的字节范围，并预先构造准确的 `AccessUnitContext`。文件 I/O、MP4 sample table、edit
计算、manifest/presentation 选择和 JSON 序列化全部在计时区外；计时边界只包围
`Ac4DecoderSession::decode_access_unit`，Core 模式不启用 core-band diagnostics。

timing 对每个组合执行 20 次全新 Session 的首 AU 测量；随后完成一个完整预热 pass，调用
`reset` 并复用 Session 容量。稳态测量至少运行 5 个完整 pass、累计至少 2 秒，最多 30 pass；
本次实际范围为 Core 5–23 pass、Full 5–7 pass。百分位采用 nearest-rank。实时预算按每个 AU
实际解码采样数和 48 kHz 采样率计算；典型 2,048-sample AU 的预算为 42.67 ms，而不是对所有
AU 使用固定常数。每个案例另按预算占用保存最差的 8 个 AU，包含零基 pass/AU 索引、延迟、
预算、占用比和 miss 标志；排序与 JSON 整理均在计时区外。

allocation 使用 `stats_alloc 0.1.10` 的独立构建。每个组合先完整预热并 `reset`，再统计一个完整
pass；统计构建的执行时间不作为 timing 数据。所有 Session 都关闭 core-band diagnostics。

## 稳态延迟

| 模式 | 实时倍速范围 | 最慢整案 | 最差 p99 | 最大单 AU | 最差预算占用 | deadline miss | 冷启动首 AU 最大值 |
| --- | ---: | --- | --- | --- | ---: | ---: | --- |
| Core | 11.64x–23.10x | DME L4 1500K：11.64x | 4.486 ms，ramp control 768K | 8.138 ms，ramp control 768K | 19.07% | 0 | 4.389 ms，标准 1500K |
| Full | 5.22x–7.22x | DME L4 1500K：5.22x | 9.385 ms，ramp lengths 768K | 20.519 ms，DME L4 768K 3DoF | 48.09% | 0 | 6.713 ms，标准 1500K |

这里的“最慢整案”按整个媒体的总解码时间计算；p99、max 和预算占用分别在同模式的 12 个案例中
独立取最差值，因此不一定来自同一个媒体。每个案例的 ns/AU、p50、p95、p99、max、逐 AU
计算的最坏预算比例、8 个最差 AU 事件及运行 pass 数均保存在 timing JSON 中。上一版正式数据
在标准 1500K 的 715 次 Full 调用中捕获过单个 56.114 ms miss，但当时尚未记录 AU 索引；本轮
同案例 max 为 8.581 ms、0 miss，故目前只能记为未复现，不能追溯指定码流位置。

## 稳态分配

24/24 组合在一个完整稳态 pass 内均得到以下结果：

| 指标 | 合计 |
| --- | ---: |
| allocations | 0 |
| reallocations | 0 |
| deallocations | 0 |
| bytes allocated | 0 |
| bytes reallocated | 0 |
| bytes deallocated | 0 |

这只证明当前 12 条媒体、Core/Full 两种模式在“完整预热后复用同一 Session”的边界内无堆操作；
不代表 Session 创建、首次容量增长、MP4 预解析、出口制品组装或其他尚未覆盖码流也无分配。

## CPU 热点

固定输入为 `probe_axes_single_object/master_ac4_1500K.m4a`。每种模式先完整预热，再循环解码约
30 秒；macOS `sample` 以 1 ms 间隔抓取 20 秒。Full profile 完成 24 pass / 3,432 次 AU 调用，
Core 完成 52 pass / 7,436 次 AU 调用。原始 `sample` 文本保留在 `target/perf/`，不进入版本控制。

### 组件归并

| 组件 | Core 样本 | Core 占比 | Full 样本 | Full 占比 |
| --- | ---: | ---: | ---: | ---: |
| 解析 / 熵解码 | 54 | 0.33% | 28 | 0.17% |
| ASF / IMDCT | 1,162 | 7.00% | 541 | 3.25% |
| A-SPX / QMF | 14,629 | 88.19% | 12,283 | 73.75% |
| A-JOC 重建 | 0 | 0.00% | 3,084 | 18.52% |
| Scene 组装 | 163 | 0.98% | 105 | 0.63% |
| 其他 | 581 | 3.50% | 613 | 3.68% |
| 合计 | 16,589 | 100.00% | 16,654 | 100.00% |

归并基于 top-of-stack 符号的 namespace 和调用祖先。由 A-SPX 调用的通用 `math` 符号归入
A-SPX/QMF；系统等待、内存 primitive 和未归类框架开销归入“其他”。这是采样归因，用于识别
热点优先级，不是插桩得到的精确阶段耗时。

### Core top-of-stack 前十

| 排名 | 符号（省略 crate 前缀与 hash） | 样本 | 占比 |
| ---: | --- | ---: | ---: |
| 1 | `aspx::qmf::synthesise` | 7,241 | 43.65% |
| 2 | `aspx::qmf::analyse` | 6,232 | 37.57% |
| 3 | `asf::imdct::ifft::stockham_stage` | 612 | 3.69% |
| 4 | `_platform_memmove` | 361 | 2.18% |
| 5 | `math::log2` | 359 | 2.16% |
| 6 | `asf::imdct::transform::transform` | 221 | 1.33% |
| 7 | `full_ajoc::asf::FullAjocAsfDecoder::decode_frame` | 199 | 1.20% |
| 8 | `aspx::tna::prediction_filters` | 179 | 1.08% |
| 9 | `frame_alignment::FrameAlignmentState::process` | 114 | 0.69% |
| 10 | `aspx::preflatten::pre_flatten` | 96 | 0.58% |

### Full top-of-stack 前十

| 排名 | 符号（省略 crate 前缀与 hash） | 样本 | 占比 |
| ---: | --- | ---: | ---: |
| 1 | `aspx::qmf::synthesise` | 8,966 | 53.84% |
| 2 | `ajoc::reconstruction::reconstruct_frame` | 2,919 | 17.53% |
| 3 | `aspx::qmf::analyse` | 2,831 | 17.00% |
| 4 | `_platform_memmove` | 325 | 1.95% |
| 5 | `asf::imdct::ifft::stockham_stage` | 278 | 1.67% |
| 6 | `math::log2` | 162 | 0.97% |
| 7 | `ajoc::decorrelator::process_timeslot` | 141 | 0.85% |
| 8 | `__semwait_signal` | 101 | 0.61% |
| 9 | `asf::imdct::transform::transform` | 100 | 0.60% |
| 10 | `full_ajoc::asf::FullAjocAsfDecoder::decode_frame` | 99 | 0.59% |

`__semwait_signal` 来自 profiler 在 3 秒 attach 窗口结束前捕获的少量等待样本，已归入“其他”。

## QMF 合成分段归因

为避免在 `no_std` DSP 中插入高频时钟读取，分段仍使用 macOS `sample`。内部
`qmf-split-profile` feature 只在采样构建中把 `synthesise` 固定为父符号，并把每时隙拆成状态
搬移、`128 × 64` 合成调制和多相窗尾三个 `inline(never)` helper；普通构建强制内联这些 helper。
状态搬移后另有一个仅该 feature 启用的 `black_box` 读，用来阻止编译器把 helper 尾调用成
`memmove` 而丢失栈帧。性能工具会拒绝在这种构建上运行 timing 或 allocations。

Core/Full 沿用标准 1500K 输入，各运行约 30 秒，并以 1 ms 间隔采样其中 20 秒。汇总器只读取
`sample` 的 `Call graph` 区段，按三个互斥 helper 的 inclusive 样本计数；原始 profiler 文本仍只
保留在 `target/perf/`。专用二进制 SHA-256 为
`84aa7fab810e75b9dce12436b68005dc2142085c533987fde544532d7a7f22e6`。

| 模式 | 主线程样本 | QMF 合成 / 全程 | 状态搬移 / QMF（全程） | 合成调制 / QMF（全程） | 多相窗尾 / QMF（全程） | 未归类 / QMF |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Core | 16,594 | 7,108 / 42.83% | 83 / 1.17%（0.50%） | 6,864 / 96.57%（41.36%） | 159 / 2.24%（0.96%） | 2 / 0.03% |
| Full | 14,384 | 7,578 / 52.68% | 109 / 1.44%（0.76%） | 7,311 / 96.48%（50.83%） | 158 / 2.08%（1.10%） | 0 / 0.00% |

专用构建的 QMF 总占比与原始函数级采样的 Core `43.65%`、Full `53.84%` 接近；差值属于独立
采样和函数边界变化，不能作为 timing 回归数据。两种模式中，调制本身都占整个解码约四至五成，
而彻底消除窗尾的理论全程收益上限也只有约 `1%`，因此下一节 `0.186%` 的实测结果并不反常。
据此把快速调制分解作为独立候选，并继续以逐位 PCM 与普通 portable release A/B 决定是否保留；
结果见下一节。

## QMF 合成成对调制（已保留）

完整推导、数值约束与被否决方案见
[ADR-0008](decisions/0008-paired-qmf-synthesis-modulation.md)。令 `q = 2sb+1`、
`a = q(2n+1)`，合成输出 `n` 与 `127−n` 的相位分别等于 `−exp(ja)` 与 `conj(exp(ja))`。
因此每个子带只需计算一次 `re·cos` 和 `im·sin`，再分别以 `imaginary−real`、
`real+imaginary` 喂给两个累加器。每个输出仍按 `sb = 0…63` 的原顺序累加，不使用 FMA、SIMD、
额外表或工作区。

测试逐项核对全部 4,096 个相位对，并用 19 个非零时隙和 10 个有限边界时隙对定义式做逐位
差分；完整合成仍与拆分前字面实现逐位一致。真实向量的 Core、A-SPX 和 A-JOC 对象 PCM 摘要
全部不变。五轮交替 release 微基准的中位数为 `4,012.994 → 2,360.331 ns/slot`，局部内核
`1.700×`。

A/B 冻结二进制 SHA-256：before 为
`5f3b6c38b9493f833c45d91b452a9db2d0541afe63e5cb2eb0936a1c32c4cc5b`，after 为
`58218f7b80c28f0800aec88dfa826f88f6402f3209d5accf8903605390b34066`。全部 24 项各先完整预热，
再固定测 5 个 pass；两个二进制各跑五轮，顺序为 B/A、A/B、B/A、A/B、B/A。下表按每项五轮
中位数汇总：

| 模式 | 汇总 before | 汇总 after | 汇总提升 | 单项总时长提升范围 | p99 提升范围 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Core | 18.463 s | 14.596 s | 20.94% | 20.61%–21.25% | 18.85%–21.77% |
| Full | 43.611 s | 32.994 s | 24.35% | 23.31%–25.84% | 22.29%–27.58% |

24/24 项总时长改善，24/24 项 p99 改善。标准 1500K 的独立五轮 A/B 中，Core 五个 pass
中位总时长为 `2.776 → 2.201 s`（20.73%），Full 为 `6.105 → 4.656 s`（23.73%）。优化后
allocation 构建复测 24/24 项，稳态 allocation/reallocation/deallocation/bytes 仍全部为零。

全套 after 首轮的 Full DME L4 768K 3DoF 在 pass 3 / AU 67 捕获一次 `60.310 ms`，超过
`42.667 ms` 预算；另外四轮未 miss。随后同一案例再跑五轮 after、3,575 次 AU 调用，0 miss，
最大 `7.719 ms`。原始 miss 保留在汇总 JSON，不把它抹成“0”；结合该案例总时长改善 24.59%、
p99 改善 24.90% 和定向复测结果，将其记录为未复现的单次 wall-clock 离群值，而非稳定回归。

探索过的 128 点 FFT 前/后旋转虽有 `3.339×` 局部速度，但 2,432 个输出中 1,435 个（59.00%）
位模式变化，违反逐位门禁，已否决。64 KiB 完整相位矩阵保持逐位输出，但只有 `1.181×`，收益
和缓存代价都不如成对方案，也未保留。

## QMF 合成尾段实验（未保留）

首个候选删除 `synthesise` 中 640 项 `f64` 临时窗，把加窗乘法与最终 64 相位求和融合；每个
相位仍严格按 `low0, high0, …, low4, high4` 的原顺序累加，且不使用 `mul_add`。测试中保留
优化前的字面参考实现，19 个非零 QMF 时隙分两次调用后，PCM 与全部 1,280 项合成状态均逐位
相同；现有 Core、A-SPX 和 A-JOC 对象 PCM 真实向量基线也全部逐位通过。

A/B 使用两个冻结的 portable release 二进制，before SHA-256 为
`9b9f45fe033dffc4c3622aa78ff34a7992e0c313a351b3befe47c6f55782d26e`，after 为
`76cbb2354ae63b71c63c085224c1551b0e34547236b1723997ad76129fe0c681`。每个二进制运行 5 次
完整 24 项 timing，顺序为 B/A、A/B、B/A、A/B、B/A；原始文件保留在 `target/perf/`，不进入
版本控制。主案例固定为 Full 标准 1500K 的 5 个完整 pass；其他案例若因累计 2 秒条件跨过
不同 pass 数，则按单个完整 pass 的总时长比较。

| 判据 | before 中位数 | after 中位数 | 变化 | 门槛 | 结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| Full 标准 1500K 总解码时长 | 5.807970 s | 5.797148 s | 提升 0.186% | 至少提升 5% | 不通过 |
| 24 项最差单 pass 总时长回归 | — | — | +0.490% | 不超过 +2% | 通过 |
| 24 项最差 p99 回归 | — | — | +0.765% | 不超过 +3% | 通过 |
| deadline miss 合计 | 0 | 0 | 0 | 不新增 | 通过 |

五轮 before 的最大单 AU 为 27.985 ms，五轮 after 为 19.454 ms，均未 miss。Full 标准 1500K
的 5 次 after 运行中，最差八项里出现最多的 AU 索引也只各出现 3/40 次，未形成稳定的码流
位置聚集。由于主收益只有 0.186%，远小于测量门槛，即使其余护栏均通过也按约定撤销候选；
这说明 `synthesise` 的函数级热点不能归因于该 10 项多相尾段。结合运算规模，下一步应先把
`128 × 64` 合成调制与窗尾分开归因，再决定是否立项快速调制分解，而不是继续微调临时数组。

## 复现命令

从仓库根目录运行普通 portable release 构建：

```bash
cargo run --release -p macindecode-ac4-perf --features audio-decode -- \
  timing --output target/perf/m4-pro-timing.json

cargo run --release -p macindecode-ac4-perf \
  --features audio-decode,allocation-stats -- \
  allocations --output target/perf/m4-pro-allocations.json
```

热点采样分别把 `--mode` 设为 `core` 和 `full`：

```bash
cargo run --release -p macindecode-ac4-perf --features audio-decode -- \
  profile --mode full --duration-seconds 30 \
  --output target/perf/m4-pro-full-profile.json
```

看到 `PROFILE_READY pid=<pid>` 后，在另一个终端执行：

```bash
sample <pid> 20 1 -file target/perf/m4-pro-full.sample.txt
```

QMF 分段归因使用专用构建；该 feature 不能用于 timing/allocation：

```bash
cargo build --release -p macindecode-ac4-perf \
  --features audio-decode,qmf-split-profile

target/release/macinac4-perf profile --mode full --duration-seconds 30 \
  --output target/perf/qmf-split-full-profile.json

sample <pid> 20 1 -file target/perf/qmf-split-full.sample.txt

target/release/macinac4-perf qmf-sample-summary \
  --sample target/perf/qmf-split-full.sample.txt \
  --profile target/perf/qmf-split-full-profile.json \
  --output target/perf/qmf-split-full-summary.json
```

不要把 `target/perf/*.sample.txt` 加入版本控制；其中包含本机 profiler 元数据。正式 JSON 只记录
相对输入标识、运行参数、机器信息和指标，不记录媒体内容或私有工具路径。

## 基线边界

本数据仅代表当前 M4 Pro、当前 12 条 A-JOC 长帧向量和当前 portable release 实现，是人工分析
基线，不是 CI 门禁。它尚未覆盖 x86-64、短帧、channel-based、direct-object、C ABI、fuzz、
长期运行或未来 SIMD 路径。任何后续性能改动都应独立立项，并继续通过现有逐位 PCM 基线后再
与本报告比较。
