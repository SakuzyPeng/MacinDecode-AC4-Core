# Presentation 与元数据闭环计划

> **状态：实施中；部分外部向量受限。** alternative presentation 的名称、target 与逐
> substream activation/dataset selection 前缀已完成构造验证；公共 metadata 后缀、音频
> substream tools metadata、EMDF payload 与 alternative dataset 数据路径仍待实现。当前
> 工具链可再生产普通 presentation payload 与 dialog enhancement 正向候选；非空 EMDF
> payload 和 alternative presentation/dataset 仍按第 5 节保持外部向量待验证。当前实际进度
> 以[实施路线图](ROADMAP.md)为准。

## 1. 目的

在现有 M4 与 M5 之间增加 **M4.5：Presentation/Metadata 解析闭环**，补齐以下三组
只读解析能力：

1. 完整解析 `ac4_presentation_substream()`，包括普通与 alternative presentation。
2. 完整解析音频 substream metadata 中当前按长度跳过的标准元素，包括对话增强状态。
3. 解析并透明保留 EMDF payload envelope、时序配置与原始 payload。

规范是唯一的行为依据。其他实现只能用于发现遗漏、设计差分实验或构造反例，不作为规范引用、
正确性 oracle 或运行时依赖。

## 2. 不变边界

- `Ac4SceneFrame` 仍表示**未执行 presentation 处理的渲染前场景**，新增解析结果不得改写
  其 PCM、对象 identity、更新顺序或时间线。
- 本计划不实现 renderer，不接目标设备，也不执行 DRC、dialog enhancement、
  group/associated gain、custom downmix、loudness correction 等额外音频处理；相关字段只按
  码流原值解析、验证和保留。
- Channel-based PCM 重建延后；为解析 presentation payload 所需的拓扑信令可以保留，但不得
  借此宣称 Channel-based 可播放。
- M4.5 只把 alternative 路径补到普通路径已经达到的**语法解析层级**；A-JOC full
  reconstruction 的既有能力与本计划相互独立。
- EMDF 首期不解释注册表或私有 datatype 的业务语义，只保证 envelope、时序、路由字段与
  payload bytes 无损保留。
- target/device category 是只读选择信令，不由本项目结合本机设备自动选择。用户偏好、目标
  设备策略、扬声器映射和最终渲染均不在本计划内。
- 当前 CLI v1 契约与 JSON schema 在实现落地前不增加计划字段。

[ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 已记录 Scene 输出边界和
EMDF opaque policy；后续实现不得在没有新 ADR 与明确需求的情况下增加 presentation
处理器、默认处理或原地覆盖场景 PCM。

## 3. M4.5：Presentation/Metadata 闭环

### 3.1 Presentation payload

按 `TS103190-2:v1.3.1:6.2.2.3`、`6.2.9` 与 `6.3.10` 完整解析并保留：

- presentation name、target level、target device category、ducking depth；
- substream activation map 与 alternative dataset index；
- dialnorm、further loudness info 与 presentation DRC；
- substream-group gains 与 associated-audio scaling/pan；
- custom downmix 参数与 loudness-correction 码值。

当前首个增量已解析 `b_alternative` 控制的 selection 前缀：presentation name 分片、target
level/device category/ducking/loudness correction 码值，以及逐音频 substream 的 active 与
alternative dataset index。普通 presentation 没有该前缀；解析器只给出公共 metadata 后缀
的精确 bit offset，尚不把后缀标为已解析。

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

## 4. 明确延后或不在范围内

- Channel-based 音频重建链延后；本计划只消费其与 presentation payload 解析共用的拓扑上下文。
- renderer、扬声器/双耳布局、目标设备接入和本机能力探测不在范围内。
- 不新增 `PresentationProcessor` 或 `ProcessedPresentationFrame`，不执行 DRC、DE、gain、
  downmix、loudness correction，也不以解析结果修改任何 PCM。
- 不自动选择 alternative target 或 dataset；解析层只保留全部候选与码流顺序，信息不足、重复
  或上层未明确选择时不得猜测。

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
  自动 target/dataset 选择的依据；
- 获得授权码流或具备对应配置入口的编码器后，必须补做正向真实向量、独立解析交叉检查和回归门禁，
  才能关闭各自的外部验证状态。

真实媒体继续遵守仓库策略，不进入版本控制；`case.json`、工具版本、命令、hash 和可公开的派生统计
必须记录。

手工拼装的 AC-4 码流只计入构造测试，不能冒充独立工具链向量。直接修改真实 AC-4 payload
也不能关闭外部验证状态。

### 5.3 解析层与集成验证

- 解析测试必须覆盖 presence gate、最小/最大计数、截断、长度越界、变长字段溢出、I/P-frame
  状态复用、reset 与事务回滚。
- alternative target 与 dataset 只验证码流顺序、原始码值和拓扑关联，不用本机设备或自动
  target 选择充当 oracle。
- 构造码流只关闭分支覆盖，不关闭外部向量待验证状态；取得真实样本后再增加独立交叉检查。
- 启用全部 metadata 解析后，原始 `Ac4SceneFrame` PCM、对象 identity、事件顺序与时间线必须
  保持不变。

## 6. 文档落地顺序

实施时按以下顺序更新正式文档：

1. **已完成：** [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 冻结输出与
   状态边界。
2. 把 M4.5 的解析交付物和退出条件写入 `ROADMAP.md`。
3. 在 `ARCHITECTURE.md` 增加 metadata 状态所有权，在 `OUTPUT_CONTRACT.md` 增加
   presentation metadata 与 opaque EMDF 契约；不增加处理后 PCM 契约。
4. 在 `SPEC_TRACEABILITY.md` 录入经锁定 PDF 核对的精确条款，在
   `TEST_VECTOR_STRATEGY.md` 录入可获得的真实向量、外部向量待验证项和解析门禁。
5. 实现及机器接口真正落地后，再版本化更新 CLI 输出契约与 schema；不得提前发布空字段。
