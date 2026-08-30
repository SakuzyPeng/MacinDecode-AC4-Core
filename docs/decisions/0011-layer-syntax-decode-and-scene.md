# ADR-0011：按语法、解码与场景重整职责边界

- 状态：Accepted，第 4、6 条与「影响」「未采用的方向」中以 direct-object 完成为前提的表述由
  [ADR-0012](0012-defer-direct-object.md) 重新锚定；第 2 步要求的 re-export facade 阶段由
  [ADR-0013](0013-extract-decode-crate.md) 以「发布前无兼容负担」为由跳过，边界改由层门禁
  直接证明
- 日期：2026-08-29
- 关系：替代 `ARCHITECTURE.md` 中尚未实施的 `audio-core` / `ajoc` / `oamd`
  三路拆包草图；保持 [ADR-0007](0007-preprocessed-scene-rust-api-boundary.md) 的
  Scene 公共边界不变

## 背景

workspace 已经形成六个 crate：三个 `no_std` 核心库、公开 inspection 库、CLI 和内部性能
harness。`macindecode-ac4-bitstream` 也已经从初期的 bounded parser 扩展为同时包含：

- sync/TOC/topology、presentation、OAMD、EMDF 与音频语法；
- ASF 反量化与 IMDCT、A-SPX/QMF、A-JOC 参数与对象重建；
- 音频元素驱动、表 188 对齐和统一 Full A-JOC engine。

早期架构文档把 `macindecode-ac4-audio-core`、`macindecode-ac4-ajoc` 和
`macindecode-ac4-oamd` 记为可能的目标 crate。当时 PCM 与 Scene API 尚未建立，这些名称只用于
表达职责，不是已决定的发布包。

现在 Core/A-SPX/Full 三层 PCM 基线和 A-JOC Core/Full Scene API 已经稳定到足以重整内部依赖，
但 direct-object 尚未完成。实际实现还表明 A-JOC 重建与 ASF/A-SPX/QMF 共享大量数值类型、工作区
和时间状态；OAMD 则同时参与 topology、substream、解码控制与 Scene 语义组装。按规范章节直接
拆成三个 crate 会过早公开内部 DSP 类型，并可能制造不自然的反向依赖。

## 决策

1. **保留三层职责，而不是按三个规范子系统立即拆包。** 长期依赖方向是：
   bounded bitstream syntax/metadata → audio decode/DSP engine → pre-render Scene。
2. **先在现有 crate 内建立 `syntax`、`decode` 与 `engine` 边界。** 移动实现时保留现有公共路径的
   re-export，并用测试和依赖门禁证明边界后，再进行物理 crate 提取。
3. **物理拆包时优先只新增一个 `macindecode-ac4-decode`。** 它依赖
   `macindecode-ac4-bitstream`，拥有 ASF/A-SPX/A-JOC 数值处理、QMF、表 188 对齐与 Full engine；
   `macindecode-ac4-scene` 同时依赖 bitstream 的语法模型和 decode 的解码入口。
4. **OAMD 暂不成为独立 crate。** raw/quantized OAMD、解析状态及其 topology 关联继续属于
   bitstream；映射后的对象状态、更新排序和跨帧 Scene 事件继续属于 scene。direct-object 完成后
   若出现可复用且无环的中间模型，再单独评估拆包。
5. **MP4、inspection、CLI 与性能 harness 保持外层消费者。** MP4 不解释音频工具语义，Scene
   不读取容器，inspection 不进入 DSP；CLI 和 perf 可以组合这些库，但不得成为核心库的依赖。
6. **FFI 继续延后。** 只有在 Rust Scene API、direct-object 和真实宿主所有权需求稳定后，才建立
   版本化 `macindecode-ac4-ffi`；它依赖 Scene，Scene 不依赖 FFI 或平台胶水。
7. **拆分不得混入数值或输出行为变更。** 每个迁移增量必须分别通过默认与 `audio-decode` 测试、
   `no_std` 检查、三层 PCM 基线、Scene/CLI 契约和规范分发门禁。

如果任何相关 crate 已经发布，物理拆包还必须通过兼容 facade、re-export 和正常的 semver 迁移
保护既有路径；不得只因 workspace 内部可以同步修改就假定外部没有消费者。

## 理由

- `audio-decode` 现有 feature 已经接近语法/metadata 与完整 DSP 的自然边界，可以先作为迁移
  seam，而不必同时冻结多个新 crate 的内部类型。
- ASF、A-SPX 与 A-JOC 的终端处理共同依赖 QMF、帧对齐和事务状态，一个 decode crate 能保持
  状态所有权完整；只有出现独立复用或构建需求时才值得继续细分。
- OAMD 的 raw 与 Scene 语义本来就处于边界两侧，保留这种分工比增加中间 crate 更清楚。
- 先做内部模块化和兼容 re-export，可以把结构风险与数值风险分开，由现有逐位基线裁决。

## 影响

正面影响：

- `macindecode-ac4-bitstream` 可以逐步收敛为名称所表达的语法与 metadata 层。
- inspection、MP4 和默认无规范表构建不必接触完整 DSP engine。
- Scene 保持容器无关和 presentation 处理前边界，同时不再直接依赖低层 DSP 子模块布局。
- 将来若确有需要，A-JOC 或 FFI 仍可在已有单向依赖上继续拆分。

代价与限制：

- 构建脚本、规范表和 feature 所有权需要随 decode 边界一起迁移，不能只移动 Rust 源文件。
- 兼容 re-export 会在过渡期保留一层 facade 和重复文档入口。
- direct-object 完成前，OAMD 的最终可复用中间模型仍未冻结。

## 未采用的方向

### 立即建立 `audio-core`、`ajoc` 与 `oamd` 三个 crate

会把 QMF、工作区、量化模型和跨帧状态提前变成跨 crate 接口，并把尚未完成的 direct-object
边界固化，当前收益不足以抵消版本与构建复杂度。

### 永久保留一个包含全部功能的 bitstream crate

现有依赖图没有环，功能也能工作，但 parser、DSP 和 engine 长期共用一个公共模块树会扩大变更面，
并让 metadata-only 消费者依赖与其需求无关的职责名称和发布节奏。

### 先做 FFI 再重整内部层次

会把当前内部类型和所有权偶然形态冻结成跨语言兼容负担；FFI 必须消费已经收口的 Scene 边界。
