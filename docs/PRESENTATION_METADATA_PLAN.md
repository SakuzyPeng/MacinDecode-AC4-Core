# Presentation 与元数据闭环计划

> **状态：实施中；部分外部向量受限。** alternative presentation 的名称、target 与逐
> substream activation/dataset selection 前缀、公共 additional-data envelope，以及
> dialnorm/further-loudness 前缀、DRC 长度 envelope、substream-group gain 原始更新与有效状态、
> associated-audio scaling/pan、custom downmix 与 loudness correction 已完成构造验证；DRC
> I-frame 配置与逐帧 data envelope 也已解析，跨帧状态可用前一有效配置解析 dependent data，
> `audio-decode` 下可显式解码 Huffman gains；音频 substream 的 tools metadata 已严格定界并
> 解析 dialogue-enhancement presence、I/dependent configuration gate 与 7 比特配置；默认构建
> 仍以原始 bit view 保留 `de_data()`/simulcast body，`audio-decode` 下已可显式解码完整帧内
> Huffman data 与 simulcast，并按物理 substream 事务性延续配置、panning 与两份参数索引。
> EMDF payload envelope、时序/transcoding 配置与 opaque bytes 已完成构造验证；non-A-JOC
> `metadata()` 与 A-JOC `audio_data_ajoc()` 两处动态数据中的 alternative OAMD 原始 dataset
> 及 `b_keep` 有效状态已完成构造验证。当前工具链可再生产普通 presentation payload
> 与 dialog enhancement 正向候选；非空 EMDF payload 和 alternative presentation/dataset 仍按第 5 节
> 保持外部向量待验证。当前实际进度以[实施路线图](ROADMAP.md)为准。

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
- immersive indicator、OAMD common timing、advanced dialogue-enhancement 原始参数与保留
  `add_data`；
- dialnorm、further loudness info 与 presentation DRC；
- substream-group gains 与 associated-audio scaling/pan；
- custom downmix 参数与 loudness-correction 码值。

首个增量已解析 `b_alternative` 控制的 selection 前缀：presentation name 分片、target
level/device category/ducking/loudness correction 码值，以及逐音频 substream 的 active 与
alternative dataset index。第二个增量在 selection 之后验证 `b_additional_data` 的 4 比特
字节数与 `variable_bits(2)` 扩展，完成 `byte_align` 后在声明的独立边界内解析 immersive
indicator、`pres_ch_mode == -1` 路径的 OAMD common timing 和 12/28 比特 advanced DE 原始
参数，并以无分配 bit view 保留剩余 `add_data`。普通 presentation 没有 selection 前缀；
拓扑现已按 Pseudocode 25/26 与 `6.3.3.1.29`–`6.3.3.1.31` 同时派生完整/core
channel mode、four-back、top-pairs 与 LFE，作为 `custom_dmx_data()` 的不可歧义上下文。
两条路径现在都解析 7 比特 `dialnorm_bits` 与可选 `further_loudness_info(1, 1)`，并返回紧随
响度字段的 `drc_metadata_size_value` 精确 bit offset。其 5 比特长度与 `variable_bits(3)`
扩展会严格定界完整 `drc_frame()`，保留 `b_drc_present` 和无分配原始 bit view。共享响度解析
保留 version/practice、dialgate/correction、programme boundary、量化响度与 RTLL 原值；
未定义 extension 以无分配 bit view 回取。拓扑的 `b_pres_ndot` 已显式传入 presentation parser；
DRC present 的 I-frame 会解析 1–8 个 decoder modes、输出电平、repeat/default/curve/gains
profile、E-AC-3 profile 及完整 compression-curve 原始参数，dependent frame 不错误读取配置。
I-frame `drc_data()` 进一步解析 repeat profile 的有效 gain/curve 形态、gainset 长度/版本和
curve reset/reserved；未知版本及 gainset body 始终以有界 bit view 保留。`audio-decode` 下的
独立 API 可按调用方显式提供的 channel-group/subframe 形状解码 version 0/1 的 `DRC_HCB`，还原
最多 128 个整数 dB₂ gain 与 version 1 扩展边界，但不应用 gain。`PresentationDrcState` 按
presentation 隔离前一有效配置，stateful parse 会据此解析 dependent-frame data；成功 I-frame
替换配置，DRC 缺席的 I-frame 清空配置，缺少历史或 data 不匹配时失败且不提交状态。
DRC envelope 后按规范 `n_substream_groups` 判定 group-gain
presence：单 group 不消费该字段，多 group 则区分未携带、`b_keep` 沿用和逐 group 新传六比特
`sg_gain` 码值。`PresentationSubstreamGroupGainState` 按 presentation 隔离有效码值：首次收到
`b_keep = 0` 前按规范使用全零的 0 dB 码，`b_keep` 沿用前值，新值替换整个数组，未携带或单
group gate 不存在时归零；`b_pres_ndot` 独立帧从全零起算。未 reset 的 dependent frame 若改变
group 数则失败且不提交状态，避免把旧数组静默映射到新拓扑。状态只保留六比特码值，不换算或
应用 gain。其后的
`b_associated` 与三个独立 scale presence、可选 8 比特 scale 原值、mono gate 和
`pan_associated` 已解析；scale 的 `0x00..=0xfe` 与静音码 `0xff` 均保留，pan 的合法
`0x00..=0xef` 原样保留，规范禁止的 `0xf0..=0xff` 失败关闭。解析止于
`custom_dmx_data()` 的精确 offset。随后已派生六种 `bs_ch_config`，以固定容量数组保留
1–4 个 output configuration，并逐分支解析 screen/back/top routing tool 与所有 3 比特 gain
原值。stereo gate 同时考虑完整/core mode，保留 LoRo/LtRt、LFE 与 preferred method；unused
output config 和 stereo surround 保留码失败关闭。custom/stereo/LFE 的 gate 未传与显式 absent
保持可区分。随后按 full/core/object mode gate 解析 `loud_corr()`，保留所有 presence 与 5 比特
correction 原值；core LoRo/LtRt 共用 presence，object correction 分支包含 9.X.4，规范解释为
0 dB 的码值 `31` 仍合法保留。末尾 `byte_align` 的填充值不解释，但对齐后必须恰好耗尽有界
payload，额外整字节失败关闭。这里不执行 gain、dB 换算、角度换算、pan 或 downmix。

当前无状态 API 已显式接收 TOC/拓扑上下文并解析 I-frame 配置及其 data envelope；stateful API
另行显式接收按 presentation 隔离的 DRC 或 group gain 状态，feature-gated gain API 再接收表
168/169 与 P2 表 69 派生的形状。所有长度 envelope 先验证边界，成功后才提交跨帧状态；seek、
换源、拓扑变化或不连续由调用方显式 reset。

### 3.2 Audio-substream metadata

补齐 `TS103190-2:v1.3.1:6.2.7` 与共享的
`TS103190-1:v1.4.1:4.2.14.11`/`4.3.14`：

- 不再仅记录并跳过 `tools_metadata_size`；先严格定界并保留完整 tools metadata，再解析
  `dialog_enhancement()` 的配置与逐帧数据；
- 保留 loudness version、practice type、dialgate、correction type、program boundary 等当前只读走
  的原始字段；
- 配置按物理 substream 隔离，I-frame 与 dependent 更新完成后才事务性替换历史；失败不提交，
  seek、拓扑变化、不连续或丢帧后由调用方显式清空相应历史；
- 区分 `dialog_enhancement()` 与已经实现的 A-JOC `ajoc_dmx_de_data()`，两套状态不得共用。

前两个增量已按 P2 `6.2.2.2`、`6.2.7.1`、`6.2.7.5`–`6.2.7.6` 与 P1
`4.2.14.11`–`4.2.14.13`、`4.3.12.1.1`、`4.3.14.2`–`4.3.14.3` 把
`tools_metadata_size` 作为精确比特长度建立独立 bounded reader。当前支持的
`bitstream_version = 2` 对应 `sus_ver = 1`，因此 tools 区段不含 audio-substream DRC；解析器
读取 `b_de_data_present`，缺席时要求声明区段恰好只有这一位。活动分支直接使用前置 info 的
`b_audio_ndot` 作为 `b_iframe`：I-frame 强制读取 7 比特 `de_config()`，dependent frame 先读取
`b_de_config_flag`，再区分沿用或显式更新配置。2 比特 `de_method`、2 比特 `de_max_gain` 与
3 比特 `de_channel_config` 均按原值保留；由表 171 派生 channel count，并拒绝 mono/stereo
不允许的 channel configuration。I-frame、dependent 沿用和 dependent 更新的配置前缀最短分别
为 8、2 和 9 比特，均不得越过 tools 边界借用随后的 `b_emdf_payloads_substream` 或对齐位。
一个 info 覆盖多个物理 substream 且 ndot 合取为假时，现有拓扑不能恢复每条 substream 的精确
`b_iframe`；DE 缺席仍可解析，活动分支则失败关闭。

完整 tools 区段与配置后的 `de_data()`/simulcast body 都可从原 payload 重建零拷贝 bit view。
第三个增量在 `audio-decode` 下提供显式无状态解码：本帧有新配置时直接使用，dependent
`KeepPrevious` 则要求调用方提供前一有效配置；随后解析 position/data keep、1–2 个 5 比特
panning index、双声道 M/S gate、5 比特 hybrid contribution，以及 channel mode 13/14 的
`b_de_simulcast` 与第二份 data。P1 附录 A.4 的 `DE_HCB_ABS_0`、`DE_HCB_DIFF_0`、
`DE_HCB_ABS_1`、`DE_HCB_DIFF_1` 四张码本分别按 `cb_off = 0/31/30/60` 映射，固定容量最多
保留 3 channel × 8 band 的 24 个 absolute/differential 整数码值；成功必须恰好耗尽 tools
body。默认构建不依赖本地规范码本，继续保留相同原始 bit view。

第四个增量新增 `DialogEnhancementState`，调用方为每个物理 `substream_index` 分别持有一个实例。
I-frame 从空候选开始；dependent frame 延续 configuration、primary panning、最近一次传输的
primary/simulcast 参数，以及仅属于上一帧的两份 `de_par_prev`。primary 与 separate-core
simulcast 的参数历史彼此独立，第二份 data 不会覆盖第一份的 differential 基准。dependent
configuration 更新时，参数基准按表 171 的 L/R/C 身份迁移；上一帧未使用的声道取零，M/S
表示只与相同声道对的 M/S 历史对应。panning keep 与 parameter keep 仍要求完整映射兼容；仅
`de_max_gain` 改变不影响兼容性。

P1 表 78 的 I-frame 还原从首个 absolute index 开始：同一 channel 沿 band 用 `ref_val` 累加，
下一 channel 的 band 0 以前一 channel 的 band 0 为锚点，再沿新 channel 累加；dependent frame
逐 channel/band 加到各自 `de_par_prev`。上一帧 DE 不活动时，规范要求 differential 基准归零；
`de_keep_data_flag` 的“latest transmitted”历史仍单独保留。panning keep 只接受上一帧兼容值。
所有 configuration、position、primary 与 simulcast 更新在整帧成功后一次提交；任何帧内解码、
keep 或还原错误均不污染已提交状态。I-frame absence 清空全部状态；缺少精确物理 `b_iframe`
上下文时，stateful API 失败而不猜测随机访问边界。

现有真实向量的 `tools_metadata_size = 1` 且 `b_de_data_present = 0`，只关闭 absence 路径的真实
验证；活动分支与跨帧状态仍只有构造验证。解析结果只保留有效整数索引，不反量化或执行 dialogue
enhancement，也不修改 PCM。

### 3.3 EMDF 与 alternative 数据路径

按 `TS103190-1:v1.4.1:4.2.4.4`、`4.2.14.14` 与 `4.3.15`：

- 解析 payload ID、sample offset、duration、group ID、priority、processing/discard/duplicate 标志；
- 以有界容器保留 payload bytes，并设置 payload 数量与单 payload 大小上限；超过上限、终止符缺失、
  长度越界或变长字段溢出均返回结构化错误；
- 完成 alternative audio/OAMD dataset 的原始解析与状态归属，移除该分支的笼统 unsupported；
  dataset 选择仍由上层显式执行；
- 未注册或未知 EMDF ID 不报语义错误，只按其 discard/processing 标志交给上层路由。

EMDF 项已完成构造验证：固定容量保存 32 个有序 payload descriptor，单 payload 采用 Annex G
24 比特最大帧长作为上限；原始 8 比特元素即使不在字节边界也可从原 substream 零拷贝重建。
解析结果保留 sample offset、duration、group ID、codec data、priority、processing/discard 与
duplicate 标志，但不解释注册表或私有 datatype。

alternative OAMD 的首个增量已实现 P2 `6.2.7.1`/`6.2.8.3`/`6.2.8.12` 中 non-A-JOC
`metadata()` 路径：上下文显式携带当前物理 direct-object substream 的对象描述、group timing
合并后的 `num_obj_info_blocks` 与精确 `b_audio_ndot`。解析器验证全部 `object_info_block()`、
ducking/category、扩展 dataset 数、BED/ISF/DYN/LFE 的 gain/position gate、common/per-object
数据点，以及每个有界 additional-data 区域内的 `ext_prec_alt_pos()`；未定义尾部以零拷贝 bit
view 保留。结果按码流顺序暴露全部 dataset，不读取 presentation target，也不应用 gain/位置。
为避免每个普通 substream 固定携带最坏 256 个 block 与所有 dataset，大集合保留在原 payload，
公共结构只保存已验证边界并按需迭代。`b_keep` 原值已保留。A-JOC `audio_data_ajoc()` 现在也
复用同一解析器，分别保留 core/downmix 与 full/upmix 的完整 block 和 dataset 边界。语法观察
不选择或应用 dataset；Core/Full 对象出口在应用语义落地前以类型化
`AlternativeObjectMetadata` 失败关闭，避免静默输出错误 PCM。`OamdAlternativeDataSetState`
按物理 substream、non-A-JOC/A-JOC core/full domain 与 dataset loop index 隔离，拥有化保存规范
已定义的 gain、标准/扩展精度位置，并在 dependent `b_keep` 帧复用；无历史、I-frame keep、
dependent 布局变化与 domain/index 串用均事务性失败。同一 A-JOC 物理 substream 内的 downmix
与 upmix 列表不能互相覆盖。opaque 尾部仍由当前帧原始 view 保留，状态不猜测未来扩展语义。
余项只是真实向量验证；Channel-based 路径没有对象上下文，按既定范围继续失败关闭。

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
2. **已完成：** 把 M4.5 的解析交付物和退出条件写入 `ROADMAP.md`。
3. **已完成：** 在 `ARCHITECTURE.md` 增加 metadata 状态所有权，在 `OUTPUT_CONTRACT.md` 增加
   presentation metadata 与 opaque EMDF 契约；不增加处理后 PCM 契约。
4. 在 `SPEC_TRACEABILITY.md` 录入经锁定 PDF 核对的精确条款，在
   `TEST_VECTOR_STRATEGY.md` 录入可获得的真实向量、外部向量待验证项和解析门禁。
5. 实现及机器接口真正落地后，再版本化更新 CLI 输出契约与 schema；不得提前发布空字段。
