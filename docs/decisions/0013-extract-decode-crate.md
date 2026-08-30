# ADR-0013：提取 `macindecode-ac4-decode` crate

- 状态：Accepted
- 日期：2026-08-30
- 关系：实施 [ADR-0011](0011-layer-syntax-decode-and-scene.md) 的物理拆包；保持
  [ADR-0007](0007-preprocessed-scene-rust-api-boundary.md) 的 Scene 公共边界不变

## 背景

ADR-0011 已把长期依赖方向冻结为 bounded syntax/metadata → decode/DSP engine →
pre-render Scene，并决定物理拆包时优先只新增一个 `macindecode-ac4-decode`。拆包前，
`macindecode-ac4-bitstream` 同时拥有纯语法、规范表生成、Huffman 解码、ASF/A-SPX/A-JOC
数值重建与 Full engine；默认 metadata-only 消费者因此共享了与自身无关的构建职责和模块树。

生产实现已经具备清晰的单向边：数值路径读取 bitstream 层产生的语法模型和 bounded bit
view，而 bitstream 解析不需要任何 PCM、QMF 或对象重建类型。现有三段逐位 PCM 基线、Scene
事务测试与层门禁也足以裁决搬运是否改变行为。

## 决策

1. **新增公开的 `macindecode-ac4-decode` crate。** 它依赖
   `macindecode-ac4-bitstream`，拥有 ASF、A-SPX、A-JOC、声道工具、QMF、表 188 对齐、
   Full engine 与相关跨帧解码状态。
2. **`macindecode-ac4-bitstream` 收口为纯语法与 metadata crate。** 它继续拥有
   `BitReader`、sync/TOC/topology、presentation、audio metadata、raw/quantized OAMD 与
   opaque EMDF，不依赖 decode，也不再拥有 feature、`build.rs` 或规范表。
3. **规范表流水线整体迁入 decode。** `build.rs`、`build_support`、`spec_lock`、PDF
   生成表消费者、随附 C 表解析器和 Huffman 机制保持单一真相源；不按 metadata/DSP
   消费者复制构建逻辑。
4. **两类 Huffman 编码 metadata 跟随表消费者迁移。** presentation DRC gains 与
   dialogue-enhancement data 在规范上属于 metadata，不属于 DSP；但其完整解码需要同一批
   随附 C 表，因此实现位于 decode。bitstream 只保留 configuration、presence 与 bounded
   raw view。
5. **跨 crate 行为通过扩展 trait 提供。** decode 分别以
   `PresentationDrcGainSetExt` 和 `DialogEnhancementMetadataExt` 为 bitstream 类型增加
   显式解码入口，避免 Rust 禁止的外部类型 inherent impl，也避免语法 crate 反向依赖
   规范表。bit view 通过验证边界的公共构造器/reader 交接，不公开内部字段。
6. **Scene 同时依赖 syntax 与 decode。** 它继续负责 presentation 选择、Session 事务和
   渲染前输出；MP4、inspect、CLI 与 perf 的职责不变。FFI 仍延后，OAMD 仍不独立成 crate。
7. **提取本身不得改变数值或输出。** 默认与 `audio-decode` 测试、`no_std` 目标、三段
   PCM 基线、Scene/CLI 契约、层门禁、规范分发和 crates.io 打包共同作为迁移门禁。

## 理由

- Cargo 现在直接强制 `bitstream -> decode` 不存在，职责方向不再只靠模块约定维护。
- metadata-only 构建不再执行规范表构建脚本，也不需要 `libm` 构建依赖。
- ASF/A-SPX/A-JOC 共用的工作区、QMF 和时间状态仍在一个 crate 内，不会过早冻结更多跨
  crate DSP 接口。
- 扩展 trait 清楚标示“语法视图 + 可选规范表解码”的边界，同时让调用形式保持接近原 API。

## 验证证据

决策 7 的迁移门禁已全部收口：

- **三段逐位 PCM 基线全部一致。** `scripts/decode_check.py` 的 `core`、`aspx`、`objects`
  三段各十二条真实向量摘要与提取前完全相同。这是「纯搬运」这句话唯一的直接证据——编译与
  单元测试只证明代码还能跑，证明不了 PCM 没变。
- **三个构建配置的 clippy / test 全绿。** 默认与 `audio-decode` 自提取起保持通过；
  `spec-tables` 在补齐覆盖后首次作为独立 CI 门禁通过。提取时它尚未纳入持续集成裁决。
- **`no_std` 目标未回归。** thumbv7em-none-eabi 上默认 Scene API 与完整 decoder 两条检查均
  通过，MSRV 1.98.0 独立复验 `--features audio-decode`。
- **边界与分发门禁通过。** `check_layers.py` 的逐 crate 方向、`check_spec_distribution.py`
  的规范分发边界与 `cargo package --workspace` 的打包检查均在新布局下通过。

## 影响

workspace 现在包含七个 crate，其中六个公开发布。内部发布顺序增加一层：
`bitstream → decode → scene/mp4 → inspect → CLI`。启用完整音频的调用方需要在自己的 feature
中转发 `macindecode-ac4-decode/audio-decode`；`macindecode-ac4-bitstream` 不再提供同名 feature。

源码公共路径从 `macindecode_ac4_bitstream::{asf, aspx, ajoc, ...}` 移到
`macindecode_ac4_decode::{asf, aspx, ajoc, ...}`。这些 crate 尚未发布，因此本次不保留旧路径
facade；首次发布前的文档、脚本、基准命令和规范追踪路径必须同步到新位置。

## 未采用的方向

### 在 bitstream 中保留 decode re-export

这会要求 bitstream 反向依赖 decode，立即形成依赖环；通过 feature 或可选依赖也不能消除该环。

### 分别为 DRC/DE 复制 Huffman 表构建逻辑

会产生两套 C 表解析、哈希锁与版权分发边界，增加静默漂移风险。让实现跟随唯一表消费者，
同时由语法层保留原始 view，边界更小且可审计。

### 同时拆出 A-JOC 或 OAMD crate

当前 A-JOC 与 ASF/A-SPX/QMF 共用数值状态，OAMD 则横跨 raw syntax 与 Scene 语义；继续细分
会扩大公共接口且没有独立构建收益，仍按 ADR-0011 延后评估。
