# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Dolby AC-4 对象音频解码核心（Rust 2024、`no_std`、MSRV 1.98），输出渲染前音频场景，不做渲染。

`AGENTS.md` 已给出命令清单、代码风格、测试与提交约定，本文不重复，只补它没有的架构大图与易踩的坑。

## 两个构建配置

`audio-decode` **不是可选功能，是版权隔离**。附录 A 的 Huffman 码本只以 ETSI 随附 C 文件给出，那些文件不可重分发，因此依赖它们的谱解析与重建被关在这个 feature 后面。关闭时容器、TOC、拓扑、OAMD、metadata 仍然完整可解。

**任何改动都要在两个配置下各过一遍 fmt / clippy / test。** 只测一个配置是这个仓库最常见的失误：`cargo clippy --fix` 会在两个配置之间来回删 import（默认配置下未使用的，正是 `audio-decode` 需要的），`#[cfg]` 加在内容混合的整个模块上也会在另一配置下断裂。

```bash
./scripts/fetch_specs.py                       # 启用 audio-decode 前必须先取表
cargo test --workspace
cargo test --workspace --features audio-decode
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features audio-decode -- -D warnings

# 单个测试：按名称子串过滤
cargo test -p macindecode-ac4-bitstream --features audio-decode aspx::dequant
cargo test -p macindecode-ac4-cli --features audio-decode -- --exact wire::tests::nonfinite_numbers_become_json_null
```

CI 分三个 job：`quality` 只跑默认配置的 fmt / clippy / test，加 CI 中配置的无 PDF Python 审计和检查器测试；`audio-decode` 跑该 feature 的 clippy / test，`msrv` 在锁定的 1.98.0 上独立复验 `--features audio-decode`。PDF 支持的 SFB/A-SPX/A-JOC 审计仍需本地运行。**`fmt` 只在第一个 job 检查，MSRV 只在开启 feature 时验证**——本地只跑一个配置时这两处最容易漏。

## 依赖边界（ADR-0003）

三层，不要混为一谈：

- **目标侧零依赖**——`macindecode-ac4-bitstream` 与 `macindecode-ac4-mp4` 不依赖任何外部 crate（后者只依赖前者），`no_std` 依赖图不受影响。
- **构建期**只允许精确锁版本的 `libm = "=0.2.16"`，且只出现在 `[build-dependencies]`。
- **CLI 是宿主工具**，有 clap / serde / serde_json / quick-xml。它不受目标侧约束。

`core` 不提供超越函数，`sqrt` 起就不在其中。运行期需要的实数函数在 `macindecode-ac4-bitstream/src/math.rs` 自实现（ADR-0005），**不使用 `f64::mul_add`**——它不在 `core`，且在有无 FMA 的目标上舍入次数不同。

## 构建期表与冻结摘要

`build.rs` 只调度，实现在 `build_support/`。两类表的条件不同：

- **纯数学表**（反量化、IFFT 根、IMDCT 前后旋转、KBD 窗、QMF 调制相位）在**任何**配置下生成，与 ETSI 附件无关。
- **Huffman 码本**只在 `audio-decode` 下从 `spec/*.c` 解析，构建期校验 Kraft 等式与前缀无关性，任一不成立即中止构建。

表值由冻结的 SHA-256 闭锁，`scripts/check_transform_tables.py` 用标准库独立复算。**改动 `build_support/` 后表值变化会让构建失败，这是设计意图**，不是要绕开的障碍——须重做高精度审计后再更新摘要。

## A-SPX 是现在最大的子系统

`aspx/` 是 bitstream 里最大的单一子系统（约二十个模块，占该 crate 源码四成上下），逐节对应 `5.7.6.3`–`5.7.6.5`。**模块索引在 `aspx/mod.rs` 的文件头**，按「推导层（不需要 Huffman，任何配置下都在）／解码层（只在 `audio-decode` 下存在）」分栏列全，不要靠目录名猜哪个模块管哪一节。

`pipeline.rs` 是编排点，文件头那张四段通路表里藏着这个子系统的判据方法论：**`Y ≡ 0` 时 `Q_out` 就是延迟后的输入**，所以在 `5.7.6.4.5` 的组装接上之前，中间每加一段都**不改变输出**——而「输出没变」对一条全错的接线同样成立。因此每一段必须自带**不看输出也能生效**的判据（延迟线互校、时隙对齐、跨帧状态逐个隔离观察、逐包络的组数与量化档），直到最后一段落地才收口成「`Q_out` 减去旁路输出逐位等于本帧的 `Y`」。加新段时照这个套路走，别指望终点判据。

跨帧状态集中在 `state.rs`（一条声道一份，一起创建、一起重置），帧内缓冲集中在 `workspace.rs`（按合法配置的上界预留）。两者都由调用方提供，生产路径不逐帧分配。

## 三层 PCM 导出不可混比

`export-core-pcm`、`export-aspx-pcm` 与 `export-objects-pcm` 分别冻结 core、A-SPX
诊断和 full 对象层：

- **逐路顺序不同**——A-SPX 层按 `Pseudocode 14a` 排成 A-JOC 的输入顺序，不进 A-JOC 的 LFE 单独排在最后；响应里的 `role` 区分两者。
- **时间轴不同**——PCM 与 QMF 控制各按表 188 的 `d_pcm` / `d_ctrl` 对齐（`frame_alignment.rs`），不是同一个 raw frame 直接配对。
- **对象层再次改序**——artifact 轨带 `PcmTrackSource::AjocObject { object_index }`，LFE 按 `Pseudocode 15` 插入；`output_index` 才是 WAVE 交织位置。
- **基线不同**——`vectors/decode_baseline.json`、`vectors/aspx_baseline.json` 与 `vectors/objects_baseline.json` 各自冻结，`decode_check.py --stage ... --update` 只动指定段。共用文件会让一次更新把三层一起重冻。

LFE 的延迟是**判读，不是抄写**：`5.7.6.5.3` 称 `δ_ASPX` 是 A-SPX 引入的总延迟，`6.2.10` 又把 LFE 排除在 A-SPX 之外，而 `Pseudocode 15` 要把它并回已经吃过这段延迟的输出。取 A-SPX 的元素让 LFE 走 `Y ≡ 0` 通路补齐；取 SIMPLE 的元素**不能**复用那条——SIMPLE 整个元素都不激活 A-SPX，走了会凭空加进 192 或 384 个样本。依据写在 `element_drive.rs` 文件头与 `SPEC_TRACEABILITY.md` 第 7 节。

## CLI 的两段式输出

`trace` 采集 → 产出 **legacy JSON 文本** → `wire::prepare` 解析并重投影成 v1 envelope。所有命令都返回 `Result<String, CliError>`，`main.rs` 统一交给 `wire`。

耦合因此走的是运行期字符串键：`trace` 改一个字段名，`wire` 的 `root.remove("topology")` 会失配并报 `internal.invariant_failed`（有守卫，不会静默）。

契约见 `docs/CLI_OUTPUT_CONTRACT.md`。schema 的严格程度**分层**：骨架（envelope、source 判别、validation 的四个 section、每个 section 的八个分类、artifact、audio）锁死；叶子（`frames`、`track`、八个分类的内部字段）是自由对象，留给 A-SPX 各子节继续加统计。这条界线由 `crates/macindecode-ac4-cli/tests/common/result_schema.rs` 的 `success` 固定，必需键从 schema 文件读出，两侧任一单方面改动都会失败。

## CLI 内部的几条不成文规则

- **`trace` 的依赖声明**：外部依赖路径只在 `trace/mod.rs` 声明一次并向下 re-export，生产子模块用 `use super::{具体名称}` 显式列出。不要改成 glob——glob 会让 `unused_imports` 彻底失效，多余的依赖再也不会告警。
- **`trace/invariants.rs` 是 trace 重建判据的单一声明点**：`ReconstructionInvariant` 的枚举、`ALL` 与 `name` 由同一份变体列表生成，同时驱动 trace JSON 和 `scripts/audio_check.sh`，加一条不必两边各改。
- **`trace` 只服务 `trace` 子命令**：场景采集与三层 PCM 导出已经迁入 `macindecode-ac4-scene::Session` 的逐 AU 事务边界，不得重新依赖整文件 trace 的收尾判据；feature trace 可以执行 full/wet DSP，但只累计 observation，不留存 PCM。

## workspace 级 lint 决定了代码长相

`unsafe_code = "forbid"`；`indexing_slicing` 与 `arithmetic_side_effects` 是 warn，而 CI 用 `-D warnings`。所以生产路径里的下标与算术**要么 checked，要么带 `reason` 的局部 allow**——规范伪码的下标算式贴着原文写时会大量触发，各模块顶部那些 `#![allow(...)]` 就是这么来的。代价见下面「放宽的 lint 会盖住真实缺陷」那一条。

## 判据的做法

这个仓库用**注入实验**验证判据，不靠"测试通过了"。新增或修改判据时按三步走：

1. **缺陷注入**——故意破坏被验的逻辑，判据必须失败。不失败就说明它什么也没测。
2. **等价变体**——换一种同样正确的写法，判据必须保持沉默。响了就说明它锁死的是实现细节而非行为。**判定「等价」时用到的前提，本身必须有判据保证**：「噪声边界比较写成 `>=` 是等价的」依赖边界严格递增，而实现原本只是假定它成立。
3. **不注入的基线**——什么都不破坏时判据必须**通过**。只验「注入 → 失败」的一侧，那个失败可能来自完全无关的原因，判据看着灵敏其实恒响。

动手注入之前先确认它真的改变行为：模 2 下恒等的 `−2`、值相等时删哪一个，两次都被当成缺陷注入，实际是等价变体，判据沉默才是对的。

已经踩过并留下痕迹的陷阱：

- **自证循环**：判据引用实现自己的常量（`PAN_OFFSET`、`TERMS`、`EPSILON_INV`），改实现时判据跟着改，永远成立。用字面量另写一份。**已经踩过三次**，每次都是在别处刚做完自查的情况下漏掉的——自查要单独问一遍「期望值从哪里来」。
- **测试信号的对称性抵消缺陷**：`z = i^t` 在偶数时隙上让每个协方差元素都退化成实数，于是「漏掉共轭」这类缺陷被整体消掉，三条判据同时失效。判据本身没写错，是输入让缺陷不可观察。比判据写弱更难发现。
- **为触发 A 而挑的极端输入会屏蔽 B**：上一条的常见来源。为了让限幅咬住，把该子带的 `est` 与 `scf_noise` 都取成零——限幅确实咬住了，但 `est · gain²` 与噪声下调这两项同时恒为零，「boost 用未限幅的增益」「用未限幅的噪声」两类注入一条判据都不响。挑输入去触发某个分支时，回头看一遍这组取值把哪些项抹平了。
- **判据没连到被测对象**：把公式在测试里抄一遍再核对解析值，只证明抄对了。确认断言路径真的调用了被测函数。
- **防御互相掩护**：三层检查中任一层失效都被另外两层兜住，单点注入全部无效。需要专门构造能穿透其余各层的输入。
- **新加的检查让旧判据恒成立**：给核对脚本加了第二个摘要后，测试桩只隔离第一个，于是空扫描无论如何都返回失败——三条「注入故障 → 返回 1」的 fail-closed 判据同时失去鉴别力，删掉脚本里的检查它们照样全过。这是上一条的反向：不是防御互相兜底，而是新防御把旧判据的前置条件永久满足了。加检查时要回头跑一遍「不注入的基线」。
- **空断言**：对初始状态恒真的断言（`out` 本来就是空的，断言 `out.len() == 0`），或者哨兵落在被测函数根本不会写的位置（HF 生成只写高带，哨兵却放在低带）。哨兵要放进函数真正会改的范围，再整体快照比对。
- **判据挂在可选输入后面**：测试在向量缺失时 `return`，CI 上等于没跑。不依赖媒体的路径要单独构造夹具。
- **散文与断言互相矛盾却双双通过审阅**：注释写「跨区间走两格，下一区间首项 k+2」，同一测试十行之下断言的是 `k+1`。断言是对的——它会跑；散文没有任何东西会验，审阅时人读散文、机器跑断言，两条路径从不交汇。这里散文承担的是判读依据，它错了等于规范判读记录错了，而且那句话当时已经同步复制进 `ROADMAP` 与 `SPEC_TRACEABILITY`，还被放大成「音调与噪声两节的一处差别」——噪声那边同样是接续，差别是凭空造的。**散文里出现具体数值或关系时，回头找同文件的断言核对一遍。**
- **夹具让判据成立的前提没被钉住**：「末项 k 接到首项 k+1」只在首个时隙的区间内偏移 `atsg_sig[0]·(num_ts_in_ats − 1)` 归零时成立，而起点是夹具推导出来的、调用处看不见。这类前提不会静默失效（违反时断言会响），但报出的失败会指向判读本身，读起来像是那条规范读法错了。**前提要么进前置断言，要么在注释里连同它的公式一起写明**，别只写结论。
- **放宽的 lint 会盖住真实缺陷**：模块顶部 `#![allow(clippy::arithmetic_side_effects)]` 是为了让规范的下标算式贴近原文，代价是它同时吞掉了一处真实的加法溢出。allow 的范围越大，越要自己补回被盖住的那类检查。
- **借来的枚举范围不等于穷尽**：判定「这类注入不可能被观察到」时扫了 904 组配置，得出「是冗余的纵深防御」。但那个 sweep 是为 patch/limiter 表建的，`AspxBandTables::derive` 的五个参数里它只跑四个——`aspx_noise_sbg` 被固定为 0，**恰恰因为它不影响那两张表**，而这正是它能造出「patch 相同、频带不同」反例的原因。丢掉它的理由和它相关的理由是同一条。**声称穷尽某个空间之前，核对枚举的自由度是不是被判定对象的自由度**，别复用为另一个问题划的边界。
- **按符号的拼写检索，而不是按它约束的东西**：`5_X`/`7_X` 在两份 PDF 里只出现在两行 `if` 里，据此写下「规范没有任何一处给出定义，取值是判读」。但 P1 表 67 的 `basic_metadata()` 把同一段语法写成 `if (5 ≤ ch_mode ≤ 10)`，连内层 5–6 / 9–10 的拆分都在——它根本不提那个符号，全文检索原理上就找不到。规范会用等价条款重述同一约束，**要查的是「哪些条款约束这个字段」，不是「哪些页出现这个词」**。这是上一条的检索版本：枚举的自由度（含某字符串的页）又一次不是判定对象的自由度（约束该字段的条款）。

一条错的「已验证穷尽」比没有结论坏得多：上面那次不只是没找到，还把结论写进函数文档与追踪矩阵（「注入它不会有判据响，这不是判据缺口」），等于留下路标叫后来者别查。没有结论时下一个人还会去看，有了假结论他就绕开了。**不确定就写不确定**，不要为省事把「我没找到」写成「不存在」。

错误类型也是接口：报错的**原因**不对（把长度失配报成越界、把噪声域报成信号域）等于诊断在说谎，和值算错同级。

**跨实现对照只验证读法，不验证读法之外的自洽性。** 用独立的 Python 参考复算伪码，两边一致只说明两份代码对同一段伪码理解相同；伪码本身与规范其余条款矛盾时，一致的两份实现会一起错。限幅器表删掉 `sbz` 就是这样：对照全绿，而它与「子带组表包含最高组上边界」的通则冲突。

## 测试向量与规范

编码媒体不进版本控制，两种缺失策略并存，不要搞混：ADM/DAMF 集成测试缺向量时 **skip**（可用 `MACINAC4_PROBE_AXES_VECTOR` 显式指定，显式路径不存在则立即失败）；`scripts/decode_check.py` 缺任一媒体即 **失败**。因此数值基线只能在素材齐全的本机跑，CI 只测这个脚本的 fail-closed、分段隔离与原子更新逻辑——绿色 CI 不代表 PCM 没变。

向量生产链的本机工具路径来自未入库的 `.env.local`（照 `.env.local.example` 抄）。三条链由 `--profile` 选：`build_vector.sh` 取单个 `default|dme_ac4|dee_ims|all`，`check_tools.sh` 还可用 `+` 组合。**省略时只走 `default`**，另两条不会顺带跑到。

`spec/` 只有 `MANIFEST.json` 入版本控制，PDF 与 C 表由 `scripts/fetch_specs.py` 按 `member_sha256` 校验释出。哈希不匹配一律作为错误，不得直接把新哈希写进清单。

条款引用格式 `TS103190-1:v1.4.1:<clause>` / `TS103190-2:v1.3.1:<clause>`。**核对之前写"待录入"，不得凭记忆填条款号。**

## 决策记录

- ADR-0001 语言与项目边界
- ADR-0002 标量重建的数值格式
- ADR-0003 变换三角常量的来源（含依赖边界）
- ADR-0004 混合基 Stockham IFFT
- ADR-0005 目标侧的实数函数
- ADR-0006 Core PCM/QMF 增益边界
- ADR-0007 预处理前 Scene Rust API 边界

## 当前边界

M1–M4 与 M6 已完成。链路从 `ac4_substream()` 经 ASF、IMDCT、表 188 对齐与 A-SPX 到 `Q_out,ASPX`，再经 A-JOC 参数重建、wet/去相关、LFE 插回和对象终端 QMF 合成；12 条基线媒体共 1 140 帧，core 与 A-SPX 两份既有逐位基线保持不变，新增对象基线单独冻结。

`export-objects-pcm` 已公开 full 对象音频；`export-full-adm-bwf` 通过同一 Scene Session
batch 把全部 full 对象、LFE 与 full OAMD 配成真实 ADM。
它默认写 BW64/九位时钟；Logic 模式写 RF64/五位时钟和按实际总轨数生成的 dbmd，两者 PCM
必须逐字节一致。LFE 按来源标签定位后只进 7.1.2 bed 第 4 轨，不能从其可变输出位置反推。
`export-full-damf` 复用同一配对门禁和 S24LE writer，写真实 DAMF 三件套；home 对应
0.5.1，3DoF 对应 0.6.0，类型只允许改变 manifest 的版本/type，metadata、CAF 与
OAMD-derived `headTrackMode` 必须相同。旧 `export-damf` 仍是粉红噪声试听探针，三件套
字节不得因共用 writer 重构而改变。
这些出口以及 core/A-SPX/诊断适配都已经过 `Ac4SceneFrame`；CLI 旧 collection、三层 PCM
sink、parser workspace 与第二套 DSP 已删除。M5 仍不因 direct-object 未验证路径而宣称完整；
公共 C ABI（M7）尚未开始。
真实音频出口现在包括三层 WAVE，以及受限子集的
`export-full-adm-bwf`、`export-full-damf`、`export-core-caf`。

未覆盖路径一律 **fail-closed**，不静默降级：`b_static_dmx = 1`、channel-based、direct-object/mixed、SIMPLE、1 024 及以下短帧、活动 companding、FIC/TIC 交织、活动 core DE 全部显式拒绝。README 的支持矩阵是这份清单的权威版本，写着「未观察到」的行只描述当前工具与素材，不等于规范不支持。

详见 `docs/ROADMAP.md` 与 `docs/SPEC_TRACEABILITY.md`。
