# ADR-0006：Core PCM 与 A-SPX QMF 的 2 倍量级边界

- 状态：Accepted；属于跨条款解释，尚无 ETSI 勘误或官方逐样本向量确认
- 日期：2026-08-22

## 背景

ETSI TS 103 190-1 V1.4.1 对 IMDCT 的 2 倍量级留下了一组不能只靠单条伪码消解的矛盾：

1. `Pseudocode 62` 在后旋转中除以 `N`。
2. `Pseudocode 64` 直接执行 `pcm[n] = overlap[n]`，随后又把
   `pIMDCT,ch` 定义为该 `pcm` 数组的内容，没有写 `×2`。
3. 紧随其后的 `5.5.3` 块切换示例在 480、480、960 三次重叠相加中都明确写了
   `factor of 2`，并称 composition buffer 的最早 `frame_length` 个样本会交给处理链中的
   下一个工具。
4. `5.7.3` 把 frame-aligned Core 时域样本定义为 QMF analysis 输入，`5.7.6.1` 又称
   IMDCT 输出经 frame alignment 后送入 analysis QMF bank；两处都没有另列一个 2 倍换算。

因此，规范没有一句可以单独引用为“`5.5.3` 明文规定 QMF 前除以 2”。但是，把本实现已在
Core PCM 出口应用的 `×2` 原样送进 `Pseudocode 65`，会产生另一项可直接观察的矛盾：QMF
分析是线性的，直通低带的振幅随之乘 2、能量增加 `6.0206 dB`；`Pseudocode 82` 从码流
反量化出的绝对高带信号目标并不继承该 PCM 因子。`5.7.6.5.3` 在 crossover 处组合直通低带
与生成高带时，两种量级因而立即形成接缝。

真实 768 kbit/s A-JOC 节目验证了该模型。旧路径的 core 直接出口在
13.125–13.5 kHz 与 13.5–13.875 kHz 两带间为 `-7.976367 dB`，本地 20 对象聚合为
`-8.105 dB`；只把 2 倍因子移出 QMF 域后分别变为 `-2.660284 dB` 与 `-2.897883 dB`。
从下一个完整 375 Hz 频带开始，相对提升稳定在 `5.962–5.965 dB`。外部参考解码器的对应
出口为 `-2.511597 dB` 与约 `-2.524 dB`；参考解码器不是规范 oracle，但 Core 与对象两层
同时收敛、变化量接近理论 6 dB，且接缝恰好绑定 A-SPX crossover，足以否决“让 2 倍 Core
PCM 不经换算直接进入绝对标度的 A-SPX QMF 域”这一解释。

完整实验、输入摘要、频带定义及参考边界记录在
[A-SPX crossover 实验](../experiments/aspx_crossover_normalization_observation.json)。

## 决策

1. **低层 QMF 函数保持规范伪码的原始量级。** `analyse` 与 `synthesise` 逐行实现
   `Pseudocode 65`/`66`，不把 AC-4 PCM 的 2 倍策略藏进原型窗、调制矩阵或 `1/64`
   合成归一化。

2. **在本解码器的 Core PCM/QMF 边界做互逆表示换算。** `analyse_ac4_pcm` 先执行字面
   QMF analysis，再把全部复数子带样本除以 2；所有 A-SPX、A-CPL、A-JOC 等 QMF 域工具
   完成后，`synthesise_ac4_pcm` 才把字面 QMF synthesis 的 PCM 乘回 2。这样 QMF 域看到的
   是 `Pseudocode 64` 的 overlap 量级，而公开 Core PCM 仍保持 `5.5.3` 示例及既有输出契约
   使用的量级。

3. **这是一项内部表示选择，不是高频补偿器。** 若另一个实现让 IMDCT 只输出未乘 2 的
   overlap、直接运行 `Pseudocode 65/66`，其全部 QMF 中间量应与本实现相同，最终 PCM 只差
   一个全局 2 倍表示因子。不得把本决策扩展成 crossover EQ、固定 `+6 dB` 高频增益、
   limiter、归一化器或针对参考解码器带宽的低通。

4. **只在实际跨越 Core PCM/QMF 边界时应用。** 纯 Core PCM 不经过该换算；独立使用 QMF
   滤波器组时直接调用 `analyse`/`synthesise`。对象音频的 A-SPX 路径、A-SPX bypass 以及
   只执行 Part 2 `4.8.3.9` 的 analysis-only 路径必须使用同一内部量级。Part 2 对特定
   `immersive_channel_element / ASPX_SCPL` 明列的 `g = 2` 等分支增益仍在规范指定的位置
   独立应用，不能与本边界换算互相替代。

5. **文档必须保留证据等级。** 可以称本决策为 P62、P64、5.5.3、P65/P66、P82 及端到端
   连续性共同支持的规范解释；不能称作 ETSI 已明文规定的 QMF 接口增益，也不能把外部参考
   输出写成规范证明。

6. **更强证据到来时重新裁决。** 若 ETSI 发布勘误、新规范版本、官方 PCM conformance
   vector 或可核验的参考源码，必须优先于本 ADR；届时重跑 Core、A-SPX、对象 PCM 与
   ADM/DAMF 全部基线，不能只修改文档措辞。

## 判据

- 边界单元测试分别锁定 analysis 后的 `0.5`、synthesis 后的 `2.0`，并确认换算不改变两侧
  QMF 历史状态。
- `analyse`/`synthesise` 的窗口、调制、延迟、分帧连续性和多相往返判据保持不变。
- A-SPX 与对象真实媒体基线接受有意数值变化；纯 Core 基线保持不变。
- 完整节目要求 crossover 下方低带的端到端电平不变，从下一完整频带起相对提升接近
  `6.0206 dB`，且不得改变采样率、帧数、声道数或时间轴。
- DRP 仅在 18 kHz 以下、且解码层级可比时用于行为交叉检查；其约 18.375 kHz 以上的停止带
  不构成本地输出目标。

## 影响

- A-SPX 的直通低带与 `Pseudocode 82` 绝对高带目标在同一内部量级上组合，14 kHz 附近不再
  因固定 2 倍错配产生台阶。
- 公开 Core PCM 的既有量级、低层 QMF API 与纯 Core 基线不变。
- A-SPX 和 A-JOC 对象基线发生有意变化；输出 writer 仍只执行既有的 `2^-15` 映射或统一
  节目峰值策略。
- 规范追踪中新增一项明确的解释风险：实现行为有跨条款与端到端证据，但没有官方勘误背书。

## 被否决方案

### 把 2 倍 Core PCM 原样送入 Pseudocode 65

最贴近 Part 1 `5.7.6.1` 的数据流字面，但会让低带能量相对绝对高带目标增加
`6.0206 dB`，并在真实节目 crossover 处立即形成稳定台阶；Core 与对象两层都与参考输出
产生同方向偏差。

### 删除 IMDCT 的 2 倍并改变全部公开 PCM 量级

可以得到相同的 QMF 内部量级，但会让纯 Core PCM 及所有既有公开出口整体降低 6 dB，并直接
放弃 `5.5.3` 三次 factor-of-2 示例。没有官方输出量级向量时，不以一次全局契约变更代替局部、
互逆且可验证的表示换算。

### 在 crossover 上方固定增加 6 dB

只掩盖一个节目上的症状，无法解释 A-SPX 的绝对标度、bypass、对象重建或其他 crossover；
也会错误放大已经处于正确内部量级的路径，故拒绝。
