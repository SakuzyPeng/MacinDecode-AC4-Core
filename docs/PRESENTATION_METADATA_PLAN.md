# Presentation 与元数据闭环计划

> **状态：规划；部分外部向量受限。** 本文只冻结实施边界、阶段顺序与验收门槛，不表示相关
> 能力已经实现。当前工具链可再生产普通 presentation payload 与 dialog enhancement 正向
> 候选；非空 EMDF payload 和 alternative presentation/dataset 仍按第 5 节暂缓。当前实际
> 进度仍以[实施路线图](ROADMAP.md)为准。

## 1. 目的

在现有 M4 与 M5 之间增加 **M4.5：Presentation/Metadata 闭环**，并在 M6 之后增加
**M6.5：Presentation 处理输出**。两阶段补齐以下三组能力：

1. 完整解析 `ac4_presentation_substream()`，包括普通与 alternative presentation。
2. 完整解析音频 substream metadata 中当前按长度跳过的标准元素，包括对话增强状态。
3. 解析并透明保留 EMDF payload envelope、时序配置与原始 payload。

规范是唯一的行为依据。其他实现只能用于发现遗漏、设计差分实验或构造反例，不作为规范引用、
正确性 oracle 或运行时依赖。

## 2. 不变边界

- `Ac4SceneFrame` 仍表示**未执行 presentation 处理的渲染前场景**。
- DRC、dialog enhancement、group/associated gain、custom downmix 与 loudness correction
  不得静默改写 `Ac4SceneFrame` 的 PCM。
- 处理后的 PCM 是调用方显式请求的独立派生输出，暂命名为
  `ProcessedPresentationFrame`。
- M4.5 只把 alternative 路径补到届时普通路径已经达到的解码层级；A-JOC full
  reconstruction 仍属于 M6，不得借 M4.5 提前宣称完成。
- EMDF 首期不解释注册表或私有 datatype 的业务语义，只保证 envelope、时序、路由字段与
  payload bytes 无损保留。
- 仅实现 AC-4 码流明确传输的规范处理。用户偏好、目标设备响度策略、扬声器映射和最终渲染
  仍由外部系统负责。
- 当前 CLI v1 契约与 JSON schema 在实现落地前不增加计划字段。

[ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 已记录上述输出边界、
M4.5/M6.5 拆分和 EMDF opaque policy；后续实现不得在没有新 ADR 的情况下改为默认处理或
原地覆盖场景 PCM。

## 3. M4.5：Presentation/Metadata 闭环

### 3.1 Presentation payload

按 `TS103190-2:v1.3.1:6.2.2.3`、`6.2.9` 与 `6.3.10` 完整解析并保留：

- presentation name、target level、target device category、ducking depth；
- substream activation map 与 alternative dataset index；
- dialnorm、further loudness info 与 presentation DRC；
- substream-group gains 与 associated-audio scaling/pan；
- custom downmix 参数与 loudness-correction 码值。

解析 API 必须显式接收 TOC/拓扑上下文以及前一有效 DRC 配置。所有长度 envelope 先验证边界，
成功后才提交跨帧状态；dependent frame 缺少历史配置时失败关闭。

### 3.2 Audio-substream metadata

补齐 `TS103190-2:v1.3.1:6.2.7` 与共享的
`TS103190-1:v1.4.1:4.2.14.11`/`4.3.14`：

- 不再仅记录并跳过 `tools_metadata_size`；解析 `dialog_enhancement()` 的配置与逐帧数据；
- 保留 loudness version、practice type、dialgate、correction type、program boundary 等当前只读走
  的原始字段；
- 配置按物理 substream 隔离，I-frame 更新成功后才替换历史；seek、配置变化、不连续和解析失败
  清空相应历史；
- 区分 `dialog_enhancement()` 与已经实现的 A-JOC `ajoc_dmx_de_data()`，两套状态不得共用。

### 3.3 EMDF 与 alternative 数据路径

按 `TS103190-1:v1.4.1:4.2.4.4`、`4.2.14.14` 与 `4.3.15`：

- 解析 payload ID、sample offset、duration、group ID、priority、processing/discard/duplicate 标志；
- 以有界容器保留 payload bytes，并设置 payload 数量与单 payload 大小上限；超过上限、终止符缺失、
  长度越界或变长字段溢出均返回结构化错误；
- 完成 alternative audio/OAMD dataset 的选择、状态归属与解析，移除该分支的笼统 unsupported；
- 未注册或未知 EMDF ID 不报语义错误，只按其 discard/processing 标志交给上层路由。

### 3.4 独立处理内核

实现可对调用方 PCM 缓冲区独立运行的标量基线：presentation DRC、dialog enhancement、
substream-group/associated gain、custom downmix 和 loudness correction。内核必须：

- 不依赖 `Ac4SceneFrame` 或容器层；
- 显式携带声道/组件布局、采样数、状态和工作区；
- 无信号或 metadata absent 时成为可验证的 identity；
- 记录增益域、舍入、饱和、denormal、延迟和跨平台一致性策略；
- 稳态路径不做逐帧堆分配。

M4.5 只验证这些内核及 alternative 路径到当前可用 PCM 层级，不生成最终 presentation PCM。

## 4. M6.5：Presentation 处理输出

M6 完成 A-JOC full reconstruction 后，增加可选 `PresentationProcessor`：

```text
Ac4SceneFrame
    + explicit presentation/target/output request
    -> PresentationProcessor
    -> ProcessedPresentationFrame
```

调用方必须显式提供 presentation 与目标选择；多目标或信息不足时不得猜测。派生输出至少记录：

- 来源 access unit、配置代次与 presentation ID/index；
- alternative target、device category、dataset 与实际激活的 group/substream；
- 请求和实际输出布局；
- 实际应用的 DRC、DE、gain、downmix 与 loudness-correction 操作；
- 各阶段延迟、总有效采样范围、concealment 与状态完整性。

处理器关闭时，不得改变原场景的 PCM 位模式、元数据、对象 identity、更新顺序或采样时间线。
处理失败不得污染后续帧状态；恢复必须遵守完整随机访问点与外部 discontinuity 规则。

## 5. 验证与退出条件

### 5.1 当前向量能力

2026-08-17 使用相同 PCM 对 Dolby Atmos Master ADM 元数据做了差分实验，并分别交给 Logic 与
Dolby Atmos Renderer 的 768 kbit/s AC-4 后端编码：

- 修改 DAMF 对象的 `dialog`/`music` 标记后，Dolby Conversion Tool 生成的 ADM BWF 与基线
  逐字节相同；
- 把床与对白对象拆成两个 `audioContent` 后，两套编码后端的 AC-4 `mdat` 仍分别与各自基线
  逐字节相同，`b_de_data_present` 仍为 0；
- Dolby Atmos ADM profile 只允许一个 `audioProgramme`；多 programme 会被判为无效并移除；
- `alternativeValueSet` 会在规范化时被移除，complementary object 会被该 profile 拒绝；
- programme 名称与响度元数据会被恢复或移除，不能控制 AC-4 presentation payload。

随后使用 Dolby Encoding Engine 5.2.1-5994839 的 `encode_to_ims_ac4` 服务，以
`probe_axes_single_object` 的合成 DAMF 生成了 256 kbit/s IMS 裸流。基线使用随附模板；
差分流只把 `ims_legacy_presentation` 从 `false` 改为 `true`：

```bash
./scripts/build_vector.sh --profile all vectors/probe_axes_single_object/case.json
```

该 case 的 `dee_ims` 数组现已把基线与 legacy 差分作业接入正式向量生产链。脚本在 DEE 可见的
隔离 staging 中渲染官方 raw AC-4 模板，编码后封装为
`master_ac4_ims_256K.m4a`/`master_ac4_ims_legacy_256K.m4a`，并记录作业参数与工具指纹；
生成媒体、DEE 日志和本机路径仍不进入版本控制。

| 配置 | SHA-256 | 可复核结果 |
| --- | --- | --- |
| IMS 基线 | `99a44431e4087ea14d03f8601743e5ced0deb425a825fed15dbe884826080dc4` | 141 帧；1 个 presentation；DE enabled，最大增益 9 dB |
| `ims_legacy_presentation=true` | `85bd7df38ca975d172d0f9585dfa503e48b6bff3752ef9fecae589f00081985c` | 141 帧；2 个 presentation，共享 1 个物理音频 substream |

表中是 DEE raw AC-4 的实验哈希。正式 M4A 的稳定 `payload_sha256` 分别为
`e7f65e5f19dc5470ce1bba3a9de01fcc8e666b023cff44b9d4e250d6aa370348` 与
`fcacb69514509fa6b6f908b6cfaa2d130f3be8b27d5215b2898ab932b844bdd1`；完整文件哈希会随 MP4
创建时间变化，以 `provenance.json` 的 payload 值作为回归基准。

本仓库的拓扑解析与 MediaInfo 26.01 交叉核对还确认：IMS presentation 携带 dialnorm
`-19`、ATSC A/85 integrated loudness 与五类 Film light DRC；legacy 流的两个 presentation
在全部 141 帧中 `alternative == false`。两条流的 `n_add_emdf_substreams` 均为 0，且没有
`payloads_substream_index`，因此其中的 `emdf_info()` 保留字段不能视为非空 EMDF payload。
legacy 流可用于“多 presentation 共享物理 substream”的回归，不能冒充 alternative 向量。

因此当前独立工具链已提供“非空普通 presentation payload”和 dialog enhancement 正向候选；
非空 EMDF 与 alternative presentation/dataset 仍没有可控生产入口。IMS 是 channel-based
路径，只验证 presentation/metadata，不替代 A-JOC 对象重建向量。这是当前编码工具及其
Dolby Atmos ADM/IMS profile 的行为边界，不表示 AC-4 规范不支持待验证语法。除非工具能力
改变，不得重复把通用 AXML 改写或 legacy presentation 当作这两类向量的生产方案。

### 5.2 临时分层门禁

每个语法分支都必须有构造测试，至少覆盖：最小与最大合法值、presence gate、截断、坏长度、
变长字段溢出、envelope 越界、I/P-frame 状态复用、reset 与事务回滚。

真实码流验证按当前可获得能力拆分：

- 非空普通 presentation payload 与 dialog enhancement 是 M4.5 的必需真实向量；DEE IMS
  已接入 `build_vector.sh`，解析落地后必须纳入本机回归；
- DEE IMS legacy 流只作为多 presentation/共享 substream 回归，不关闭 alternative 的外部
  验证状态；
- 非空 EMDF payload 与 alternative presentation/dataset 暂记为**外部向量待验证**，不阻塞
  解析代码合入、M4.5 的“实现完成（构造验证）”状态或 M5 开始；
- 待验证分支不得标为“真实码流已验证”，不得据此宣称完整 conformance，也不得作为默认启用
  presentation 处理的依据；
- 获得授权码流或具备对应配置入口的编码器后，必须补做正向真实向量、独立参考输出和回归门禁，
  才能关闭各自的外部验证状态。

真实媒体继续遵守仓库策略，不进入版本控制；`case.json`、工具版本、命令、hash 和可公开的派生统计
必须记录。

手工拼装的 AC-4 码流只计入构造测试，不能冒充独立工具链向量。直接修改真实 AC-4 payload
也不能关闭外部验证状态。

### 5.3 DSP 与端到端验证

- 内核测试覆盖 absent/identity、已知常量增益、分段增益、跨帧配置复用、声道映射、溢出与
  非有限值拒绝。
- 对 downmix 使用能量、对称性、静音保持和单通道脉冲路由判据；不能只做自编码—自解析往返。
- DRC/DE 必须验证配置变化、dependent frame、seek/reset 和显式延迟。
- M6.5 至少使用一份独立参考输出核对 target 选择、增益、时延和下混结果。
- 禁用处理器时，原始 `Ac4SceneFrame` 必须逐位或逐字段保持不变。

## 6. 文档落地顺序

实施时按以下顺序更新正式文档：

1. **已完成：** [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 冻结输出与
   状态边界。
2. 把 M4.5/M6.5、交付物和退出条件写入 `ROADMAP.md`。
3. 在 `ARCHITECTURE.md` 增加 metadata 状态所有权与可选处理分支，在
   `OUTPUT_CONTRACT.md` 增加 presentation metadata、opaque EMDF 与派生输出契约。
4. 在 `SPEC_TRACEABILITY.md` 录入经锁定 PDF 核对的精确条款，在
   `TEST_VECTOR_STRATEGY.md` 录入可获得的真实向量、外部向量待验证项和 DSP 门禁。
5. 实现及机器接口真正落地后，再版本化更新 CLI 输出契约与 schema；不得提前发布空字段。
