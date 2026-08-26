# 架构设计

## 1. 目标边界

MacinDecode-AC4-Core 的输入是 raw AC-4 sync frame，或由容器适配层提供的 AC-4 access unit。输出是渲染前音频场景，而不是固定声道 PCM。

核心边界如下：

```text
encoded access units
    -> decoded audio components
    -> reconstructed objects / spatial object groups
    -> time-aligned OAMD
    -> scene frames
```

场景帧之后的扬声器映射、对象声像计算、双耳化和设备处理均属于外部系统。

## 2. 当前 workspace 与目标拆分

截至首版场景数据契约落地，workspace 有四个实际 crate：

| crate | 当前职责 | 是否允许平台依赖 |
|---|---|---|
| `macindecode-ac4-bitstream` | bit reader、sync/TOC/拓扑、OAMD，以及 ASF/A-SPX/A-JOC 音频语法 | 否 |
| `macindecode-ac4-scene` | 容器无关的 `Ac4SceneFrame` 数据契约和流式 Rust API；自持 A-JOC engine，并发布 Core/Full 对象/LFE normalized PCM、到期 group common 与 downmix/upmix 逐对象更新的借用视图 | 否 |
| `macindecode-ac4-mp4` | `ac-4`、`dac4`、sample table、edit/priming 适配 | 否 |
| `macindecode-ac4-cli` | MP4/raw trace、测试诊断、core/A-SPX/full 对象 PCM、合成 ADM/DAMF 探针、真实 full DAMF、full ADM BW64/RF64 与固定网格 CAF | 可以 |

bitstream/CLI 路径现已贯通核心带、A-SPX 与 A-JOC full 对象标量 PCM，并提供受限子集的固定 core 网格 CAF、合成 ADM/DAMF 诊断探针、full ADM 与 full DAMF 输出；`macindecode-ac4-scene` 已建立公共数据契约，并在 `audio-decode` 下由公开 `decode_access_unit` 事务驱动同一 A-JOC engine，按配置把 Core 诊断或 Full 重建的对象/LFE normalized PCM、group OAMD common 与对应 downmix/upmix 逐对象状态放在同一份表 188 到期快照中，以 Session 自有存储的借用视图返回。`export-core-pcm`、`export-aspx-pcm`、`export-core-caf`、`export-adm-bwf`、`export-damf`、`export-objects-pcm`、`export-full-adm-bwf` 与 `export-full-damf` 已消费同一 Scene batch adapter：raw 侧剥离 sync wrapper，MP4 侧提供 bounded AU 与外部时间，再在 Scene 外应用 edit 投影；需要历史量级的 PCM 出口统一把 Scene normalized 样本乘精确的 `2^15`。`DecodedAccessUnit` 上独立的 normalized pre-A-SPX 核心带侧车服务核心带基线，不进入 `Ac4SceneFrame` renderer 契约；调用方必须显式启用，普通 renderer 默认不复制这份诊断 PCM。合成 ADM/DAMF 诊断探针只保留已完整解码并完成控制对齐的场景元数据。core/full artifact 出口还把已选择 presentation 的元素、common 与更新时间线桥接到既有 writer。Core 模式对未应用的 dialogue enhancement 继续 fail closed。Scene 组装会保留 control source AU、raw timing、ramp、完整更新后状态与 changed mask，以 `(offset, 码流顺序)` 排列帧内事件，并把越过帧尾的更新留在有界复用队列；只有 offset 0 的自足更新进入帧起点状态，逐对象绝对生效时间若相对码流顺序倒退则作为无效码流失败关闭。以下名称仍只表示规划职责边界，**不是当前已经存在的 crate**：

| 规划边界 | 目标职责 |
|---|---|
| `macindecode-ac4-audio-core` | 反量化、变换、预测、耦合和基础 PCM 重建 |
| `macindecode-ac4-ajoc` | A-JOC 空间对象组重建；量化数据解析当前位于 bitstream crate |
| `macindecode-ac4-oamd` | OAMD 状态与帧内时间线；当前位于 bitstream crate |
| `macindecode-ac4-ffi` | 稳定 C ABI 和所有权边界，可包含少量平台胶水 |

是否按这些名称拆包，要等 PCM 与场景 API 的边界稳定后决定。无论最终包数如何，MP4、FFI 或 CLI 都不得反向污染解码核心。

### 2.1 当前源码内职责边界

当前实现按状态所有权和输出边界拆为：

| 目录 | 职责 |
|---|---|
| `macindecode-ac4-bitstream/src/oamd/` | common/object/payload 语法、跨帧状态和描述适配 |
| `macindecode-ac4-bitstream/src/substream/` | channel/object/group substream 声明与共享读取逻辑 |
| `macindecode-ac4-bitstream/src/topology/` | 拓扑解析、引用验证和随机访问状态机 |
| `macindecode-ac4-bitstream/build_support/` | 数学表、QMF、规范 C 表/Huffman 与 SHA-256；`build.rs` 只调度 |
| `macindecode-ac4-scene/src/model.rs` | timeline、presentation、bed/object PCM、group 级 OAMD common、逐对象更新及借用输出模型 |
| `macindecode-ac4-scene/src/session.rs` | Session 控制面、presentation/mode 选择、Core/Full A-JOC 拓扑门禁与 engine 所有权 |
| `macindecode-ac4-scene/src/full_engine.rs` | 同一 AU 候选到 A-JOC engine 的事务输入及结构化错误投影 |
| `macindecode-ac4-scene/src/error.rs` | 可重试截断、选择、unsupported、码流与不变量错误及 AU/语法上下文 |
| `macindecode-ac4-cli/src/trace/` | MP4/raw trace、普通音频统计，以及对统一 Full engine observation 的 A-JOC census 聚合 |
| `macindecode-ac4-cli/src/container.rs` | trace 与 Scene batch 共用的 AC-4 轨定位、sample table 辅助及 MP4 edit 时间投影 |
| `macindecode-ac4-cli/src/scene_batch.rs`、`pcm_batch.rs`、`metadata_batch.rs` | bounded raw/MP4 AU 到 Session 的 batch 适配、Scene 外历史 PCM 量级/轨序适配、OAMD 整文件桥接与 edit 投影 |
| `macindecode-ac4-cli/src/scene_export/` | core CAF 与 full artifact 选择、OAMD 与 PCM 配对、节目级 S24LE full PCM 交织、确定性探针信号 |
| `macindecode-ac4-cli/src/adm/`、`damf/` | 格式专属 metadata、容器与原子提交 |
| `macindecode-ac4-cli/src/wire.rs` | 内部状态到 CLI v1 DTO 的投影、stdout JSON 与 stderr JSONL |

`oamd::Type`、`substream::Type`、`topology::Type` 是三个 bitstream 领域的规范公共路径；
领域内部子模块不构成公共 API。CLI wire DTO 不由 trace 状态派生，避免内部统计结构变化
意外改写外部契约。机器可读边界见 [CLI 输出契约 v1](CLI_OUTPUT_CONTRACT.md)。

## 3. 依赖方向

当前 crate 依赖图（箭头表示“依赖”）：

```text
macindecode-ac4-cli -> macindecode-ac4-mp4 -> macindecode-ac4-bitstream
macindecode-ac4-cli -----------------> macindecode-ac4-bitstream
macindecode-ac4-cli --------------------> macindecode-ac4-scene
macindecode-ac4-scene ---------------> macindecode-ac4-bitstream
```

目标职责方向如下；虚线之外的规划 crate 尚未建立：

```text
macindecode-ac4-bitstream
        |
        +--------------------+
        |                    |
macindecode-ac4-audio-core   macindecode-ac4-oamd
        |                    |
        +---- macindecode-ac4-ajoc-+
                    |
             macindecode-ac4-scene
               /          \
      macindecode-ac4-mp4    macindecode-ac4-ffi
               \          /
               macindecode-ac4-cli
```

当前 `macindecode-ac4-scene` 已定义容器无关的数据模型和 Session 控制面；`decode_access_unit` 只接收调用方定界的 access unit 与已经换算为整数采样的位置，不读取或解释 MP4 字节。`macindecode-ac4-mp4` 只负责 access unit 与时间线，不解释音频工具语义；CLI 的 `scene_batch` 消费两者，并在 Scene 返回以后执行 WAVE 兼容所需的 edit 与尺度投影。调用方可以把 DSI 等系统层选择信息保留在泛型 `PresentationSelectionMetadata<T>` 中，再由已选 `ScenePresentation` 按双方唯一的 effective ID 取得只读关联；数组下标不作为身份，身份不可用的 opaque 项会令关联保持 `Indeterminate`，metadata 不进入解码配置，也不形成 Scene 到 MP4 的依赖。presentation 处理前 Scene、选择、时间、所有权、normalized PCM 与 raw OAMD 的正式边界见 [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md)。

## 4. 目标解码会话

`Ac4DecoderSession` 的控制面和 feature-gated Full engine 所有权已经建立；它对应一个连续 AC-4 解码时间线并至少持有：

- 当前 bitstream version 和序列级配置。
- presentation、group 与 substream 配置缓存。
- 音频工具的跨帧历史。
- direct-object 或 A-JOC 重建历史。
- OAMD 的上一个有效状态。
- 编解码器采样位置、输出采样位置和已知延迟。
- discontinuity 与随机访问状态。

以下事件必须显式重置或重建状态：

- 新序列或不兼容配置变化。
- 容器 discontinuity。
- 无法安全延续的损坏帧。
- 调用方请求 seek，并从随机访问点重新开始。

状态不能隐藏在全局变量中，也不能由调用方猜测是否仍然有效。

## 5. 目标 access unit 处理顺序

1. 校验调用方已经定界并剥离 sync wrapper 的 `raw_ac4_frame`。
2. 解析 TOC，生成不可变的帧描述。
3. 解析 presentation、group 与 substream 关系。
4. 对所需音频 substream 执行核心解码。
5. 如果是 direct-object，关联对象音频与 OAMD。
6. 如果是 A-JOC，执行 full reconstruction，生成空间对象组。
7. 解码当前帧的 OAMD 更新，并与上一帧状态合并。
8. 报告编解码器与表 188 对齐延迟，但不在此层执行 MP4 edit 裁切。
9. 组装一个或多个 `Ac4SceneFrame`。
10. 返回借用场景、等待状态或结构化错误。

## 6. 时间模型

内部统一使用采样整数：

- `codec_sample_start`：当前解码帧在连续解码时间线中的起始采样。
- `duration_samples`：当前输出覆盖的采样数。
- `update_offset_samples`：OAMD 更新相对于场景帧起点的位置。
- `ramp_duration_samples`：元数据插值持续长度。
- `source_sample_start`：调用方媒体时间轴中的可选起点。
- `presentation_sample_start`：调用方已经应用 edit 或其他映射后的可选呈现起点。

MP4 第一帧可能具有负 PTS、skip samples 或 edit list 偏移。这些信息必须由容器层保留，不能通过丢弃一个 access unit 来隐式处理。

## 7. 内存与实时约束

- 初始化或配置变化时分配工作区；稳定解码期间避免逐帧堆分配。
- PCM 工作区按最大受支持配置预留并复用。
- 场景帧借用 Session 自有的输出缓冲区，有效期截止于下一次可变调用。
- 解析器必须支持有限输入切片，不得越过 access unit 边界寻找数据。
- 标量实现是正确性基线；SIMD 只能替换具有等价测试的局部内核。
- 不以 nightly-only API 作为公共构建前提。

## 8. 数值策略

标量重建的数值格式由 [ADR-0002](decisions/0002-numeric-format-for-reconstruction.md) 确定：频域中间量与 PCM 用 `f32`，变换累加器用 `f64`，运行期超越函数一律查表或位构造，表值冻结摘要，禁用 FMA 与快速数学重排。变换所需的三角常量没有整数出口，来源另由 [ADR-0003](decisions/0003-trigonometric-tables-for-the-transform.md) 确定：构建期用锁定版本的 `libm` 生成，目标侧不链接它。IFFT 标量基线已按 [ADR-0004](decisions/0004-mixed-radix-stockham-ifft.md) 落地：使用 radix-4/2/3/5 的 Stockham autosort、一个 16 KiB 固定 scratch、正号且不在 IFFT 内归一化；前/后旋转与 KBD 窗表已按同一机制落地，三张变换常量表齐备，`5.5.2.2` 的六个步骤已接合为完整 IMDCT；Full engine 工作区与逐声道重叠状态现由 `Ac4DecoderSession` 间接持有并统一重置。其余约束是：

QMF 合成的 portable 标量路径另按 [ADR-0008](decisions/0008-paired-qmf-synthesis-modulation.md) 成对处理负共轭输出行：共享乘法与相位加载，但每个输出仍按规范子带顺序执行 f64 累加，因此保持既有逐位 PCM。终端多声道入口再把相邻两路排成局部 2-lane AoSoA，让稳定编译器在 `no_std`、safe Rust 下沿声道轴生成 SIMD；lane 间没有归约，奇数尾声道仍调用同一标量入口。PCM 形状在状态推进前统一预检，热循环按固定 64-sample chunk 写回，portable ARM64 release 中不再保留越界 panic 慢路径。改变加法结合顺序的 FFT 分解不属于当前路径。

- 所有规范量化值保留原始整数表示，避免过早转为浮点。
- 元数据坐标同时保留量化码值和规范化表示。
- PCM 的内部格式不得泄漏为无法升级的 ABI 假设。
- 每个 DSP 模块必须记录舍入、饱和、denormal 和跨平台一致性策略。

## 9. 错误模型

错误分为三类：

| 类型 | 示例 | 行为 |
|---|---|---|
| `NeedMoreData` | TOC 或内联 control 解析在边界确定前耗尽输入 | 不改变会话状态，允许补齐同一 AU 后重试 |
| `Selection` / `Unsupported` | presentation 歧义或编码路径未覆盖 | 不产生输出，清空可继承历史并等待受支持的完整随机访问点 |
| `InvalidBitstream` / `DecodeFailure` | 已定界 AU 的声明 payload 越界、语法损坏或 DSP 失败 | 不执行 concealment，清空历史并等待完整随机访问点 |
| `InternalInvariant` / `ResetRequired` | PCM 非有限、形状或所有权不变量失败 | 不产生半帧，并要求调用方显式 `reset` |

任何错误都应携带 bit offset、语法路径、帧索引和相关 presentation/substream 标识，便于与规范及测试向量对应。

**变长字段的窄化与组合运算一律失败关闭。** `variable_bits()` 按规范返回 `u64`，结果既可能是长度或计数，也可能是版本与标识符；无法表示为 `u32` 时必须返回 `ValueOverflow`，不得饱和或回落为小值，否则不同码值会静默别名。扩展值与基值相加或按 `2^n` 缩放也必须做 checked 运算，不能把 `checked_shl` 误当作数值溢出检查。`BitReader::variable_bits_u32()` 与内部的组合 helper 统一固定这条规则。

## 10. 可观测性

调试构建必须支持结构化 trace，至少包含：

- frame index、byte/bit offset 和 frame size。
- bitstream version、frame rate、sample count。
- presentation/group/substream 拓扑。
- `dac4` presentation 选择信令及其与首帧 TOC 的 ID 级交叉核对。
- direct-object 或 A-JOC 路径选择。
- 对象或空间对象组数量。
- OAMD 更新的采样位置与字段变化。
- 状态 reset、concealment 和 unsupported feature。

trace 是验证接口，不应通过解析日志文本才能使用。CLI 成功结果遵循版本化 JSON envelope，
诊断遵循 stderr JSON Lines；help/version 是唯一保留的普通文本出口。
