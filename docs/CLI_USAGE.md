# CLI 用法指南

本文档包含 `macinac4` 命令行工具的完整用法参考。基础介绍见 [README](../README.md)，
机器可读的 JSON 输出规范见 [CLI 输出契约 v1](CLI_OUTPUT_CONTRACT.md)。

## 通用约定

- 除 `inspect` 默认的英文 text 报告外，业务命令成功时 stdout 是带
  `schema`/`version` 的 JSON v1 envelope；`inspect --format json` 同样使用该 envelope。
  warning 与 error 是 stderr 上逐行的 JSONL。
- 参数错误返回 2，运行期错误返回 1，失败时 stdout 为空。
- 显式 `--help`、`--version` 仍在 stdout 输出普通文本并返回 0。
- 字段与旧 JSONPath 的完整迁移见 [CLI 输出契约 v1](CLI_OUTPUT_CONTRACT.md)。
- 下列参数表不重复列出每个子命令都支持的 `-h` / `--help`。

## 前置条件

默认构建不需要受版权限制的规范附表：

```bash
cargo test --workspace
cargo run --bin macinac4 -- trace path/to/input.m4a
cargo run --bin macinac4 -- inspect path/to/input.m4a
```

完整音频语法测试需要从官方 ETSI PDF 本地生成 Rust 表，并获取随附 C 表：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
cargo test --workspace --features audio-decode

# 显式运行依赖本地 AC-4 素材的真实向量测试
cargo test -p macindecode-ac4-cli --features audio-decode -- --ignored
```

## 命令参考

### `trace`
输出 AC-4 容器、拓扑与语法 trace JSON。

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  trace path/to/input.m4a
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |

### `inspect`

单遍扫描 MP4/M4A 或 Annex G raw AC-4，输出类似 DRP/MediaInfo 的只读比特流元数据报告。
该命令不需要 `audio-decode` feature，不解析 `trace` JSON，也不执行响度、DRC、Dialogue
Enhancement、downmix 或 PCM 处理。

```bash
# 默认：固定章节和字段顺序的英文纯文本
cargo run --bin macinac4 -- inspect path/to/input.m4a
cargo run --bin macinac4 -- inspect path/to/input.ac4 --format text

# 自动化：CLI result v1 JSON envelope
cargo run --bin macinac4 -- inspect path/to/input.m4a --format json
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或 Annex G raw AC-4 输入。 |
| `--format <FORMAT>` | 否 | `text` | `text` 或 `json`；失败诊断始终是 stderr 上的 JSONL。 |

text 章节固定依次为 `Audio`、各 `Presentation`、各 `Substream`、`Issues`；所有字段都会
明确显示值或 `Not present`、`Not applicable`、`Unknown`、`Unsupported`，输出无颜色且末尾
恰有一个换行。JSON 的 `result.inspectResult` 固定包含 `source`、`stream`、
`presentations`、`audio_substreams`、`issues`；有码值的语义字段同时保留 `raw_code`。

报告以首个完整 independent 配置为基准；相同配置继续聚合。拓扑或稳定元数据改变时，
相关字段变为 `unknown`，并在 `issues` 中记录帧下标。presentation 解析状态按 effective ID
隔离，重复引用的物理 audio substream 只计一次。MP4 的 sync word/CRC 为
`not_applicable`；raw 报告 sync word、CRC 覆盖帧数与错误数。结构损坏、截断和无 AC-4 帧
仍返回非零状态与结构化诊断；CRC 错误、保留码和已知未支持语法则尽可能生成可用报告与
issue。

Rust 应用无需启动 CLI：`macindecode-ac4-inspect::inspect_path` 检查文件，
`inspect_bytes` 检查调用方持有的字节。公共 `InspectReport` 可直接读取字段或调用
`render_text()`；其 Serde 输出对应 CLI 的裸 `result.inspectResult`，不包含
`schema`/`version`/`command` envelope。

术语和换算依据 ETSI，目标只是提供“类似 DRP 的可读信息”，不复刻或推断 DRP 私有字段与
算法。`Metadata authentication ID` 首版固定为 `unsupported`，因为尚无已确认的 ETSI
等价字段；不会根据编码器或品牌猜测为 `Dolby`。MediaInfo 可用于公开字段交叉核对，但不是
规范 oracle。

### `export-core-pcm`
导出 A-JOC 下混信号的核心带 PCM。

```bash
# A-JOC 下混信号的核心带 PCM（WAVE_FORMAT_EXTENSIBLE Float32，DIRECTOUT）。
# MP4 edit list 已生效；目标文件必须不存在。内容不缩放，保持 ±32768 量级。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-core-pcm path/to/input.m4a --output path/to/core.wav
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `--presentation <INDEX>` | 否 | `AutoUnique` | 零基 eligible presentation 下标；省略时仅在 eligible presentation 唯一时成功。 |
| `-o, --output <FILE>` | 是 | — | 新建的 WAVE_FORMAT_EXTENSIBLE Float32 文件；不会覆盖已有路径。 |

### `export-aspx-pcm`
导出补上 A-SPX 高频后的下混信号 PCM。

```bash
# 补上 A-SPX 高频后的下混信号 PCM。逐路顺序是 Pseudocode 14a 的 A-JOC 输入，
# LFE 单独排在最后（响应的 role 会区分），故与上一条导出不可混比。
# PCM 与 QMF 控制分别按 P1 表 188 的 d_pcm / d_ctrl 对齐；不是同一 raw frame 直接配对。
# 只放行已确认的长帧 A-SPX 子集；SIMPLE、1024 及以下短帧、活动压扩与
# FIC/TIC 交织均显式拒绝。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-aspx-pcm path/to/input.m4a --output path/to/aspx.wav
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `--presentation <INDEX>` | 否 | `AutoUnique` | 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。 |
| `-o, --output <FILE>` | 是 | — | 新建的 WAVE_FORMAT_EXTENSIBLE Float32 文件；不会覆盖已有路径。 |

### `export-objects-pcm`
导出 full A-JOC 对象 PCM。

```bash
# full A-JOC 对象 PCM；LFE 按 Pseudocode 15 插回，output_channel 即 WAVE 交织位置。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-objects-pcm path/to/input.m4a --output path/to/objects.wav
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `--presentation <INDEX>` | 否 | 唯一 eligible presentation | 按零基下标选择；省略时若不唯一则拒绝。 |
| `-o, --output <FILE>` | 是 | — | 新建的 WAVE_FORMAT_EXTENSIBLE Float32 文件；不会覆盖已有路径。 |

### `export-damf`
生成指定对象的粉红噪声 DAMF 试听探针。

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-damf path/to/input.m4a --output path/to/new-package --object 2:1
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `-o, --output <DIR>` | 是 | — | 新建的 DAMF 包目录；该路径必须不存在。 |
| `--object <SELECTOR>...` | 二选一 | — | 选择 `OBJECT` 或 `SUBSTREAM:OBJECT`；可重复或逗号分隔。 |
| `--all-objects` | 二选一 | — | 选择 presentation 内全部动态全频对象。 |
| `--presentation <INDEX>` | 多 presentation 时 | 唯一 presentation | presentation 下标。 |
| `--mode <MODE>` | 否 | `full` | 选择 `full` 或 `core` 对象集合。 |
| `--fps <RATE>` | 否 | `24` | `23.976`、`24`、`25`、`29.97`、`29.97df` 或 `30`。 |
| `--probe-level-dbfs <DBFS>` | 否 | `-18` | 每路粉红噪声的理论峰值。 |
| `--stem <NAME>` | 否 | 输入文件名 | 三件套文件名主干。 |
| `--strict-mapping` | 否 | 关闭 | 发现无法精确映射的 AC-4 元数据时失败。 |

`export-damf` 生成 `<stem>.atmos`、`<stem>.atmos.metadata` 与 `<stem>.atmos.audio`。输出固定为 48 kHz、24-bit little-endian CAF：前十路是静音 7.1.2 bed，之后每个选中对象各有一路连续粉红噪声。对象选择必须显式给出；可重复/逗号分隔使用 `--object SUBSTREAM:OBJECT`，或使用 `--all-objects`。输入有多个 presentation 时还必须指定 `--presentation INDEX`。输出目录必须尚不存在，程序会先完整写入同级临时目录再原子提交；无法等价写入 DAMF 的字段会报告警告，`--strict-mapping` 可把警告升级为整包失败。

### `export-full-damf`
将 full A-JOC 对象 PCM、可选 LFE 与 full OAMD 写成真实 DAMF 包。

```bash
# 同一趟 full 重建得到真实对象 PCM、LFE 和第二份/full OAMD；默认 DAMF 0.5.1/home。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-damf path/to/input.m4a --output path/to/full-package
# 3DoF 只改 manifest 的 version/type；metadata、CAF 与 headTrackMode 不变。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-damf path/to/input.m4a --output path/to/full-3dof-package \
  --presentation-type 3dof --fps 29.97df
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `-o, --output <DIR>` | 是 | — | 新建的 DAMF 包目录；该路径必须不存在。 |
| `--presentation <INDEX>` | 否 | 唯一 eligible presentation | 按零基下标选择；省略时若不唯一则拒绝。 |
| `--presentation-type <TYPE>` | 否 | `home` | 选择 `home`（DAMF 0.5.1）或 `3dof`（DAMF 0.6.0）manifest。 |
| `--fps <RATE>` | 否 | `24` | `23.976`、`24`、`25`、`29.97`、`29.97df` 或 `30`。 |
| `--stem <NAME>` | 否 | 输入文件名 | 三件套文件名主干。 |
| `--strict-mapping` | 否 | 关闭 | 发现无法精确映射的 AC-4 元数据时失败。 |

`export-full-damf` 不提供对象子集，也不生成试听噪声。它与 `export-full-adm-bwf` 共用 Scene Session batch adapter、节目级增益和 S24LE 交织 writer：前十轨是 7.1.2 compatibility bed，可选 LFE 只进入第 4 轨，其余九轨静音；全部 full Objects 从第 11 轨开始。因此 CAF audio payload 与 full ADM `data` payload 逐字节一致。对象按连续 full OAMD 与 `PcmTrackSource::AjocObject { object_index: 0…N−1 }` 显式配对，LFE 按来源标签定位；采样率固定 48 kHz，不执行重采样。

互操作提示：`export-full-damf` 生成的 `home`/DAMF 0.5.1 三件套已实测可由 Dolby Media Encoder（DME）直接读取并编码 AC-4，也可由 Dolby Atmos Conversion Tool 直接读取并转换其他 Atmos 母版格式，均无需先转换为 ADM BWF。manifest 保留 `creationTool: MacinDecode-AC4-Core full A-JOC` 与实际 `creationToolVersion` 即可；这两个字段如实记录 DAMF 包的创建工具，不需要改写为 ADM BWF `dbmd` 语境中的 `Created using Dolby equipment`。DME 与 Dolby Atmos Conversion Tool 均为用户自行安装的外部工具，不随本项目分发。

Dolby Atmos Conversion Tool 随安装包提供命令行程序 `cmdline_atmos_conversion_tool`，适合脚本化批量转换与互操作检查。例如把 DAMF 转为 ADM BWF/WAV：

```bash
cmdline_atmos_conversion_tool \
  --pm_in path/to/full-package/<stem>.atmos \
  --output_path path/to/converted.wav \
  --output_format wav
```

该可执行文件通常位于 Dolby Atmos Conversion Tool 的安装目录而不在 `PATH` 中；调用时可使用绝对路径。输出 `wav` 时，`--output_path` 应指向具体文件而不是目录。完整选项以 `cmdline_atmos_conversion_tool --help` 为准。

默认 `--presentation-type home` 写 `version: 0.5.1`/`type: home`；选择 `3dof` 只把 manifest 改为 `version: 0.6.0`/`type: 3dof`。两种 package 的 metadata、CAF、对象顺序、增益和时间线完全相同，逐对象 `headTrackMode` 仍由 full OAMD 决定，并不因 3DoF 被强制改写。`--fps` 只写 manifest，默认 24；启用 `--strict-mapping` 后，发现无法映射的字段会在创建输出目录前使命令失败。

### `export-adm-bwf`
直接生成包含粉红噪声试听探针的 ADM BWF 或 RF64。

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-adm-bwf path/to/input.m4a --output path/to/probe.wav --object 2:1
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-adm-bwf path/to/input.m4a --output path/to/logic-probe.wav --object 2:1 \
  --compatibility logic --fps 29.97df
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `-o, --output <FILE>` | 是 | — | 新建的 ADM BWF 文件；不会覆盖已有路径。 |
| `--object <SELECTOR>...` | 二选一 | — | 选择 `OBJECT` 或 `SUBSTREAM:OBJECT`；可重复或逗号分隔。 |
| `--all-objects` | 二选一 | — | 选择 presentation 内全部动态全频对象。 |
| `--presentation <INDEX>` | 多 presentation 时 | 唯一 presentation | presentation 下标。 |
| `--mode <MODE>` | 否 | `full` | 选择 `full` 或 `core` 对象集合。 |
| `--fps <RATE>` | 否 | `24` | Logic `dbmd` 帧率；标准 BW64 不使用。 |
| `--probe-level-dbfs <DBFS>` | 否 | `-18` | 每路粉红噪声的理论峰值。 |
| `--compatibility <MODE>` | 否 | `standard` | 选择 `standard` BW64 或 `logic` RF64/`dbmd`。 |
| `--strict-mapping` | 否 | 关闭 | 发现无法精确映射的 AC-4 元数据时失败。 |

`export-adm-bwf` 固定使用 `Atmos_Master` 作为 ADM programme/content 名称，并直接生成 [ITU-R BS.2088](https://www.itu.int/rec/R-REC-BS.2088/) 64 位容器：默认 `--compatibility standard` 固定写 `BW64`；`--compatibility logic` 固定写 Logic Pro 可识别的 `RF64`，并加入独立生成、逐段校验的 Logic 兼容 `dbmd`。两种配置无论文件是否超过 4 GiB，首块都固定为 `ds64`；这里的“64 位”指文件和 `data` 大小使用 64 位寻址，不是把采样改成 64-bit。PCM 仍固定为 48 kHz、24-bit little-endian，声道布局同样是十路静音 7.1.2 bed 加每个选中对象一路独立粉红噪声。ADM 图以兼容性广泛的 ITU-R BS.2076-2 profile 写入 `axml`，轨道映射写入 `chna`；OAMD 的 active、gain、位置、宽度、priority 和 ramp 直接写入对象 `audioBlockFormat`。Logic 配置还把 ADM 时钟字段量化为五位小数，以适配其导入器；非时钟型的 `interpolationLength` 保留完整精度。`--fps` 同步写入 Logic 兼容 `dbmd`，可选 `23.976`/`24`/`25`/`29.97`/`29.97df`/`30`，默认为 `24`；标准 BW64 不使用该值。输出文件必须不存在，并通过同目录临时文件原子提交。这些文件都是合成 ADM 试听探针，不声明等同于编码前母版；`dbmd` 仅用于明确请求的 Logic 互操作配置。

### `export-full-adm-bwf`
把 full 重建对象与第二份/full OAMD 封装为真实 ADM。

```bash
# 把 full 重建对象与第二份/full OAMD 封装为真实 ADM；默认标准 BW64。
# Logic Pro 互操作时改用 RF64、五位 ADM 时钟和按实际总轨数生成的 dbmd。
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-adm-bwf path/to/input.m4a --output path/to/full-adm.wav
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-adm-bwf path/to/input.m4a --output path/to/full-logic.wav \
  --compatibility logic --fps 29.97df
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `-o, --output <FILE>` | 是 | — | 新建的 ADM BWF 文件；不会覆盖已有路径。 |
| `--presentation <INDEX>` | 否 | 唯一 eligible presentation | 按零基下标选择；省略时若不唯一则拒绝。 |
| `--compatibility <MODE>` | 否 | `standard` | 选择 `standard` BW64 或 `logic` RF64/`dbmd`。 |
| `--fps <RATE>` | 否 | `24` | Logic `dbmd` 帧率；标准 BW64 不使用。 |
| `--strict-mapping` | 否 | 关闭 | 发现无法精确映射的 AC-4 元数据时失败。 |

`export-full-adm-bwf` 固定使用 `Atmos_Master` 作为 ADM programme/content 名称，不接受对象子集，取 A-JOC 上混后的全部 full 对象和第二份/full OAMD。场景与 PCM 由同一趟 Scene Session batch 解码共同采集；只接受所选 presentation 的单一物理 A-JOC substream，并要求 full OAMD 在存在 LFE 时将其置于 `object 0 / Bed`；有 LFE 时动态对象严格连续为 `1…N`，无 LFE 时则为 `0…N−1`，再按显式 `PcmTrackSource::AjocObject { object_index }`/`Lfe` 来源标签配对。LFE 在 `Pseudocode 15` 输出中的位置可以在对象首、中或尾，但始终写入 ADM 7.1.2 bed 第 4 轨；九条其余 bed 轨静音，full Objects 从第 11 轨开始。24-bit PCM 使用节目级增益策略、MP4 edit/preroll 时间线、原子 no-clobber 写出和 OAMD 映射。

默认 `--compatibility standard` 生成 BW64、九位 ADM 时钟且不写 dbmd；`--compatibility logic` 生成 RF64、五位时钟和 Logic 兼容 dbmd，`--fps` 只控制 dbmd 帧率码，`interpolationLength` 不随时钟降精度。768K 回归向量固定产生 20 个 full Objects、30 轨、288000 帧；Logic dbmd 为 564 字节，segment 10 声道数为 30。两种模式只改变容器元数据，PCM 与轨序逐字节一致。它通过公开的 `Ac4SceneFrame` batch adapter 采集对象 PCM 与 full OAMD，不加入未证实的 2400-sample DRP 偏移；未覆盖 full 分支继续返回 `unsupported.coding_path`。

### `export-core-caf`
对已验证的固定 core 网格绕过对象渲染，直接写 Apple 扬声器布局 Float32 CAF。

```bash
# 对已验证的固定 core 网格绕过对象渲染，直接写 Apple 扬声器布局 Float32 CAF。
# 固定乘 2^-15，不做归一化/限幅；外部真峰值处理必须保留 CAF chan tag。
cargo run --release -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-core-caf path/to/input.m4a --output path/to/core.caf
```

| 参数 | 必需 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `<INPUT>` | 是 | — | MP4/M4A 或裸 AC-4 输入。 |
| `-o, --output <FILE>` | 是 | — | 新建的 CoreAudio Float32 PCM CAF；不会覆盖已有路径。 |
| `--presentation <INDEX>` | 否 | 唯一 eligible presentation | 零基 presentation 下标；省略时按 `AutoUnique` 选择。 |

`export-core-caf` 通过 `Ac4DecoderSession(DecodeMode::Core)` 同步取得 A-SPX PCM 与 downmix OAMD；活动 dialogue enhancement 在 Session 边界返回 `unsupported.coding_path`。它不改变上述对象语义，只对可以证明等价的编码器模板提供便捷直写。它要求 `object 0 = LFE BED`、连续 `1…N = Dynamic`、全程活动且为默认 0 dB、位置逐事件精确匹配 5/7/9/11 点整数模板，并且没有非零 width、扩展坐标、zone、screen、depth、distance 或 divergence。可见区起点若带 ramp，还必须存在同一固定状态的 preroll 前态；OAMD common 必须全程一致，且不得携带自定义 trim、screen、bed distribution/render 等未固化工具。对应输出及 Apple 顺序为：5.1 `L R C LFE Ls Rs`、5.1.2 再加 `Ltm Rtm`、5.1.4 再加 `Vhl Vhr Ltr Rtr`、7.1.4 在高度声道前插入 `Rls Rrs`。7.1.4 因此把 q9/q10 排到第 7/8 轨，q5…q8 排到第 9…12 轨。PCM 固定乘 `2^-15` 写 Float32 little-endian，不扫描节目峰值、不归一化、不限幅；超过 `±1.0` 的有限样本原样保留。响度和真峰值由外部工具处理，处理后必须保留或重写 CAF `chan` layout tag。任何不满足严格模板的合法 core 返回 `mapping.unsupported`；最终文件以同目录 hard link 原子 no-clobber 发布，没有强制覆盖选项，并发导出也不会替换先到达的目标。

## 输出层级关系

core/A-SPX 两条 WAVE 是 A-JOC 前的诊断层；`export-objects-pcm` 是 full 重建音频。`export-full-adm-bwf` 把第二份/full OAMD 配到真实 PCM；`export-full-damf` 将同一对象场景封装为 DAMF；`export-core-caf` 只对严格固定网格提供直接扬声器槽位映射。编码媒体不进入版本控制，因此 GitHub Actions 只测试 `decode_check.py` 的 fail-closed、分段隔离与原子更新逻辑；真实数值基线必须在素材齐全的本机运行。

## 验证脚本

```bash
./scripts/audio_check.sh path/to/input.m4a
./scripts/trajectory_check.py vectors/<case_id>
./scripts/decode_check.py
./scripts/ajoc_census.py
./scripts/dme_native_check.py
./scripts/emdf_census.py
```

本地逐位回归基线分 core/aspx/objects 三份，默认三段都跑。每段基线中的十二条 A-JOC 媒体全部必须存在且成功，既有条目不得跳过。未入基线且走尚未实现编码路径的媒体会逐条列名跳过；至少一条须真正解码。`--update` 仅在该段门禁全部通过后原子更新对应基线，不会波及另外两段。

## 测试向量与集成测试

无条件测试与本地真实向量测试明确分组：普通 `cargo test` 只运行无条件用例，并把真实向量用例
显示为 `ignored`；只有追加 `-- --ignored` 才会执行后者。真实向量测试读取被版本控制排除的
`probe_axes_single_object` 编码产物；素材缺失时显式运行会失败，不再伪装成通过。也可用
`MACINAC4_PROBE_AXES_VECTOR=/path/to/master.m4a` 指定素材。

`case.json` 可同时声明 default、DME A-JOC、DME native 与 DEE IMS 作业。配置 `.env.local` 后，
`--profile dme_ac4` 只运行 DME A-JOC；`--profile dme_native` 运行 channel-based/native IMS；
`--profile dee_ims` 只运行 DEE；`--profile all` 运行四条链。省略 profile 仍保持原 default 行为。
DME A-JOC 的 Level/3DoF 输出名、隔离 DAMF 0.6.0/type 3dof、timing manifest 与对象
`headTrackMode` 行为不变，三条产物继续进入轨迹与三层 PCM 基线。

DME native 的 speaker WAVE 只从纯 bed case 的信号配方按 SMPTE 顺序重建，拒绝静默丢弃对象；
DAMF IMS 直接读取 canonical DAMF。general 固定 24 fps、24 帧 I-frame 间隔及 -2000 samples
offset；music 使用 native fps、关闭 Dialogue Intelligence 且 offset 为 0。当前六条 DME native
媒体与两条 DEE IMS 都是 channel-based，轨迹和 PCM 门禁按 manifest 具名跳过；
`dme_native_check.py` 单独验证 `ch_mode`、帧数和 DE New/Keep/absence，`emdf_census.py` 则冻结
六条非空 presentation EMDF 签名并逐条检查本地零路由媒体。详见
[测试向量策略](TEST_VECTOR_STRATEGY.md)。

## 补充说明

规范 PDF、附表、专有工具和生成媒体均不进入版本控制；本机工具路径写入 `.env.local`。
