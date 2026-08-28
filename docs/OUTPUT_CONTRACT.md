# 渲染前输出契约

> **状态：A-JOC Core/Full 子集的流式 Rust 场景入口已发布，core/A-SPX 诊断基线、CoreCAF、ADM/DAMF 诊断探针与三条 Full CLI batch 出口已落地。** M4 已收口容器、拓扑、OAMD、音频语法、core/A-SPX PCM 与受限的动态 A-JOC core 对象输出；M6 又完成 full A-JOC 上混、LFE 插回和对象 PCM 导出。`macindecode-ac4-scene` 现已定义 `Ac4SceneFrame` Rust 数据模型、presentation/mode 选择和结构化错误，并在 `audio-decode` 下由公开 `decode_access_unit` 按配置选择同一 A-JOC engine 的 Core 诊断出口或 Full 重建出口，把对应的对象/LFE PCM、group OAMD common、完整 downmix/upmix 对象状态、raw timing、changed mask 与跨帧事件队列放入 Session 自有、可复用的表 188 对齐存储后借用返回；`export-core-pcm`、`export-aspx-pcm`、`export-core-caf`、`export-adm-bwf`、`export-damf`、`export-objects-pcm`、`export-full-adm-bwf` 与 `export-full-damf` 已改为消费该 Session。batch adapter 在 Scene 外应用容器 edit 投影，需要历史量级的 PCM 时将 normalized 样本乘精确的 `2^15`，并保持既有轨序；pre-A-SPX 核心带通过 `DecodedAccessUnit` 上不属于 SceneFrame 的 normalized 诊断侧车传递，但必须显式启用，普通 renderer 默认不复制它。ADM/DAMF 诊断探针只保留已完整解码并完成控制对齐的场景元数据。所选 presentation 的 OAMD 更新继续投影到既有 writer 契约。M4.5 的 presentation processing metadata 与 opaque EMDF 已在 bitstream 层解析和保留，但尚未作为 `Ac4SceneFrame` 字段发布，也未应用到 PCM。实施进度以[路线图](ROADMAP.md)为准。

## 1. 目的

本契约描述 MacinDecode-AC4-Core 与外部渲染器之间的语义边界。它不固定 Rust 内部结构布局，也不等同于第一版 C ABI。决策依据见 [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md)。

公共 Rust 输出概念命名为 `Ac4SceneFrame`。

## 2. 场景帧

一个场景帧表示一段连续 PCM 以及在这段 PCM 内生效的场景状态：

```text
Ac4SceneFrame
  timeline
  presentation
  oamd_common_states[]
  beds[]
  objects[]
  metadata_updates[]
  diagnostics
```

### 2.1 Timeline

必须包含：

- 采样率。
- codec、source 与 presentation 三条互不折叠的帧起始采样位置；后两者由调用方提供。
- PCM 持续采样数。
- PCM 所属 access unit 与到期控制来源 access unit 索引。
- 是否为随机访问起点。
- 是否发生 discontinuity、concealment 或配置变化。
- 已知的 codec delay、priming 与独立的表 188 PCM/control alignment delay。

所有内部时间均以整数采样表示。秒和时间码只作为展示或容器适配结果；Scene 层不解释 MP4
edit，只透传调用方已经完成的 source/presentation 映射。

### 2.2 Presentation

必须包含足以说明当前输出来源的信息：

- presentation ID、version 和兼容等级。
- 关联 group/substream 标识。
- channel-based、direct-object 或 A-JOC 路径。
- full/core decode 模式。

当前配置代次由同一场景帧的 timeline 独立报告；配置变化时递增。

### 2.3 OAMD common

presentation 引用的每个 group 都必须能独立公开其有效 `oamd_common_data()`：

- 保留 group 下标、合并复用后的完整原始码值，以及来源 access unit 是否显式刷新。
- reset 后尚无自足状态时必须与“使用规范默认值”区分。
- master screen ratio、全局 trim、bed render info 与全局 headphone 不得复制成单对象私有属性，也不得在通用映射不完整时丢弃。

### 2.4 Bed

每个 bed 至少包含：

- 同一配置代次内稳定的场景 ID。
- 声道配置或扬声器标签列表。
- 每个 component 的 PCM 视图。
- 增益、静音和关联内容分类等可用元数据。

bed PCM 不在本项目内映射到最终设备布局。

当前 A-JOC Core/Full 子集只把各自出口实际解出的 LFE component 作为原生 bed；ADM/DAMF
writer 的九路静音 7.1.2 compatibility bed 不进入 Scene。

### 2.5 Object 或空间对象组

每个输出单元至少包含：

- 同一配置代次内稳定的 `element_id`。
- 输出种类：direct object、A-JOC Core object、A-JOC spatial object group 或其他规范定义单元。
- PCM 视图和有效采样范围。
- 与 OAMD 状态的关联键。
- active、importance 和可用内容分类。

Core 输出是 A-SPX 后、A-JOC 上混前的对象信号；Full 输出可能是空间对象组。两者都不是
原始 ADM 母版中的可靠 identity，API 不得暗示与创作对象必然一一对应。

对照解码实测表明，输出单元的**槽位总数**由编码配置决定，与创作对象数量无关：三个源构成、布局、时长都不同的码流，产生了逐字段一致的槽位拓扑。但**实际承载信号的槽位数取决于内容**，且单个孤立对象不会被摊到多个槽位。详见测试向量策略 9.2。

由此对本契约的约束是：

- `element_id` 在同一配置代次及普通 discontinuity 后保持稳定；配置变化或显式 `reset` 会换发，
  且同一 Session 生命周期内数值不复用。它不表达任何与创作对象的对应关系。
- 输出单元的数量、顺序和索引都不得被调用方用于推断源制作结构。
- **大量输出单元可能全程静音。** 契约必须允许调用方廉价地判断某个单元在当前帧是否承载信号，否则下游会为恒零单元付出全额渲染代价。当前 PCM 的 `has_signal` 必须与 OAMD 的 `metadata_active` 分开。
- 调用方若需要建立与母版的对应，只能基于信号特征匹配；本项目不提供该映射，也不应在 API 中留出暗示其存在的字段。

### 2.6 Metadata update

一个场景帧允许有零个或多个帧内更新。每个更新至少包含：

- `element_id`。
- `offset_samples`。
- `ramp_duration_samples`。
- 产生本更新的 control source access unit 索引；跨帧排队不得改写该来源。
- 本次更新实际改变的字段集合。
- 更新后的完整有效状态。
- 生效的 OAMD timing 原始编码，并区分 control source access unit 刷新与跨帧继承；表索引路径和 11 比特显式路径不得折叠为同一份原始表示。

首版已验证的 Scene-owned 语义字段包括：

- Cartesian position、linear gain、importance 与 size。
- screen/depth factor。
- zone、trim 与 headphone。

distance、divergence 等尚无可靠通用映射的字段只保留 raw。如果规范字段无法无损映射到通用
场景概念，应保留规范原始码值，而不是静默丢弃或伪造语义值。

### 2.7 Presentation processing metadata 与 opaque EMDF

当前实现只在 `macindecode-ac4-bitstream` Rust API 公开这些解析结果；`Ac4SceneFrame` 尚无
对应字段。未来接入 Scene 或其他回放端观察面时，必须保持以下契约：

- presentation name/target、逐 substream activation/dataset map、响度、DRC、group gain、
  associated audio、custom downmix、loudness correction 与 dialogue-enhancement 都保留 presence、
  码流顺序和原始量化值；当前帧传输形态与跨帧有效状态不得折叠成一个无法区分来源的值。
- alternative OAMD 必须同时携带物理 substream、non-A-JOC/A-JOC core/full domain 和 dataset
  loop index。解析层公开全部候选，不按 target、设备能力或数组顺序自动选择，也不把未选择的
  gain/位置并入普通 OAMD 状态。
- EMDF descriptor 保留 payload ID、时序、路由、processing/discard/duplicate 等配置及声明长度；
  未注册或私有 datatype 的 payload 以原始 8 比特元素无损保留，不因未知 ID 失败，也不发明
  Scene 语义。终止符、容量和边界仍必须在发布 view 前完整验证。
- 原 payload 中的 DRC/DE/EMDF/alternative opaque view 只在其有界输入切片的生命周期内有效。
  若未来由 Session 发布，必须改为借用 Session 拥有且已验证的存储，并沿用下一次可变调用即
  失效的 Rust 生命周期；不得只暴露无法安全回取数据的裸 offset 或长期裸指针。
- 任何这些字段的解析、状态还原或 opaque 保留都不得原地修改 `Ac4SceneFrame` PCM、对象 identity、
  OAMD 事件顺序或时间线。DRC、DE、gain、downmix 与 loudness processing 需要独立、显式请求的
  后续处理接口。

在 alternative dataset 尚未由上层明确选择并应用时，Core/Full 输出继续以结构化
`AlternativeObjectMetadata` 原因失败关闭；active dialogue enhancement 也不得被静默忽略。
这两个 blocker 表示处理语义尚未接入，不表示对应 metadata 没有被解析和保留。

## 3. 状态语义

- 未在当前帧更新的元数据继承上一有效状态。
- random access 或 reset 后，不允许继承 reset 前状态。
- 同一帧内多个更新按 `offset_samples` 排序；相同位置的规范顺序必须稳定。
- 只有在帧起点已经生效的自足更新才能进入对象的初始完整状态；控制尚未到期时必须报告
  `state_complete = false`。
- 调用方可以仅根据连续 `Ac4SceneFrame` 重建完整场景状态，不需要回读解码器内部对象。
- seek 后的首个输出必须明确说明场景状态是否完整。
- presentation DRC 与 group gain 状态按 presentation 隔离；dialogue enhancement 按物理 audio
  substream 隔离；alternative OAMD 再按物理 substream、dataset domain 与 loop index 隔离。
  EMDF opaque envelope 不建立跨帧 datatype 状态。任一键变化或连续性丢失后都不得复用旧值。
- metadata 状态更新必须事务性提交；解析或还原失败不能发布半更新，也不能污染上一份有效状态。

## 4. PCM 语义

首版 Scene PCM 固定为 normalized planar `f32`：内部 `±32768` 按 `f32` 乘 `2^-15`，`1.0`
是 nominal full scale，合法 overrange 不削波，非有限值作为内部不变量失败。公共契约显式携带：

- sample format。
- planar/interleaved 布局。
- channel/component 数量。
- 每个 plane 的 stride 与有效长度。
- 是否经过 concealment。

Scene 只保留这一份 normalized PCM，不提供内部尺度兼容副本。需要既有 `±32768` 量级的 CLI
adapter 在 Scene 外乘精确的 `2^15`；受支持真实媒体的 PCM 基线继续锁定这一转换，但不把
`f32` 下溢区中不可逆的合成内部位型纳入公共契约。pre-A-SPX 核心带由调用方通过
`Ac4DecoderConfig::with_core_band_diagnostics(true)` 显式启用，再从
`DecodedAccessUnit::core_band_pcm` 作为同样 normalized 的独立诊断侧车借用；默认配置不复制
这份 PCM。该侧车不是 `Ac4SceneFrame`，不得把传输侧 `(element, channel)` 冒充 renderer 元素。

PCM 缓冲区由 Session 拥有，`DecodedAccessUnit`/`Ac4SceneFrame` 通过 Rust 生命周期借用到下一次
Session 可变调用。禁止返回生命周期不明的裸指针。该 `f32` 决策只固定 Rust Scene 语义，
不固定未来 C ABI 的内存布局。

## 5. 验证语义

以下字段应支持精确比较：

- 帧和更新的采样位置。
- presentation/group/substream 拓扑。
- 原始量化元数据码值。
- 配置变化和状态 reset。

以下内容通常不能直接与编码前 DAMF 逐样本比较：

- 有损核心解码后的 PCM。
- A-JOC 重建的空间对象组 PCM。
- 编码器执行自适应对象分组后的输出 identity。

此类内容应使用相关性、时延、能量、串扰、轨迹一致性和最终受控渲染结果进行验证。

## 6. 首版 API 暂不承诺

首版不承诺：

- 固定最大对象数。
- ADM object ID 与 AC-4 输出 ID 相同。
- 每个 access unit 恰好对应一个场景帧。
- 所有 presentation 同时解码。
- Rust 结构布局直接作为 C ABI。

normalized `f32`、借用所有权及其他已确定边界由 [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 固化；其余约束需要新的证据与 ADR。
