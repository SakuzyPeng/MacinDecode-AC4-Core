# ADR-0007：Presentation 处理前的 Scene Rust API 边界

- 状态：Accepted
- 日期：2026-08-24
- 关系：补充 [ADR-0001](0001-language-and-project-boundary.md) 的公共接口边界；不改变
  [ADR-0002](0002-numeric-format-for-reconstruction.md) 与
  [ADR-0006](0006-core-pcm-qmf-gain-boundary.md) 的内部数值决策

## 背景

M4 与 M6 已经分别建立 Core/A-SPX PCM 和 Full A-JOC 空间对象组重建。早期 CLI survey
同时承担整文件巡检、Full DSP、OAMD 聚合、MP4 edit 投影与格式写出；这种形态可以冻结诊断
基线，却不能作为 renderer 或 GUI 的流式库边界：它要求整文件所有权，输出使用解码器内部
`±32768` 量级，并把容器、presentation 选择和场景状态隐含在同一批处理过程里。

M5 因此新增 `macindecode-ac4-scene`，由 `Ac4DecoderSession` 自持同一 A-JOC engine、表
188 控制延迟、OAMD 历史和可复用输出存储。Rust API 覆盖已经由真实向量验证的动态
Core/downmix 与 Full/upmix A-JOC 子集；direct-object 目前只有构造拓扑覆盖，不能借公共
API 的存在宣称 M5 完成。

另一方面，[Presentation 与元数据闭环计划](../PRESENTATION_METADATA_PLAN.md)还将增加普通与
alternative presentation metadata、dialog enhancement、DRC、gain、custom downmix、
loudness correction 和 opaque EMDF。若不先冻结边界，这些处理可能静默改写渲染前 PCM，
或者迫使首版 Scene 布局成为将来的 C ABI。

## 决策

### 1. `Ac4SceneFrame` 是 presentation 处理前的场景

`Ac4SceneFrame` 表示调用方选择的 A-JOC 重建层级已经完成、但尚未执行 presentation
processing 或最终渲染的场景：Core 是 A-SPX 后、A-JOC 上混前的 downmix 对象出口，Full
是 A-JOC 上混后的对象出口。Scene 层不得应用 DRC、dialog enhancement、group/associated
gain、custom downmix、loudness correction、扬声器映射或双耳化，也不解释 MP4 edit。

M4.5 可以解析、验证并保留这些处理所需的 metadata；M6.5 若实现处理器，必须由调用方显式
请求并返回独立的 `ProcessedPresentationFrame`（暂名）。处理失败或关闭处理器时，不得原地
改写 `Ac4SceneFrame` 的 PCM、状态、identity、事件顺序或时间线。

### 2. 一个 Session 显式选择一个 presentation

`Ac4DecoderConfig` 使用以下选择策略：

- `AutoUnique` 只在恰有一个 eligible presentation 时成功；零个或多个都返回结构化选择错误。
- `Index(u32)` 使用零基码流下标。
- `Id(u32)` 要求 `presentation_id` 存在且在当前配置中唯一；缺失和重复都失败。

配置代次变化后必须重新解析选择。不得默认取第一项，也不得把一个旧下标或 ID 的解析结果静默
沿用到新配置。首版不同时解码全部 presentation；需要并行输出时由调用方建立独立 Session，
直到另一个 ADR 定义共享 DSP 与状态所有权。

系统层已经解析的选择 metadata 不进入 `Ac4DecoderConfig`。调用方可用泛型
`PresentationSelectionMetadata<T>` 保持原始只读视图，再由已选 `ScenePresentation` 按
effective presentation ID 关联：双方该 ID 都必须唯一；无 ID 时双方都只能有一个无 ID 项。
来源数组下标不参与身份判断，重复 ID 与多路无 ID 不作猜测性绑定；opaque body 的未知身份
也不得冒充明确无 ID 参与回退。集合中只要仍有身份不可用项，就不能证明其他候选唯一，关联
保持 indeterminate。

### 3. Scene 不拥有容器语义

`decode_access_unit` 只接收调用方已经定界并剥离 sync wrapper 的 `raw_ac4_frame`。
raw sync 拆包、MP4 sample table、edit list 与文件 I/O 分别属于同步、MP4 和应用适配层。

Scene 不解析 `dac4`，也不拥有 DSI 字节。泛型 presentation metadata 只提供身份 envelope 与
调用方值的只读关联，因此 MP4 适配层可以把已经定界的 DSI presentation（包括未知版本的
原始 body）直接作为 `T`，而不增加 Scene 到 MP4 的依赖或让 DSI 取代 TOC 配置。

`AccessUnitContext` 中的 AU index、discontinuity、random-access hint、
`source_sample_start`、`presentation_sample_start` 与 `priming_samples` 都是调用方提供的通用
外部事实；Scene 只透传，不猜测它们的容器来源，也不自行裁切 PCM。

时间字段保持正交：

- `codec_sample_start` 是 Session 连续解码时间线。
- `source_sample_start` 与 `presentation_sample_start` 是调用方已经换算的可选位置。
- 表 81 codec delay 与表 188 PCM/control alignment delay 分开报告。
- 元数据更新相对对应 SceneFrame 定位，并保留产生它的 control source AU。

一个 AU 可以产生零个或多个 SceneFrame。等待完整随机访问点是带空帧序列的
`WaitingForRandomAccess` 状态，不伪装成一帧静音输出。

### 4. Session 拥有状态与输出，Rust 生命周期定义有效期

Session 统一拥有音频语法、ASF、A-SPX、A-JOC、QMF、LFE、OAMD、表 188 延迟环、跨帧
事件队列、元素描述与 PCM 缓冲。`DecodedAccessUnit` 和 `Ac4SceneFrame` 只借用这些存储，
有效期截止于下一次对同一 Session 的可变调用；安全 Rust 的借用规则负责阻止陈旧视图。

稳定配置下预留并复用工作区、帧、元素、plane 与 metadata 容量，不把逐帧 `to_vec` 作为
正常路径。`SceneElementId` 在同一配置代次及普通 discontinuity 后保持稳定；配置变化或显式
`reset` 建立新 identity 域，同一 Session 生命周期内数值不复用。该 ID 只用于映射和序列化，
不代表 ADM 创作对象。

首版 Rust 结构保持私有字段和访问器，可扩展枚举采用 `#[non_exhaustive]`。本 ADR 不冻结结构
布局、指针形式或 C ABI；M7 必须另行定义跨语言所有权与版本化布局。

### 5. Scene PCM 固定为 normalized planar `f32`

解码器内部 `±32768` 标量在 Scene 出口按 `f32` 乘 `2^-15`。`1.0` 是 nominal full scale；
合法 overrange 保留且不削波。任何非有限样本都是内部不变量失败，不允许发布半帧。

公共视图显式报告 `F32`、planar layout、plane 数量、stride、有效长度与每 plane 样本数。
该换算只定义 Scene 出口，不改变 ADR-0002/0006 的内部 DSP 量级。Scene 只拥有 normalized
plane，不保留第二份内部尺度样本。CLI batch adapter 在 Scene 外乘精确的 `2^15` 恢复既有
输出量级，并以受支持真实媒体的 PCM 基线约束兼容性；`f32` 下溢区中无法往返的合成内部位型
不属于公共 Scene 或 CLI 制品契约。

启用 `audio-decode` 且调用方通过 `with_core_band_diagnostics(true)` 显式请求时，
`DecodedAccessUnit` 还可借用当前 AU 的 pre-A-SPX ASF 核心带诊断侧车；默认 renderer 不复制
它。该侧车同样是 normalized planar `f32`；传输侧 `(element, channel)` 身份不属于
`Ac4SceneFrame`，不经过表 188 场景对齐，也不得被解释成对象或 bed。Session 仍在同一次
engine 事务成功后才发布它，任何解析、DSP、形状或有限值失败都会与 Scene 输出一起作废。

当前原生 bed 只公开码流实际解出的 Core 或 Full LFE component。full ADM/DAMF writer 所需的
九路静音 7.1.2 compatibility bed 是制品适配，不属于渲染前场景。

### 6. 语义状态与 raw OAMD 同时保留

Core 输出标记为 A-JOC Core object，Full 输出标记为 spatial object group；两者都不暗示与
编码前创作对象一一对应。当前 PCM 的 `has_signal` 与 OAMD 状态的 `metadata_active` 独立
表达，静音对象仍可携带有效 metadata。

每个对象公开帧起点的完整有效状态；控制尚未到期的 warm-up 帧使用
`state_complete = false`，不合成默认对象状态。每次更新携带 element ID、帧内 offset、ramp、
完整更新后状态、`MetadataFields` changed mask、control source AU 及原始 OAMD 更新。更新按
`(offset, 码流顺序)` 稳定排列，越过帧尾的事件进入有界跨帧队列；reset 后不得继承旧状态。

Scene-owned 语义值使用 `f32`，只覆盖已经由规范公式和测试验证的 Cartesian position、linear
gain、importance、size、zone、screen/depth、trim 与 headphone。`RawOamdState` 和
`RawOamdUpdate` 完整保留量化码值、presence、timing 与更新来源；distance、divergence 等尚无
可靠通用映射的字段只保留 raw，不伪造语义值。

未来 EMDF 采用相同原则：先验证有界 envelope、路由和时间，未知或私有 datatype 以 opaque
bytes 无损保留；没有注册表依据时不得发明 Scene 语义。

### 7. 失败关闭且不执行首版 concealment

- TOC 或内联 control 解析在尚未验证完整边界时耗尽输入，返回 `NeedMoreData`；Session 状态
  不变，同一 AU 可以补齐后重试。若完整 TOC 的 substream size 已经证明某个声明 payload
  越过调用方提供的 bounded `raw_ac4_frame`，则返回 `InvalidBitstream` 并失效历史；此时追加
  字节会改变调用方声称已经确定的 AU 边界，不属于同一调用契约下的可重试截断。
- channel-based、direct-object、mixed、static downmix、Core/Full BED 或 ISF 分配、多 A-JOC
  substream 与未覆盖 Full 分支返回结构化 `Unsupported`。
- 坏语法或 DSP 失败不输出静音 concealment，清空全部相关历史并进入
  `WaitingForRandomAccess`。
- 形状、所有权或非有限值等内部不变量失败后要求显式 `reset`。
- 错误尽可能携带 AU、presentation/group/substream、语法路径和 bit offset。

## 理由

- 容器只提供 AU 与时间，Scene 只负责解码状态和渲染前语义，renderer/GUI 不必依赖 MP4 或
  CLI。
- 借用输出使陈旧 PCM 在类型层面不可继续使用，同时允许实时调用方避免逐帧复制。
- normalized PCM 给 Rust renderer 稳定、常见的数值边界，又不破坏既有诊断 writer 的历史
  基线。
- semantic + raw 双轨既让 GUI 直接消费已验证字段，也避免把未知规范信息不可逆地丢失或
  误译。
- 严格 presentation 选择和随机访问门禁让配置歧义、seek 与坏帧显式可见，而不是生成看似
  合法但来源错误的输出。

## 影响

正面影响：

- renderer/GUI 可以逐 AU 消费稳定的 Rust 场景契约。
- MP4、raw sync、文件写出与 presentation processing 可以独立演进和测试。
- CLI 迁移后，Full census 与 Scene 输出必须消费同一 bitstream engine，不再保留第二套 Full
  DSP 实现。

代价与限制：

- 需要既有 CLI batch adapter 复制借用帧、应用 priming/edit 投影，并在 Scene 外恢复
  `±32768` 输出尺度；normalized `f32` 下溢区的合成内部位型不保证可逆。
- 需要跨可变调用持有场景的调用方必须自行复制。
- 同时解码全部 presentation、direct-object、presentation processing 与 C ABI 都不在首版
  范围；M5 仍是部分完成。

## 后续方向

- 若调用方需要在 AU 尚未定界时直接增量送入字节，应新增独立的 sync/framing 或 incremental
  ingestion API，由该层拥有未完成字节并统一报告 `NeedMoreData`；不得把 bounded
  `AccessUnit` 的 payload 越界静默改成可追加输入。输入所有权、容量上限与状态提交语义需要
  另一个 ADR 冻结。
- 若 renderer 需要跨配置代次延续应用侧 identity，应新增显式 generation/element remap 事件
  或由调用方建立映射；不得根据旧 `element_id`、数组位置或编码侧 object index 猜测同一对象。
  映射证据、失配行为与 C ABI 表达需要另一个 ADR 冻结。

## 被否决方案

### 默认选择第一个 presentation

在多 presentation 或配置变化时会静默输出错误节目，无法由调用方发现，故拒绝。

### 在 Scene 内应用 MP4 edit 或 presentation processing

会把容器和目标策略绑定到解码核心，并使同一原始场景无法派生不同处理结果，故拒绝。

### 直接公开内部 `±32768` PCM

适合既有逐位诊断基线，但会把历史实现量级变成 renderer 契约；Scene 出口改用精确定标的
normalized `f32`。

### 返回长期拥有的整文件场景

便于 batch writer，却要求无界累积 PCM 与 metadata，不能满足流式 GUI/renderer 和稳定解码
不扩容的目标。整文件累积留给 Scene 外 adapter。

### 只公开语义 OAMD 或只公开 raw OAMD

前者会丢失尚不能可靠映射的码值，后者会让每个 renderer 重复实现同一规范公式；因此保留双轨。

### 让首版 Rust 结构直接成为 C ABI

会冻结集合、枚举和借用布局，阻止已知的后续扩展。C ABI 留给 M7 的版本化 opaque handle 与
显式缓冲契约。
