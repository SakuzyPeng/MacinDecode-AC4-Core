# CLI 输出契约 v1

本文固定 `macinac4` 的机器可读 stdout/stderr。渲染前场景语义仍由
[输出契约](OUTPUT_CONTRACT.md)描述；本文只约束命令行进程边界。

对应的机器可读定义：

- [成功响应 JSON Schema](../crates/macindecode-ac4-cli/schema/cli-result-v1.schema.json)
- [诊断 JSON Schema](../crates/macindecode-ac4-cli/schema/cli-diagnostic-v1.schema.json)

## 1. 进程与流

- `trace`、`export-damf`、`export-full-damf`、`export-adm-bwf`、
  `export-full-adm-bwf`、`export-core-caf`、`export-core-pcm`、`export-aspx-pcm`、
  `export-objects-pcm` 成功时，stdout
  只包含一份两空格缩进的 JSON，末尾恰有换行。
- warning 和 error 写入 stderr，每条诊断是一行紧凑 JSON；不得跨行，也不得混入
  普通日志。
- 失败时 stdout 为空。运行期失败返回 1，Clap 参数错误返回 2。
- 显式 `--help`、`--version` 仍在 stdout 输出普通文本并返回 0。
- v1 直接替换旧的未版本化 JSON，不提供 legacy 开关。

所有成功响应使用同一 envelope：

```json
{
  "schema": "macinac4.cli-result",
  "version": 1,
  "command": "trace",
  "result": {}
}
```

消费者必须同时检查 `schema`、`version` 与 `command`，不得只凭某个内部字段猜测
结果种类。

## 2. `trace` 结果

`trace.result` 固定包含 `source`、`frames`、`validation`。`source.kind` 是来源判别字段。

MP4 来源：

```json
{
  "source": {
    "kind": "mp4",
    "boxes": [],
    "track": {},
    "presentation": {},
    "dac4": {},
    "derived": {},
    "first_samples": []
  },
  "frames": {},
  "validation": {}
}
```

Annex G 来源：

```json
{
  "source": {
    "kind": "annex_g",
    "payload_bytes": 0,
    "escaped_size_frames": 0,
    "crc": {
      "protected_frames": 0,
      "failures": 0
    },
    "first_frames": []
  },
  "frames": {},
  "validation": {}
}
```

`validation` 固定包含 `topology`、`oamd`、`audio_substream`、`ajoc`。前三项始终是
验证 section；未启用 `audio-decode` 时 `ajoc` 为 `null`。每个非空 section 都固定包含：

```text
coverage
references
timing
configuration
spectrum
pcm
invariants
observations
```

MP4 的 `source.dac4.presentations` 保存 DSI v1 的只读选择信令；
`source.dac4.first_toc_comparison` 按 effective presentation ID 把它与首帧 TOC 关联，并
报告未匹配 presentation 与逐字段失配。它不按两边数组下标配对，且不参与解码器配置。
DSI 中的 filter、扩展配置、语言标签、名称原始字节和 `skip_area` 统一输出完整 `length`、
最多 64 字节的 `hex_prefix` 及 `truncated`，不把大块不透明数据展开成 JSON number 数组。

不适用的分类是空对象。`observations` 收纳不构成通过/失败判据的细节与首帧样本。
原有成对 min/max 字段统一为 `{ "min": ..., "max": ... }`。任何非有限浮点值写为
JSON `null`；相应的 `*_nonfinite` 计数不变。

schema 的严格程度是分层的，消费者应据此判断可以依赖到哪一层：

- **骨架锁死**：envelope、`source` 的两个判别分支、`validation` 的四个 section、
  每个 section 的八个分类、`artifact` 与 `audio`，都是 `additionalProperties: false`
  且 `required` 覆盖全部 `properties`。增删任何一个键都是契约变更。
- **叶子演进**：`frames`、`track`、`presentation`、`dac4`、`derived`、`package`
  与八个分类的**内部**字段是自由对象。A-SPX 每个子节都会新增统计，锁死它们会让
  每加一条都要同时改 schema；这些字段可以只增不减地演进，恕不另行通告。

这条界线由 `crates/macindecode-ac4-cli/tests/common/result_schema.rs` 的 `success` 固定。
八种导出 wire 投影均由无媒体单元测试经过它；有可用输入时，端到端集成测试也复用同一
检查。必需键从本 schema 文件读出，两侧任一单方面改动都会让测试失败。

## 3. 导出结果

八个导出命令都至少包含：

```json
{
  "artifacts": [
    {
      "kind": "...",
      "path": "...",
      "bytes": 0
    }
  ],
  "audio": {
    "sample_rate_hz": 48000,
    "bit_depth": 24,
    "channels": 11,
    "frames": 288000,
    "format": "..."
  },
  "objects": [],
  "unmapped": []
}
```

- DAMF 的 `artifacts` 按 manifest、metadata、CAF 顺序列出，另有 `package`。探针 DAMF
  的 kind 为 `damf_manifest`/`damf_metadata`/`caf_audio`；真实 full DAMF 为
  `full_damf_manifest`/`full_damf_metadata`/`full_damf_audio`。
- full DAMF 另有 `tracks`、`scale`、`bandwidth`、`channel_order`；`package` 固定带
  `version`、`type` 与 `stem_artifacts = 3`。
- ADM BWF 另有 `profile`、`container`、`compatibility`、`axml_bytes`、`dbmd_bytes`。
- full ADM BWF 具有相同公共字段，artifact `kind` 为 `full_adm_bwf`；对象与轨道另带
  `ajoc_object`、`source_output_channel`，其 `channel_order` 为
  `"7.1.2_bed_then_ajoc_full_objects"`。
- core CAF 另有 `container`、`layout`、`tracks`、`scale`、`bandwidth`、
  `channel_order`；artifact `kind` 为 `core_speaker_caf`。
- core PCM 另有 `tracks`、`scale`、`bandwidth`；`tracks` 保留物理
  `(substream, element, channel)` 描述。
- A-SPX PCM 再以 `channel_order = "ajoc_input_then_lfe"` 明确其逐路语义。
- objects PCM 的 artifact `kind` 为 `objects_pcm_wave`，并以
  `channel_order = "ajoc_objects_with_lfe_reinserted"` 明确最终逐路语义。
- `path` 是命令显示给用户的路径；`bytes` 从已经原子提交的 artifact 读取。
- 每个 `unmapped` 项同时产生一条 `mapping.lossy` warning。两侧条数与 context 内容一致。

DAMF YAML 固定为 UTF-8、LF、两空格缩进和末尾换行；字符串使用确定性的 YAML 1.2
双引号 scalar。ADM AXML 由 quick-xml 事件 writer 输出，属性顺序由构造顺序固定，
动态属性和文本自动转义。CHNA、dbmd 与 PCM 的二进制布局不由本契约升级改变。

## 4. 结构化诊断

stderr 每行符合下列形状：

```json
{"schema":"macinac4.cli-diagnostic","version":1,"level":"error","command":"trace","code":"input.read_failed","message":"无法读取输入文件","context":{"path":"input.m4a","cause":"..."}}
```

`message` 是供人阅读的中文说明；自动化只依赖 `code` 和所需的 `context` 键。
稳定 code 集合如下：

| code | 含义 |
|---|---|
| `cli.invalid_arguments` | 参数、子命令或必需选项无效 |
| `feature.required` | 当前构建未启用所需 feature |
| `input.read_failed` | 输入无法读取 |
| `input.invalid` | 输入为空或不满足命令前置条件 |
| `parse.failed` | 容器或 AC-4 语法解析失败 |
| `selection.invalid` | presentation、对象或导出选项无效 |
| `mapping.unsupported` | 请求的映射无法执行 |
| `mapping.lossy` | 成功导出但存在有损映射；仅 warning |
| `output.exists` | 目标已存在，拒绝覆盖 |
| `output.create_failed` | 无法创建目标或临时 artifact |
| `output.write_failed` | 写入或 flush 失败 |
| `output.commit_failed` | 原子发布失败 |
| `unsupported.coding_path` | 码流合法，但走的编码路径本实现尚未覆盖 |
| `serialization.failed` | 成功结果无法序列化 |
| `internal.invariant_failed` | 内部结果形状或重建不变量失效 |

`unsupported.coding_path` 与 `internal.invariant_failed` 的分界是**谁出的问题**：
前者说码流用了一条我们没实现的路径，重跑、换参数都没用，得先补实现；后者说我们
自己的不变量破了。把前者报成后者等于诊断在说谎，自动化也没法据此区分「暂不支持」
与「真的坏了」。已识别编码路径时，`context.scene_path` 给出该码流的实际编码路径；
若失败发生在路径识别之前则省略该字段，不根据请求的解码模式猜测。

## 5. 旧 JSONPath 到 v1 的完整迁移

下表以旧响应根 `$` 为起点。`.*` 表示整个子树保持原字段名和内部形状；没有列出的
新字段（envelope、`kind`、artifact `bytes`、明确单位字段等）是 v1 新增字段。

### 5.1 MP4 trace

| 旧 JSONPath | v1 JSONPath |
|---|---|
| `$.container.top_level_boxes` | `$.result.source.boxes` |
| `$.container.track_index` | `$.result.source.track.track_index` |
| `$.container.media_timescale` | `$.result.source.track.media_timescale` |
| `$.container.media_duration` | `$.result.source.track.media_duration` |
| `$.container.duration_seconds` | `$.result.source.track.duration_seconds` |
| `$.container.sample_count` | `$.result.source.track.sample_count` |
| `$.container.sample_bytes` | `$.result.source.track.sample_bytes` |
| `$.presentation.*` | `$.result.source.presentation.*` |
| `$.dac4.*` | `$.result.source.dac4.*` |
| `$.derived.*` | `$.result.source.derived.*` |
| `$.frames.first` | `$.result.source.first_samples` |
| `$.frames.count` | `$.result.frames.count` |
| `$.frames.presented_count` | `$.result.frames.presented_count` |
| `$.frames.sync_frames` | `$.result.frames.sync_frames` |
| `$.frames.toc_parse_failures` | `$.result.frames.toc_parse_failures` |
| `$.frames.dac4_toc_mismatches` | `$.result.frames.dac4_toc_mismatches` |
| `$.frames.iframe_global_frames` | `$.result.frames.iframe_global_frames` |
| `$.frames.stss_iframe_mismatches` | `$.result.frames.stss_iframe_mismatches` |
| `$.frames.sequence_first` | `$.result.frames.sequence_first` |
| `$.frames.sequence_last` | `$.result.frames.sequence_last` |
| `$.frames.sequence_discontinuities` | `$.result.frames.sequence_discontinuities` |

### 5.2 Annex G trace

| 旧 JSONPath | v1 JSONPath |
|---|---|
| `$.format` (`raw_ac4_syncframe`) | `$.result.source.kind` (`annex_g`) |
| `$.frames.payload_bytes` | `$.result.source.payload_bytes` |
| `$.frames.escaped_frame_sizes` | `$.result.source.escaped_size_frames` |
| `$.frames.crc_protected` | `$.result.source.crc.protected_frames` |
| `$.frames.crc_failures` | `$.result.source.crc.failures` |
| `$.frames.first` | `$.result.source.first_frames` |
| `$.frames.count` | `$.result.frames.count` |
| `$.frames.toc_parse_failures` | `$.result.frames.toc_parse_failures` |
| `$.frames.iframe_global_frames` | `$.result.frames.iframe_global_frames` |
| `$.frames.sequence_first` | `$.result.frames.sequence_first` |
| `$.frames.sequence_last` | `$.result.frames.sequence_last` |
| `$.frames.sequence_discontinuities` | `$.result.frames.sequence_discontinuities` |

### 5.3 topology 验证

以下映射同时适用于 MP4 与 Annex G。

| 旧 `$.topology.<field>` | v1 `$.result.validation.topology...` |
|---|---|
| `frames_parsed`、`parse_failures`、`first_error` | `coverage.<field>` |
| `substream_size_overruns`、`dangling_group_references`、`substream_reference_failures` | `references.<field>` |
| `stss_random_access_mismatches`、`full_random_access_frames`、`audio_only_random_access_frames` | `timing.<field>` |
| `source_changes`、`reset_events`、`waiting_for_random_access_frames` | `timing.<field>` |
| `awaiting_random_access`、`decoding_delay` | `timing.<field>` |
| `frames_differing_from_first`、`scene_path`、`presentations` | `configuration.<field>` |
| `substream_groups`、`total_objects`、`config_generations` | `configuration.<field>` |
| `first_frame` | `observations.first_frame` |

### 5.4 OAMD 验证

旧前缀为 `$.topology.oamd`，新前缀为 `$.result.validation.oamd`。

| 旧字段 | 新相对路径 |
|---|---|
| `located`、`parsed`、`failures`、`first_error` | `coverage.<field>` |
| `timing_frames`、`timing_carryover_frames`、`max_align_bits` | `timing.<field>` |
| `max_block_offset_samples`、`max_ramp_duration` | `timing.<field>` |
| `common_data_frames`、`common_data_sync_mismatches` | `configuration.<field>` |
| `dyndata_blocks`、`history_dependent_blocks` | `configuration.<field>` |
| `min_obj_info_blocks` | `configuration.object_info_blocks.min` |
| `max_obj_info_blocks` | `configuration.object_info_blocks.max` |
| `first_timing` | `observations.first_timing` |

### 5.5 普通 audio substream 验证

旧前缀为 `$.topology.audio_substream`，新前缀为
`$.result.validation.audio_substream`。

| 旧字段 | 新相对路径 |
|---|---|
| `located`、`parsed`、`failures`、`first_error` | `coverage.<field>` |
| `min_audio_size` | `configuration.audio_size_bytes.min` |
| `max_audio_size` | `configuration.audio_size_bytes.max` |
| `min_metadata_bytes` | `configuration.metadata_bytes.min` |
| `max_metadata_bytes` | `configuration.metadata_bytes.max` |
| `max_tools_metadata_bits`、`dialnorm_frames`、`substream_loudness_frames` | `configuration.<field>` |
| `first_detail` | `observations.first_detail` |

### 5.6 A-JOC 验证

旧前缀为 `$.topology.ajoc_audio`，新前缀为 `$.result.validation.ajoc`。

| 旧字段 | 新相对路径 |
|---|---|
| `frames`、`parsed`、`substreams`、`parsed_substreams`、`failures`、`first_error` | `coverage.<field>` |
| `min_fill_bits` | `timing.fill_bits.min` |
| `max_fill_bits` | `timing.fill_bits.max` |
| `max_dmx_obj_info_blocks`、`max_umx_obj_info_blocks` | `timing.<field>` |
| `dmx_object_info_blocks`、`umx_object_info_blocks` | `timing.<field>` |
| `derive_timing_from_dmx`、`intra_frame_update_frames` | `timing.<field>` |
| `some_signals_inactive`、`oamd_extension_present` | `configuration.<field>` |
| `companding_frames`、`companding_active_frames` | `configuration.<field>` |
| `position_changes`、`differential_positions`、`state_failures` | `configuration.<field>` |
| `aspx_add_harmonic_frames`、`aspx_interleaved_frames` | `spectrum.<field>` |
| `aspx_variable_framing_frames`、`aspx_balance_frames` | `spectrum.<field>` |
| `scale_factor_bands` | `spectrum.scale_factor_bands` |
| `scale_factor_min` | `spectrum.scale_factor.min` |
| `scale_factor_max` | `spectrum.scale_factor.max` |
| `scale_factor_failures`、`scale_factor_first_error` | `spectrum.<field>` |
| `scaled_lines`、`scaled_peak`、`scaled_nonfinite` | `spectrum.<field>` |
| `scale_failures`、`scale_first_error` | `spectrum.<field>` |
| `ungrouped_lines`、`ungroup_failures`、`ungroup_first_error` | `spectrum.<field>` |
| `ungroup_count_mismatch`、`ungroup_energy_drift` | `spectrum.<field>` |
| `pcm_frames`、`pcm_samples`、`pcm_peak`、`pcm_nonfinite` | `pcm.<field>` |
| `synthesis_failures`、`synthesis_first_error` | `pcm.<field>` |
| `pcm_silent_input_frames`、`pcm_zero_output_with_nonzero_input_frames` | `pcm.<field>` |
| `ajoc_reconstruction_failures`、`ajoc_reconstruction_first_error` | `pcm.<field>` |
| `objects_nonfinite`、`objects_nonfinite_first_error` | `pcm.<field>` |
| `object_shape_mismatches`、`object_shape_first_error` | `pcm.<field>` |
| `reconstruction_invariants` | `invariants.reconstruction` |
| `first_detail`、`first_positions`、`position_timeline` | `observations.<field>` |
| `position_timeline_truncated` | `observations.position_timeline_truncated` |

### 5.7 `export-damf`

| 旧 JSONPath | v1 JSONPath |
|---|---|
| `$.manifest` | `$.result.artifacts[?(@.kind == 'damf_manifest')].path` |
| `$.metadata` | `$.result.artifacts[?(@.kind == 'damf_metadata')].path` |
| `$.audio` | `$.result.artifacts[?(@.kind == 'caf_audio')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.duration_samples` | `$.result.audio.frames` |
| `$.objects` | `$.result.objects` |
| `$.unmapped` | `$.result.unmapped` |

`audio.bit_depth`、`audio.channels`、`audio.format`、每个 artifact 的 `bytes` 和
`package` 是 v1 新增字段。`audio.format` 固定为 `caf_s24le`；旧实现曾把实际为
signed little-endian 的 CAF 错标为 `caf_s24be`，文件字节不受本次标签修正影响。

### 5.8 `export-full-damf`

该命令直接产生 v1 结果，没有旧公开 JSONPath：

| 内部字段 | v1 JSONPath |
|---|---|
| `$.manifest` | `$.result.artifacts[?(@.kind == 'full_damf_manifest')].path` |
| `$.metadata` | `$.result.artifacts[?(@.kind == 'full_damf_metadata')].path` |
| `$.audio` | `$.result.artifacts[?(@.kind == 'full_damf_audio')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.duration_samples` | `$.result.audio.frames` |
| `$.package_version` | `$.result.package.version` |
| `$.presentation_type` | `$.result.package.type` |
| `$.objects`、`$.tracks`、`$.unmapped` | `$.result.<field>` |
| `$.scale`、`$.bandwidth`、`$.channel_order` | `$.result.<field>` |

artifact 顺序固定为 manifest、metadata、CAF；`audio.format = "caf_s24le"`，
`bandwidth = "aspx"`，`channel_order = "7.1.2_bed_then_ajoc_full_objects"`。
省略 `--presentation` 时使用 `AutoUnique`，只有一个 eligible presentation 才成功；
显式参数按零基下标选择，选择失败使用 `selection.invalid`。
对象带 `selector`、零基 `ajoc_object`、`source_output_channel`、`damf_id` 与一基
`track_index`；bed ID 固定为 `0…9`，对象 ID 从 10 连续递增。home 的 `package` 为
`{"version":"0.5.1","type":"home","stem_artifacts":3}`，3DoF 则只把前两项改为
`0.6.0`/`3dof`。两种类型的 metadata、CAF、对象列表、轨序和 `scale` 必须相同。

### 5.9 `export-adm-bwf`

| 旧 JSONPath | v1 JSONPath |
|---|---|
| `$.file` | `$.result.artifacts[?(@.kind == 'adm_bwf')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.bit_depth` | `$.result.audio.bit_depth` |
| `$.channels` | `$.result.audio.channels` |
| `$.duration_samples` | `$.result.audio.frames` |
| `$.adm_version` | `$.result.profile` |
| `$.container` | `$.result.container` |
| `$.compatibility` | `$.result.compatibility` |
| `$.axml_bytes` | `$.result.axml_bytes` |
| `$.dbmd_bytes` | `$.result.dbmd_bytes` |
| `$.objects` | `$.result.objects` |
| `$.unmapped` | `$.result.unmapped` |

`audio.format` 与 artifact `bytes` 是 v1 新增字段。

### 5.10 `export-full-adm-bwf`

该命令直接产生 v1 结果，没有旧公开 JSONPath：

| 内部字段 | v1 JSONPath |
|---|---|
| `$.file` | `$.result.artifacts[?(@.kind == 'full_adm_bwf')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.bit_depth` | `$.result.audio.bit_depth` |
| `$.channels` | `$.result.audio.channels` |
| `$.duration_samples` | `$.result.audio.frames` |
| `$.adm_version` | `$.result.profile` |
| `$.container`、`$.compatibility` | `$.result.<field>` |
| `$.axml_bytes`、`$.dbmd_bytes` | `$.result.<field>` |
| `$.objects`、`$.tracks`、`$.unmapped` | `$.result.<field>` |
| `$.scale`、`$.bandwidth`、`$.channel_order` | `$.result.<field>` |

artifact `kind` 固定为 `full_adm_bwf`，`bandwidth` 固定为 `"aspx"`，
`channel_order` 固定为 `"7.1.2_bed_then_ajoc_full_objects"`。
省略 `--presentation` 时使用 `AutoUnique`，只有一个 eligible presentation 才成功；
显式参数按零基下标选择，选择失败使用 `selection.invalid`。每个 `objects` 项带
full OAMD `selector`、零基 `ajoc_object`、`source_output_channel`、一基
`track_index` 与 `audio_object_id`；对象 `tracks` 项保留前四者。LFE 是第 4 条 bed
track 的 `role = "bed"`/`essence = "lfe"`，并以 `source_output_channel` 记录其在
`Pseudocode 15` PCM 中的原位置；其余九条 bed track 为静音。

默认 `compatibility = "standard"` 时，`container = "BW64"` 且
`dbmd_bytes = null`；Logic 模式为 `container = "RF64"`，`dbmd_bytes` 是实际 chunk
payload 大小。两种模式的音频、对象选择与轨序相同；只改变容器、ADM 时钟精度与 dbmd。
`scale` 记录节目级 `global_linear_gain` 和 `source_peak`。

### 5.11 `export-core-caf`

该命令直接产生 v1 结果，没有旧公开 JSONPath：

| 内部字段 | v1 JSONPath |
|---|---|
| `$.file` | `$.result.artifacts[?(@.kind == 'core_speaker_caf')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.channels`、`$.frames`、`$.format` | `$.result.audio.<field>` |
| `$.container`、`$.layout` | `$.result.<field>` |
| `$.objects`、`$.tracks`、`$.unmapped` | `$.result.<field>` |
| `$.scale`、`$.bandwidth`、`$.channel_order` | `$.result.<field>` |

`audio.bit_depth` 固定为 32，`audio.format` 固定为 `caf_lpcm_f32le`，`container`
固定为 `CAF`。`layout` 是 `5.1`、`5.1.2`、`5.1.4` 或 `7.1.4`；
`channel_order` 是相应 CoreAudio tag 的空格分隔顺序。每个 `tracks` 项都带一基的
`track_index`、`speaker`、`role` 和源 `selector`；q 来源另带 `ajoc_input`，LFE
只有 `role = "lfe"`。`objects` 逐 q 记录映射到的 speaker/track；严格门禁不允许
有损近似，所以成功时 `unmapped` 为空。

`scale` 固定为
`fixed_linear_gain=0.000030517578125;internal_±32768_to_pcm_f32le;normalization=none;limiter=none`。
Float32 样本可超过 `±1.0`；命令不测量 sample/true peak，不归一化、限幅或削波。

### 5.12 `export-core-pcm`

| 旧 JSONPath | v1 JSONPath |
|---|---|
| `$.file` | `$.result.artifacts[?(@.kind == 'core_pcm_wave')].path` |
| `$.sample_rate` | `$.result.audio.sample_rate_hz` |
| `$.channels` | `$.result.audio.channels` |
| `$.frames` | `$.result.audio.frames` |
| `$.format` | `$.result.audio.format` |
| `$.tracks` | `$.result.tracks` |
| `$.scale` | `$.result.scale` |
| `$.bandwidth` | `$.result.bandwidth` |

`audio.bit_depth`、artifact `bytes`、空的 `objects` 与 `unmapped` 是 v1 新增字段。

省略 `--presentation` 时使用 `AutoUnique`，只有一个 eligible presentation 才成功；
显式参数按零基下标选择。选择失败使用 `selection.invalid`，成功 wire schema 不变。

### 5.13 `export-aspx-pcm`

与 `export-core-pcm` 的公共字段相同，但 artifact `kind` 为 `aspx_pcm_wave`，并新增：

| 内部字段 | v1 JSONPath |
|---|---|
| `$.tracks` | `$.result.tracks` |
| `$.scale` | `$.result.scale` |
| `$.bandwidth` | `$.result.bandwidth` |
| `$.channel_order` | `$.result.channel_order` |

`tracks` 的 A-JOC 输入带 `role = "ajoc_input"` 与 `ajoc_input`；LFE 只带
`role = "lfe"`，不得获得伪造的 A-JOC 输入下标。

presentation 选择语义与 `export-core-pcm` 相同。

### 5.14 `export-objects-pcm`

与前两条 PCM 命令共用 Float32 WAVE、DIRECTOUT、`±32768`、edit list、原子写出
与拒绝覆盖语义。artifact `kind` 固定为 `objects_pcm_wave`，`bandwidth` 固定为
`"aspx"`，`channel_order` 固定为 `"ajoc_objects_with_lfe_reinserted"`。

对象轨固定带 `role = "ajoc_object"`、`ajoc_object` 与 `output_channel`；LFE 轨
固定带 `role = "lfe"` 与 `output_channel`，不带伪对象下标。`output_channel` 是
零基 WAVE 交织位置，已经反映 `Pseudocode 15` 的 LFE 插回。

省略 `--presentation` 时使用 `AutoUnique`，只有一个 eligible presentation 才成功；
显式参数按零基下标选择。选择失败使用既有 `selection.invalid`，成功 wire schema 不变。
