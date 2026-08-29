# MacinDecode-AC4-Core

[English](README.en.md) | 中文

Dolby AC-4 对象音频解码核心 · Rust 2024 · MSRV 1.98 · `unsafe` 禁用

## 这是什么

AC-4 是 Dolby 的下一代音频编解码器，支持基于对象的沉浸式音频（Dolby Atmos）。MacinDecode-AC4-Core 的目标是在不执行最终渲染的前提下，将 AC-4 码流还原为**渲染前音频场景**——包含 bed、对象 PCM、对象音频元数据（OAMD）和采样级时间线——交给外部渲染器使用。

本项目**不负责**扬声器/耳机渲染、响度管理、AC-4 编码或 Dolby 产品认证。

本项目是独立的开源实现，与 Dolby Laboratories 不存在隶属、赞助或认可关系。
Dolby、Dolby Atmos 与 AC-4 是其各自权利人的商标；文中名称仅用于兼容性说明。

## 特性

- **容器与同步**：MP4 (`ac-4` sample entry / `dac4`) 和 raw AC-4 sync frame 解析
- **场景拓扑**：Presentation / Group / Substream 关系、随机访问与配置代次状态机
- **Presentation 元数据**：响度/DRC/DE 只读语法、alternative OAMD、opaque EMDF 路由与 census
- **OAMD 时间线**：跨帧状态延续、帧内更新、ramp 和 seek 后完整性标记
- **音频核心解码**：反量化、IMDCT、联合声道矩阵、A-SPX 频谱扩展与 QMF 合成
- **A-JOC full 重建**：对象矩阵、wet/去相关、LFE 插回、终端 QMF 合成
- **场景 Rust API**：容器无关的借用视图、Session 控制面、presentation 选择与结构化错误
- **多格式导出**：PCM WAVE、ADM BWF (BW64/RF64)、DAMF (0.5.1/0.6.0)、Apple CAF
- **规范可追踪**：规范派生逻辑采用 `TS103190-1:v1.4.1:<clause>` / `TS103190-2:v1.3.1:<clause>` 引用格式，并维护条款↔实现↔测试追踪矩阵
- **`#![no_std]` 核心**：解码核心无平台依赖，禁用 `unsafe`

## 快速开始

### 前置条件

- Rust ≥ 1.98（[安装](https://rustup.rs/)）

也可以从 [GitHub Release](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/releases)
下载带完整音频解码和 ADM/DAMF 导出的预编译 `macinac4`。自动发布覆盖
Linux、macOS、Windows 的 x86_64 与 ARM64，产物及
SHA-256 校验方式见[多平台二进制发布](docs/BINARY_RELEASE.md)。预编译二进制已在
构建 runner 内从锁定的官方规范生成所需静态表，用户运行时不需要另行下载规范。

### 构建与测试

```bash
cargo build --workspace
cargo test --workspace
```

### 运行 trace

```bash
cargo run --bin macinac4 -- trace path/to/input.m4a
```

### 查看人读比特流报告

```bash
cargo run --bin macinac4 -- inspect path/to/input.m4a
cargo run --bin macinac4 -- inspect path/to/input.m4a --format json
```

同一报告也可以不启动子进程，直接从 Rust 调用：

```rust
use macindecode_ac4_inspect::{InspectSourceHint, inspect_bytes, inspect_path};

fn inspect_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let file_report = inspect_path("path/to/input.m4a")?;
    let bytes = std::fs::read("path/to/input.ac4")?;
    let memory_report = inspect_bytes(&bytes, InspectSourceHint::default())?;
    println!("{}", file_report.render_text());
    println!("memory frames: {}", memory_report.source.frame_count);
    Ok(())
}
```

`serde_json::to_value(&file_report)` 对应 CLI envelope 内的 `result.inspectResult`。

### 完整音频解码

从源码构建时，完整音频功能需要从官方 ETSI 规范在用户本地生成静态表，
并获取规范随附 C 表。
这些输入与生成物均被 Git 忽略，也不会进入 crates.io 包：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
cargo test --workspace --features audio-decode

# 有条件的本地真实向量测试；默认测试只会将它们列为 ignored。
cargo test -p macindecode-ac4-cli --features audio-decode -- --ignored
```

## 基础用法

以下是最常用的命令。完整的 10 个子命令参考见 [CLI 用法指南](docs/CLI_USAGE.md)。

**检视码流**——输出容器、拓扑与语法的结构化 JSON：

```bash
cargo run --bin macinac4 -- trace path/to/input.m4a
```

**导出 full 对象 PCM**——A-JOC 上混后的全部对象与 LFE：

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-objects-pcm path/to/input.m4a --output path/to/objects.wav
```

**导出 ADM BWF**——真实 full 对象与 OAMD 封装为标准 BW64：

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-adm-bwf path/to/input.m4a --output path/to/full-adm.wav
```

成功时 stdout 通常是带 `schema`/`version` 的 JSON v1 envelope；`inspect` 默认英文 text
是显式例外。失败时 stdout 为空，参数错误返回 2，运行期错误返回 1。

## 项目结构

```text
macindecode-ac4-cli ──→ macindecode-ac4-inspect ──→ macindecode-ac4-mp4
                         │                          │
                         └──────────────────────────┴→ macindecode-ac4-bitstream
macindecode-ac4-scene ──────────────────────────────→ macindecode-ac4-bitstream
```

| Crate | 职责 | `no_std` |
|---|---|---|
| [`macindecode-ac4-bitstream`](crates/macindecode-ac4-bitstream) | 比特流解析、TOC/OAMD/EMDF、ASF/A-SPX/A-JOC 音频重建 | ✅ |
| [`macindecode-ac4-inspect`](crates/macindecode-ac4-inspect) | MP4/raw AC-4 文件级聚合报告、JSON DTO 与英文 text renderer | — |
| [`macindecode-ac4-scene`](crates/macindecode-ac4-scene) | `Ac4SceneFrame` 数据契约及 A-JOC Core/Full 流式 Rust API | ✅ |
| [`macindecode-ac4-mp4`](crates/macindecode-ac4-mp4) | ISO BMFF box、`dac4`、sample table、edit/priming 时间线 | ✅ |
| [`macindecode-ac4-cli`](crates/macindecode-ac4-cli) | `macinac4` 工具：inspect、trace、PCM/ADM/DAMF/CAF 导出 | — |

## 数据流

```text
MP4 / raw AC-4
    → 容器与同步层
    → TOC / presentation / substream
    → 音频核心解码 (反量化 → IMDCT → PCM)
    → A-SPX 频谱扩展 (QMF)
    → A-JOC full 对象重建
    → OAMD 时间线
    → [M5] Ac4SceneFrame     ← Core/Full PCM/OAMD 借用场景入口已接入
    → 外部渲染器
```

## 当前进度

| 里程碑 | 状态 | 摘要 |
|---|---|---|
| M0 文档与工具链 | ✅ | 规范版本/哈希/向量来源/工具指纹冻结 |
| M1 容器与同步 | ✅ | MP4/raw 定界，ffprobe/Bento4/MediaInfo 交叉验证 |
| M2 TOC 与拓扑 | ✅ | Presentation/Group/Substream，随机访问状态机 |
| M3 OAMD 与时间线 | ✅ | 跨帧状态、帧内更新、seek 后完整性 |
| M4 音频核心基线 | ✅ | 反量化→IMDCT→A-SPX，12 条 A-JOC 媒体 core/A-SPX 逐位基线冻结 |
| M4.5 Presentation/Metadata | ✅（受限） | 只读解析与 DE/EMDF 真实媒体门禁完成；alternative、非零 DE body 与其他 EMDF 类型仍待样本 |
| M6 Full A-JOC 重建 | ✅ | 对象矩阵/wet/LFE/QMF 终端合成，第三份逐位基线冻结 |
| M5 场景 API | 🚧 | A-JOC Core/Full 借用 Rust API、core/A-SPX 基线、CoreCAF、ADM/DAMF 诊断渲染器与 Full batch 出口已接入；direct-object 待完成 |
| M7 公共 ABI | 🔲 | C ABI、SIMD 优化、fuzz |

详细进度、已知限制和音频重建支持矩阵见[实施路线图](docs/ROADMAP.md)。

## 设计原则

1. 正确性优先于优化；先建立可追踪的标量基线，再优化热点。
2. 比特流输入默认不可信；解析层不得依赖未检查的索引或长度。
3. 解码时间线使用整数采样位置，不以浮点秒作为内部基准。
4. 容器时间、编解码器时间和渲染时间必须分层表示。
5. A-JOC 为有损对象重建，验证不能简单等同于母版 PCM 的逐样本比较。
6. 规范条款、实现模块和测试案例必须能够相互追踪。
7. 仓库不提交专有二进制、授权 SDK、客户媒体或不可再分发测试素材。

## 文档

| 文档 | 说明 |
|---|---|
| [架构设计](docs/ARCHITECTURE.md) | 目标边界、依赖方向、时间模型、数值策略 |
| [CLI 用法指南](docs/CLI_USAGE.md) | 全部 10 个子命令的完整参考 |
| [CLI 输出契约 v1](docs/CLI_OUTPUT_CONTRACT.md) | 机器可读 JSON stdout/stderr 规范 |
| [多平台二进制发布](docs/BINARY_RELEASE.md) | 六目标自动构建、GitHub Release 与 SHA-256 校验 |
| [crates.io 发布检查](docs/CRATES_IO_RELEASE.md) | 包元数据、归档门禁、人工发布顺序与发布后抽查 |
| [渲染前输出契约](docs/OUTPUT_CONTRACT.md) | 场景帧语义与渲染前边界 |
| [实施路线图](docs/ROADMAP.md) | 里程碑详情、支持矩阵、已知限制 |
| [测试向量策略](docs/TEST_VECTOR_STRATEGY.md) | 向量生产链、验证层级、外部参考 |
| [规范可追踪性](docs/SPEC_TRACEABILITY.md) | 条款↔实现↔测试追踪矩阵 |
| [ADR 决策记录](docs/decisions/) | 语言、数值、变换与 Scene API 边界等 7 份 |

## License

[MIT](LICENSE)
