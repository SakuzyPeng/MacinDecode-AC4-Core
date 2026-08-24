# 规范可追踪性

## 1. 规范基线

实现基线为 `TS103190:2025-07`，已锁定：

| 文档 | 版本 | 发布 | 页数 |
|---|---|---|---|
| ETSI TS 103 190-1 Channel based coding | V1.4.1 | 2025-07 | 318 |
| ETSI TS 103 190-2 Immersive and personalized audio | V1.3.1 | 2025-07 | 254 |

截至锁定时点，这两版是各自的最新版本。

规范文件受 ETSI 版权保护，不进入版本控制。URL、发布日期与 SHA-256 记录在 `spec/MANIFEST.json`，由脚本获取与校验：

```bash
./scripts/fetch_specs.py            # 缺失则下载，随后校验全部哈希
./scripts/fetch_specs.py --verify   # 只校验本地文件，不访问网络
```

哈希不匹配一律作为错误处理。ETSI 可能就地更新已发布文件，此时必须人工核对差异，而不是直接把新哈希写入清单。后续升级规范版本必须通过 ADR，并明确列出行为差异和需要重跑的测试集合。

规范随附的 symbolic C tables（`ts_103190_tables.c`、`ts_103190_tables_part2.c`）随附件一并锁定哈希，可作为表格核对来源，但不视为完整参考解码器。

## 2. 引用格式

代码、设计说明和测试采用统一引用格式：

```text
TS103190-1:v1.4.1:<clause/table/equation>
TS103190-2:v1.3.1:<clause/table/equation/annex>
```

示例占位：

```text
TS103190-2:v1.3.1:clause <待录入>
```

在条款尚未核对前必须保留“待录入”，不得凭记忆填写条款编号。

## 3. 追踪矩阵

以下矩阵随实现推进逐条补充精确条款：

表中的 `macindecode-ac4-audio-core`、`macindecode-ac4-ajoc` 与 `macindecode-ac4-oamd` 仍是目标职责边界，并非当前已存在的 crate；`macindecode-ac4-scene` 已建立首版数据模型，当前量化音频语法与 OAMD 实现仍集中在 `macindecode-ac4-bitstream`。实际结构见架构设计第 2 节。

| 能力 | 规范部分 | 目标模块 | 主要验证 |
|---|---|---|---|
| sync frame 与帧长度 | Part 1 | `macindecode-ac4-bitstream` | 合法/截断/损坏帧语料 |
| TOC 与序列配置 | Part 1/2 | `macindecode-ac4-bitstream` | trace 与独立 inspection 对比 |
| presentation/group/substream | P2 `6.2.1.3`–`6.2.1.14`；P1 `4.2.3.3`–`4.2.3.5`、`4.2.3.7`、`4.2.3.11`、`4.2.14.15` | `macindecode-ac4-bitstream` | `dac4` 与 TOC 一致性；`payload_base`/尺寸自洽；引用精确覆盖索引表 |
| 音频 substream 框架与 metadata | P2 `6.2.2.2`；P1 `4.2.14.1`–`4.2.14.4`、`4.3.4.1`；`sus_ver` 语义 P2 `6.3.2.5.4` | `macindecode-ac4-bitstream::audio_substream` | 解析后恰好落在 substream 末尾 |
| Huffman 码本 | P1 附录 `A.0`–`A.5`；P2 附录 `A` | `macindecode-ac4-bitstream::huffman` | 构建期哈希 + Kraft + 前缀无关；逐符号往返 |
| ASF 表格与派生量 | P1 附录 `B`、表 `A.2`–`A.15`；`4.3.6.1`–`4.3.6.2`（表 99–110） | `macindecode-ac4-bitstream::asf::tables` | 表内自洽约束；`check_sfb_tables.py` 反向核对 PDF |
| ASF 成帧与窗口分组（44,1/48 kHz） | P1 `4.2.8.1`–`4.2.8.2`（表 37、38）；`4.3.6` `Pseudocode 2`–`5` | `macindecode-ac4-bitstream::asf::framing` | 16 种半帧组合的窗口恰好铺满一帧；`num_windows` 落在 `4.3.6.2.6` 的取值集合内；高采样率显式拒绝 |
| ASF 熵编码谱数据 | P1 `4.2.8.3`–`4.2.8.6`（表 39–42）；`4.3.6.3`–`4.3.6.6`；`5.1.2`（`Pseudocode 19`、`20`） | `macindecode-ac4-bitstream::asf::spectrum` | 基数分解对 1 241 个符号可逆；标度因子与噪声填充条件互补；手工构造帧落点精确 |
| 声道元素与立体声侧信息 | P1 `4.2.6.2`、`4.2.6.7`–`4.2.6.8`、`4.2.6.11`、`4.2.7.1`–`4.2.7.2`、`4.2.10`–`4.2.11`；语义 `4.3.5`、`4.3.8`–`4.3.9`（表 94、114） | `macindecode-ac4-bitstream::channel` | 各元素落点与构造长度相等；SSF 与共享/独立 `sf_info` 分支由注入实验区分 |
| MDCT 立体声与三声道矩阵 | P1 `5.3.1`–`5.3.2`（`Pseudocode 59`）、`5.3.3.2`–`5.3.3.3`（表 178） | `macindecode-ac4-bitstream::channel`、`macindecode-ac4-bitstream::full_ajoc::asf` | mode 0/选择性 M/S/全带 M/S/SAP 构造谱线逐带验算；表 178 十二个选择码逐项核对，保留值 fail-closed；真实 768K 的 SAF/Apple APAC 方向左右差由 `+3.419 dB` 收敛至 `+0.300 dB`，DRP 参照为 `+0.469 dB` |
| PCM/QMF 控制帧对齐 | P1 `5.6`（表 188）、`5.7.2` | `macindecode-ac4-bitstream::frame_alignment`、`macindecode-ac4-bitstream::full_ajoc` | 八档 `d_pcm`/`d_ctrl` 逐行核对；连续 PCM 环形延迟、1/2/4 帧控制 FIFO 与 reset；九条 A-SPX 逐位基线；真实 768K 节目的 14 kHz 上下能量包络残差由 1 664 samples 收敛为 0（64-sample 分辨率） |
| A-SPX 静态表与时隙换算 | P1 `5.7.6.3.1.1`（模板表、表 190–191）、`5.7.3.2`（表 189）、`5.7.6.3.3`（表 192、`Pseudocode 75a`）；表 126、`Pseudocode 79` | `macindecode-ac4-bitstream::aspx::tables` | 表 190/191 反查模板表偶数下标；表 189 与 `frame_length/64` 互证；`check_aspx_tables.py` 反向核对 PDF |
| A-SPX 子带组表推导 | P1 `5.7.6.3.1.1`–`5.7.6.3.1.3`（`Pseudocode 67`–`70`） | `macindecode-ac4-bitstream::aspx::bands` | 全部合法配置下派生边界均取自主表且严格递增；`num_sbg_noise` 的整数判据与浮点定义逐点一致 |
| A-SPX 时频成帧 | P1 `5.7.6.3.3.1`（表 193、194，`Pseudocode 76`–`77`）；表 128 | `macindecode-ac4-bitstream::aspx::frames` | 表 194 五行恰好覆盖 `num_aspx_timeslots` 的值域；噪声边界是信号边界的子集；跨度阈值的整数判据与浮点定义一致 |
| A-SPX 语法元素 | P1 `4.2.12`（表 50–58）；语义 `4.3.10`（表 122–136）；`Pseudocode 79` | `macindecode-ac4-bitstream::aspx::syntax` | 各元素落点与构造长度相等；`aspx_balance`、`aspx_tic_copy`、逐包络分辨率三处分支由注入实验区分 |
| A-SPX 码本选择 | P1 表 `A.16`–`A.33`；`Pseudocode 79`；`4.3.10.8.3` | `macindecode-ac4-bitstream::aspx::codebooks` | 十八个标识映射到互不相同的码本；`cb_off` 由码本长度推出并与规范标注比对 |
| `var_channel_element` 组装 | P2 `6.2.4.4`；语义 `6.3.5.5`–`6.3.5.6`（表 77） | `macindecode-ac4-bitstream::var_element` | 三条分支的落点与构造长度相等；逐 A-SPX 元素的交叉偏移跨帧各自沿用 |
| A-JOC 数据与码本 | P2 `6.2.5`（表 78–80，`Pseudocode 27`）；附录 `A.1.1` | `macindecode-ac4-bitstream::ajoc` | 稀疏/非稀疏与时间/频带方向的落点相等；十二个标识按码本名比对；`cb_off` 与附录标注一致 |
| A-JOC 对话增强与床信息 | P2 `6.2.3.5`–`6.2.3.6`；语义 `6.3.6.6`–`6.3.6.7`（表 82，`Pseudocode 28`） | `macindecode-ac4-bitstream::ajoc::de` | 表 82 十六个取值逐一往返且前缀无关；`num_dlg_obj` 跨帧沿用后落点仍相等；I 帧上缺席的配置读作空配置 |
| A-JOC 参数频带映射 | P2 `5.7.3.1`（表 28）；P1 `5.7.7.2`（表 197） | `macindecode-ac4-bitstream::ajoc::bands` | 本地生成的八列单调非降、自 0 起、逐频带满射、末值为 `num_bands − 1`；`check_ajoc_tables.py` 从两份 PDF 逐格反查，并锁定 15/12/9/7 四列与表 197 相同 |
| `audio_data_ajoc` 组装 | P2 `6.2.3.4`、`6.2.8.3`；语义 `6.3.4.7`、`6.3.9.3.6` | `macindecode-ac4-bitstream::audio_data` | 全链路落点与构造长度相等；core/full timing 独立、derive 分支、I 帧非零块数、LFE 信号顺序与事务状态均有定向断言；解出的块自带对象与块下标，可直接喂给 `OamdState` |
| 编解码帧长与帧率因子 | P1 `4.3.3.2.5`–`4.3.3.2.6`（表 82–84）、`4.3.3.5`（表 87） | `macindecode-ac4-bitstream::toc` | 表 83 十四行逐条核对；表 87 允许的每个因子，商都落回表 83 的取值集合 |
| `ac4_substream` 与音频数据接合 | P2 `6.2.2.2`、`6.2.3.4`；P1 `4.2.4.2`、`4.3.4.1`、`4.3.3.7.8` | `macindecode-ac4-bitstream::substream_audio` | `audio_data_ajoc()` 在 `audio_size` 声明的区段内走完，越界报错且越界位置即区段末尾；八条实测流 568 帧的 `fill_bits` 全部小于 8 |
| ASF 量化重建、缩放与解组 | P1 `5.1.3`（`Pseudocode 21`）、`5.1.5`（`Pseudocode 25`）；码本 `A.1` | `macindecode-ac4-bitstream::asf::{reconstruct, dequant}` | 手工验算并跨窗口组延续的 DPCM 链；缺失差值显式失败；标度因子落在 `5.1.3.2` 规定的 `0…255`，八条流 41 693 个频带零越界；反量化表与增益常量由整数判据正确舍入，完全立方数恰为整数，八条流 99 万条谱线零非有限值；布局按 `AsfLayoutKey` 核对，标度因子再与当前工作区精确比对；解组的双射性由下标编码验证，实测 11 030 528 条谱线非零数零失配、能量漂移 `4.2×10⁻¹⁶` |
| IMDCT 块切换、窗口与 IFFT | P1 `5.5.2.2`（`Pseudocode 60`–`64`）、`5.5.3`（表 186、187） | `macindecode-ac4-bitstream::asf::imdct` | 窗口三段和精确等于块长，十五档全部有序对覆盖；生产 Stockham IFFT 在十五档长度上与定义式差分，固定 16 KiB scratch，根表摘要及轴点/共轭/象限换位/单位圆判据闭合。前/后旋转与 KBD 生产表已接入，另由切分、角度、Princen-Bradley 与右窗镜像判据覆盖；完整 IMDCT 由分析—合成完美重建、未加窗后半与定义式差分、`N_full` 延迟等价、`5.5.3` 文字示例差分与混合块长延迟恒定五条判据闭合 |
| 音频核心工具 | Part 1 | `macindecode-ac4-audio-core` | 标量单元测试和频域诊断 |
| direct-object | Part 2 | `macindecode-ac4-scene` | 自生成对象母版 |
| A-JOC 数据与重建 | Part 2 | `macindecode-ac4-ajoc` | PRBS、相关性、串扰和轨迹 |
| OAMD | P2 `6.2.2.4`、`6.2.8.1`–`6.2.8.12`、`6.2.9.9`–`6.2.9.10`；语义 `6.3.9`；位置映射 `4.8.3.4.2` 表 7 | `macindecode-ac4-bitstream::oamd` | `byte_align` 残余 < 8；common/timing/additional 跨帧复用；整帧提交或整帧回滚；`trajectory_check.py` 逐轴比对母版轨迹 |
| OAMD → DAMF 试听探针 | P2 `6.3.9` 语义；DAMF 0.5.1 | `macindecode-ac4-cli::{trace,damf}` | MP4 edit/priming 与裸流时间线；48 kHz/24-bit CAF；确定性对象粉红噪声；三件套经本地 ADM 规范化工具接受 |
| full A-JOC → DAMF | P2 `4.8.3.1`、`4.8.3.4.2`、`5.7.2.3`、`6.3.2.8.1`、`6.3.9`；DAMF 0.5.1/home、0.6.0/3DoF | `macindecode-ac4-cli::{scene_export,damf}` | 单趟 full PCM/OAMD 配对；48 kHz/24-bit S24LE CAF 与 full ADM `data` 逐字节一致；home/3DoF 只改 manifest version/type，OAMD-derived `headTrackMode` 不变；见 §5.55 |
| OAMD → ADM BWF 试听探针 | P2 `6.3.9` 语义；ITU-R BS.2076-2、BS.2088-2；EBU Tech 3285 Supplement 6；Dolby Atmos Master ADM Profile v1.0 | `macindecode-ac4-cli::{trace,adm}` | 标准配置强制 `BW64`，Logic 配置强制 `RF64`、五位时钟及逐段校验 `dbmd`；两者首块均为 `ds64`；`chna`/`axml` 图一致性；48 kHz/24-bit PCM；MP4/raw 时间线与无半成品；EBU EAR 解析和 0+5+0 渲染接受 |
| full/core decode | Part 2 | `macindecode-ac4-scene` | 同一码流的模式差异 |
| 渲染前输出边界 | Part 2/附录，待核对 | `macindecode-ac4-scene` | `Ac4SceneFrame` 契约测试 |
| MP4 `dac4` | ETSI/ISO 封装规范，待锁定 | `macindecode-ac4-mp4` | Bento4/MediaInfo 差分 |

## 4. 实现要求

每个规范驱动模块至少应记录：

- 对应规范版本和精确条款。
- 输入字段及其允许范围。
- 状态是否跨帧延续。
- 规范中的默认值和错误条件。
- 使用的表格、常量和量化公式来源。
- 数值格式、舍入和饱和行为。
- 关联测试案例 ID。

复杂公式应在函数文档中说明变量映射，但不得大段复制规范文本。

## 5. 表格导入

规范 PDF 表与随附 C tables 的处理原则：

1. 保存原附件 hash 和版本信息，不直接修改原文件。附件与其内部成员的 SHA-256 记录在 `spec/MANIFEST.json`。
2. 使用版本锁定的 Python 依赖与可重复脚本生成 Rust `const` 数据。
3. 生成器验证维度、元素数量、类型范围与来源 hash。
4. Rust 侧对生成结果做结构性测试，而非只比对首尾值。
5. PDF 表先写入被忽略的 `spec/generated/`，构建脚本校验摘要后复制到 `OUT_DIR`；C 表直接由构建脚本解析。输入与生成结果都不进入版本控制或 crate 包。

统一准备流程是：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
./scripts/check_spec_distribution.py --generated
```

`spec-tables` 只消费 PDF 生成表；`audio-decode` 自动包含它，并继续消费随附 C 表。
生成物摘要记录在 `spec/MANIFEST.json`，同时编译进 crate 的 `spec_lock.rs`，使从
crates.io 解包构建时不必把清单本身放进外部目录。

### 5.1 Huffman 码本（`crates/macindecode-ac4-bitstream/build_support/spec.rs`）

附录 `A.0` 规定全部 Huffman 码本以随附 zip 给出，PDF 正文只列码本名称与长度，因此码本数值完全不经人工转写。

流程是 `scripts/fetch_specs.py` 校验 zip 成员哈希后释出 `.c` 到 `spec/`，显式启用 `audio-decode` feature 时构建脚本再解析它：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py                                            # 释出并校验 C 表
./scripts/generate_spec_tables.py                                   # 从官方 PDF 生成本地 Rust 表
cargo test -p macindecode-ac4-bitstream --features audio-decode
```

**默认构建与 crate 包不依赖这些未分发文件。** 两个 feature 默认关闭，是为了不把 ETSI 的分发限制传染给整个工作区——移走 `spec/` 下的本地文件后 `cargo test --workspace` 仍照常通过。构建脚本只依赖锁定的 `libm`；不会联网、启动 Python 或解压文件。它会对**实际读入的字节**重算 SHA-256 并与 crate 内置摘要比对，版本不匹配立即失败。

构建期校验，任一不成立即中止构建。三类判据的**作用域各不相同**，不可互相替代：

| 判据 | 防的是 |
|---|---|
| 读入字节的 SHA-256 与清单一致 | 输入文件被就地改动或替换 |
| `_LEN` 与 `_CW` 等长；Kraft 等式 `Σ 2^-len == 1`；码字前缀无关且不越界；内部节点数恒为符号数减一 | 码本自身不是完备前缀码 |
| `_LEN` / `_CW` 双向配对；码本总数为 84、符号总数为 4 917 | 手写解析器静默漏表；规范基线变更 |

五类注入实验确认这些断言会触发，而非摆设：

| 注入 | 触发的判据 |
|---|---|
| 改一个码长（17 → 16） | Kraft 等式 |
| 改一个码字，码长不变 | 前缀冲突 |
| 只在注释里插一个空格 | SHA-256 |
| 解析器不再识别 `int32`（`_CW` 的类型） | `_LEN` 缺少对应 `_CW` |
| 解析器静默丢弃一张码本 | 码本总数 84 |

第三行说明哈希不冗余：两项结构检查都不动，且结构检查只覆盖 Huffman 码本，管不到后续要导入的数值表（`QWIN`、`CDF_TABLE`、`ASPX_NOISE` 等）。

后两行说明总数与配对断言也不冗余，但要注意它们的**实际作用域**：哈希校验排在解析之前，因此输入损坏永远先被哈希拦下，这两条断言只可能因解析器回归或基线变更而触发。解析器是手写的，恰是此处最脆的一环。

前缀无关同时**判定了比特序**：把码字按位反转（LSB 优先）解释时，84 张码本无一保持前缀无关。因此 MSB 优先是数据得出的结论，不是沿用惯例的假设。

Rust 侧逐符号走完全部 84 张码本共 4 917 个符号，核对解出的下标与消耗的比特数。该用例不校验表值——表值由上述哈希与结构断言保证——它校验的是解码器与构造侧对比特序、叶子编码的理解一致。

### 5.2 ASF 尺度因子频带表（`crates/macindecode-ac4-bitstream/src/asf/tables.rs`）

附录 B 与附录 A 的表 A.2–A.15 **只存在于规范正文**，没有随附的机器可读版本。`scripts/generate_spec_tables.py` 从用户本地的官方 PDF 确定性抽取这些值，写入被忽略的生成文件；仓库与 crate 包不再保存转录副本。该路径由 `spec-tables` 启用。

**自动测试（`asf/tables.rs` 内，消费已校验的本地生成文件）：**

- 首项为 0，严格递增，末项等于变换长度；
- 项数等于表 B.1 的 `num_sfb` 加一；
- 频带宽度单调不减，**末带除外**——变换长度非 2 的幂时末带被截短，故排除项本身也是判据：非末带处出现下降即表有错；
- 同一张附录 B 表内各列共享公共前缀，只有末项不同；
- `num_sfb < 2^n_msfb_bits`，即表 106 与表 B.1 互相约束；
- 表 109 关于对角线对称，且每一项等于两个半帧窗口数之和减去分组基数——与 `Pseudocode 3` 是同一事实的两种独立表述；
- 码本元数据满足 `cb_mod^CB_DIM == codebook_length`，有符号码本区间对称、无符号码本自 0 起算。

**外部审核（`scripts/check_sfb_tables.py`）：** 从 PDF 按字形 x 坐标重新抽取附录 B 的 `num_sfb` 与 538 个 `sfb_offset`，并与本地生成文件逐值比对。改用坐标而非 `pdftotext -layout`，是因为后者在这张表上有根本性歧义——`47 896` 既可能是「sfb 47，值 896」也可能是千位分隔的 `47896`，纯文本无法区分。表 99–110、A.2–A.15 与表 186 则由统一生成器直接解析并按形状及交叉约束校验。

两者的**作用域必须分清**。注入实验：把 `sfb_offset` 中间一项由 600 改为 601，并在共享该前缀的三列上同步修改。结果是自动测试**全部通过**——严格递增、末项、项数、带宽单调、列间前缀一致性全都不受影响——而 PDF 核对三处全部抓出，并定位到「第 40 项 PDF 600 vs Rust 601」。

结论：自洽性再强也排除不了整列被系统性抽错，PDF 反向核对不可省。核对依赖 `spec/` 与锁定版本的 pdfplumber，两者缺一默认报错；只有显式传入 `--allow-missing` 才跳过，因此它也不能取代 Rust 结构测试。

一处已由测试纠正的误读：表 B.7 排成六列，最初被当作一个六列组，前缀一致性用例立即报错。实际它是两组独立的三列（`256/240/192` 与 `128/120/96`），由表头三行的采样率组合区分。

### 5.3 A-SPX 模板子带组表（`crates/macindecode-ac4-bitstream/src/aspx/tables.rs`）

`5.7.6.3.1.1` 的两张模板表同样只存在于规范正文，共 44 个边界值；它们与表 189/192、表 194 一起由统一生成器写入本地生成文件。

**自动测试（`aspx/tables.rs` 内）：**

- 两张模板表严格递增；
- 边界数等于组数加一——该式由 `Pseudocode 67` 在 `aspx_stop_freq == 0` 时的最大索引 `2 * aspx_start_freq + num_sbg_master` 化简得到，故模板表长度写错会被立即发现；
- 表 189 的 `num_qmf_timeslots` 与 `frame_length / 64` 互证。

**外部审核（`scripts/check_aspx_tables.py`）：** 从 PDF 抽取两张模板表、表 190/191 的 24 个起止子带、表 189 与表 192 各 8 行、表 194 的 50 个边界，与本地生成文件逐值比对。表 193 的 `noise_mid_border` 是文字表达式而非数值表，不在生成范围内，只由单元测试按表中四种情形逐一断言。

**作用域**必须分清，且这次的边界比 §5.2 更锐利：`Pseudocode 67` 以 `2 * aspx_start_freq` 索引模板表，因此表 190/191 **只覆盖得到偶数下标**。注入实验：

| 注入 | 单元测试 | PDF 核对 |
|---|---|---|
| 高分辨率模板的内部奇数下标改变但仍严格递增 | **通过** | 抓出并定位到具体项 |
| 高分辨率模板的内部偶数下标改变但仍严格递增 | **通过** | 抓出并定位到具体项 |

模板表高段步长为 3（`44,47,50,53,56,59,62`），奇数下标项改动仍可保持严格递增，因而绕过全部自洽约束。低段步长为 1，任何单项改动都会破坏递增——所以盲区仅限高段的奇数下标。

### 5.4 A-SPX 噪声子带组数的整数判据（`crates/macindecode-ac4-bitstream/src/aspx/bands.rs`）

`Pseudocode 70` 用浮点对数定义 `num_sbg_noise = max(1, floor(aspx_noise_sbg * log2(sbz/sbx) + 0.5))`。本 crate 为 `no_std` 且不引入数学库，故改用等价的整数判据：结果为 `k` 当且仅当 `k` 是使

```
2 * sbz^(2n) >= 4^k * sbx^(2n)
```

成立的最大整数（`n = aspx_noise_sbg`）。推导是把 `n * log2(sbz/sbx) >= k - 0.5` 两边乘 2 后取 2 的幂，同时消去半整数与对数；`n ≤ 3`、子带号 `≤ 62`，故 `62^6` 的中间量在 `u64` 内。

该变形是**精确的**，不是定点近似。测试对 `n ∈ [0,3]` 与全部 `1 ≤ sbx < sbz ≤ 62` 共 7 000 余组逐点比对 `f64` 定义；浮点只出现在测试里。注入实验：去掉左侧的 `* 2`（即把半整数舍入退化为向下取整），该用例失败。

`Pseudocode 77` 的 `case 2` 同样含浮点：`span > num_aspx_timeslots / 6.0 + 3.25`。两边乘 12 得 `12 * span > 2 * num_aspx_timeslots + 39`，右端恒为整数。测试对表 194 的五个时隙数与 0–24 的全部跨度逐点比对浮点定义。

### 5.5 A-SPX 时频成帧（`crates/macindecode-ac4-bitstream/src/aspx/frames.rs`）

表 194 `tab_border` 的 50 个边界值同样只存在于规范正文。时隙数 6 的四分割 `{0,2,3,4,6}` **不是均分**（跨度 2、1、1、2），因此整表必须转录而非由公式生成。

**自动测试：**

- 五行的 `num_aspx_timeslots` 恰好覆盖 `num_aspx_timeslots()` 的全部取值——两张表本无关联，一个来自表 189/192 的换算，一个是 FIXFIX 的边界表，取值集合却完全重合，故互为转录判据；
- 每行每列首项为 0、末项为时隙数、严格递增、项数等于 `num_env + 1`；
- **噪声边界是信号边界的子集**——这条把 `num_env` 为 2 与 4 的两列绑在一起；
- 表 193 的四种情形（VARFIX／其余 × `aspx_tsg_ptr` 正负）逐一断言。

注入实验：把时隙 6 的四分割由 `{0,2,3,4,6}` 改成 `{0,2,4,5,6}`，它仍然严格递增、首尾正确、项数正确，但噪声边界 `{0,3,6}` 的中项 3 不再落在信号边界上，子集判据失败；PDF 核对同时抓出。

**为什么这一层属于解析而非重建：** `aspx_freq_res_mode` 取 2 时（表 124），每个包络的频率分辨率由该包络的时隙跨度决定，而分辨率决定 `aspx_ec_data()` 取 `num_sbg_sig_highres` 还是 `num_sbg_sig_lowres`（`Pseudocode 78`），从而改变 Huffman 解码次数。边界算错，整段熵编码数据错位。

**跨帧状态：** VARFIX 与 VARVAR 在非 I 帧用 `previous_stop_pos` 替代 `aspx_var_bord_left`，该变量初值为 `num_aspx_timeslots`。`aspx_config` 与 `aspx_xover_subband_offset` 同样只在 I 帧传输，非 I 帧沿用上一个 I 帧的值。

### 5.6 A-SPX 码本偏移（`crates/macindecode-ac4-bitstream/src/aspx/codebooks.rs`）

表 A.16–A.33 的十八张码本随 `build.rs` 生成，但 `cb_off` 只标在 PDF 的表头里，不在随附的 C 表内。**它没有进入实现**：十二张 DF／DT 表上 `cb_off == (codebook_length - 1) / 2` 恒成立，而 `codebook_length` 已由生成的 trie 给出，故直接推算。

支撑该推算的是另一条关系：`len_DF == len_DT == 2 * len_F0 - 1`，在六组（信号四组、噪声两组）上全部成立。F0 编绝对值 `[0, N)`，差值范围便是 `[-(N-1), N-1]` 共 `2N-1` 个取值，偏移自然是 `N-1`。两条关系都由单元测试在十八张表上验证，另有一条用例把 PDF 标注的六个 `cb_off` 作为第二来源逐一比对。

因此这一处**没有任何手工转录进入实现**，与 §5.1 的 Huffman 码本同类。

### 5.7 A-SPX 语法元素（`crates/macindecode-ac4-bitstream/src/aspx/syntax.rs`）

主判据是落点：每个元素解析后消耗的比特数必须等于构造长度。任一字段宽度或循环次数写错，落点立即偏移。

注入实验与它们暴露的**测试覆盖缺口**：

| 注入 | 首轮结果 | 补测后 |
|---|---|---|
| `aspx_balance` 为真时仍读第二份 `aspx_framing()` | 1 项失败 | — |
| 频率方向首项误用 `DF` 而非 `F0` 码本 | 4 项失败 | — |
| 表 126 的 VARFIX 与 VARVAR 码字互换 | 1 项失败 | — |
| 忽略 `atsg_freqres`，一律取 `num_sbg_sig_highres` | **全部通过** | 2 项失败 |
| `aspx_tic_copy` 为真时仍读 `aspx_tic_left`／`aspx_tic_right` | **全部通过** | 1 项失败 |
| `aspx_balance` 为真时仍读第二份 `aspx_tna_mode` | — | 2 项失败 |

后两组说明落点判据本身没有问题，缺的是**取值覆盖**：首轮全部用例都取 `aspx_freq_res_mode == 3`（恒高分辨率），高低两张表恰好不产生差异；而 `aspx_tic_present` 一律为 0，`aspx_hfgen_iwc_2ch()` 最复杂的那段从未进入。补测覆盖了 `freq_res_mode` 的 0 与 1、时间交织的六种标志组合、频率交织的四种组合。

`aspx_freq_res_mode` 取 0 时 FIXFIX 只传输一个 `aspx_freq_res` 并复制到全部包络，非 FIXFIX 则逐包络传输——这是表 53 两个分支的差异，也是 `frames` 与本层唯一的接合点。

### 5.8 `var_channel_element` 的状态粒度（`crates/macindecode-ac4-bitstream/src/var_element.rs`）

三处跨帧状态的**作用域各不相同**，混为一谈会在非 I 帧上错位：

| 状态 | 作用域 | 依据 |
|---|---|---|
| `aspx_config()` | 每个 `var_channel_element()` 一份 | P2 `6.2.4.4` 在元素层调用一次 |
| `aspx_xover_subband_offset` | 每个 `aspx_data_1ch/2ch` 各一份 | 表 51/52 在数据元素内部传输 |
| `previous_stop_pos` | 每个数据元素的每个声道各一份 | `5.7.6.3.3.1` 的 NOTE |

`aspx_config` 与 `aspx_xover_subband_offset` 只在 I 帧传输并更新；`previous_stop_pos` 则在每个 A-SPX 间隔结束后更新，供下一帧沿用。因此 `AspxState` 的粒度是**数据元素**而非帧，`aspx_config` 不在其中，由 `VarChannelState` 单独持有。

该粒度错误无法由单帧用例发现：I 帧每次都重读交叉偏移，共用一份状态与逐元素持有毫无差别。注入实验——把 `state.aspx.get_mut(aspx_used)` 改成 `get_mut(0)`——在只有 I 帧的用例集上**全部通过**；补上一个「I 帧写入三个不同偏移、非 I 帧沿用」的用例后才被抓出。偏移决定 `num_sbg_sig_highres`，符号数随之改变，落点立刻偏移。

**工作区由调用方提供。** 一个 `ChannelElement` 约 60 KiB，`n_fullband_dmx_signals_minus1` 占 4 位故最多 16 个全频带信号，最坏情形需要九个声道元素（十六信号取偶数分支得八个 `two_channel_data()` 加 LFE，十五信号取奇数分支得六加二加 LFE，同为九个）。合计约 540 KiB，放在栈上并不现实，故由调用方决定存储并跨帧复用。两组工作区的容量在读取任何比特前核对。

### 5.10 落点判据的四类盲区（`crates/macindecode-ac4-bitstream/src/audio_data.rs`）

`audio_data_ajoc()` 是本项目最长的一条解析链，五处注入里**四处首轮全部通过**。这批缺口比之前任何一层都集中，值得单列，因为它们各自代表落点判据的一类固有局限：

| 注入 | 首轮为何漏过 | 补测手段 |
|---|---|---|
| `dmx_active_signals_mask` 误用含 LFE 的对象数 | 原用例只断言「至少读了 4 位」，多读一位仍然成立 | 改为完整元素的精确落点 |
| 时间数据缺席时把块数当成 0 而非报错 | 该分支从未被任一用例走到 | 专门构造 `b_dmx_timing == 0` 且状态无历史 |
| 跨帧状态直接写而非用副本 | 只有「首帧就失败」的场景，此时无旧状态可毁 | 先跑一帧建立状态，再让次帧走到末尾才截断 |
| I 帧首块的 `b_no_delta` 恒为假 | 用例全用「对象不活动」的块，该标志此时不影响任何字段 | 改用活动对象，并断言 `basic_info_status` |

第一、二、三行是**覆盖**问题：判据本身有效，只是没有数据把分支或维度区分开。第四行不同——活动对象下两条路径**比特数恰好相同**（`b_no_delta` 为假时，`default_metadata` 那一位会被当作 reuse 标志读掉），落点判据对它天然无感，只能断言解析出的状态。

这与 §5.9 记录的码本映射是同一类：**落点等式约束的是长度，不是语义**。凡是「长度相同但含义不同」的分歧，都必须另找判据。

### 5.9 A-JOC 与 A-SPX 的码本差异（`crates/macindecode-ac4-bitstream/src/ajoc.rs`）

两处 Huffman 数据的语法几乎同构——`F0` 打头、`DF` 续频带、`DT` 走时间——但**码本参数的规律相反**，不可互相套用：

| | A-SPX（表 `A.16`–`A.33`） | A-JOC（附录 `A.1.1`） |
|---|---|---|
| `len_DF` | `2 * len_F0 - 1` | `len_F0` |
| `len_DT` | `2 * len_F0 - 1` | `2 * len_F0 - 1` |
| `cb_off(DF)` | `(len - 1) / 2` | **0** |
| `cb_off(DT)` | `(len - 1) / 2` | `(len - 1) / 2` |

因此 A-SPX 那套「差分码本一律取一半偏移」的推算若照搬到 A-JOC，`DF` 段会整体偏移。两侧各有一条按规范标注比对的用例锁住这一点。

另有两处取值方向与直觉相反，单列记录以免日后"顺手统一"：

- 表 79 `ajoc_quant_select`：**0 为 Fine，1 为 Coarse**；
- 表 78 `ajoc_num_bands_code`：**单调递减**，码值越大频带越少（0 → 23 band，7 → 1 band）。

**码本映射需要按名字比对，其余判据都挡不住。** 这是补写 A-JOC 时发现并回溯修补 A-SPX 的一条教训：

| 判据 | 为何挡不住整组错配 |
|---|---|
| 十二（十八）个标识互不相同 | 成对互换后仍然互不相同 |
| 落点等于构造长度 | 测试的构造侧与解析侧共用同一张映射表，映射写错时两边一起错 |
| 码本长度关系 | A-SPX 的 `DF` 与 `DT` 长度相同（如 141/141），互换后长度关系不变 |

注入实验：A-JOC 把 `COARSE` 与 `FINE` 的 `F0` 互换、A-SPX 把 `DF` 与 `DT` 互换，都只有比对 `ALL_CODEBOOKS` 里码本名的用例能抓出。该名字来自 `build.rs` 生成的代码，与手写的 `match` 分支互为独立来源。

### 5.11 `audio_size` 作为判据（`crates/macindecode-ac4-bitstream/src/substream_audio.rs`）

`ac4_substream()` 的音频区段结构是 `audio_data` + `fill_bits`(VAR) + `byte_align`，长度由 `audio_size` 声明（P1 `4.3.4.1`）。把读取器限制在该区段上之后：

- **多读**必然撞到区段边界报 `ReadError`；
- **少读**只表现为偏大的 `fill_bits`，规范对它不设上限，因此不构成错误。

判据是单向的，与 §5.10 的第四类盲区同源。只断言「解析失败」抓不到读取器范围写错——若读取器覆盖整个载荷，缺掉的比特会从 metadata 的开头补上，随后仍会在载荷末尾耗尽而报同一类错误。区分两者的是**越界位置**：用例断言 `bit_position + remaining_bits` 恰好等于 `audio_size * 8`。

实测的八条流共 568 帧，`fill_bits` 全部落在 `0…7`，即该编码链恒不写填充，落点精确到字节。`scripts/audio_check.sh` 把这一点当作门禁；它比规范要求强，一旦某条流的 `fill_bits` 达到 8 或以上，需要先分清那是编码器的填充还是本实现少读了字段。

**解析上下文只能从别处推出，各有一条独立的错法：**

| 参数 | 来源 | 错法 |
|---|---|---|
| `b_iframe` | `ac4_substream_info_ajoc()` 的 `b_audio_ndot`（`6.2.2.2` 的实参表） | 误取 TOC 的 `b_iframe_global` |
| `frame_len_base` | TOC 的 `fs_index` + `frame_rate_index`，再除以 `frame_rate_factor` | 忽略因子；或只比因子上界而放行表 87 没有的 3（1 536 恰好被 3 整除） |
| `frame_rate_fraction` | 引用 group 的 presentation | 把分散在 2/4 个传输帧的 codec frame 当作完整载荷；当前在重组前显式拒绝 |
| substream 采样率 | TOC 基础采样率 × A-JOC `sf_multiplier` | 忽略倍率，把 96/192 kHz 当作 48 kHz；当前只支持 44.1/48 kHz |
| `dmx_objects` / `umx_objects` | 分别取 `dmx_assignment` 与 `upmix_assignment` | 两侧共用上混分配 |

`frame_rate_factor` 大于 1 时一个 info 元素描述 2 或 4 个连续解码的 substream，各有自己的 `b_audio_ndot`（P1 `4.3.3.7.8`）。`crate::substream` 只保留它们的合取——那是随机访问点判定要的量，不是单个元素的 I 帧标志。本层因此显式拒绝该情形而不用合取值代替。`frame_rate_fraction` 大于 1 同样要求先完成跨传输帧重组。

trace 在解析前按 substream 下标合并所有 group 引用：同一物理载荷只解析一次，会改变语法的上下文不一致则整帧失败。`frames`/`parsed` 与 `substreams`/`parsed_substreams` 分别使用同一统计单位。任一 A-JOC 解析错误会清空对应 substream 历史；拓扑解析失败、来源变化、reset 或等待随机访问点则清空全部历史。

### 5.11a 逐对象动态数据的两个下标（`crates/macindecode-ac4-bitstream/src/audio_data.rs`）

`oamd_dyndata_single()` 按「对象在外、块在内」两层循环展开，写进调用方的定长工作区。若只写 `ObjectInfoBlock`，对象与块的归属就靠**填充顺序**这条隐式约定传递——而每个信息块的长度可变，任一侧的循环顺序改动都不会被落点判据发现（长度不变，语义变了，仍是 §5.10 的第四类盲区）。

因此工作区元素改为 [`crate::oamd::OamdMetadataBlock`]，由解析器写出 `object_index` 与 `block_index`。这也让两条 OAMD 路径共用同一个状态机入口：`oamd_substream()` 走 `OamdState::apply`，`audio_data_ajoc()` 走 `OamdState::apply_blocks`，规则一致，只是数据来源不同。

core 与 full 各需一份 `OamdState`：两侧的对象集合不同（`n_fullband_dmx_signals + b_lfe` 对 `n_fullband_upmix_signals + b_lfe`），共用一份会让上混的更新落到下混对象上。

整批更新是事务性的。A-JOC 一次交进来的是**整帧所有对象所有块**，中途失败若已写下前几块，下一帧就会在半新半旧的状态上继续推进差分位置。trace 因此先在临时状态上逐块解算并记录中间位置，core/full 两批都成功后再一起提交；时间线同时保留 substream 与 block 下标，避免多物理子流下的对象下标冲突。

### 5.12 I 帧上缺席的对话增强配置（`crates/macindecode-ac4-bitstream/src/ajoc/de.rs`）

`b_dmx_de_cfg` 为假时规范只说配置不在本帧（`6.3.6.6.1`），没有说该沿用什么。实测的八条流**每一帧都是 `b_dmx_de_cfg == 0`**：该编码链根本不带对话增强。若在 I 帧上也报「无历史配置」，整条链一帧都解不下来。

I 帧按 `4.5.2` 必须可独立于前序帧解码，因此缺席只能读作 `de_main_dlg_flag[]` 全零，`num_dlg_obj` 为 0，系数循环不执行。反之则编码器无法写出一个不带对话增强的 I 帧。非 I 帧上的缺席仍是沿用，无历史即报错——那才是真正的「未知」。

另有一处**语义与语法不一致**，不可据前者改后者：`6.3.6.6.2` 说 `b_keep_dmx_de_coeffs` 在 I 帧或 `b_dmx_de_cfg` 为真时应被解码器忽略，那说的是不得据它复用前一帧的系数；`6.2.3.5` 的语法表里系数是否出现无条件由该标志决定，故比特消耗不因 `b_iframe` 改变。

### 5.13 帧与 substream 的两套计数单位（`crates/macindecode-ac4-cli/src/trace/audio_substream.rs`、`trace/ajoc/stats.rs`）

一帧可含多条物理 A-JOC substream，因此音频巡检有两套单位：`frames`、`parsed`、`failures`、`intra_frame_update_frames` 每帧最多变动一次，`substreams` 与 `parsed_substreams` 逐条累加。把帧级字段写进逐条的循环，实测数据抓不到——**这条编码链每帧恰好一条 A-JOC substream，两套单位恒相等**，`scripts/audio_check.sh` 的 `parsed == frames` 与 `parsed_substreams == substreams` 同时成立。这类错误只能靠构造用例暴露，与 §5.10 的落点盲区并列，是验证工具自身的一类固有盲区。

失败标志另有一处不对称。上下文冲突与下标超容量发生在任何 substream 进入槽位之前，此时它根本不计入 `declared`：若一帧的 A-JOC 下标全部超界，`declared` 与 `parsed` 同为 0 而**相等**，"全部解析成功"的判据成立，只有显式传入的失败标志能让该帧作废。

这条路径是可达的，不是死代码：`substream_index` 读作 2 比特、取值 3 时以 `variable_bits(2)` 扩展，因此取值不受 `MAX_SUBSTREAMS` 约束；CLI 对 `validate_substream_references` 的失败只计数、不阻断后续巡检，超界的下标因此会一路走到音频统计里。

### 5.14 标度因子取值域是弱判据（`crates/macindecode-ac4-bitstream/src/asf/reconstruct.rs`）

`5.1.3.2` 的重建分三步：DPCM 还原绝对标度因子、由标度因子得增益 `2^((sf−100)/4)`、反量化 `sign(q)×|q|^(4/3)` 后相乘。三步均已实现，数值格式见 [ADR-0002](decisions/0002-numeric-format-for-reconstruction.md)。后两步的判据与第一步性质不同，见 §5.15。

规范给了一条取值域约束：`5.1.3.2` 的 NOTE 规定只有 `0…255` 是合法标度因子值。**这条约束比它看起来弱。** 四类注入里只有一类被它抓住：

| 注入 | 单元测试 | 取值域判据（八条实测流） |
|---|---|---|
| 偏移取 59 而非 60 | 4 项失败 | **未抓住**，范围由 `77…159` 变为 `83…193` |
| 首带也消费差值 | 5 项失败 | **未抓住**，范围变为 `19…95` |
| 不跳过全零频带 | 1 项失败 | 抓住，25 帧越界 |
| 不跳过无码本频带 | 未抓住 | 未抓住 |

原因是余量：合法区间宽 256，而八条流 41 693 个频带的实测取值只占 `77…159`，两侧各有大片空间吸收系统性偏移。真正区分对错的是手工验算、跨窗口组延续且缺失差值即失败的 DPCM 链；取值域只作兜底。`scripts/audio_check.sh` 同时要求 `scale_factor_failures == 0`、频带数非零且取值范围完整，避免重建路径未执行时形成空检验。

最后一行不是覆盖缺口，而是**等价变换**：`Pseudocode 21` 的两个条件里 `sfb_cb != 0` 不构成独立约束——码本 0 的区段不写谱线（`decode_spectral` 直接跳过），该带的 `max_quant_idx` 必然停在 0，已被第二个条件挡住。保留它只为与规范原文逐行对应。

这与 §5.10 的第四类盲区同源：**取值域约束的是范围，不是关系**。DPCM 是累加链，链上任一步的偏移都不改变结果的量级，只改变位置。

### 5.15 反量化与增益：自证判据与独立判据（`crates/macindecode-ac4-bitstream/src/asf/`）

`5.1.3.2` 的后两步没有比特可数，落点判据在此完全失效。可用的判据分两类，**只有第二类抓得住错误**。

**自证判据**：拿实现里的常量当期望值。写下来像测试，实际什么都不约束——把 `QUARTER_POWERS` 的四项顺序打乱，`assert_eq!(gain(100+r), QUARTER_POWERS[r])` 两边一起变，用例照过。首轮就是这么写的，注入立即暴露。

**独立判据**：不引用被测对象自身的性质。三条支撑了这两步：

| 判据 | 形式 | 抓住的错误 |
|---|---|---|
| 完全立方数的 `q^(4/3)` 是整数 | `q = k³ ⟹ 表值恰为 k⁴`，`k = 1…20` | 表值整体偏移、下标错位 |
| 增益随 `sf` 严格单调递增 | `gain(sf) < gain(sf+1)`，全 256 档 | 常量顺序错乱 |
| 常量是正确舍入 | `((2m−1)·2^(e−1))⁴ ≤ 2^r ≤ ((2m+1)·2^(e−1))⁴`，全整数 | 差 1 ulp |

第三条与反量化表的生成判据同源：`v = m × 2^e` 是 `2^(r/4)` 的最近 f32，当且仅当 `2^r` 落在两个邻接中点的四次方之间。四次方对照若带 `1e-6` 容差则差 1 ulp 照过，而 ADR-0002 声称增益「精确到最后一位」——声称的强度要有判据的强度相配。

八类注入全部被捕获：常量顺序反、常量差 ±1 ulp、截断除法代替 `div_euclid`、偏置取 99、反量化丢符号、表下标不取绝对值、无标度因子的频带不清零、输出缓冲不足不报错。

**实测**：八条流约 99 万条谱线走完整条重建链，非有限值 0 个，峰值 `1.0×10⁷`–`1.9×10⁷`。`scripts/audio_check.sh` 把 `scaled_nonfinite == 0` 与 `scaled_lines > 0` 一并作为门禁——后者防的是空检验，统计若一条谱线都没走过，前者会永远成立。

**输入必须匹配，且混用不会自己暴露。** `scale_factors(workspace, layout)` 与 `scale_spectrum(workspace, layout, factors, out)` 都接受多个独立参数，而频带取舍与谱线偏移全部来自 `layout`、码值全部来自 `workspace`：窄布局配宽谱只是少还原几个频带，返回值看上去完全合法（实测 4 个频带变成 1 个，无任何错误）。因此 `AsfWorkspace` 与 `ScaleFactors` 各记一份 `AsfLayoutKey`，两个入口都先核对。

键只记成帧参数（窗口数、分组、逐组 `max_sfb` 与变换长度、总谱线数），不复制 16 × 65 的偏移表。成帧参数一致时频带划分也一致，因此等价布局对象可以互换；参数一旦不同，结果必然不同。

布局相同不代表谱与标度因子可以跨声道混用。共享 `sf_info` 的双声道以及三声道元素具有完全相同的布局，但每个声道各自传输参考标度因子与 DPCM 差值。`scale_spectrum` 因而从当前工作区再还原一次标度因子并精确比较；数值不同即返回 `ScaleFactorSourceMismatch`，数值相同则缩放结果本就等价，无需按对象身份拒绝。

`scale_factors` 一侧的核对不能省。链条末端确实会因 `workspace` 与 `layout` 不符而失败，但**只调 `scale_factors` 而不做缩放的调用方**——例如只统计标度因子取值范围——拿不到任何信号。

匹配布局下频带偏移映射失败是不可达的（`coded_band_count ≤ max_sfb` 保证 `sect_sfb_offset` 有定义，`end ≤ total_lines = quant.len()` 保证切片合法），仍返回 `InvalidBandRange` 而非静默跳过：将来若因重构而可达，报错好过输出静音。该分支没有用例，与 §5.13 的退化分支同类。

### 5.16 谱解组：排列的自洽判据（`crates/macindecode-ac4-bitstream/src/asf/reconstruct.rs`）

`5.1.5.2` 的 `Pseudocode 25` 把码流里「组 → 频带 → 组内窗口」的编排还原为「窗口 → 频率升序」。它**只搬运不计算**，因此「不重不漏」等价于正确，判据强度远高于前两步：

- **双射性**——用 `1…lines` 编码输入下标，输出里每个非零值反查回唯一来源，且全部输入都被搬到。
- **组内交织**——512 点变换的前两带各宽 4 线，码流顺序是「带 0 窗 0、带 0 窗 1、带 1 窗 0、带 1 窗 1」，输出里窗 1 整体后移一个变换长度，位置可手工验算。
- **长帧恒等**——单窗口时输出即输入，`max_sfb` 之上补零；输出缓冲预填非零才能约束「确实清了零」。

双射性单测一组是不够的：窗口下标跨组累加的错误在单组布局上不改变任何输出，必须同时覆盖「一组两窗口」与「两组各一窗口」两种分组形态。四类注入全部被捕获——源不按窗口偏移、目标不按窗口偏移、窗口下标不跨组累加、输出不清零。

`coded_band_count` 里的 `.min(num_sfb_48(...))` 是**等价变换**而非独立约束：`AsfPsyInfo::parse` 已在 `max_sfb > num_sfb` 时返回 `MaxSfbOutOfRange`，任何能通过解析的布局都满足该上界。与 §5.14 记录的 `sfb_cb != 0` 同类。

**实测**：八条流的解组共写出 11 030 528 条谱线，非零谱线数逐声道零失配，能量最大相对漂移 `4.2×10⁻¹⁶`——正是 f64 求和顺序差异的量级（约 `2⁻⁵¹`）。`scripts/audio_check.sh` 把四项一并作为门禁：`ungroup_failures == 0`、`ungroup_count_mismatch == 0`、`ungroup_energy_drift < 1e-12`、`ungrouped_lines > 0`。

两条判据强度不同，不可互相替代。能量判据带 `1e-12` 相对容差，而最小非零谱线是 `0.0186`（`sf` 下界 77、`|q| = 1`），丢掉它造成的相对能量变化约 `5×10⁻²⁰`——能量判据完全无感，非零数判据立即发现。注入「不比非零数」时门禁仍通过，正是因为那次丢的是大幅度谱线、先被能量判据抓住。

**多窗口在实测中占 0.3 %–9.5 %，最大 16 个窗口**，因此这一步在真实数据上不是恒等变换。

### 5.17 谱噪声填充尚未实现（`5.1.4`）

`b_snf_data_exists` 在八条实测流上**恒为假**，噪声填充一次都没出现，与 `b_dmx_de_cfg == 0` 同类——编码器不使用该工具。

实现它需要三样 ADR-0002 未覆盖的东西：`Pseudocode 22` 的 `1.44269504 × log(band_rms)` 即任意实数的 log2、`Pseudocode 23` 的 `pow(2.0, 0.5 × noise_rms)`、以及 `GetRandomNoiseValue()` 这个正态分布发生器（状态 `nRndStateSnf`，由 `sequence_counter` 经 `Pseudocode 24` 初始化）。前两者不能像 `2^((sf−100)/4)` 那样靠位构造精确给出，需要另立数值方案；后者是跨帧状态。

**前两样已由 [ADR-0005](decisions/0005-real-functions-without-a-math-library.md) 解决**（`5.7.6.4` 的 HF 生成同样需要 `log2`/`exp2`，见 5.26），第三样仍未解决。推迟的结论不变，因为主要理由——`b_snf_data_exists` 恒为假——与数值方案无关。

因此该工具推迟，并且**只能靠构造码流覆盖**——当前材料无法验证任何实现。

### 5.18 IMDCT 的三角常量与 IFFT 选型（`crates/macindecode-ac4-bitstream/src/asf/imdct.rs`）

`5.5` 需要三类常量，**都无法沿用 §5.15 的整数判据**：

| 用途 | 函数 | 出处 |
|---|---|---|
| 前/后旋转因子 | `−cos(2π(8k+1)/16N)`、`−sin(…)` | `Pseudocode 60`、`62` |
| IFFT 旋转 | `cos(4πkn/N)`、`sin(4πkn/N)` | `Pseudocode 61` |
| KBD 窗 | 零阶修正贝塞尔 `I(x) = Σ (x^k / (2^k k!))²`，再求和开方 | `5.5.3` |

反量化表能用整数判据正确舍入，靠的是 `v³ = q⁴` 这个代数出口——「哪个 f32 最接近 `q^(4/3)`」等价于一次整数立方比较。三角函数没有这样的出口：它的值是超越数，任何有限的整数关系都只是逼近。

**但三类的难度并不相同，把它们拆开看，只有三角函数真正缺实现。** `I(x)` 是全正项级数，`term_k = term_{k−1} × (x/2) / k` 迭代；α = 6 时在 `k = 33` 停止，连同 `term_0` 共累加 34 项，四则运算即可。平方根在 IEEE 754 中是正确舍入的运算，与 `+ − × ÷` 同级，任何合规实现位模式相同（实测：f32 正规格化全域等步长抽样 208 万个值、f64 随机 300 万个值，`libm` 与硬件指令零差异）。

方案见 [ADR-0003](decisions/0003-trigonometric-tables-for-the-transform.md)：`cos`/`sin` 由**构建期依赖 `libm = "=0.2.16"`（`default-features = false`）**生成，表的完整位序列冻结 SHA-256，构建期重算比对；目标侧不链接 `libm`，运行期只查表。当前约束已收敛为「目标侧零依赖，构建期只允许 `libm` 一个 crate 且精确锁版本」。

选它而不是 `std::f64::cos`（`build.rs` 本就链接 `std`，零依赖）的实测理由：**同样 10 664 个三角值，`libm` 与 macOS `std` 的 f64 结果有 365 个位模式不同**。用 `std` 生成，表值就由构建机决定。收窄为 f32 后这 365 个差异全部消失（f64 的 53 位尾数对 f32 有 29 位余量），但「差异恰好被吸收」不是可复现性——换一台构建机没有任何机制担保同一结论。**摘要冻结保证的是所有成功构建只能产出同一张表；平台若算出不同位序列就闭锁失败。它不保证表的每一位都正确舍入。** 正规格化 f32 相邻数的相对间距约在 `2⁻²⁴` 到 `2⁻²³` 之间，最大约 `1.19×10⁻⁷`；`2⁻²⁴ ≈ 6.0×10⁻⁸` 是最近舍入的半 ulp 误差界，不能称作 1 ulp。

**初始表值另由高精度审计锚定（`scripts/check_transform_tables.py`）。** 以 100 位十进制精度（约 332 bit）全域复算三角函数与 KBD；π 由 Machin 公式算出，`cos`/`sin` 用泰勒级数，IFFT 完整圆周由第一象限经换位、变号与共轭派生。KBD 按规范给定的同一 `I₀` 级数重算，但改用 Decimal 高精度与不同终止判据。四张表得到摘要 `cd702d…524e`、`6e57e…c895`、`76261b…ef6b` 与 `4e7f81…0d25`（末一张是 `5.7.3`/`5.7.4` 的 QMF 调制表，见 5.20）；摘要直接从 ADR-0003 读取，并与 `build_support/math.rs`、`build_support/qmf.rs` 的生产摘要交叉核对，文档、审计与实际构建不能静默漂移。脚本只用 Python 标准库，不需要规范 PDF，也不进入构建依赖；CI 的 `quality` 检查会运行脚本。

审计同时给出一个余量：全部 33 016 项中，舍入余数距 f32 中点最近的一项仍有 `7.11×10⁻⁵`，远大于工作精度的误差，因此**每一项的舍入判定都是确凿的**。

构建期再检查以下结构自洽条件；计算时把已存储的 f32 提升到 f64：

| 表 | 构建期判据 | 实测最大偏差 |
|---|---|---|
| 旋转因子 | 对每个 `N`：`abs(xcos1[k]² + xsin1[k]² − 1) ≤ 1×10⁻⁷`；两值均为负；`xcos1` 严格递增、`xsin1` 严格递减 | `8.15×10⁻⁸` |
| KBD 左窗 | 对每个 `N`：`abs(KBD_LEFT(N,n)² + KBD_LEFT(N,N−1−n)² − 1) ≤ 1×10⁻⁷`；值域 `(0, 1]`；单调不减 | `8.04×10⁻⁸` |
| IFFT 根 | 对每个 `M`：轴点逐位等于 `±1/+0.0`；共轭关系逐位成立；第一象限符号正确；单位圆偏差 `≤ 1×10⁻⁷` | `7.47×10⁻⁸` |

两条平方恒等式只能证明内部配对自洽：共同角度偏移、成对符号翻转或成对重排仍可能保持平方和，因此不能充当正确性锚点；象限与顺序检查补足表结构，高精度摘要复算才锚定具体值。Princen-Bradley 恒等式同时说明右窗不必单独建表：`KBD_RIGHT(N, 2N−1−n) = KBD_LEFT(N, n)`，逆序索引即可。

IFFT 已由 [ADR-0004](decisions/0004-mixed-radix-stockham-ifft.md) 落为 radix-4/2/3/5 的 production Stockham autosort，因子顺序固定为 power-first，并使用一个 16 KiB scratch。规范直写的 `O(M²)` DFT 是独立 oracle，两个因子顺序在十五档 `M=N/2` 上均通过差分；生产入口另锁定非法长度不改写输入、正号、自然顺序与无归一化。完整 f32 根表的摘要是 `76261b…ef6b`，单位圆最大偏差 `7.47×10⁻⁸`；端到端根量化探针的相对 RMS 误差为 `4.12×10⁻⁸`，最坏归一误差为 `1.47×10⁻⁶`。

**三张常量表已全部落地**，前/后旋转与 KBD 左窗按同一机制生成、核对摘要并跑构建期结构判据。运行期只按变换长度切片查表，另有一层判据专门针对**摘要管不到的东西**——摘要固定的是连续存储的字节，切分、索引与镜像都在它的视野之外：

| 运行期判据 | 防的是 |
|---|---|
| 每档旋转因子恰 `N/2` 对、KBD 窗恰 `N_W` 项，合计 5 332 与 10 664 | 偏移表切错档；某档读到相邻档的尾部时值仍是合法余弦，摘要照样通过 |
| 首末两项的角度符合 `2π(8k+1)/16N`（容差 `1×10⁻⁶`，不绑定宿主位模式） | 行号错位、分母写错 |
| 读出的窗值满足 Princen-Bradley，且首项接近 0、末项接近 1 | 切分错位导致两端不再互补 |
| 右窗逐位等于左窗逆序 | `KBD_RIGHT` 的镜像关系写成正序 |

四类注入验证了分工：右窗写成正序只有镜像判据抓得住（长度仍然对）；KBD 误用旋转因子的偏移表被切分与 Princen-Bradley 同时抓住；切片右端少一项只有切分判据抓；行号错位一档则触发切分、α 与角度三条。构建期另有两类注入被结构判据捕获：旋转因子不取负号违反象限，KBD 求和漏掉端点 `p=N` 使 Princen-Bradley 偏差升到 `7.43×10⁻⁷`。

`I₀` 的逐项递推写法按 ADR-0003 第 5 条固定。实测 `term × (x/2 ÷ k)` 与 `(term × x/2) ÷ k` 在这十五档上给出**同一张表**，摘要不变——浮点下两者的舍入一般不同，此处相同是巧合而非可依赖的性质。

### 5.19a 完整 IMDCT 的五条判据（`crates/macindecode-ac4-bitstream/src/asf/imdct/transform.rs`）

`Pseudocode 60`–`64` 的六个步骤已接合。低层变换 API 的工作区（80 KiB，声道间复用）与重叠缓冲（每声道一份，长 `N_full`）仍由其调用方提供，不自行分配；Full engine 已把这份调用方所有权收进 `Ac4DecoderSession` 自持 decoder，边界由 [ADR-0007](decisions/0007-preprocessed-scene-rust-api-boundary.md) 固定。

| 判据 | 覆盖 | 实测 |
|---|---|---|
| 分析—合成完美重建 | 全链路乘积：前旋转、IFFT、后旋转、展开、加窗、重叠相加与 PCM 增益 | 十五档全部长度，合法 `N_full` 下单位增益，最大偏差 `≤ 2×10⁻⁵` |
| 未加窗后半与 IMDCT 定义式差分 | 纯变换，不含窗 | 十五档，最大偏差 `≤ 5×10⁻⁶` |
| `N_full` 只改变延迟不改变内容 | Step 6 的搬移分支 | 三组配置，**逐位相同** |
| `5.5.3` 文字示例差分 | 独立直写的窗分段、重叠偏移、factor of 2、搬移与后半存储 | 480、480、960 三块逐块比较完整 PCM 与 overlap，偏差分别 `≤ 2×10⁻⁵`、`≤ 1×10⁻⁵` |
| 混合块长重建延迟恒定 | 写入偏移、搬移量与输出取址在块长变化下的自洽性 | 22 个块、5 种切换（含表 187 的 `1 024\|8*128` 与 `8*128\|1 024`），最大偏差 `1.8×10⁻⁷`，逐块核对缓冲无残留 |

前三条各有不可替代的作用。完美重建看的是整条链路的乘积，窗与变换的错误可以互相掩盖，故需要第二条只看变换本身；第三条则把同一组谱线分别放入两个合法 `N_full`，扣除两者的相对 `nskip` 后要求输出逐位相同，从而把 Step 6 的缓冲搬移与分析侧几何隔离开来。它使用精确相等而非容差，因为改变 `N_full` 只应增加整数延迟，不应改变任何样本位。

前三条的七类注入验证了分工：Step 2 谱线索引互换、Step 4 漏掉 `1/N`、Step 5 的 `y` 索引取反各触发前两条；Step 5 符号写反与右窗当左窗用只触发完美重建；Step 6 搬移顺序颠倒只触发延迟等价；`nskip` 与 `nskip_prev` 互换触发三条中的两条加静音用例。

**第五条锁的是用法，不是窗形状。** 它的分析侧复用 `left_window_shape`/`right_window_shape`，形状一改两侧一起改，重建照样成立——实测把 `Nw` 截到 1 024 时本判据全过，只有 `equal_block_lengths_give_a_pure_taper` 与等长重建拦下。它与第四条也有重叠：试过的七类注入（写入偏移固定为 0、写入偏移改用上一块长度、搬移量误用 `nskip_prev`、搬移步长误用 `N_prev`、输出改取相加段、左右窗形状参数对调、右窗渐变段不逆序）全部同时触发第四条。第五条的不可替代之处在**覆盖面**：第四条只有 `frame_length = 1 920` 的 480/960 一组几何，第五条覆盖 2 048 族的 2 048/1 024/128 与 22 个块的连续序列，并显式核对每块之后 `≥ (N_full + N)/2` 无残留。

**PCM 重建增益为 `1.0`。** 规范这里存在一处内部不一致：`Pseudocode 62` 把 IMDCT 内部样本除以 `N`，`Pseudocode 64` 的代码又直接输出重叠和，但紧随其后的 `5.5.3` 块切换示例在三次重叠相加中都明确要求 factor of 2。实现保留伪码规定的 `1/N` 内部样本与未缩放 overlap 状态，只在复制到 PCM 时乘 2；这与标准 `2/N` IMDCT 等价，也让无归一化正向 MDCT 的十五档往返都以单位增益重建。回归测试因此不再把 `0.5` 当成可接受结果。

`OverlapBuffer` 的输入域同样由规范表闭合：构造器只接受表 99 的八档 `N_full`，每次变换再要求 `N` 属于该 `N_full` 在表 100/103 中的变换族。`N_full = 120` 以及 `N_full = 1 024、N = 960` 等跨族组合都会在改写状态前失败。满足 `N ≤ N_full` 但跨族的组合共 58 种，仅凭长度比较全部漏过。

**`5.5.3` 的文字示例是第四条判据，也是 IMDCT 小节唯一的完整数值例子。** 它用自然语言写成，与 `Pseudocode 63`/`64` 相互独立：`frame_length = 1 920`，块序列 480、480、960。测试给上一完整块注入非零 overlap，用直写 IMDCT 和硬编码的窗分段另建参考路径，因而首次扩展右窗、重叠偏移 720/480、块三的「240 个 0 + KBD left 480 + 240 个 1」、factor of 2、搬移及后半存储都进入逐样本比较，而不再只核对形状元数据。

**注入非零 overlap 是右窗 `skip` 段唯一的可观察条件，不是为了更完整。** 该段只在 `skip > 0` 时存在，而 `skip > 0` 只出现在块长切换处；此时右窗作用的是上一块的存量，缓冲若为全零，乘 1 与乘 0 的结果相同。实测对比：把右窗 `skip` 段从 1 改成 0，非零初值下 2 个用例失败，**全零初值下 0 个**；作为对照，把渐变段的逆序去掉在两种初值下都是 3 个用例失败——那类错误在等长块上就已暴露，等长块的 `skip` 恒为 0。

### 5.19b 帧级合成不设 composition buffer（`crates/macindecode-ac4-bitstream/src/asf/imdct/frame.rs`）

`5.5.3` 规定一帧内各块处理进一个至多 4 096 样本的 composition buffer，处理完后交出最早的 `frame_length` 个。**本实现不设该缓冲，各块直接写入调用方的输出。**

依据**不是**「块长之和等于 `frame_length`」。那句话只说明数量对得上，说不了对齐；先前本节据此立论，是把「取走全部」当成了必然。真正的依据是同一小节示例所描述的移位寄存器语义：每块输出恒取重叠缓冲的 `[0, N)`，随后整体左移 `N`，新块的未加窗后半写到 `[(N_full − N)/2, (N_full + N)/2)`。由此得两条：

- **输出时间轴匀速。** 读指针每块前进 `N`，块中心每块前进 `(N_prev + N)/2`；两者由 `c_i = t_read_i + (N_full + N_i)/2` 联系，差分即 `(N_{i−1} + N_i)/2`，对任意块序列恒成立。延迟是常数，与块长无关。
- **`N_full` 的缓冲够用。** 右窗先从 `(N_full + min(N, N_prev))/2` 起归零，当前块的左半随后最多写到 `(N_full + N)/2`，故每块结束后下标 `≥ (N_full + N)/2` 恒为零。若下一块更长，它新增的右侧区间落在这段零区；左侧区间则按设计与仍存活的历史重叠。

两条由第五条判据实测闭合。注入「输出改取刚相加的那一段」——即误以为输出须跟随写入偏移——触发四条用例，包括本判据。规范给的 4 096 恰是 `2 × N_full`（96 kHz 的 8 192 与 192 kHz 的 16 384 同样是两倍），与本实现「`N_full` 重叠缓冲 + `frame_length` 输出缓冲」的总量相同。

先前实现设了 composition buffer，其余量搬移分支**永远不被执行**，注入实测确认——把「取样时不从最早处取」「余量不前移」两处改坏，没有任何用例失败。不可达代码不是安全余量。

**一个可观察的后果：帧内靠后的短块，其能量要到下一帧才出现在输出里。** 写入偏移 `(N_full − N)/2` 最远可达 960（`N = 128`、`N_full = 2 048`），而读指针每块只前进 `N`。`probe_axes_single_object` 第 47 帧正是这种情形：静音三帧后瞬态起振，该帧 9 个窗口（1 024 + 8×128）输入峰值 `4.5×10⁵` 而输出全零，第 48 帧即出声。「输入非零却输出全零」因此是合法情形，CLI 只记录该计数，不作门禁。

块序列读自窗口布局：窗口 `w` 的变换长度是它所属组的 `transform_length`。它与帧长由不同字段决定——前者出自 `asf_transform_info()` 与 `asf_psy_info()`，后者出自表 83——因此两者相等本身就是跨层判据。

**判据必须建在不等长多组布局上。** 等长布局会让「窗口顺序」与「窗口到组的映射」双双退化为恒等：所有窗口长度相同时，反转处理顺序不改变任何切片，取首组长度与取所属组长度给出同一个值。实测确认这两类错误在等长布局上**完全不可观察**，无论比较得多细。现用前半 8 个 128 点窗口、后半 1 个 1 024 点窗口的 `Split` 布局（`transf_length` 取 0 与 3），九个窗口各自成组，参考路径独立按窗口升序取块长逐块变换，与帧层输出逐位比较。

四类注入验证：窗口顺序反转触发两条用例；窗口长度取首组触发三条；把总量核对移到变换之后，触发「拒绝不应推进重叠状态」——先变换再发现帧长不符会留下半帧的重叠状态，而调用方无从回滚。块长之和的相等判据需要**双向**用例：只测 `total > frame` 时，把它放宽成 `total > frame` 不会被发现；补上 `total < frame` 后两个方向的放宽各触发两条用例。

**帧间状态延续要两条互补的判据，而「相邻帧输出不同」不是其中之一。** 后者几乎必然成立——输入本就逐帧不同——因此对「overlap 是否延续」零信息量：注入「每帧重置 overlap」，它 4 个用例全部通过。实际用的两条是：其一，帧层输出与一条**跨帧保持 overlap** 的逐块参考路径逐位相同；其二，同一帧再从空状态合成一次，结果必须不同。同一注入下前者在帧索引 1（第二帧）的样本 0 即失败。两条不可互相替代——若被测与参考路径共享同一错误假设（都不延续），前者会一致通过，而后者不依赖参考路径，单独保留时同样抓住该注入。参考路径另用独立的 `ImdctWorkspace`，只为隔离两条路径之间的可变 scratch；它不是变换算法的独立 oracle，不能发现两侧共同调用的 `transform::transform` 中的同源错误。

**整数骨架自带一条可闭合的判据**：窗口三段长度之和必须精确等于对应块长——左窗为 `N`，右窗为 `N_prev`。两侧**不对称**（`Nw` 相同而铺开宽度分别由当前块与前一块决定），这正是块长切换时保持时域混叠抵消所必需的；把任一侧的块长取错，和立刻对不上。遍历十五档长度的全部有序对，七类注入全部被捕获：左右窗互换块长、`Nw` 取较长者、`Nskip` 不折半、奇数差值静默截断、α 表抄错一档、α 表反序。


### 5.19c 声道级失败必须让 overlap 失效（`crates/macindecode-ac4-cli/src/trace/spectrum.rs`）

重叠缓冲是跨帧状态。标度因子还原、缩放与解组三条路径中任意一条失败，这一帧就没有推进该声道的缓冲；旧的后半与下一帧不再时间相邻，再叠上去是错误的重叠相加，**而输出看上去完全正常**——没有非有限值、没有失败计数、峰值也在量级内。三处失败因此都清空该声道的状态，等价于把下一帧当作起解点。

`(substream, element, channel)` 三元组是隔离单位：同一 element/channel 下标在不同物理 substream 下是不同信号，实测八条流只有一条 A-JOC substream（下标恒为 2），因此这一维当前不可观察，属于潜在修复。


**缩放失败此前被整个丢弃。** 该分支既不计数也不记录，而 `scaled_lines` 与 `ungrouped_lines` 会一起不增，所以 `pcm_samples == ungrouped_lines` 也看不见它。统一 Full engine 现在把标度因子、缩放、解组、非有限值与合成失败作为结构化 ASF error/observation 交出；CLI 只累计稳定 trace 字段，不再自行执行缩放、解组或 IMDCT。对应 overlap 由 engine 在 substream/reset 边界清空，`audio_check.sh` 继续对 JSON 做 fail-closed 门禁。

**旧场景收尾曾与 shell 各抄一份重建清单。** 当时 `synthesis_failures` 只进了脚本，旧 `finish_scene` 会放行同一输入。现在制品出口直接由 Scene Session 的逐 AU 事务和 `SceneBatchError` 分类负责；旧 collection、三层 PCM sink、`finish_scene` 及 CLI 自持的六组解析工作区已经删除，生产与测试都不再运行第二套 Full DSP。

`reconstruction_invariants!` 仍一次声明 trace JSON 的 15 个稳定名字，`scripts/audio_check.sh` 只消费 `result.validation.ajoc.invariants.reconstruction`。`every_reconstruction_invariant_is_exposed_in_trace_json` 以穷尽 `match` 为每个变体注入计数并验证 JSON；Scene Session 的失败事务由 bitstream/Scene 单元测试闭合，batch adapter 另核对结构化错误分类与输入路径。

状态延续失败仍会同时增加帧级 `failures`，合成失败仍可能同时破坏 `pcm_samples == ungrouped_lines`；因此具体类别在 JSON 清单中排在总括类别之前。这个顺序只影响 trace 诊断可读性，不再决定制品出口是否提交。

### 5.19d 重建 PCM 的逐位回归基线（`scripts/decode_check.py`）

`export-core-pcm` 把 A-JOC 下混信号的核心带重建写成 WAVE_FORMAT_EXTENSIBLE 32 位浮点 WAVE。Scene Session 只在调用方显式启用诊断侧车时发布 normalized `f32`；CLI batch adapter 随后精确乘 `2^15`，恢复历史 `±32 768` 量级，writer 不再削波、缩放或取整。`data` 块逐样本直接写恢复后的 `f32::to_bits()`；DIRECTOUT 声道掩码不虚构扬声器位置，`fact` 记录每声道帧数。受支持真实媒体的 SHA-256 基线继续逐位约束整个文件及容器形状，但 `f32` 下溢区中无法经过 normalized 公共边界往返的合成内部位型不属于制品契约。摘要由 Python 的 `hashlib` 计算，与本仓库的代码无共同来源。

MP4 输入先按 edit list 投影到呈现时间线：media edit 裁掉 codec priming/tail，empty edit 写为静音。除摘要外还记录呈现形状（采样率、声道数、帧数、声道来源三元组）。形状先比：它变了摘要必然也变，而形状差异一眼能看出是声道少了还是长度变了。

编码媒体被版本控制排除，因此实际数值比较是本地门禁。默认输入同时覆盖基线条目与本地新增媒体：每段基线中的十二条输入缺少或解码失败时一律失败，**既有条目永远不许跳过**；未入基线的输入若明确报 `unsupported.coding_path`，可逐条列名跳过，当前两条 channel-based IMS 产物走这一分支。其余新增媒体仍因没有基线而失败，且一条都没真正解码时整次门禁失败，不能让“全部尚未实现”变成免检。CI 运行 `scripts/test_decode_check.py`，覆盖缺件失败、条件跳过、全部跳过失败、路径约束、失败更新不改写旧基线、下面三段的隔离，以及**入库基线的注释必须与脚本现在会写出的一致**。

**三段导出，三份基线，各自冻结。** `--stage core` 校验 `vectors/decode_baseline.json`（`export-core-pcm`），`--stage aspx` 校验 `vectors/aspx_baseline.json`（`export-aspx-pcm`，见 5.41），`--stage objects` 校验 `vectors/objects_baseline.json`（`export-objects-pcm`，见 5.53），默认三段都跑。全部 fail-closed 规则共用，只有命令、基线文件与逐路来源的写法不同。**基线文件必须分开**：核心带那份的价值正在于「不因上层改动而变」，共用文件会让一次 `--update` 把三层一起重冻。`--stage ... --update` 只改指定一层；一段失败仍继续报告后续段。core、A-SPX 与 objects 对多 presentation 向量都从各自基线的 `presentation_overrides` 读取显式零基下标，禁止静默选择第一项；迁移只新增该选择元数据，既有 PCM 摘要与形状不重冻。

带宽扩展那段的逐路来源写作 `substream:role:ajoc_input`，LFE 写作 `substream:lfe`。**含 `role` 是必要的**：这一段的整数下标是 `Pseudocode 14a` 的 A-JOC 输入顺序，LFE 不进入 A-JOC；只记下标的话，把 LFE 标成 `ajoc_input` 既不改变声道数也不改变摘要，基线会静默接受一份语义已经错了的导出。这个接缝在 Rust 单元测试里够不到——`drive_frame` 需要真实解析摘要与 A-SPX 数据才能调用——实测注入后本门禁报「声道来源从 …`2:lfe` 变为 …`2:ajoc_input:5`」，不注入退出码 0。

对象段把对象写作 `substream:ajoc_object:object:output_channel`，LFE 写作 `substream:lfe:output_channel`。对象身份与 WAVE 交织位置不能合成同一个下标：`Pseudocode 15` 可把 LFE 插在 0、中间或尾端，插回后的对象输出位置会偏移，但 `ajoc_object` 不变。

**三段基线都证明「没有意外改变」，不单独证明「正确」。** 参考解码器最浅只暴露对象 PCM，因此 core/A-SPX 两个中间层无法与对象层 oracle 直接比较；本地对象层虽已到达同类接口，但逐对象身份、延迟和量级的外部差分验证明确留到下一轮，本次新增基线不冒充 oracle。

它填的缺口是真实的：此前全部门禁都是结构性或统计性的——落点等式、非有限值计数、峰值是否有限、样本数守恒——改掉一处舍入而不改变这些量时无人报警。三组注入实测：

下表的三组注入在 `--stage core` 上实测；`--stage aspx` 消费同一条重建链，同类注入同样会改摘要。

| 注入 | 单元测试 | `audio_check.sh` | `decode_check.py` |
|---|---|---|---|
| `sf = 120` 的增益扰动 1 ulp | 1 条失败 | 通过 | **失败，摘要变化** |
| 仅在窗口组数 ≥ 5 的帧上扰动增益 1 ulp | **0 失败** | **通过** | **失败，八条全部** |
| 第 1 000 条谱线扰动 1 ulp | 0 失败 | 通过 | 通过 |

第二组是基线不可替代的证明：窗口组数 ≥ 5 的成帧只出现在真实码流里（实测占 0.3 %–9.5 %），人造 fixture 到不了，因此单元测试与统计门禁都看不见。第三组没触发任何一层，原因是该处谱线为零，`0.0f32` 异或 1 得到的次正规数经 IMDCT 后相对幅度约 `10^-49`，回到 f32 时被完全舍去——**扰动确实没有到达输出**，不是判据失效。

**MP4 edit list 必须应用到 PCM，否则基线冻的是错的东西。** 首版导出交出的是解码器在 codec/media 时间线上的连续输出，没有走容器声明的呈现区间。八条向量的 `elst` 都是单条 `(segment_duration, media_time = 2 048)`，即**头部一整帧 priming、尾部若干样本**都不属于呈现内容。逐样本核对：投影后的 PCM 与未投影输出的 `[2048, 2048+96000)` 完全相同，八个声道无一例外；被裁掉的头部 2 048 个样本各声道峰值只有 `0.3`–`25.3`（编码器起振），而尾部 2 304 个样本峰值高达 `8×10³`–`1.6×10⁴`——那是最后几帧的 MDCT 拖尾，**不是静音填充**，留着就等于给每条流多接了 48 ms 的音频。修正后 `afinfo` 报的时长从 `2.090667` 秒变成正好 `2.000000` 秒，与母版一致。

**投影不设「恒等就跳过」的快路径。** 实测八条向量的 `media_time` 恒为 2 048，`source_start == 0` 这一支从不成立，其中的长度判断更是双重不可达；注入把该判断删掉，单元测试与基线都不失败。恒等时照走一遍只多一次 `output_frames` 的拷贝。删除后补了恒等与裁尾两条用例，同一注入立即失败。

`probe_ramp_control` 与 `probe_ramp_lengths` 的 `payload_sha256` 相同（`7a255fb0…`，见 9.2c 记录的「编码器忽略 `rampLength`」），两者的 PCM 摘要也相同。这是基线自带的一条确定性核对：同一份 `mdat` 必须解出同一份 PCM。


### 5.20 QMF 分析与合成滤波器组（`crates/macindecode-ac4-bitstream/src/aspx/qmf.rs`）

`5.7.3` 的 `Pseudocode 65` 与 `5.7.4` 的 `Pseudocode 66` 已实现，共用表 D.3 的 640 抽头原型窗。64 子带、复数调制，状态与工作区都由调用方提供、不分配。调制按定义式直算（分析每时隙 `64 × 128` 次复数乘累加），改写成 128 点 FFT 加前后旋转属于另一次选型，先把语义与判据钉住。

**原型窗不经人工转写。** `QWIN[640]` 与 Huffman 码本同源，取自 `member_sha256` 校验过的随附 C 表；`build_support/spec.rs` 负责 `float` 数组解析（`f`/`F` 后缀按 C 语义去掉后仍以 f32 就近舍入）。构建期核对两条**与摘要无关**的结构判据：

- **镜像**：`|QWIN[n]| = |QWIN[640−n]|`，且仅在 `n` 为 128 的倍数（128、256、384、512）时反号。
- **多相功率互补**：每个相位 `p` 满足 `Σ_{k=0}^{9} QWIN[64k+p]² = 1`，实测最大偏差 `2.8×10⁻⁷`。

两条各有覆盖面：把某一项改 1 ulp 时镜像先失败；同时改 `n` 与 `640−n` 让镜像仍成立，则只有功率互补看得见（实测偏差 `7.8×10⁻⁴`）。冻结的摘要 `870d3c80…` 只保证「解析与 f32 化没有走样」，不说明表值正确——说明后者的是上面两条。

**调制表按 ADR-0003 的规则生成。** `exp(j·(π/128)·(sb+0.5)·m)` 化简为 `2π·(2sb+1)·m / 512`，故是一张 512 点整圈表，摘要 `4e7f81b7…` 已进 ADR-0003 并由 `scripts/check_transform_tables.py` 以 100 位十进制独立复算锚定（模平方最大偏差 `6.7×10⁻⁸`，轴点精确，共轭对称）。

| 判据 | 覆盖 | 实测 |
|---|---|---|
| 64 种输入相位的多相级联结构 | 延迟、旁瓣位置与对称、纹波长度、实现噪声底 | 见下 |
| 分析—合成往返重建 | 两条伪码的状态左移、窗下标、折叠与抽取顺序、两族调制相位、`1/64` 归一化 | 宽带随机信号最大相对偏差 `8.7154×10⁻⁴` |
| 状态跨调用延续 | `qmf_filt` 的跨帧语义 | 分两次喂入与一次喂入**逐位相同** |
| 静音与非法长度 | 空输入不留残留；长度不成立时不改写状态 | — |

**Core PCM 的 `×2` 在 QMF 边界如何落位是跨条款解释，不是 `5.5.3` 的明文接口规则。** `Pseudocode 62` 除以 `N`，`Pseudocode 64` 又直接输出未写 `×2` 的 overlap；但紧随其后的 `5.5.3` 三次重叠示例都写了 factor of 2，并称 composition buffer 会交给下一个工具。与此同时，`5.7.3`/`5.7.6.1` 只说 frame-aligned Core/IMDCT 输出进入 QMF analysis，没有另列换算。这组文字本身不能单独证明 QMF 前应除以 2，也不能忽略 P64/P82 的内部量级与可观察连续性。正式裁决、证据等级和推翻条件见 [ADR-0006](decisions/0006-core-pcm-qmf-gain-boundary.md)。

当前生产链以互逆表示换算同时满足两侧：纯 `analyse`/`synthesise` 逐行保持 `Pseudocode 65/66`；`analyse_ac4_pcm` 在字面分析后除以 2，所有 A-SPX/A-JOC 工具完成后由 `synthesise_ac4_pcm` 乘回 2。旧接线把已经乘 2 的 Core PCM 原样送入分析，使直通低带的振幅多 2 倍、能量多 `6.0206 dB`，而 `Pseudocode 82` 的绝对高带目标不随之改变，真实节目随即在 crossover 处形成同量级台阶。边界单元测试分别把两侧的 `0.5/2.0` 与滤波器状态逐值锁定；低层 QMF 的窗口、调制、延迟与归一化不因此改变。该结果可称为跨条款与端到端证据共同支持的规范解释，不能称为 ETSI 已发布的勘误。

**延迟是 577 而不是 640 − 64 = 576。** `Pseudocode 65` 把一块的第一个样本喂到 `qmf_filt[63]`（块内最旧），两族调制的指数 `2n−1` 与 `2n−255` 又各带半个样本的偏移。64 种块内输入相位逐一注入冲激，峰值全部恰好落在「输入位置 + 577」，无一例外。

**往返级联是以 64 个样本为周期变化的多相系统，不是单一 LTI 滤波器。** 先前只在 `signal[0]` 注入冲激，把那一个分支的 9 个抽头外推成全局冲激响应，并据此给出唯一的 `H` 与“线性相位”结论；逐相位扫测显示抽头值随输入在 64 样本块内的位置变化，因此那两项外推不成立。

每个输入相位 `p` 仍有明确且更细的结构：主瓣位于 `p + 577`，结构性旁瓣只落在 `p + 577 + 128m`（`1 ≤ |m| ≤ 4`），关于各自中心逐位对称；网格上 `|m| ≥ 5` 的尾部精确为零。对称是**每个多相分支**的性质，不单独证明整个周期时变系统没有相位失真。全相位门禁锁定以下最坏值：

| 量 | 64 相位实测 | 门禁 |
|---|---:|---:|
| 主瓣对 1 的最大偏差 | `2.384×10⁻⁷` | `≤ 1×10⁻⁶` |
| 结构性旁瓣最大幅度 | `2.229×10⁻⁴` | `≤ 2.5×10⁻⁴` |
| 128 网格外的 f32 噪声 | `1.409×10⁻⁸` | `≤ 2×10⁻⁸` |
| `|m| ≥ 5` 的网格尾部 | 精确 0 | 精确 0 |

**`8.7×10⁻⁴` 只保留为固定宽带信号的回归量。** 它是最大逐样本偏差除以信号 RMS，依赖信号与测量窗口，不能当作滤波器组的单一特征值。本实现、Python 双精度参考实现、以及把原型窗换成未经 f32 舍入的十进制值，三者都给出 `8.7154×10⁻⁴`，说明差异不是查表精度造成的；把测量起点从 577 推到 2 048 该值也不变，故与启动瞬态无关。若要给整个级联定义幅频与相位特性，需要另行推导多相失真/混叠传递函数，不能从任一分支的冲激响应直接得到。

全相位结构判据补上了 phase-0 盲区：相位 63 的网格外噪声为 `1.409×10⁻⁸`、最大旁瓣为 `2.229×10⁻⁴`，都高于旧判据按相位 0 设置的预算。它也保留原判据的注入强度：整体增益偏 `5×10⁻⁵` 时，旧的总误差预算容得下而主瓣门禁失败；抽取偏移 192 写成 191 会把旁瓣挪出 128 网格。其余八类伪码注入仍由宽带与结构两条往返判据共同覆盖。

**已接入受限的真实声道路径。** QMF 分析、A-SPX 参数链与合成现可通过 `export-aspx-pcm` 走完真实码流；这是 A-JOC 前的诊断出口，只放行长帧、未激活 companding 且未使用 FIC/TIC 的已确认子集。终端场景 PCM 仍须经 A-JOC 重建与 LFE 插回，见 5.41。

### 5.21 Companding（`5.7.5`）推迟：现有素材上从不启用

实现前先探明可达性，结论与 `5.1.4` 同类。八条流的实测：

| 流 | `companding_control()` 传输 | 其中 `b_compand_on` 为真 |
|---|---:|---:|
| `probe_bed_only` 256K | 49 / 49 帧 | **0** |
| 其余七条 | 0 帧 | — |

两种「不可达」各有成因，因此分开计数而不是合成一个数：

- **七条流根本不传输该元素。** `4.2.11` 只在 A-SPX 且下混信号数不超过 5 时传输 `companding_control()`；这七条的 `n_dmx_signals` 都超过 5。
- **唯一传输的那条恒为关。** 256 kbps 流每帧都带 `companding_control(5)`，但五个 `b_compand_on` 全假、`b_compand_avg` 也假，49 帧无一例外。

即：**没有任何一帧的 QMF 数据会被压扩改动**。此时实现 `5.7.5` 只会得到一段在真实素材上永远不执行的代码——与 `5.1.4` 的处境相同，且那一节已经记录了同样的理由。

统计已进 trace 的 `result.validation.ajoc.configuration.companding_frames` 与 `companding_active_frames`，不再是一次性探针：两者都按帧计数，同帧多条 substream 先合并；后者同时识别逐声道的 `b_compand_on` 和独立生效的 `b_compand_avg`。换入会启用压扩的向量时数字会动，届时再实现。语法侧（`4.2.11` 表 49 的 `companding_control()`）早已解析并有单元测试覆盖，推迟的只是 `5.7.5` 的解码过程。在此之前，`export-aspx-pcm` 会在数值通路前拒绝任一活动压扩帧，不会把未压扩的 PCM 冒充成完整解码。

### 5.22 A-SPX 参数域实测，据此定实现顺序（`5.7.6`）

`5.7.6` 是 M4 主线剩下的唯一大件，且**每帧都发生**——与 `5.7.5` 压扩、`5.1.4` 噪声填充不同，它不能推迟。实现前先扫一遍八条流实际用到的分支，4 818 个声道观测：

| 维度 | 实测分布 | 结论 |
|---|---|---|
| 成帧类别 | `FIXFIX` 98.6 %、`FIXVAR` 0.7 %、`VARFIX` 0.7 % | 变长成帧**可达**，不能只做单包络 |
| 信号包络数 | 1（98.6 %）、2、3、4 | 多包络可达 |
| 噪声包络数 | 1（98.6 %）、2 | |
| `aspx_qmode_env` | 仅在变长成帧下为真（68 次） | 与成帧类别绑定 |
| `aspx_add_harmonic` | 472 / 4 818 非零 | 音调生成器 `5.7.6.4.4` **可达** |
| `fic_used_in_sfb` / `tic_used_in_slot` | **0 / 4 818** | 交织编码**不可达** |
| `tna_mode` | 取值 0（4 719）、1（8）、2（91） | 模式 3 不可达 |
| `aspx_balance` | 立体声元素恒为 `false` | 平衡式立体声**不可达** |

交叉几何只出现三种配置：

| `sba` / `sbx` / `sbz` | `num_sb_aspx` | master 组数 | 观测数 |
|---|---|---|---|
| 36 / 36 / 56 | 20 | 8 | 4 230 |
| 32 / 32 / 62 | 30 | 12 | 343 |
| **28 / 30** / 62 | 32 | 14 | 245 |

第三种的 `sba ≠ sbx`，即 `5.7.6.3.2` 的**低带滤波区间非空**，那条路径因此可达；前两种退化为空。

由此定出实现顺序与取舍：**必须做**变长成帧、多包络、音调生成、`tna_mode` 0–2、低带滤波区间；**可据实推迟**交织编码与平衡式立体声，两者与 `5.7.5` 同类，写了也没有素材能执行。推迟不等于诊断导出可以猜测执行：`export-aspx-pcm` 对实际使用 FIC/TIC 的帧显式失败。

四项可达性已进 trace 的 `result.validation.ajoc.spectrum.aspx_add_harmonic_frames`、`aspx_interleaved_frames`、`aspx_variable_framing_frames`、`aspx_balance_frames`，均按帧计数（同帧多条 substream 先合并，见 `FrameTally`），并在元素的两个声道之间取或——只要一个声道用到，实现就得覆盖。

**逐声道取或在实测素材上仍不可观察。** 把它退化成只看声道 0，八条流的四个计数一字不变；因此真实素材不能为该分支作证。构造回归另用完整语法解析出双声道 `AspxData`，且成对：**正对照**左路保持 FIXFIX 且不加音调，右路单独使用 FIXVAR 并令 `aspx_add_harmonic[0]` 为真；**负对照**两路都是 FIXFIX、都不加音调。两者都喂给与生产路径相同的扫描函数。

负对照不是凑数——只有正对照时，把成帧判据写反（`== FixFix`）照样通过，因为左路的 FIXFIX 会顶上去；`add_harmonic` 改成恒真也一样。实测三类注入现在都被捕获：只看声道 0、成帧判据写反、`add_harmonic` 恒真。

这样覆盖了逐声道语义，但没有把实测盲区伪装成真实覆盖；素材侧仍与 `(substream, element, channel)` 隔离维度的处境相同（见 5.19c）。

### 5.23 低带滤波与 QMF 延迟线（`5.7.6.3.2`）

`Pseudocode 75` 做两件事：把 `Q_in,ASPX` 在交叉子带 `sbx` 处截断，并整体延迟 `ts_offset_hfgen` 个 QMF 时隙后交给高频生成器。延迟量取自表 192，只有两档：`frame_length ≥ 1 536` 为 6 个时隙，其余为 3 个。

前 `ts_offset_hfgen` 个时隙取自**上一个 A-SPX 区间的尾部**，因此模块持有跨区间状态。状态固定保存表 192 上限的最近 6 个时隙：当次延迟为 3 时读最后 3 项，为 6 时读全部。这样既不保存整个矩阵，也避免两档切换时把历史前半错当成末尾——按当次 `offset` 索引状态时，6 → 3 会把上一区间倒数第 6…4 个时隙当成倒数第 3…1 个，错开整整 3 个时隙。

**区间长度有硬下限，且界限是紧的。** 表 189 的八档 `num_qmf_timeslots` 是 32、30、24、16、15、12、8、6，下限恰好等于 `MAX_TS_OFFSET_HFGEN = 6`，因此「一次填满 6 个历史时隙」对任何合法输入都成立。先前为「攒不满就滚动」留的分支对合法输入不可达——注入把它改坏无人报警——已删除，改为 `IntervalTooShort` 在入口显式拒绝。

**截断是「不写」而不是「写零」。** `sb ≥ sbx` 留给高频生成器填，本步不碰。作为代价，调用方递进来的输出缓冲在低带侧必须是干净的：复用上一次的结果时，高频生成器只覆盖 `sb ≥ sbx`，低带残留会一路混进输出。`LowBandError::OutputNotCleared` 把这一条挡在入口，并报出具体的时隙与子带。

| 判据 | 覆盖 | 实测 |
|---|---|---|
| 连续区间拼成一份延迟副本 | 延迟量、跨区间尾部的取用位置、边界不丢不重 | 4 个区间逐时隙逐子带比对 |
| 延迟按 6 → 3 → 6 切换 | 合法帧长切换时始终读取最近的历史 | 两次边界逐时隙逐子带比对 |
| 交叉子带以上不被改动 | 截断语义 | 预置哨兵，`sb ≥ sbx` 逐项不变 |
| `sbx = 0` 不写任何子带但状态照常推进 | 全带交给高频生成时延迟线仍须记住尾部 | 下一区间的前 3 个时隙正确取到上一区间末尾 |
| 非法输入不推进状态 | 延迟不为 3/6、区间短于 6、`sbx > 64`、长度不符、缓冲不洁 | — |

第一条最强，也是本模块的主判据：样本值按 `全局时隙号 × 100 + 子带号` 编码，因此搬错一格会直接报出是哪一格，而不是只说「不相等」。五类注入全部被它捕获：尾部取区间开头而非末尾、延迟少一个时隙、历史时隙顺序反了、不保存尾部、首区间不做静音前置。截断写到 `sbx` 之上则由第三条捕获。

tiling、反量化与 HF 生成均已实现，低带输出也已交给预平坦化与 HF 生成消费；该链现已经 5.38–5.41 接入受限的真实声道诊断路径。

### 5.24 A-SPX 包络解码（`5.7.6.3.4`）

`Pseudocode 80`（信号）与 `Pseudocode 81`（噪声）把码流里的差分符号累加成量化标度因子。方向语义两侧一致：

- **频率方向**（`delta_dir == 0`）：`qscf[sbg] = Σ_{i≤sbg} δ·data[i]`，从最低子带组起的前缀和，**不引用任何历史**；
- **时间方向**（`delta_dir == 1`）：`qscf[sbg] = prev[sbg] + δ·data[sbg]`，`prev` 是上一个包络；本区间首个包络则取上一区间的最后一个包络。

`δ` 在 `ch == 1 且 aspx_balance == 1` 时为 2，否则为 1。实测 `aspx_balance` 恒为假（见 5.22），因此 `δ = 2` 这一支在现有素材上不可达，由构造用例覆盖。

**信号侧比噪声侧多一层分辨率映射。** 相邻两个包络的 `atsg_freqres` 可能不同，`sbg` 要经 `sbg_idx_high2low`/`sbg_idx_low2high` 换算才对得上。映射由低分辨率边界是高分辨率边界子集这一事实推出，扫一遍即可，无需搜索。噪声恒用 `sbg_noise`，没有这一层。

跨区间历史因此要留三样：信号标度因子、**上一区间最后一个包络的分辨率**（`freq_res_prev` 决定用哪个方向的映射）、噪声标度因子。

合法 `AspxInterval` 的信号与噪声包络数都至少为 1，解码入口据此拒绝任一侧为空，避免一次成功调用只初始化半份历史。量化标度因子沿用 `i16`，但乘加使用 checked 运算；`Pseudocode 80`/`81` 没有饱和语义，越界会带类型、包络与子带组位置返回错误，且不提交历史或部分输出。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 映射与边界表自洽 | `high2low` 单调不减、`low2high` 严格递增、两者互逆、起点边界相等 | 四条结构性质，均不依赖本模块的推导过程 |
| 频率方向是前缀和且不读历史 | 方向语义与历史隔离 | **把历史换成完全不同的值，结果必须逐位不变** |
| 时间方向在区间内与跨边界都接上一个包络 | 链式累加与跨区间状态 | 两个包络 + 下一区间首包络 |
| 分辨率切换时按两个方向映射取值 | `Pseudocode 80` 的三分支 | 差分全零，结果必须等于映射后的源组 |
| `δ = 2` 让整条链翻倍 | 平衡式立体声第二声道 | 与 `δ = 1` 的结果比值 |
| 无历史时的时间方向被拒绝 | 首区间不得假定历史为零 | 信号与噪声两侧各一 |
| 信号或噪声包络为空时被拒绝 | 单一历史标志只表示完整区间 | 两个方向各一 |
| 标度因子乘加溢出时被拒绝 | 不引入规范没有的饱和语义 | 信号与噪声两侧各一，输出与历史均不提交 |
| 拒绝不推进历史 | 事务性 | — |

第二条是这批里最不寻常的一条：它不检查数值本身，而是检查**结果对历史不敏感**。这类"应当无关"的判据能抓到一整类误读，而逐值比对抓不到——注入「频率方向拿历史当起点」只有它失败。

七类注入全部被捕获：`high2low` 与 `low2high` 用反、映射推导时边界比较取 `low` 而非 `low+1`、频率方向误读历史、时间方向跨区间取历史第 0 组、`δ` 恒为 1、噪声时间方向不接上一包络、失败路径上仍推进历史。

另用构造输入覆盖任一侧为空与信号/噪声乘加溢出；分辨率切换用高 → 低 → 高三包络连续执行，两个映射方向都进入生产分支。

量化标度因子现已由反量化、包络调整与组装链消费，并经 5.38–5.41 串入受限的真实声道诊断路径。

### 5.25 A-SPX 标度因子的反量化与立体声解码（`5.7.6.3.5`）

`Pseudocode 82`/`83`/`84` 把 `qscf` 变成线性标度因子 `scf`，落在 `crates/macindecode-ac4-bitstream/src/aspx/dequant.rs`。

**标度因子在能量域，不是幅度域。** 规范 `3.1` 把 signal scale factor 定义为「average energy of the signal within the region in a QMF matrix」。这一条把两档量化步长钉死：`scf = 2^(qscf/a)` 取 `10·log10`，`a = 2` 得 1,505 dB/步、`a = 1` 得 3,010 dB/步，正是 `aspx_qmode_env` 的两档；若误按幅度域取 `20·log10`，两档会变成 3 dB 与 6 dB。

由此还能定死伪码没写明的一件事：**`qscf/a` 是实数除法**。整数除法会让 `a = 2` 时相邻的奇偶 `qscf` 撞在一起，1,5 dB 档退化成 3 dB 档，两档就不再有区别。

**不需要通用的 `pow`。** 指数只可能是整数或半整数：`2^n` 由 f32 指数域直接构造（无舍入），半整数部分乘 `core` 里正确舍入的 `√2` 或 `1/√2`。这既是目标侧零依赖的要求，也比通用幂函数精确。指数落到 f32 正规数之外一律报错——上溢的 `inf` 会顺着 HF 生成污染整个 QMF 域，`5.7.6.3.5` 没有定义这两种情形。

**平衡式立体声有一条代数恒等式可用。** `Pseudocode 84` 的两个分母满足 `1/denom_a + 1/denom_b = 1`，于是 `scf_a + scf_b = nom = 2·scf_mono(qscf_a)`：两声道能量之和只由和声道决定，平衡声道只决定怎么分。这条判据的右端完全由 `Pseudocode 82` 算出，因此跨两段伪码互证。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| `2^n` 与逐次乘二逐位相同 | 指数域构造 | 不依赖位模式的朴素累乘作第二来源 |
| 两档步长确为 1,5 dB 与 3,0 dB | 能量域、实数除法、`a` 未取反 | 对相邻 `qscf` 取 `10·log10` 比值 |
| 1,5 dB 档奇偶严格可分 | 实数除法 | 相邻值严格递增，且恰好差一个 `√2` |
| `Pseudocode 82`/`83` 的两条公式 | 逐值 | 期望值用字面量 6 独立算出 |
| 标度因子恒为正有限值 | 恒假分支的前提 | 五个基点 × 两档 |
| 两声道之和等于 `2·scf_mono` | `Pseudocode 84` ↔ `82` | 六个平衡偏移，信号与噪声两侧 |
| 平衡参数取 12 时均分 | `PAN_OFFSET` | **字面量 12，不引用实现常量** |
| 平衡参数单调搬运能量 | 两个分母的方向 | 十三个偏移逐点比较 |
| 指数越界被拒绝且不提交输出 | 不引入饱和或最终除法下溢 | 单声道与联合解码、信号与噪声两侧 |
| 最大有限半整数指数仍可解码 | 不经不可表示的整数幂中间值 | `127,5` 同时出现在 `nom` 与分母 |
| 两声道网格不一致被拒绝 | `atsg` 必须相同，错误域准确 | 信号组数不同与噪声包络数不同 |

十二类注入全部被捕获，且预测的判据全部命中：`a` 两档取反、`qscf/a` 用整数除法、噪声指数符号反、`num_qmf_subbands` 当成 32、两个分母互换、`nom` 少了 `+1`、噪声侧分母也除以 `a`、`PAN_OFFSET` 当成 6、半整数时两个分母的 `√2` 同向、指数越界改成夹住、`exp2i` 偏置写成 126、1,5 dB 档对负数用向零取整。

另用构造边界覆盖联合解码最终商下溢与 `127,5` 次幂；前者在提交前拒绝，后者不得因不可表示的 `2^128` 中间值被误拒绝。

其中两条值得记：

- **两个分母互换只有单调性判据抓得到。** `1/denom_a + 1/denom_b = 1` 对交换对称，能量守恒判据完全看不见这个错误。
- **`PAN_OFFSET` 当成 6 一开始一条判据都没抓到。** 原因是居中用例用实现里的 `PAN_OFFSET` 算期望值，常量改了期望跟着改，判据成了自证。改成从 `Pseudocode 84` 抄字面量 12 之后才抓得到。

**`Pseudocode 82` 有一条恒假的分支，本实现不写。** 伪码是：

```c
if (aspx_sig_delta_dir[atsg] == 0 && qscf_sig_sbg[0][atsg] == 0
    && scf_sig_sbg[1][atsg] < 0)
    scf_sig_sbg[0][atsg] = scf_sig_sbg[1][atsg];
```

第三个条件永远为假：`scf = num_qmf_subbands · pow(2, x)`，64 与 `pow(2, ·)` 都是正的。写成 `qscf_sig_sbg[1][atsg] < 0` 才讲得通（首组为 0 时用第二组补），但改动规范语义需要证据，这里既不按字面写死代码、也不按推测改，而是用「标度因子恒为正有限值」把前提钉住：它一旦失败，就说明标度因子换了表示，那条分支需要重新审视。附带一提，该分支还无条件访问下标 1，`num_sbg_sig[atsg] == 1` 时会越界——同样因为分支不可达而不必处理。

`scf` 已由包络估计与补偿增益消费，并最终进入高频组装；整条参数链已经 5.38–5.41 串入受限的真实声道诊断路径。`aspx_balance` 在实测素材上恒为假（见 5.22），`Pseudocode 84` 仍由构造用例覆盖。

### 5.26 目标侧的实数函数（`crates/macindecode-ac4-bitstream/src/math.rs`）

`5.7.6.4` 的 HF 生成第一次需要在运行期对任意实数求超越函数，ADR-0002 记录的约束（`core` 里 `sqrt` 起就没有）到此不能再靠查表绕开。决策见 [ADR-0005](decisions/0005-real-functions-without-a-math-library.md)。

**需求先勘查再动手。** 该节 640 行正文里 `pow` 出现 15 次，但逐条看下来只有一处需要任意实数指数：

| 需求 | 次数 | 处理 |
|---|---|---|
| `pow(x, 2)`、`pow(x, k≤3)`、`pow(-1, n)` | 10 | 四则运算 |
| `pow(2,-20)`、`pow(10,-12)`、`pow(10,5)` | 4（3 个不同常量） | 编译期常量 |
| `sqrt` | 6 | 新增 |
| `log10`、`pow(10, ·)`（同在 `Pseudocode 85`） | 2 | 新增，归结为 `log2`/`exp2` |

出口是 **f64 的四则运算在 `no_std` 下完全可用**：位运算精确规约之后，剩下的部分用级数在 f64 里算到远超 f32 所需的精度；`log2` 的规约比值、Horner 求值与换底用高低两个 f64 保留舍入残差。目标侧依赖仍为零。

| 判据层 | 覆盖 | 手法 |
|---|---|---|
| 精确点 | 规约路径 | 2 的整数次幂（`−1074…1023`，含次正规）、完全平方数 |
| 代数律 | 级数系数 | `log2(xy)=log2 x+log2 y`、`exp2(a+b)=exp2 a·exp2 b`、两者互逆 |
| `std` 对照 | 整体 | 逐点 ulp，`sqrt`/`log2`/`exp2` 各 1 ulp |
| 硬件对照 | `sqrt_f32` | 27 001 个采样点与 `f32::sqrt` **逐位相同** |
| 高精度锚点 | 随源码走的证据 | `scripts/check_math.py` 用 `Decimal` 80 位复算 52 个值 |

三层缺一不可：只有精确点会漏掉级数（2 的幂上级数取值恒为 0，根本不参与求值）；只有代数律会漏掉整体缩放；只有 `std` 对照则把正确性寄托在宿主 libm 上，而目标侧没有 `std`。

八类缺陷注入全部被捕获：`log2` 级数只留前 3 项、`1/7` 写成 `1/9`、`exp2` 级数减到 4 项、换底除以 `LOG2_E`、`sqrt` 迭代减到 3 步、`decompose` 不处理次正规、`exp2` 不夹指数、`sqrt` 漏掉偶指数调整。

**另有一组等价变体，要求判据保持沉默。** 换一种同样正确的写法时判据不该失败，否则它锁死的是实现细节而不是行为：`log2` 折叠阈值改用 1,5、`exp2` 两次乘法对调、`sqrt` 初值改成常数 1、`log2` 级数多算两项——四条全部无判据失败。这组对照是判据是否过紧的唯一证据，缺了它，「注入全被抓到」可以靠把判据收紧到锁死实现来达成。

两处值得记：

- **`2^n` 必须夹住指数、余量单独相乘。** `n` 取最近整数，可以落在正规指数范围外而结果仍可表示（`2^1023,5 < f64::MAX`）。`aspx::dequant` 的 `pan_denominators` 在 `2^127,5` 上犯过这个错，本模块随后在 `exp2(1023,5)` 上又犯了一次——**同一类错误在两个模块各出现一次**，都是最终值可表示、中间值不可表示。
- **往返判据的容差必须随指数量级走。** `log2(x)` 量级到 1024 时 f64 在那里的分辨率是 `2^-42`，`exp2` 把它放大成约 718 ulp。即便两个函数都正确舍入，`x = 2^-1022` 附近的往返也回不到 2 ulp 以内。判据取 `|log2(x)|·ln2 + 4` 个 ulp，实测比值 0,403。曾经误把这个数学下界当成实现缺陷去"修"`exp2` 的乘法顺序，实测两种顺序都是 0 ulp。

### 5.27 A-SPX 预平坦化（`5.7.6.4.1.2`）

`Pseudocode 85` 把低带 `Q_low` 的谱包络在 dB 域拟合成一条三阶多项式，用它代表整体谱斜率，再翻成增益向量供 HF 生成搬移子带时取倒数。落在 `crates/macindecode-ac4-bitstream/src/aspx/preflatten.rs`。

**拟合改在中心化坐标里做。** 伪码把 `x[i]` 直接取成子带号，于是正规方程 `AᵀA` 的元素是幂和 `S_k = Σ i^k`；实测几何下 `sbx` 在 28…40 之间，`S₆/S₀` 跨 `6,1×10⁷` 到 `5,4×10⁸`。改在 `u_i = (2i − (sbx−1))/(sbx−1) ∈ [−1, 1]` 上拟合后，同样的跨度降到 **0,17**——八个数量级。这不改变结果：最小二乘解唯一，换基只换系数的表示，拟合值 `slope[sb]` 不变；`polynomial_fit()` 本身是信息性引用（Numerical Recipes，[i.13]），规范只规定它是最小二乘意义下的拟合。因此本模块**不公开 `poly_array`**——那四个系数依赖基的选择。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 残差与设计矩阵每列正交 | 正规方程本身 | 幂次上界写字面量 3，四种 `sbx` |
| 拟合值的四阶差分为零 | 结果确实落在三次多项式空间 | 等距点上的恒等式 |
| 三次输入被精确还原 | 拟合不引入偏差 | 相对偏差 `< 10⁻¹¹` |
| 平坦包络给出单位增益 | `mean − slope` 的配平 | 32 个子带 |
| 整体抬高不改变增益 | **"应当无关"型** | 功率乘 100，相对偏差 `< 10⁻⁴` |
| `+1` 地板让静音子带有限 | 伪码那个加一的用途 | 全静音与单带静音 |
| 实部与虚部等价 | `re² + im²` 的对称性 | 两种单边输入逐位相同 |
| 只统计给定时隙区间 | 区间边界 | 与单独输入**逐位相同** |
| 下降谱给出上升增益 | 增益是斜率的倒数 | 逐子带单调 |
| 四个点是最小合法输入 | 三阶拟合的自由度 | 点数等于自由度时残差为零 |

前两条合起来唯一刻画最小二乘的三阶拟合，且都与怎么解方程无关：少了前者，任意三次曲线都能过关；少了后者，残差正交只说明投影方向对，不说明投影到了三次多项式空间。

十二类注入全部被捕获。三处是自查时发现的判据缺陷，都不是实现的问题：

- **残差正交曾引用实现的 `TERMS`。** 把阶数从 3 改成 2 时，检查范围跟着缩到 `u²`，`u³` 那列的失配就看不见了——与 5.25 的 `PAN_OFFSET` 是同一个自证循环。改成从 `Pseudocode 85` 抄字面量 3 之后才抓得到。
- **测试辅助把功率全放在实部、虚部留零。** 「漏掉虚部功率」这条注入一条判据都不失败，因为虚部路径根本没被执行到。改成两部分摊，并另加一条不依赖该辅助的正交分量等价判据。
- **时隙区间判据只要求「后半段有非单位增益」。** 注入「区间从 0 起而非 `first`」时混进了前半段，仍然有斜率，弱断言照样成立。改成与单独输入逐位相同，并另断言两段结果不同，避免相等变成平凡的。

**`+1` 地板的两个后果都记在判据里。** 它防止 `log10(0)` 变成 `−∞`：全静音退化成平坦包络、增益恰为 1。代价是**平移不变只在功率远大于 1 时成立**——最初用最小功率约 6 的包络写那条判据，实测偏差 2,1 %，那是规范的行为不是实现的缺陷。

增益向量已由 `5.7.6.4.1.4` 的 HF 信号生成消费，参数模块已经 5.38–5.41 串入受限的真实声道诊断路径。`5.7.6.4.1.3` 见下节。

### 5.28 A-SPX 子带音调噪声比调整数据（`5.7.6.4.1.3`）

`Pseudocode 86`–`88` 与表 195，落在 `crates/macindecode-ac4-bitstream/src/aspx/tna.rs`。产出逐 QMF 子带的复数预测系数 `alpha0`/`alpha1`，以及逐噪声子带组的 chirp 因子；两者都由 `5.7.6.4.1.4` 消费。

**规范有一处排印脱漏。** `Pseudocode 87` 第二行写作 `abs(cov[1][2])`，少了 `[sb]` 一维；同段其余六处都带 `[sb]`，且 `cov` 在 `Pseudocode 86` 中就是三维的。按 `cov[sb][1][2]` 实现。这与 `5.7.6.3.5` 的 `Pseudocode 82` 无条件索引 `[1]` 是同一类排印问题（见 5.25）。

**表 195 的两个下标与表的行列相反。** `Pseudocode 88` 取 `tabNewChirp[aspx_tna_mode[sbg]][aspx_tna_mode_prev[sbg]]`——第一维是**当前** mode；而表 195 的行标是 `aspx_tna_mode_prev`、列标是当前。实现的 `NEW_CHIRP` 因此是表 195 的转置。这不是无关紧要的写法差异：表不对称，`prev=None, cur=Moderate` 是 0,9，反过来是 0,0。

**延迟线取的是上一区间的 `[N−4, N)`，不是它的末尾 4 项。** `Q_low_prev` 末尾还有 `ts_offset_hfgen` 项，而 `Q_low[0..ts_offset_hfgen]` 本身就是那几项（`5.7.6.3.2` 的延迟）。取成末尾 4 项会让 `ts_offset_hfgen` 个时隙在协方差里算两遍。`Q_low_ext` 不物化——`num_qmf_timeslots` 取 32 时它是 42 个 `QmfSlot` 约 21 KiB，对 `no_std` 的栈过大，改为取样时直接做下标映射。

**`EPSILON_INV` 的正则化在精确奇异处才看得出作用。** 单频输入下协方差矩阵秩为 1，Cauchy–Schwarz 取等号，`denom` 精确为 0。但那时 `alpha1` 的分子也解析地为零，两条路径给出相同结果——**正则化生效与否在任何真实信号上都难以观察**。判据因此改喂手工装配的矩阵：`solve` 是纯函数，可以把「奇异」与「分子非零」这两件在真实信号上必然同时发生的事拆开。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 全 1 输入下每个元素等于项数 | 累加的起点 4 与步长 2 | 项数写字面量 |
| 五个元素等于手算值 | `Pseudocode 86` 整体 | `z = 1 + i·t`，解析值全是小整数 |
| 对角线是实数且非负 | `Σ z·conj(z)` 的性质 | 虚部逐位为零 |
| `cov[1][2] = conj(cov[2][1])` | 共轭对称 | 用那个算了没人用的元素 |
| `\|cov[1][2]\|² ≤ cov[1][1]·cov[2][2]` | Cauchy–Schwarz | 半正定性 |
| 单频被精确预测 | `Pseudocode 87` 的代数 | `z = i^t`，`alpha0 = 1`、`alpha1 = 0` 精确 |
| 奇异矩阵仍可解 | 正则化 | 手工矩阵，分子非零 |
| `alpha0` 取共轭 | `cplx_conj(cov[1][2])` | 手工矩阵，`alpha1 ≠ 0` 且 `cov[1][2]` 非实 |
| 模恰为 4 即置零 | 「greater than or equal to」 | 上界写字面量，两侧各取一点 |
| 分两个区间与一次算完相同 | 延迟线接得上 | **行为判据**，不看内部下标 |
| 表 195 逐格 | 转置方向 | 表照抄一份，与实现比对 |
| 两支平滑权重 | 3/4 与 29/32 | 权重写字面分数 |
| 阈值恰在 `2^-6` | 「小于」而非「不大于」 | 直接设状态，两侧各取一点 |
| 长度失配的诊断与事务性 | 错误类型与状态不被改写 | 哨兵状态快照 |

十六类注入全部被捕获。**六处判据缺陷是自查时发现的，都不是实现的问题**，其中四处的成因值得单列：

- **`z = i^t` 让每个协方差元素都退化成实数。** `ts` 恒为偶数，于是 `i^{2k}` 只取 ±1，共轭在结果上完全看不出来——「`mul_conj` 不取共轭」这条注入下，Hermitian、对角线实数、单频预测三条判据一条都不失败。换成 `z = 1 + i·t` 后元素带非零虚部（`cov[1][2] = 286 + 12i`），同一注入连 `cov[1][1]` 都不再是实数。**测试信号的对称性可以整体抵消掉一类缺陷**，这比判据写弱更隐蔽。
- **正则化判据自己重算了 `denom`，根本没调用 `solve`。** 它把 `Pseudocode 87` 的表达式在测试里抄了一遍再核对解析值——注入删掉实现里的 `1/(1+EPSILON_INV)` 时，判据纹丝不动。判据必须连到被测对象，抄一份公式再验证那份抄写只证明抄对了。
- **改接到 `solve` 之后，期望值仍然引用实现的 `EPSILON_INV`。** 判据与实现一起漂移：把 `2^-20` 改成 `2^-10`，两边同步变化，零条判据失败；换成字面量 `SPEC_EPSILON_INV` 后立刻捕获。**这是本项目第三次踩同一个自证循环**（`PAN_OFFSET` 见 5.25、`TERMS` 见 5.27），而且是在同一次提交里刚写完「五处判据缺陷已自查」的情况下漏掉的——先前的自查覆盖了测试信号与判据强度，唯独没检查**期望值从哪里来**。
- **`alpha0` 的共轭在 `alpha1 = 0` 时不可观察。** 单频判据里 `alpha1` 解析为零，`alpha1·conj(cov[1][2])` 整项消失，取不取共轭没有区别。

余下两处是量级问题：越界判据用几何增长序列，`|alpha|` 远超 4，把界改成 40 照样触发；chirp 阈值判据只验证「最终归零」，没钉住翻转点，阈值降到 `2^-10` 仍然通过。两条都改成在界的两侧各取一点。

**一处注释与结论相反。** 单频判据原本注着「这条判据因此同时钉住了正则化确实生效」，而同一节的结论恰恰是正则化在真实信号上不可观察——奇异时 `alpha1` 的分子也解析为零。留着它会让后来者以为正则化已被那条判据保护。

**一处诊断修正。** `chirp_factors` 的输出缓冲长度失配最初报成 `NoiseGroupOutOfRange { groups: out.len() }`：不只是错误域名不对，它还会把一个根本没超限的长度（1，而上限是 5）当作「噪声组数超限」报给调用方。已拆出 `OutputLengthMismatch { expected, provided }`。5.25 的 `GridMismatch` 曾把噪声包络数不符报成信号域，同类；那一节的「错误域准确」是一条独立判据，本节此前没有对应的一条。**错误类型本身也是接口，报错的原因不对等于诊断在说谎。**

**一处实现修正。** `alpha /= denom` 最初写成乘以倒数，比真除法多一次舍入；原文是 `/=`，已改回。五项等价变体（开方比较模、两个越界条件对调、平滑分支写成否定形式、协方差改用 `step_by`、`ext_slot` 改用 `checked_sub`）全部保持沉默。

### 5.29 A-SPX HF patch 子带组表（`5.7.6.3.1.4`）

`Pseudocode 71`，落在 `crates/macindecode-ac4-bitstream/src/aspx/patches.rs`。patch 表说明 HF 生成把低带的哪几段搬到 A-SPX 范围、每段多长、源从哪个低带子带起，是 `5.7.6.4.1.4` 的直接输入。

**不随频带表一起推导。** 它多要两个输入：`base_samp_freq` 是 TOC 级的采样率，`aspx_master_freq_scale` 是 `aspx_config()` 的字段而 [`AspxBandTables`] 推完就不再持有。把采样率塞进语法层的配置结构会让那个结构不再只是「码流里读到的东西」，故单列一次派生。

**伪码有两处在 C 里会读到数组之外，都按「不可满足即报错」处理，不静默夹紧。** 内层 `while (sb > sba - source_band_low + msb - odd) { j--; ... }` 没有下界，`j` 减到 0 时条件化简为 `source_band_low + odd > msb`，首轮 `msb = sba`，因此 `sba` 小于 `source_band_low + odd` 就会继续减到 `-1`；`if ((sbg_patch_num_sb[num_sbg_patches-1] < 3) && (num_sbg_patches > 1))` 在段数为 0 时读 `[-1]`，C 的 `&&` 短路救不了它——左操作数先求值，而 `num_sbg_patches > 1` 写在右边。另外 `do`-`while` 的唯一出口是 `sb` 恰好落到上界，这里给了与表长成正比的迭代上限兜底。**全部 904 组合法配置上这三条都不触发**，它们挡的是切错片的调用方。

**源区间恒落在低带内。** 每段的源是 `[start_sb, start_sb + num_sb)`，上端 `start_sb + num_sb = sba − odd ≤ sba`；下端非负由内层 `while` 保证——退出时 `num_sb ≤ sba − source_band_low + msb − odd − usb`，首轮 `msb = sba` 且 `usb = sbx ≥ sba`，其后 `msb = usb`，两种情形都给出 `start_sb ≥ source_band_low > 0`。这是 patch 表有意义的前提：源必须是已经解出来的低带。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 源区间落在 `[0, sba)` | 上面那条不变量 | 全 904 组配置 |
| 段非空且不超过 5 段 | 计数条件与规范上界 | 上界写字面量 |
| 边界是段长的前缀和、起于 `sbx` | `sbg_patches` 的定义 | 全配置 |
| 边界严格递增 | 段非空的推论 | 全配置 |
| 采样率只在 `goal_sb` 落进范围时起作用 | 该参数确实被用到 | 两侧各计数，都要非零 |
| 25 组锚点逐字段复现 | **数值本身** | `scripts/check_patch_tables.py` 独立复算 |
| 全部 904 组输出一致 | 锚点之外的数值 | 两个实现各算同一份 FNV-1a 摘要 |

**规范没给示例表，结构不变量因此不够。** `Pseudocode 71` 不随附任何数值，只能自己造对照。第一轮判据只有结构不变量，实测把 `goal_sb` 两档对调、末段并掉的阈值 3 改成 2、弹回表尾的阈值 3 改成 4、`start_sb` 漏减 `odd`——四项注入**全部照常通过**：它们算出的仍是一张合法的 patch 表，只是数值不对。锚点表补的正是这一层，补上后四项全部被捕获。

锚点由 `scripts/check_patch_tables.py` 从 PDF 的模板表出发逐字重写 `Pseudocode 67`/`68`/`71` 独立复算，与 Rust 实现没有共同来源。25 组锚点逐字段比较以给出可读失败；锚点之外的数值由两边分别把全部 904 组配置及其完整输出按固定顺序送进 FNV-1a，摘要必须一致。脚本还把合法配置数锁在 904，任一 `Unsatisfiable` 都以非零状态中止 CI。

**两处自查发现的判据缺陷：**

- **锚点最初漏掉两个分支。** `num_sb == 0` 的 `else` 分支（904 组里 47 组走到，其中还要 `sbx != sba` 才看得出 `msb` 取 `sbx` 还是 `sba`）与「恰好一段且不足 3 个子带」（24 组，决定并段条件写 `count > 1` 还是 `count > 0`）。对应的两项注入起初照常通过。**注入放行时先要问的是「不可达还是判据没覆盖」**——这两个都是后者，用参考实现统计分支命中数才确认。
- **核对脚本会静默地少读。** 锚点表经 `cargo fmt` 拆成多行后，原来的正则只匹配到 25 组里的 10 组，脚本却照样报「通过」。已加括号计数交叉核对；实测两种正则退化（只认 `scale=true`、要求元组末尾无逗号）都会让它以 `rc=1` 中止。

**一处不是缺陷。** `odd = (sb - 2 + sba) % 2` 里的 `-2` 在模 2 意义下恒等于不减，规范写它只是为了标明基准。把它去掉判据保持沉默——这是正确的，起初曾误当作一项注入。

### 5.30 A-SPX HF 信号创建（`5.7.6.4.1.4`）

`Pseudocode 89`，落在 `crates/macindecode-ac4-bitstream/src/aspx/hfgen.rs`。它是 `5.7.6.4.1` 的汇合点：patch 表（5.29）、TNA 系数与 chirp（5.28）、预平坦化增益（5.27）四路输入在这里第一次同时用上。

**同一次内层循环里有三个子带下标，混用任意两个都只是静默地搬错频段。**

| 下标 | 算式 | 用途 |
|---|---|---|
| `sb_high` | `sbx + sum_sb_patches + sb` | 写入的高带子带，跨 patch 连续累加 |
| `p` | `sbg_patch_start_sb[i] + sb` | 读取的低带子带，每段各自从源起点数起 |
| `g` | `sbg_noise[g+1] == sb_high` 时前进 | 噪声包络，逐时隙从 0 重数 |

`alpha` 与增益向量按 **`p`** 取，chirp 按 **`g`** 取——前者是源子带的性质（预测系数是在低带上算的），后者是目标频段的性质。判据把每个 `(子带, 时隙)` 的源值编码成 `sb*100 + ts`，于是任何一处取错都直接落在断言里。

**延迟状态的推进从 `prediction_filters` 里拆了出来。** `Pseudocode 86` 的协方差与 `Pseudocode 89` 的 HF 生成读的是同一个 `Q_low_ext`；推进若藏在前者内部，后者拿到的前四个时隙会变成本区间的尾部，整条时间轴错位一整个区间。现在两者都收 `ExtendedLowBand` 视图，`TnaDelay::advance` 由调用方在两处消费之后显式调用。这个改动是拆分时才发现的——原先的接口把顺序陷阱藏在了参数签名里。

**`prediction_filters` 把上界从 `sba` 放宽成 `sbx` 是等价变体，不是缺陷注入。** `Pseudocode 86` 只要求 `[0, sba)` 的预测系数；5.29 已穷举证明全部 904 组合法配置的 patch 源都落在这个半开区间，而频带表又恒有 `sba ≤ sbx`。因此传 `sbx` 只会额外计算 `[sba, sbx)`，`Pseudocode 89` 不会读取，多出来的工作量不改变结果。临时替换后 16 条通路判据全部保持沉默，符合这条不变量；不能为抓参数拼写而增加锁死实现的断言。生产路径仍传规范写明的 `sba`，避免无用计算。真正的边界缺陷是少算 `sba - 1`：通路夹具明确要求至少一段 patch 读到该子带，所以上界缩成 `sba - 1` 会被现有成功路径抓住。

**两张表的 `sbx` 现在交叉核对。** `sbg_patches[0]` 与 `sbg_noise[0]` 都定义为交叉子带，但它们由不同的推导产出。实现原本从 `noise_borders[0]` 取 `sbx`，等于把这条前提留给运气；改为从 patch 表取并核对噪声表，不一致报 `CrossoverMismatch`。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 系数全零时退化成纯搬运 | 三个下标的算式 | 源值编码成 `sb*100 + ts`，逐格核对 |
| 两个抽头分别回看 2 与 4 个时隙 | `n−2` 与 `n−4` | 单独置一个抽头为 1 |
| 第二抽头的权重是 `chirp²` | 两个抽头的权重不同 | 取 `chirp = 0,5` 使二者可分 |
| chirp 跟着高带走而不是源 | `g` 的推进 | 噪声边界落在 patch 正中 |
| 预平坦化除以**源**子带的增益 | `1/gain_vec[p]` | 两个源给不同增益 |
| 相邻 patch 首尾相接不重叠 | `sum_sb_patches` 的累加 | 两段源区间故意不连续 |
| 虚部用完整复乘 | 交叉项 | alpha 取纯虚数 |
| 两张表的 `sbx` 一致 | 上面那条前提 | 故意给不同的值 |
| 噪声组数与边界的合法性 | `g` 推进的前提 | 空表、单边界、超上限、非递增各一例，逐项核对错误类型 |
| 拒绝的输入不改写输出 | 事务性 | 哨兵写在**函数真正会写的位置**，整体快照比对 |

十一类注入全部被捕获，其中六类是下标或时间轴混用：`alpha` 按高带取、增益按高带取、chirp 按源取、读取用高带下标、抽头回看 1 与 2、`n` 不加 `ts_offset_hfadj`。三项等价变体（复乘拆成两个抽头分别算、`chirp²` 写成 `powi(2)`、噪声边界比较写成 `>=`）全部保持沉默。

**最后那项等价性有前提，而前提起初没有被检查。** `sb_high` 在整个循环里从 `sbx` 连续递增（patch 段恒非空，见 5.29），且 `sbg_noise` 严格递增，两条合起来才使 `sb_high` 永不跳过某个边界值，于是 `==` 与 `>=` 给出相同的 `g`。前一条由 5.29 的判据保证；**后一条最初只是假定**——实现没有校验噪声边界递增。严格递增不是多余条件：若允许 `[sbx, sbx, sbx, sbz]` 这样的双重复边界，走到 `sbx + 1` 时 `==` 会停在第一个重复边界，`>=` 却会再前进一组，两者结果不同。现已用 `NoiseBordersNotIncreasing` 拒绝这类输入，等价性的两个前提也都落到判据上。

**四处自查发现的缺陷：**

- **事务性判据的哨兵放在了函数不会写的位置。** 原本写 `out[0].re[0]`，而 HF 生成只写 `[sbx, sbx + Σ num_sb)`——低带永远不被触碰，那条断言无条件成立。注入「失败路径写脏输出」时它照常通过，唯一抓到的是另一条恰好检查了 `re[20]` 的测试。改成写 `re[sbx]` 与 `im[sbx+1]` 再整体快照比对。这与 5.27 的空断言是同一类，只是换了个形式：**不是断言恒真，而是哨兵落在被测范围之外**。
- **`last + ts_offset_hfadj` 是裸加。** 模块顶部的 `#![allow(clippy::arithmetic_side_effects)]` 为的是让 `Pseudocode 89` 的下标算式贴近原文，代价是它同时盖住了这个溢出点。改用 `checked_add`。**放宽 lint 的范围越大，越要自己补回被盖住的那类检查。**
- **噪声组数只挡了空表。** 长度为 1 时 `chirp` 也为空即可通过，而规范要求至少一组；报错还用的 `ChirpCountMismatch`，报错域不对，与 5.28 的 `OutputLengthMismatch`、5.25 的 `GridMismatch` 同类。已拆出 `NoiseGroupCountOutOfRange`，并对零组、超上限、chirp 数量不符与边界非递增逐项核对错误类型。
- **`prediction_filters` 的文档过时。** 拆出 `TnaDelay::advance` 后它不再推进状态，doc comment 却还写着「成功时 `state` 前进到本区间」。接口改了而说明没改，比没有说明更糟。

`Q_high` 已由 HF 包络估计及 `5.7.6.4.5` 组装消费；从低带到高频成品的参数模块已齐，并经 5.38–5.41 串入受限的真实声道诊断路径。

### 5.31 A-SPX 限幅器子带组表（`5.7.6.3.1.5`）

`Pseudocode 72`–`74`，落在 `crates/macindecode-ac4-bitstream/src/aspx/limiter.rs`。把低分辨率信号包络表与 patch 边界并成一张表，再把靠得太近的边界合掉，使每个八度大约留两个组。`5.7.6.4.2.2` 的 `Pseudocode 96` 与 `99` 在 `aspx_limiter == 1` 时用它；关闭 limiter 的路径只执行初始增益 `Pseudocode 95`，不需要这张表。

**伪码的一处分号会改变语义。** 原文第二个复制循环写作 `for (sbg = 1; sbg < num_sbg_patches; sbg++);`，行尾的分号让循环体成为空语句，随后的块只在 `sbg == num_sbg_patches` 时执行一次。两条证据表明是排印错误：只复制一个边界与「把 patch 边界并进限幅器表」的意图矛盾；而 `sbg == num_sbg_patches` 时写入的下标 `num_sbg_patches + num_sbg_sig_lowres` 恰好比边界数上界大一格，按字面实现**还会越界写**。按意图逐个复制。

**频带表推导第一次用到实数函数。** `num_octaves = log2(sbg_lim[sbg] / sbg_lim[sbg-1])` 与阈值 `0,245` 比较。等价的乘法形式少一次求值，但 `2^0,245 ≈ 1,185093` 是无理数，写成十进制常量会在边界比值上与规范分歧，因此照原文取 ADR-0005 的 `log2`。

**`sbz` 是必留的终止锚点。** 按 `Pseudocode 72` 字面执行，合并循环会在 904 组中的 **32 组**删掉 `sbz`，例如得到 `sbg_lim = [10, 12, 18, 24]` 而 `sbz = 28`。这与 `5.7.6.3.1` 的通用定义冲突：每张子带组表都要包含最高组的上边界；`Pseudocode 96` 与 `100` 也会把全部 `num_sb_aspx` 个子带映射到限幅器组，末项低于 `sbz` 时会进入未计算的组并继续读越表尾。独立参考实现只能证明两份代码对同一段伪码理解一致，不能消除这个冲突。因此实现把 `sbz` 与 patch 接缝一样视为不可删除的锚点：与普通边界过近时删普通边界，与 patch 锚点过近时两者都保留。

**固定数组的无效后缀必须规范化。** `LimiterTable` 派生 `Eq`，但 `remove_element()` 原先只左移、不清掉旧末项，使两张有效边界完全相同的表因推导历史不同而比较为不等。现在每次删除都清零刚失效的槽位，并用两条不同配置产出同一有效表的回归用例锁定。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 边界严格递增 | 重复边界被消掉 | 全 904 组配置 |
| 首项为 `sbx`、末项为 `sbz` | 表的完整跨度 | 全配置 |
| 每个 A-SPX 子带都映射到现存组 | `Pseudocode 96`/`100` 的下标前提 | 全配置逐子带 |
| 每个边界都来自两个源表之一 | 合并只删不造 | 全配置 |
| 相邻间隔够宽，除非两侧都是必留锚点 | 合并的两个出口 | 阈值写字面 `0,245`；另计该分支命中数 |
| 被删的边界确与留下的靠得太近 | 删除的理由 | 逐配置反查 |
| 组数在上界内且非零 | `num_sbg_lim` 的范围 | 全配置 |
| 相同有效表比较相等 | 无效后缀的规范化 | 两条不同推导路径 |
| 全 904 组的输出摘要 | **数值本身** | 与独立参考实现双向核对 |

九类注入全部被捕获：分号按字面、阈值改 `0,5`、patch 优先级反转、去掉排序、`is_element_of_sbg_patches` 不含末项、`remove_element` 少移一格、循环少查最后一对边界、复制 lowres 漏掉末项、以及摘要能独立发现的其余改动。两项等价变体保持沉默。

**摘要与 patch 表分开算。** 混成一个时，不符只说明「两张表之一变了」，定位不到是哪张。`scripts/check_patch_tables.py` 现在读两个常量，各自比对。

**fail-closed 判据曾被新摘要掩盖。** 脚本加入 limiter 摘要后，旧测试桩只隔离 patch 摘要；即使不注入任何故障，空扫描也会因 limiter 摘要不符而返回失败，三条失败测试因此恒通过。现在测试桩分别固定两个摘要，补了无注入基线和 limiter 摘要失败用例。

**一处归类错误是自查发现的。** 「重复边界时删 `sbg` 还是 `sbg-1`」最初当成注入，实际两个值相等，删任一个剩下的序列完全相同，游标位置也一样——那是等价变体，判据沉默是正确的。这与 5.29 把 `odd` 公式里模 2 下恒等的 `−2` 当成注入是同一类：**动手注入之前先确认它真的改变行为**。

### 5.32 A-SPX HF 包络调整：当前区间的估计（`5.7.6.4.2.1`）

`Pseudocode 90`–`94`，落在 `crates/macindecode-ac4-bitstream/src/aspx/hfadjust.rs`。把 `Q_high` 的实际包络、传输来的标度因子、正弦标记与由它们算出的正弦/噪声电平铺到「QMF 子带 × 信号包络」的矩阵上，供 `5.7.6.4.2.2` 使用。ADR-0005 的六个 `sqrt` 里的头两个在 `Pseudocode 94`。

**`5.7.6.3.3.2` 的 tiling 到这里才有消费者。** `sbg_sig[atsg]` 按包络的 `atsg_freqres` 在高低分辨率两张表之间选一张；组数那一维此前由包络解码逐包络携带，边界表这一维一直悬空，`Pseudocode 90`、`91`、`93` 三处同时用上。

**一处规范笔误。** `Pseudocode 91` 写 `if (atsg_sig[atsg] == atsg_noise[atsg_noise + 1])`，`atsg_noise` 同时充当循环变量与数组名。按上下文右侧应是噪声包络的时间边界表。

**`b_sine_at_end` 曾被误判为第二处笔误。** `Pseudocode 95` 定义它却不在本节读它，读的是 `Pseudocode 92` 的 `p_sine_at_end`——两者算式相同、所属区间不同：`b_` 由**本**区间的 `aspx_tsg_ptr` 与 `num_atsg_sig` 定出，唯一用途是成为**下一**区间的 `p_`。当时的结论「`5.7.6.4.2.2` 应使用本区间的 `b_sine_at_end`」把一个真实的区间差异抹掉了，实现 `5.7.6.4.2.2` 时已按上一区间取值，见 5.33。

由此带来一处顺序陷阱：`estimate` 末尾把本区间的 `b_sine_at_end` 写进跨区间状态，`5.7.6.4.2.2` 再去读就拿到本区间的值。判据因此在推进**之前**固化成 `EnvelopeEstimate::sine_onset`，下游取不到被推进过的状态——与 5.28 的 `TnaDelay::advance` 同类。

**表 53 的字面语义容易读错。** 表 53 已经执行 `aspx_tsg_ptr = tmp - 1`，所以 `Pseudocode 92`/`95` 使用的是解码后的有符号值：`−1` 对应原始码字 0，`0` 从第 0 个包络生效，等于 `num_atsg_sig` 才表示落在末尾，不能再加一。

**`Pseudocode 90` 的时间分母是可由跨条款证据裁决的漏项。** 伪码把累加边界乘了 `num_ts_in_ats`，分母却仍写未乘倍率的 ATS 包络跨度；倍率为 2 时若照抄，会把两个 QMF 时隙的能量相加而不平均。该结果与本节对 signal scale factor 的「QMF 区域平均能量」定义和正文冲突，也与 `Pseudocode 85` 明确除以 ATS 跨度乘时隙倍率的区域平均、`Pseudocode 94`/`95` 的能量配平关系冲突。因此分母采用实际累加的 QMF 时隙数 `(atsg_sig[atsg+1] − atsg_sig[atsg]) · num_ts_in_ats`。这不是为贴合 Dolby 参考输出而调音；真实 2 048 样本帧上，修正只去掉旧实现额外引入的 3,01 dB 估计偏差，PCM 交界相邻两带的整段差改善 1,67 dB。随后对完整节目 7 120 个 A-SPX frame × 9 路 fullband 做 QMF 能量闭合：所有高带的组装能量相对传输目标为 +0,016 dB，分量功率和为 +0,012 dB；首个高带的传输目标相对相邻低带为 −7,731 dB，最终 Q_out 反而已缩到 −5,645 dB。四分钟附近三个 16 秒窗口的所有高带闭合量分别为 +0,032、−0,011、−0,009 dB。由此排除 P95–P104 的补偿增益、限幅、噪声/正弦注入与组装不足，剩余接缝主要属于码流目标。输入摘要、频带指标和排除项见[实验记录](experiments/aspx_crossover_normalization_observation.json)。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 指针保留表 53 的减一结果 | 语法层与重建层的表示边界 | `−1`、`0`、`num_atsg_sig` 三个字面值；两包络下指针 0 从首包络生效 |
| 估计用模平方而非实部平方 | `pow(Q_high, 2)` 的复数语义 | 实虚部各取 3，模平方 18 |
| 倍率 2 按实际 QMF 时隙数归一化 | `Pseudocode 90` 与区域平均定义的冲突 | 32 个单位复样本除以 32 个 QMF 时隙，结果为 2；不会因倍率变成 4 |
| 插值关闭时在子带组内平均 | `aspx_interpolation` 的两支 | 只给组内一个子带功率，两模式对比 |
| 电平取信噪因子的平方根 | `Pseudocode 94` | `scf` 全 1 时因子为 0,5 |
| 正弦落在高分辨率组正中 | `sb_mid` 与表的选择 | **组宽至少 3** 的组 |
| `sine_area` 在低分辨率组内铺开 | `Pseudocode 93` 的传播 | 落在 `sine_idx` 为假的子带上 |
| 正弦标记与末尾指针跨区间延续 | 两份独立的跨区间状态 | 分别让上一列有标记、让上一指针等于包络数 |
| `sine_onset` 的两个下标各属其区间 | `atsg == aspx_tsg_ptr` 与 `atsg == p_sine_at_end` | 连跑三个区间，第三个的指针 `−1` 不指向任何包络，仍靠上一区间的末尾指针在包络 0 成立 |
| 拒绝的输入不改写输出与状态 | 事务性 | 整个结果与状态快照；覆盖短 `Q_high`、非法倍率、短 harmonic 向量 |

判据现覆盖上表各分支。**两处早期判据缺陷是自查发现的，都属于「输入让缺陷不可观察」：**

- **测试配置的第 0 个高分辨率组宽度是 1。** 那时「正中」与「组首」是同一个子带，「正弦放组首」的注入照常通过。低分辨率模板的前十组宽度都是 1，必须显式挑一个宽度 ≥ 3 的组。这与 5.28 的 `z = i^t` 让协方差退化成实数是同一类：判据没写错，是输入把缺陷消掉了。
- **`sine_area` 的判据只看被标记的那个子带。** `sine_idx[mid]` 为真时，传播与否 `sine_area[mid]` 都是真。改成在 `sine_idx` 为假而 `sine_area` 为真的子带上取判据——那才是传播的唯一证据。

**后续审查修正了实现与判读缺陷。** `aspx_tsg_ptr` 被错误地加一，令所有非负指针晚一个包络生效且“末尾”提前一格；P90 的缺项分母曾被过早判成必须照抄，使表 192 的倍率 2 路径多出一倍估计能量；未约束的 `usize` 倍率先截成 `i32` 再相乘，公开 API 可 panic 或回绕；短 `add_harmonic` 向量被静默补成 `false`。现分别以解码值直用、QMF 区域平均、`u8` 的 1/2 白名单和 fail-closed 长度检查修正，拒绝路径比较完整矩阵与两份跨区间状态。

**一处放宽是 `expect` 报出来的。** 测试模块原本带 `#[expect(clippy::float_cmp)]`，而判据比较的是 `Option<f32>`，走的是 `Option` 的 `PartialEq`，根本不触发该 lint。`expect` 在放宽没被用到时会报 `unfulfilled_lint_expectation`，`allow` 不会——这是它比 `allow` 更该优先的理由。

**已被 5.33 消费。** 七张矩阵全部是 `Pseudocode 95`–`101` 的输入。

### 5.33 A-SPX HF 包络调整：补偿增益（`5.7.6.4.2.2`）

`Pseudocode 95`–`101`，落在 `crates/macindecode-ac4-bitstream/src/aspx/hfgain.rs`。把 5.32 的七张矩阵压成三张：`sig_gain_sb_adj` 缩放 HF 生成信号，`noise_lev_sb_adj` 交给 `5.7.6.4.3` 的噪声发生器，`sine_lev_sb_adj` 是要叠加的正弦幅度。ADR-0005 六个 `sqrt` 里的后四个都在这里。

**`aspx_limiter` 决定是否进入 `Pseudocode 96`–`101`。** 表 122 明确定义 `0` 为 limiter off、`1` 为 limiter on；此前因七段伪码没有再写一层条件而误判成「无条件执行」。正确边界是：`Pseudocode 95` 始终计算初始信号增益；关闭时它与 `Pseudocode 94` 的噪声/正弦电平直接成为输出，boost 记为乘法中性元 `1`；打开时才做下面的两级钳制。接口用 `LimiterMode::Off/On` 表达，关闭路径不要求一张永远不会读取的 limiter 表。

**两级钳制。** 增益先被限幅器子带组的上限压住（`Pseudocode 96`–`98`），再按限幅损失的能量整组抬回来（`Pseudocode 99`–`101`）。两级各有硬上限：`MAX_SIG_GAIN = 1e5` 压住上限本身，`MAX_BOOST_FACT = 1,584893192` 压住抬升倍数。抬升按限幅器组统一施加，因此组内一个被压住的子带最终可以高过它自己未限幅时的增益。

**三个常量互不相同，其中两个的初值不对称。** `Pseudocode 95` 的 `EPSILON = 1` 加在实际包络上；`Pseudocode 96`/`99` 的 `EPSILON0 = 1e-12` 防除零。两处的 `nom` 初值不同——`96` 从 `0` 起、`99` 从 `EPSILON0` 起。没有旁证说明这是笔误，按字面各自保留，并专门构造判据把这个 `1e-12` 的差异变成可观察量。

**`EPSILON = 1` 顺带解释了限幅为何在均匀子带组内永不生效。** `sig_gain² = scf/((1+est)(1+noise))` 恒小于 `LIM_GAIN² · scf/est`，故 `max_sig_gain` 一定大于 `sig_gain`。限幅只在组内 `est` 起伏够大时才咬得住，判据必须显式构造这种不均匀输入。

**限幅器表的末项必须是 `sbz`，这里把它变成前置检查。** `Pseudocode 96` 与 `100` 的映射循环写作 `if (sb == sbg_lim[sbg+1]-sbx) sbg++;`，没有上界守卫；`sbg` 不越过表尾只因为 `sbg_lim[num_sbg_lim] - sbx` 恰是 `num_sb_aspx`。这正是 5.31 把 `sbz` 定为不可合并边界的理由。

**尺寸与端点不是来源证明。** 两套合法频带配置都可有 `num_sb_aspx = 22`，却分别从 `sbx = 16` 与 `10` 开始，limiter 的相对边界为 `[0,2,10,16,22]` 与 `[0,2,8,16,22]`；只比较宽度会把两个子带静默归错组。同一频带在 44,1/48 kHz 的 patch 表也可产生首末相同、内部不同的 limiter 表。`EnvelopeEstimate` 现记录完整频带来源，`LimiterTable` 记录频带与 patch 来源，开启路径逐份精确核对；有效 limiter 表的 `Eq` 仍只比较输出边界，不把来源混进值语义。

**`Pseudocode 101` 少一个左括号**（`noise_lev_sb_lim[sb]atsg]`），同一行另外两个赋值都是 `[sb][atsg]`，按之补齐。

**七层外循环合成一层。** `Pseudocode 95`–`101` 各有一层 `for (atsg ...)`，之间没有跨包络的携带量，合成一层与原文等价；`Pseudocode 95` 循环外的 `b_sine_at_end` 属于下一区间，由 5.32 固化。

**开启 limiter 时，`sig_gain_sb` 为零报错而不是照字面算。** `Pseudocode 97` 除以它，而它为零当且仅当 `scf_sig_sb` 为零；`5.7.6.3.5` 的反量化只产出正规数，故它意味着上游已坏。照字面算下去得到 `0 · ∞ = NaN`，会静默污染整条链。关闭路径不执行这个除法，也不虚构错误。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 夹具的频带与限幅器表 | 全部期望值的前提 | 断言 `sbx = 10`、`num_sb_aspx = 36`、限幅器边界 `[10,12,18,26,32,38,46]`；表推导一改先在这里断掉 |
| 均匀区间跑完整条链且两级都不钳 | `95`→`101` 的基线 | `est = 2`、`scf` 全 1，闭式解 `sqrt(0,2)`／`sqrt(0,6)`／`sqrt(1,2)` |
| limiter 关闭只保留初始量 | 表 122 的开关语义 | 复用能让 limiter 咬住的非均匀输入，要求停在 `Pseudocode 95`/`94` 且 boost 恒为 1 |
| 正弦起点包络在两处同时丢掉噪声项 | `95` 与 `99` 的 `atsg == aspx_tsg_ptr` | 两包络、指针指向第 1 个，两包络的四个量分别是 `sqrt(0,2)`／`sqrt(0,5)`／`sqrt(0,6)`／`sqrt(0,75)` |
| 上一区间的末尾指针使包络 0 成为起点 | `p_sine_at_end` 的区间归属 | 连跑两个区间，第二个的指针取 `−1`，永不等于任何包络下标 |
| 上限携带 `LIM_GAIN` 且不越出本组 | `96`／`97`／`98`／`100` 的映射 | 组内 `est` 取 `2` 与 `32`，两者都非零；同时断言相邻组的 boost 不同 |
| boost 饱和于 `MAX_BOOST_FACT` | `100` 的钳制 | `est` 全零使 boost 的分母只剩 `EPSILON0` |
| 上限饱和于 `MAX_SIG_GAIN` | `96` 的钳制 | `scf_sig = 1e12`，上限与增益都要被压到 `1e5` |
| 子带组内有正弦时噪声标度进分子 | `95` 的两支 | `scf_noise = 3`（取 1 会让两支相等），对比有无 `aspx_add_harmonic` |
| 两个 `nom` 初值不可互换 | `96` 的 `0` 与 `99` 的 `EPSILON0` | `scf_sig = 1e-14`，远小于 `EPSILON0` |
| 零标度因子报错而非除零 | 错误类型即接口 | `scf_sig = 0`，要求 `DegenerateSignalGain` |
| 同宽外来频带布局被拒绝 | 完整频带来源绑定 | 两套 `num_sb_aspx = 22`、`sbx` 分别为 16/10 的合法配置 |
| 同端点外来 limiter 被拒绝 | patch/limiter 来源绑定 | 同一频带的 44,1/48 kHz patch 表，limiter 内部边界不同 |
| 有效表相同仍拒绝外来频带来源 | `LimiterTable::source_bands` 的完整来源绑定 | 只改变 `aspx_noise_sbg`：频带表不同，patch 与有效 limiter 表完全相同 |
| 拒绝的输入不改写输出 | 事务性 | 所有拒绝路径后比较完整快照 |

**判据以 22 类缺陷注入与 4 类等价变体验证，另跑不注入的基线。** 22 类全部被捕获，4 类等价变体全部沉默。

**后续代码审查补出三条接口判据。** 原判据全部在 limiter 开启状态下运行，因而看不见合法的关闭配置；拒绝测试只用不同宽度的频带与不同首项的 limiter 表，又分别被 `SubbandCountMismatch`、`LimiterRangeMismatch` 挡住，看不见「尺寸与端点相同但来源不同」。现用一个确实会触发限幅的输入覆盖 off 分支，再用上述两组同形反例锁定完整来源。

**这三条判据自身再经八类注入验证，暴露一处空断言。** 关闭路径的判据原本只在 `aspx_add_harmonic` 全假下断言 `sine_lev_sb_adj == 0`——那在输入里就恒为零，「关闭时不透传 `Pseudocode 94` 的正弦电平」这类注入照常通过。现补一次带正弦的运行（`scf_noise = 1` 使 `Pseudocode 95` 两支相等，只有正弦动），并同时断言一个未被标记的子带仍为零。这是 CLAUDE.md 空断言那一条的又一次复发。

**第八类注入也可由合法配置观察，并补成第四条接口判据。** 先前把 `LimiterTable::matches_sources` 的频带比较误判为冗余，依据是 904 组 patch/limiter sweep 中没有「patch 相同、频带不同、范围仍匹配」的组合；但该 sweep 本来就不枚举与这两张输出表无关的 `aspx_noise_sbg`。取 `derive(false, 0, 0, 0, 0)` 与 `derive(false, 0, 0, 1, 0)`，两张频带表的噪声组数分别为 1/2，完整来源不同，patch 与有效 limiter 表却完全相同且都覆盖 `[10,46]`。让当前 estimate/频带取后者、limiter 来源取前者时，只有 `source_bands` 比较会拒绝；全程不需要非法构造。现已用这对配置锁定完整来源契约，八类行为注入全部被捕获。

**一次注入本身是等价变体。** `let Some(limiter) = limiter else` 改成 `limiter.or(limiter)` 是恒等式，判据沉默是正确的；换成「`LimiterMode::Off` 直接报错」才真正改变行为。又一次印证「动手注入之前先确认它真的改变行为」。

**两处判据缺口是注入实验发现的，根因相同。** 最初的限幅判据把被钳子带的 `est` 取成 `0`、`scf_noise` 取成 `0`——那样限幅最容易咬住，但 `Pseudocode 99` 的 `est · sig_gain_lim²` 与 `Pseudocode 97` 的噪声下调因此恒为零。于是「boost 用未限幅的增益」与「`101` 用未限幅的噪声」两类注入**一条判据都不响**：唯一能观察它们的项，正好被为了触发限幅而挑的输入抹平了。改成 `est` 取 `2` 与 `32`、`scf_noise` 取 `1` 后两类都被捕获。这与 5.28 的 `z = i^t`、5.32 的组宽为 1 是同一类——**判据没写错，是输入让缺陷不可观察**，而这次是「为触发 A 而选的输入恰好屏蔽了 B」。

**`noise_lev_sb_adj` 已被 5.34 消费**，另两张矩阵要等 `5.7.6.4.4` 与组装。

### 5.34 A-SPX 噪声发生器（`5.7.6.4.3`）

`Pseudocode 102`–`103` 与表 D.2，落在 `crates/macindecode-ac4-bitstream/src/aspx/noisegen.rs`，表由 `build_support/noise.rs` 在构建期生成。把 5.33 的 `noise_lev_sb_adj` 逐时隙铺开，乘上 512 个复数，产出与 `Q_high` 同形的 `qmf_noise`。

**表 D.2 只在随附 zip 里。** PDF 正文只给出表名、`num_columns 2` 与 `num_rows 512`，数值在 `ts_10319001v010401p0.zip` 的 `ASPX_NOISE[512][2]`。与附录 A 的 Huffman 码本同一处境，因此同样关在 `audio-decode` 之后，由构建脚本从校验过的 C 文件生成到 `OUT_DIR`，不进版本控制。

**`noise_idx_prev` 是标量，不是矩阵。** `Pseudocode 103` 写 `indexNoise = noise_idx_prev[sb][ts]`，紧随其后的正文却说它是「上一个 A-SPX 区间的**最后一个** `noise_idx`」——单数。三条证据支持标量读法：下标里的 `ts` 属于当前区间，用它索引上一区间的矩阵没有意义；矩阵读法下每个 `(sb, ts)` 各带一个基址，后面 `+ num_sb_aspx·Δts + sb + 1` 的光栅偏移就失去意义；标量读法下整个区间被同一基址平移，区间内偏移恰走 `1 … num_sb_aspx·N`，末项接上下一区间，512 项的表被连续走过。与 `Pseudocode 91` 把循环变量 `atsg_noise` 当数组用是同一类排印错误。

**`ts − atsg_sig[0]` 的单位不一致，按字面保留。** `Pseudocode 102` 的 `ts` 以 QMF 时隙计（循环从 `atsg_sig[0]·num_ts_in_ats` 起），`Pseudocode 103` 减去的却是未乘倍率的 `atsg_sig[0]`。倍率为 2 且起点非零时，区间内第一个时隙的偏移不是 0 而是 `num_sb_aspx·atsg_sig[0]`。「本意应是区间内的时隙序号」只是推测：`% 512` 使字面读法不越界，也不破坏任何规范明写的性质，**没有旁证**说它是笔误。不同于 5.32 的 P90，当前找不到定义、正文或相邻能量公式提供反证，故按字面执行。`aspx_var_bord_left` 使起点确实可以非零（VARFIX/VARVAR 的 I 帧，取值 0–3），因此这一条是可观察的，已有判据锁定。

**列序无法由规范判定。** 表 D.2 只给 `num_columns 2`，正文只说「512 个复数」。「随机相位」与「平均能量为 1」在实虚互换下都成立，数据本身也判不出来。取 C 的常规写法 `{re, im}`，**没有独立证据**；互换的后果是每个噪声样本关于 45° 线镜像，统计性质完全相同，只有与参考解码器逐位对照才可能分辨。

**负起始边界不在本节重复检查。** `Pseudocode 103` 的 `ts − atsg_sig[0]` 只在起点非负时恒非负，而 C 的 `%` 对负数向零取整会给出负下标。`AspxInterval` 只能由 `derive` 构造，而它已用 `FrameError::NegativeStartBorder` 拒绝负起点，`empty()` 的全零边界则被 `EmptyEnvelope` 挡住——这是构造点上可直接指认的不变式，不是假定。

**调整后增益携带完整来源。** `noise_lev_sb_adj` 的列取决于信号包络边界，数值还取决于 `num_ts_in_ats` 下的实际包络估计；行则相对频带表的 `sbx` 编号。因此包络数、总跨度或子带数相同都不足以证明可以相接。`EnvelopeEstimate` 把完整频带、区间与时隙倍率交给 `AdjustedGains`，噪声发生器改收 `AspxBandTables` 而非裸 `sbx`，在写输出前逐项核对来源；子带末端仍用 `checked_add` 做纵深防御。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 表项两两不同 | 反查下标的前提 | 512 项去重；不成立时下面每条的反查都会先报「同时匹配」 |
| 下标逐子带前进一格并绕表 | `Pseudocode 103` 的光栅与 `% 512` | 由写出的复样本反查下标，断言相邻差恒为 1；16×36 = 576 > 512 必定绕一次 |
| 首项落在下标 1 | `+ sb + 1` 的加一 | `master_reset` 后第一个下标是 1 而非 0 |
| 游标接续到下一区间 | `noise_idx_prev` 的标量语义 | 第二区间首项等于第一区间末项加一 |
| `master_reset` 丢弃携带值 | `5.7.6.3.1.1` 的重置 | 先跑出非零携带值，再重置，首项回到 1 |
| 起点不乘时隙倍率进偏移 | `ts − atsg_sig[0]` 的字面单位 | VARFIX、`aspx_var_bord_left = 2`、倍率 2，首项为 `num_sb_aspx·2 + 1` |
| 电平随包络边界换值 | `Pseudocode 102` 的 `atsg` 推进 | 双包络且指针指向第 1 个，两侧电平不同；用错电平会让反查找不到匹配项 |
| 只写 A-SPX 范围 | 输出边界 | 哨兵铺满 64 个子带，范围外必须逐位不变 |
| 同形外来来源被拒绝 | 来源一致性 | `[0,8,16]` 对 `[0,12,16]`、同为 22 子带但 `sbx` 不同的两套频带，以及倍率 1 对 2 |
| 拒绝的输入不改写输出与游标 | 事务性 | 每条拒绝路径比较实际传入的输出与游标快照；短输出单独保存自己的快照 |

**算式判据以 13 类缺陷注入与 4 类等价变体验证，另跑不注入的基线。** 13 类全部被捕获，4 类等价变体全部沉默。反查下标而不是把公式抄进测试，是这一节判据的关键：断言的是下标序列的**性质**（相邻差为 1、绕表、跨区间接续），不是 `Pseudocode 103` 的算式重写一遍。接口审查后来补入频带、区间、倍率三类来源判据；它们属于组合契约，不计入上述 13 类算式注入统计。

**构建期的表判据没有自动化，只能手工注入。** 构建脚本的 `#[cfg(test)]` 不被 `cargo test` 执行，而放宽构建期预算在数据正确时不改变任何输出——第 14 类注入（把逐项单位模预算放宽到 `10⁹`）因此**一条判据都不响**，这不是判据缺口而是自动化边界。手工注入「第 7 项实部加 `10⁻⁵`」验证过：逐项偏离 `1,55×10⁻⁵` 中止构建，而同一扰动下平均能量只偏离 `1,2×10⁻⁸`，远在预算之内——规范明写的「平均能量为 1」比逐项判据弱三个数量级，两条不能互相替代。改动该文件的预算后必须重做这次手工注入。

另需分清这两条拦的是**解析错误**而非文件被换掉：`verify_hash` 在解析之前就核对了 C 文件字节，后者由 `MANIFEST.json` 的 `member_sha256` 挡。

`qmf_noise` 已由 `5.7.6.4.5` 组装消费；噪声发生器与组装器已经 5.38–5.41 串入受限的真实声道诊断路径。

### 5.35 A-SPX 音调生成器（`5.7.6.4.4`）

`Pseudocode 104`–`105` 与表 196，落在 `crates/macindecode-ac4-bitstream/src/aspx/tonegen.rs`。把 5.33 的 `sine_lev_sb_adj` 逐时隙铺开，乘上四个复数，产出与 `Q_high` 同形的 `qmf_sine`。表 196 在 PDF 正文内（不在随附 C 文件里），四项恰是 `i` 的前四个幂，但仍按表的四行写出——表是规范的呈现形式，逐行对照比论证「这确实是 `i^k`」更直接。

**与噪声发生器形似而不同的四处。** 两节的外层循环逐字相同，极易顺手照抄：

- **下标与子带无关。** `Pseudocode 103` 的偏移是 `num_sb_aspx·Δts + sb + 1`，`Pseudocode 105` 只有 `Δts`——同一时隙的全部子带取同一项。
- **虚部另带逐子带符号。** `pow(-1, sb+sbx)` 只乘虚部，且用**绝对**子带号。实部没有。
- **重置条件不同。** 噪声用 `master_reset`（三个配置字段相对上一个 I 帧变化），音调用 `first_frame`（正文：**只**在编解码初始化时为 1）。配置变化不重置音调相位。
- **表长与推进粒度不同。** 噪声表 512 项、每个 `(子带, 时隙)` 走一格；音调表 4 项、每时隙走一格，非首帧基址取上一区间末项加一。

**`sine_idx_prev` 同样是标量。** 与 `Pseudocode 103` 一样写作 `[sb][ts]` 而正文说「最后一个」。这里多一条噪声那边没有的旁证：`sine_idx(sb, ts)` 声明了 `sb` 形参，而函数体除这个下标外**再没用过它**——按标量读，`sb` 恰好成为未使用的形参，与「同一时隙全部子带同相位」自洽；按矩阵读则每个子带各带基址，`pow(-1, sb+sbx)` 那套逐子带符号反而失去意义。

**`ts − atsg_sig[0]` 的量纲问题与 `Pseudocode 103` 完全相同**，同样按字面保留，同样登记在第 7 节。

| 判据 | 覆盖 | 手法 |
|---|---|---|
| 下标逐时隙走一格且全子带共用 | `Pseudocode 105` 无 `sb` 项 | 反查每个被标记子带的表项，先断言同一时隙内一致，再断言相邻时隙差 1 |
| 首帧从下标 1 起 | `first_frame` 分支取 1 而非 0 | 首项必须是表 196 的第 1 行 |
| 非首帧基址接续 | 基址是「末项**加一**」 | 在起点为 0、倍率为 1 的用例中，第二区间首项等于上一区间末项加一 |
| `first_frame` 不是 `master_reset` | 两个重置条件的区别 | 同一游标分叉成续跑与重启两条，要求结果不同；**时隙数取 15**，见下 |
| 符号只在虚部且按绝对子带号 | `pow(-1, sb+sbx)` | 相邻两个被标记子带虚部反号、实部同为零；**`sbx` 取奇数**，见下 |
| 电平随包络边界换值 | `Pseudocode 104` 的 `atsg` 推进 | 双包络且指针取 0，两包络 boost 不同 |
| 起点不乘倍率进偏移 | `ts − atsg_sig[0]` 的字面单位 | VARFIX、`aspx_var_bord_left = 2`、倍率 2 |
| 只写 A-SPX 范围 | 输出边界 | 哨兵铺满 64 子带 |
| 拒绝的输入不改写输出与游标 | 事务性 | 六条拒绝路径后比较完整快照 |

**14 类缺陷注入全部被捕获，4 类等价变体全部沉默，另跑不注入的基线。**

**两处夹具缺陷是注入实验发现的，都属于「参数取值让缺陷不可观察」。**

- **`sbx` 是偶数时「绝对子带号」与「相对子带号」不可分辨。** 默认夹具 `sbx = 10`，于是 `(sb + sbx) % 2 ≡ sb % 2`，把 `sb + sbx` 改成 `sb` 的注入**一条判据都不响**——那根本不是缺陷注入，而是该夹具下的等价变体。改用 `xover = 1` 的配置（`sbx = 11`）后两者才分得开，判据另加一条 `sbx % 2 == 1` 的前置断言把这个要求钉死。904 组配置里有 100 组的 `sbx` 为奇数，可选余地充足。
- **时隙数是 4 的倍数时 `first_frame` 的两条分支重合。** 表长为 4，16 个时隙走完恰好回到起点，游标停在 0，于是「续跑」的 `(0+1)%4` 与「重启」的 `1` 相等。改用 15 个时隙（表 194 允许）后两条分支才分开，判据另加 `carried != 0` 的前置断言。

**判据侧重复了符号逻辑，但不构成自证循环。** 反查表项的辅助函数要还原 `pow(-1, sb+sbx)` 才能匹配，因此那段奇偶判断在生产与判据两侧各有一份。专门做过「两侧同时改成相对子带号」的协同注入：`only_the_imaginary_part_carries_the_absolute_subband_sign` 仍然失败——它直接比较相邻子带虚部的符号关系，不经过反查。**重复本身不是问题，问题是重复处是否被独立的判据覆盖。**

`qmf_sine` 已由 `5.7.6.4.5` 组装消费；音调发生器与组装器已经 5.38–5.41 串入受限的真实声道诊断路径。

### 5.19 OAMD 语义与拓扑配置指纹修正（`crates/macindecode-ac4-bitstream/src/oamd/`、`substream/`、`topology/`）

**`add_per_object_md()` 没有 REUSE 语义。** `6.2.8.5` 给 `object_basic_info` 与 `object_render_info` 各配了一个状态机（`DEFAULT`/`ALL_NEW`/`REUSE`/`PART_REUSE`），而 `add_per_object_md()` 只由逐块的 `b_add_table_data` 控制存在与否，没有任何 reuse 标志。语义侧另有直接条款：`6.3.9.12.1` 规定「If any extended precision metadata element is not transmitted, its value shall be 0」。因此本块未携带该表时，扩展位置与耳机字段恢复默认值，而不是沿用前值。

**该修正在当前材料上不可观察**：八条流共 17 314 个 `object_info_block` **全部**携带 `b_add_table_data`，未携带的次数为 0，新旧行为的分歧点因此为零——端到端 ADM 导出逐字节相同。分支只由构造数据的单元测试覆盖。

**A-JOC substream info 内嵌的 `oamd_common_data()` 从跳过改为完整解析**，并在 `config_fingerprint()` 中清除：它是随机访问点携带的状态刷新，不是解码器配置，否则 present 标志在相邻帧切换会被误判为 topology 重配。

**这条同样没有实测覆盖，且盲区更深**：八条流全部 568 帧的 `b_oamd_common_data_present` 恒为假，新的解析路径一次也没有执行过。ADM 导出里出现的 `trim`、`headphone` 走的是 OAMD payload 自带的 common data，不是这条内嵌路径。**落点判据在此不构成证据**——它只说明别的路径没被破坏。

因此补一条差分：改动前的「只按 `add_data_bytes` 跳过」实现留作 oracle，对两万个伪随机比特串比较两者的比特消耗。两侧都接受的 16 380 个用例中，消耗**逐位相同**，无一例外；另有 3 347 个用例新实现拒绝而 oracle 接受——那是子元素消耗超过声明字节数的畸形输入，旧实现会静默跳到边界，位置正确而内容是错的。删掉 `AdditionalDataUnderflow` 检查后该差分立即失败。

**第三处由 DME 实测触发：presentation 级 EMDF payload 的 substream 下标是逐帧路由，不是解码器配置。** 三条 DME A-JOC 流在携带该 payload 的帧中令 `EmdfInfo.payloads_substream_index = 3`，原始 `substream_index_table()` 因而有 4 项；不携带的依赖帧只保留固定的 presentation、OAMD 与 audio 三项。旧指纹同时保留该下标和原始 `n_substreams`，于是同一配置被交替判成两种拓扑：三条流各出现 14 个配置代次、135 帧相对首帧不同，并有 135 帧等待随机访问；音频历史被清空后，首个依赖帧以「非 I 帧缺少可沿用的 `aspx_config`」失败。

修正后 `EmdfInfo::configuration_copy()` 只保留会改变负载解释的 EMDF version/key，清除 payload 下标与保留填充；`ConfigFingerprint.n_substreams` 也改为 presentation、OAMD、audio 与 HSF 固定映射所引用的下标跨度，而不是原始索引表项数。逐帧索引表仍独立执行引用范围、尺寸与 payload 定位校验，所以这项规范化不会放宽码流边界。三条 DME 流现在各为 1 个配置代次、0 个首帧差异、0 个等待帧，A-JOC 均为 143/143 帧成功。构造回归另以相同固定映射、原始索引表 1 项与 2 项的两帧断言指纹相等，并单独锁定 EMDF 规范化副本。

### 5.36 A-SPX 高频信号组装（`5.7.6.4.5`）

`Pseudocode 106`–`108`，落在 `crates/macindecode-ac4-bitstream/src/aspx/hfassemble.rs`。把 5.33 的 `sig_gain_sb_adj` 乘到 `Q_high` 上，再依次叠加 5.34 的 `qmf_noise` 与 5.35 的 `qmf_sine`，得到本区间的 `Y`。这是 A-SPX 参数侧的最后一步。

**跨帧搬运的是已组装的输出，不是输入。** `Pseudocode 106` 第一段把 `Y_prev[sb][num_qmf_timeslots + ts]` 取回本区间前 `atsg_sig[0]·num_ts_in_ats` 个时隙，正文 NOTE 给了缘由：区间右边界可以越过帧尾，那部分上一帧已经算完。这与 5.23 的低带延迟线是两回事——低带搬输入、按固定偏移取后缀，这里搬成品、长度由两帧边界共同决定。因此 `HfDelay` 与本区间左边界不等时**直接报错**而不是自动截断：串位在这里没有任何自证迹象。越帧量是 `aspx_var_bord_right·num_ts_in_ats`；该边界由 `4.3.10.4.5` 定义，语法表 53 给它 2 比特，表 192 的倍率至多 2，故上界恰为 6，是紧的。

**`num_qmf_timeslots` 不能由调用方自由搭配。** `AspxInterval` 现在保留推导时的名义 `num_aspx_timeslots`，组装器用它与 `num_ts_in_ats` 重新匹配表 189/192 的同一行，再核对显式传入的 QMF 时隙数。否则 16 ATS 的区间可借用另一合法行的 15 QMF 时隙，静默伪造 1 个越帧时隙；6 ATS 配 0 更会在旧的全局上界 6 下被完整接受。可变边界上限也按当前倍率取 `3·num_ts_in_ats`，不是一律放到全局存储上界 6。

**`Pseudocode 107`/`108` 的起点未乘倍率**，写作 `for (ts = atsg_sig[0]; ...)`，而同节的 `106` 与生成侧的 `102`/`104` 都乘了。倍率 2 且起点非零时字面读法多覆盖 `[atsg_sig[0], atsg_sig[0]·num_ts_in_ats)`，而那一段：`102`/`104` 从没往那里写过噪声与正弦；`106` 第一段恰好覆盖它，放的是**上一帧已经加过噪声与正弦**的成品。所以字面读法要么读未定义值，要么二次叠加。本实现按乘过倍率的起点叠加，等价于「字面读法且该区间为零」，而本仓库的生成器只产出 `[first·factor, end·factor)`，字面读法无从表达。登记在第 7 节。

**子带下标有两套约定。** `106` 写 `Y[sb][ts]` 用相对号、读 `Q_high[sb+sbx][ts]` 用绝对号，`107`/`108` 的 `qmf_noise[sb][ts]` 又是相对号。本实现的 `QmfSlot` 一律 64 个绝对子带，噪声与音调生成器也已写在绝对位置，故全程用绝对号。

**三段加法按原顺序分步写，不合并成乘加式**——`a*b+c` 允许后端融合成 FMA，而 ADR-0002 要求舍入次数与目标有无 FMA 无关。判据在同一个复样本上让 `Q_high`、噪声与正弦三路都非零，并选择能让「逐步舍入」「FMA 后再加」「先合并两项再加」得到三个不同 f32 的数值，直接按位锁住运算顺序。

原九条判据覆盖的 14 类缺陷注入全部被捕获、3 类等价变体沉默。其中「延迟段被叠加」那类注入模拟的正是 `107`/`108` 的字面起点，由「取回的头部必须逐位等于延迟缓冲」判据挡住。审查后再补四条回归，共 13 条：跨表或零 QMF 时隙数必须拒绝；倍率 2、右边界 3 必须恰携带 6 个时隙；倍率 1 不能借全局上界接受右边界 4；三段浮点运算必须保留三次舍入。事务性判据也从只比较 `carried` 改为先给 `tail` 填非零哨兵，再比较整个状态。**此前补判据时踩过一次空断言**：越帧缓冲的清零原本从 `HfDelay::new()` 出发断言「未用位置为零」，而它本来就是全零，对「根本不清零」这个缺陷恒真——注入实测一条都不响。改为先填非零残值再组装，缺陷才显形。

### 5.37 A-SPX 输出合并（`5.7.6.5.3`）

落在 `crates/macindecode-ac4-bitstream/src/aspx/interleave.rs`。把延迟后的 `Q_in,ASPX` 与 `5.7.6.4.5` 的 `Y` 相加得到 `Q_out,ASPX`，交给下游 QMF 域处理（`5.7.6.1` 图 6、`5.7.7` 图 7 的 `A-SPX → Advanced Coupling → QMF Synthesis`）。

**这一条款此前被整节推迟，是勘查接线时才发现漏掉的。** `5.7.6.5` 是交织波形编码，实测 `fic`/`tic` 恒为 0，故整节判为不可达。但**不可达的是交织分量，输出合并不是**——低带能出现在 `Q_out` 里全靠这条加法，没有它 A-SPX 就没有出口。

**公式遍历全部 64 个子带，不只是高带。** 因此需要一条独立的**全子带**延迟线，不能拿 `5.7.6.3.2` 的低带延迟顶替：那一步只搬 `sb < sbx`，用它等于默认 `Q_in` 在 `sb ≥ sbx` 处为零。核心带频谱确实只编到交叉频率，但 QMF 分析滤波器组有过渡带，交叉子带附近的泄漏不是严格零，按公式字面应与 `Y` 相加后一并输出。`δ_ASPX = ts_offset_hfgen`，与低带滤波同源，取值 3 或 6。

**「两种交织都不存在」时走哪条分支是判读。** 正文只给了频率交织**相加**与时间交织**替换**两个分支，没写都为 0 的情形。取相加式：替换式会把 `Y` 整个丢掉，而 A-SPX 的全部产出都在 `Y` 里。登记在第 7 节。时间交织的替换分支未实现——它只有两行，但 `aspx_tic_used_in_slot` 是逐时隙掩码，没有真实码流能验证选对了时隙。诊断 PCM 导出因此只接受两种掩码都未使用的帧。

原六条判据的 9 类缺陷注入全部被捕获、2 类等价变体沉默。**跨帧判据一开始漏掉了「历史读错位置」**：它用 `offset = 6`，而 `history_start = MAX_ASPX_DELAY − offset` 恰好为 0，「从后缀读」与「从 0 读」同义，注入无一判据响。补一条 `offset = 3` 的短延迟判据后才显形——又一次「为触发 A 而挑的极端输入屏蔽了 B」。审查后判据补到七条：状态不再只保存当次 `offset` 项，而是与低带延迟一样固定保留最近 6 项；新增表 192 从 3 切到 6 的合法帧长切换，钉住下一帧必须取得完整历史。错误路径也从只比较 `history()` 计数改为先建立非零 `tail`，再比较完整延迟内容。

### 5.38 A-SPX 通路编排（`5.7.6`）

落在 `crates/macindecode-ac4-bitstream/src/aspx/pipeline.rs`。把 5.20 的 QMF 分析与 5.23–5.37 的各工具按 `5.7.6.1` 图 6 串到 `Q_out,ASPX`，再把仅供链路终点使用的 QMF 合成留在 PCM 包装层。`5.7.6` 至此除实测不可达的交织分量（`5.7.6.5.1`/`5.7.6.5.2`，及 `5.7.6.5.3` 的时间交织替换分支）外全部落地。**这一节不新增任何规范判读**——它全部的内容是接线：谁排在谁之前、哪个缓冲对齐到哪条时间轴、哪个跨帧状态在什么时候推进。因此它的判据也和前面各节不同，验的不是公式而是**顺序与对齐**。

五档 QMF 域入口按加进来的工具递进，各有一个只追加终端合成的 PCM 包装器：

| QMF 域入口 | PCM 包装器 | 加进来的工具 | `Q_out,ASPX` |
|---|---|---|---|
| `bypass_frame_qmf` | `bypass_frame` | 分析、`5.7.6.5.3` | 延迟后的输入（`Y ≡ 0`） |
| `frame_with_low_band_qmf` | `frame_with_low_band` | `5.7.6.3.2` | 与参照逐位相同 |
| `frame_with_hf_generation_qmf` | `frame_with_hf_generation` | `5.7.6.4.1.2`–`5.7.6.4.1.4` | 与参照逐位相同 |
| `frame_with_scale_factors_qmf` | `frame_with_scale_factors` | `5.7.6.3.4`–`5.7.6.3.5`、`5.7.6.4.2`–`5.7.6.4.5` | 延迟后的输入**加 `Y`** |
| `frame_with_balanced_scale_factors_qmf` | `frame_with_balanced_scale_factors` | 上一行的平衡式双声道变体 | 两路各自加上自己的 `Y` |

`Y ≡ 0` 入口是**活动 A-SPX 通路内部的诊断参照**：它把分析、合并及两条时间轴的接线定死，供后续各段逐级对照；它不是 A-SPX 未启用模式的规范旁路。Part 1 `6.2.10` 明确 SIMPLE 不激活 A-SPX，`5.7.6.5.3` 又把 `δ_ASPX` 定义为 A-SPX 引入的延迟，因此未启用模式应在进入本通路之前分流，不能凭这条测试路径额外加入 192/384 个样本。QMF 域出口只包含分析侧历史与 `δ_ASPX`，不推进 `synthesis`；PCM 包装器才追加合成滤波器延迟。最终 PCM 判据直接量总延迟，不在测试里重抄两段各是多少——那样只会证明抄对了。

**中间三段「输出与旁路逐位相同」，这条判据在任意错误接线下同样成立。** `Q_low` 与 `Q_high` 只喂下游、不进输出，而 `5.7.6.4.5` 接上之前 `Y` 恒为零。所以每加一段都必须自带**不看输出**的判据：延迟线互校、生成的高频与低带在时间轴上对齐、跨帧状态逐个隔离观察、逐包络的组数与量化档。最后一段落地后阶梯才收口——`Q_out` 减去旁路输出必须**逐位**等于本帧的 `Y`。这一条替掉了此前三条只在 `Y ≡ 0` 时才正确的判据，也让 `prepare_frame` 清 `y` 第一次可观察：`5.7.6.4.5` 只写 `[sbx, sbx + num_sb_aspx)`，而 `5.7.6.5.3` 的相加遍历全部 64 个子带。

**事务性契约：凡能由调用参数判断的错误，都在工作区与跨帧状态改动之前返回。** 依据是这些错误的补救办法都是「修正后用同一帧重试」——区间来自另一种帧长、chirp 模式个数或取值不合法、`add_harmonic` 太短、限幅器表推不出来、高频成品的携带量对不上，调用方都能修。而 `prepare_frame()` 一旦执行就清掉了上一帧的 `y`，十一个跨帧状态里又有七个随通路就地推进（只有包络、正弦与噪声/音调两个游标先算副本、成功后才提交），此时报错等于让调用方拿着半推进的状态重试。现在的顺序是：包络解码（在历史**副本**上）→ 反量化 → 帧布局 → HF 生成表 → 时隙倍率 → 区间 → 限幅器表 → 携带量三项 → `prepare_frame()` → 通路 → 一次性提交。

这条线是逐次收紧出来的，五次修正堵的是同一类漏：表推导排在清理之后；区间校验排在 `prepare()` 之后（该顺序在平衡式入口里已经写对，没有带到单声道入口）；chirp 的**取值**校验留在 `chirp_factors` 内部，只把个数前移了；`CarryoverMismatch` 从组装内部抛出，而它的补救恰恰是「声明前置静音后重试本帧」；包络与正弦在 `assemble_high_band` 之前就提交。

**`master_reset` 的判定与提交必须分开。** 它由「本 I 帧的 `aspx_config` 与上一个 I 帧是否不同」决定，因此天然带状态。若查询本身推进历史，一帧在前置错误处失败、调用方修正后重试，重试就会读到「配置没变」而静默得到 `master_reset = false`——这正好是上一条契约要保护的场景。现在 `frame` 取 `&self` 返回判定，`commit` 另行推进；同一帧的两路声道各问一次也拿到同一份判定。

**两处纵深防御没能构造出可达的输入。** 携带量的两处上界前移之后，`assemble_high_band` 内部的失败路径逐条核对（`NoiseError`/`ToneError`/`AssembleError` 的全部变体）都构造不出来：包络数、频带布局、区间来源、时隙倍率与三路缓冲长度都已在前面定死，空包络由 `AspxInterval::derive` 的单调性检查排除。同理「区间右界短于帧长」在四个间隔类别下都不可达——FIXFIX 与 VARFIX 的终止边界恒等于名义时隙数，FIXVAR 与 VARVAR 是它加上非负的 `aspx_var_bord_right`。两处都写进源码并注明**新增失败路径时要回来补判据**，措辞是「没能构造出」而不是「不可能」：这是逐类核对的结论，不是穷举证明。

**判据缺陷。** 除阶梯本身那条外，注入与自查另外发现七处，两处值得单列：

- **一条判据在验证一条永远错的路径。** `delta_is_two` 原是 `EnvelopeInput` 上的自由字段，于是「`true` + 单声道 `dequantise`」这个组合可以被构造出来，而 `Pseudocode 84` 要求双倍步长与联合反量化成对出现，该组合永远不对。当时的判据 `the_balance_channel_doubles_the_delta_step` 断言的正是这条路径——**判据不是弱，是验错了对象**。现改为由入口按量化模式自己决定，字段不再外露，并另补一条「单声道走单倍步长」的判据（`qscf = 2` 的 fine 档必须恰好给出 128）。
- **倍率 2 的盲区踩了两次。** 区间对齐与 `5.7.6.4.2` 的判据最初都只跑 1024 采样帧，那里 `num_ts_in_ats == 1`，于是「忘了乘倍率」与正确实现完全等价。现由 `FACTOR_TIERS` 让每条相关判据在两档倍率上各跑一遍，并核对 `AdjustedGains` 携带的倍率来源。

其余五处都是「输入让缺陷不可观察」的老问题：`aspx_tsg_ptr = -1` 使 `SinePlacement::starts_by` 恒真，短路掉 `prev_at_end || carried`，整条正弦跨帧延续因此不可观察，改用带指针的区间并把该前提写成前置断言；音调表只有 4 项，16 时隙的区间让游标绕回原处，`ToneCursor` 与 `first_frame` 两条判据同时失效，改用 15 个 QMF 时隙的帧；「两路声道的携带尾部不同」不足以证明状态隔离——共享状态时第二路的尾部根本没被写过，也照样不同，改为与前置静音的参考值比对；`sig_slice` 递出整条定长数组时判据沉默，因为夹具解析出的包络数恰好等于频带表给的组数，另补一条「短包络行必须报错而不是补齐」；`from_parsed` 里对声道越界的重复检查注入后无判据响，因为三个访问器各自都挡了一次——**这一处的处理是删掉那一层**，而不是再加一条判据，否则只是把三层互相兜底变成四层。

52 条判据，两套构建配置下均通过；三条锁住这条边界——单声道与平衡式双声道的 QMF 出口都不推进终端合成状态、PCM 包装器只比它多一次合成，以及平衡式两路的合成历史各归各路。

**第三条是注入实验补上的，且它挡的不是输出错。** 把平衡式两路 `synthesise` 的状态实参整体对调，两路 PCM **逐位不变**——每一路的历史仍逐帧一致地只吃自己那一路的时隙，对调是个纯重命名。这条注入原本被登记成缺陷，判据沉默才回头核对：它真正的后果在状态侧，历史落进了另一路的 `AspxChannelState`，等到按声道重置或重新配对时清掉的是错的那份。判据因此不能只比 PCM，还要认状态的归属；能鉴别的前提是两路历史必须已经不同，故夹具跑两帧、四段 PCM 两两不同，并把该前提写成前置断言。**「一帧看不出来」是这条的另一半**：两路合成历史都从全零起，第一帧上对调它们逐位等价。

### 5.39 声道到工具的路由（P2 `6.2.4.4`、`5.7.2.1`）

落在 `crates/macindecode-ac4-bitstream/src/var_element.rs`。把 A-SPX 通路接进真实声道路径要先回答四个问题，四个的共同点是**答错了没有任何一处会报错**：错配的参数、错分的驱动方式、错序的上混输入，都照样算出有峰值、有能量、落点精确的音频。

**其一，某路核心带 PCM 的 A-SPX 参数在哪。** `var_channel_element()` 分两段传输，两段的元素数不同。`coding_config = true` 时两段的**分组**也不一样：信号 6、7、8 在声道侧属同一个 `three_channel_data()`，在 A-SPX 侧却分属两个元素（一个 `aspx_data_2ch()` 加一个 `aspx_data_1ch()`）。`coding_config = false` 时两侧尾部同为 `2 + 1`，分组一致。LFE 另占声道侧的第一个元素而**不占** A-SPX 下标。`VarChannelElement::signals()` 逐路给出两段落点，判据是双射：期望序列由解析器填好的工作区生成，覆盖 1…16 路 × 有无 LFE × 两种编码配置 × 两档 `aspx_balance`。

**其二，谁和谁必须一起驱动。** `aspx_jobs()` 按 `aspx_balance` 决定单路还是并对。`Pseudocode 84` 要求两路 `qscf` 一起反量化，拆成两次单路驱动算出的标度因子是错的。**这个标志不能从工作区事后重取**——`VarChannelWorkspace::aspx` 明确可跨帧复用，同形状而标志相反的另一帧会静默把一条 `Balanced` 变成两条 `Mono`，而按声道数核对的那种「校验」看的是形状，形状恰恰相同。现改为解析成功时把逐元素的标志固化进 `VarChannelElement`，作业接口不再接收工作区。

**其三，不经 A-SPX 的那几路各自去哪。** 这两类此前被合成一个「放行」，是错的：`4.8.3.11.1` 说 A-SPX 处理 QMF 域信号「except for the LFE channel signal」，而 `5.7.2.1` NOTE 1 进一步说 LFE **不由 A-JOC 处理**，要在输出侧重新并入（`5.7.2.3` `Pseudocode 15`）；SIMPLE 的全频带信号则只是跳过 A-SPX，其 QMF 输出照样进 A-JOC。两者的下游动作不同，故拆成 `LfeQmf` 与 `SimpleQmf`。

**其四，A-SPX 的输出送进 A-JOC 的第几路。** `5.7.2.1` 的 `Pseudocode 14a`：`m'_fb > 3` 时最后 `n_offset = 2 + (m'_fb mod 2)` 路要挪到 A-JOC 输入的**最前面**。`n_dmx_signals = 9` 带 LFE 时 A-JOC 输入是 `[s6, s7, s8, s0…s5]`。`ajoc_input_order()` 给出这个置换，判据的期望表照伪码手推，不引用实现里的 `n_offset` 算式。此前拿 `Pseudocode 15` 的 L/R/C 标志给领头输入补声道标签，证据跨错了接口：前者是重建后的**输出**排序，不能反推通用 `Qin_AJOC` 的输入标签；置换本身由 `Pseudocode 14a` 直接规定，不受这处说明修正影响。

**伪码的 `m'_fb > 3` 在边界上是惰性的。** `n_offset` 在 `m'_fb` 取 2 或 3 时恰等于 `m'_fb`，两个循环双双退化成恒等，与 else 分支同结果；1…16 逐个合法取值核对过，阈值降到 2 逐项相同（升到 4 则不同，`m'_fb == 4` 会漏掉重排）。那道判断真正防的是 `m'_fb < 2` 时 C 里 `m' - n_offset` 的下溢。**注入把阈值写成 2 不会有判据响，那不是判据缺口**——这一条原本被登记成缺陷，判据沉默后逐个取值核对才改判。

**一处互为印证。** `Pseudocode 14a` 用 `Q'in[i + 1]` 跳过 LFE，与 `signals()` 把 LFE 放在传输顺序第 0 位是两处独立来源说同一件事：前者是 A-JOC 的输入约定，后者是 `6.2.4.4` 的语法顺序。

三轮注入共 31 类缺陷全部被捕获、14 类等价变体沉默。另有三处守卫没能构造出可达输入，按纵深防御记在源码里并写明各自的前提；`is_lfe` 里那条 `channel_in_element == 0` 则是**恒真**而非不可达（带 LFE 时第 0 个声道元素恒只有一路），已删。

### 5.40 元素级 A-SPX 驱动（P2 `6.2.4.4`、P1 `5.7.6`）

落在 `crates/macindecode-ac4-bitstream/src/element_drive.rs`。它消费 5.39 的三份路由结果，把同一个 `var_channel_element()` 的全部核心带 PCM 分派到 SIMPLE QMF、A-SPX 单路/平衡式双路或 LFE 对齐通路，再按 `Pseudocode 14a` 的顺序写成 A-JOC 输入。A-SPX 元素里的 LFE 走 `Y ≡ 0` 的活动通路以补 `δ_ASPX`；SIMPLE 元素一路都不进 A-SPX，故只分析、不补该延迟。

**结构错误按整元素预检。** 全部声道必须共用表 189/192 的同一合法帧长；状态与中间量都必须覆盖传输顺序的全部声道；A-SPX 数据/配置的存在性与逐路访问、作业覆盖及 A-JOC 落点在第一路状态推进前走完。此前平衡式短切片会在 `split_at_mut(high)` 处 panic；带 LFE 的元素缺 A-SPX 数据时，则会先推进 LFE，再在后一条 Mono 作业失败。两者现分别返回容量错误与 `MissingAspxData`，状态、工作区和输出保持原样。底层 `PipelineError` 的边界不同：通路启动后的意外内部错误仍可能让此前声道部分推进，调用方必须丢弃整元素输出、重置状态并从随机访问点重新起解，接口文档不再把这类错误误称为可原帧重试。

**codec mode 变化不做猜测性衔接。** `var_codec_mode` 每帧传输，而 `Q_prev`、包络、chirp、正弦、噪声与输出延迟等历史在相应条款中写作「上一个 A-SPX interval」；规范没有说明中间夹入 SIMPLE 帧时哪些历史冻结、哪些随旁路 QMF 推进。原实现只推进 QMF 分析，恢复 A-SPX 时会把切换前的 `AspxOutputDelay` 当作相邻帧，静默重放旧时隙。现由 `ElementChannelState` 提交上一成功帧的模式，任一方向变化都返回 `CodecModeChangeRequiresReset`；只有调用方在随机访问点整体 `reset()` 后才能以新模式起解。该限制登记在第 6 节，取得规范裁决或可控切换向量前不自动清空或补推进。

九条判据覆盖三条音频接线和五类失败边界：LFE/A-SPX 时间轴、SIMPLE 零附加延迟、A-JOC 重排；逐路帧长不一、SIMPLE 表外帧长、平衡式短工作区、LFE 先行时缺数据，以及双向模式变化与显式重置。缺数据分别用「第 0 个元素就缺失」与「前一个元素合法、后一个缺失」两条钉住，后者还要求报告真正的首缺下标。注入 8 类缺陷全部被捕获、4 类等价变体沉默。

**预检层里挑出两处冗余。** 其一，帧长校验原先在查表之后又比一次 `num_qmf_timeslots(frame_len_base) != samples / 64`——`num_qmf_timeslots` 算的正是 `frame_len_base / 64`，而两侧的 `samples` 同源，那个比较**恒假**，与 5.39 里 `is_lfe` 的 `channel_in_element == 0` 同类，已删；真正起作用的是「查得到才是合法帧长」。其二，对合法解析摘要，A-SPX 元素下标连续为 `0..aspx_elements()`；入口处发现 `aspx.len() < aspx_elements()` 时，`aspx.len()` **恰好就是首个缺失下标**，与 `preflight_jobs` 逐作业 `aspx.get()` 的诊断相同。旧检查的诊断并没有错，删除它只是去掉重复的总数校验；保留 preflight 是因为它沿真实作业引用同时核对元素存在性、声道输入与覆盖关系。若调用方同时给出「前置元素内容非法」和「后续元素缺失」，两版的错误优先级可能不同——旧入口先报缺失，新预检可能先报前置内容错误；接口不承诺多重非法输入的报错顺序。主循环里 `source_of` 查不到则确属不可达（路由已核成一一对应），但仍返回错误而不是退回一个看着合理的下标。

### 5.41 A-SPX PCM 诊断导出的边界与失败门禁

`export-aspx-pcm` 落在 CLI Scene batch adapter 与 `corepcm` 写出层。adapter 默认以 `AutoUnique` 选择 presentation，也接受显式零基 `--presentation`，再把 bounded AU 交给 `Ac4DecoderSession(DecodeMode::Core)`；Session 内的同一 A-JOC engine 将逐元素核心带 PCM 交给 5.40，收集按 `Pseudocode 14a` 排列的 A-JOC 输入与已做相对对齐的 LFE，再逐路做 QMF 合成。这是 **A-JOC 前的诊断 PCM**，不是最终场景或对象 PCM。

**出口只放行已确认的子集。** 四个条件在进入数值通路前同时核对：`var_codec_mode` 必须为 A-SPX；`frame_len_base` 必须是 1 536、1 920 或 2 048，使表 192 的 `δ_ASPX` 与 `5.7.1` 的 6 时隙在数值上重合；companding 不得激活；FIC/TIC 掩码不得实际使用。SIMPLE 与短帧的绝对时间轴仍属第 6 节的未决问题，活动压扩与交织分量则尚未实现；四者都显式失败，不会被当成旁路或零效果。这些是合法但尚未支持的编码路径，CLI 因此返回 `unsupported.coding_path`；真正的解析或重建失败仍返回 `parse.failed`。

**门禁只读取解析器固化的帧身份。** `var_channel_element()` 在工作区被复用前把实际 A-SPX 分支可达性写进 `VarChannelElement`；bitstream 层的 `AspxBlocker::check` 只接受该解析摘要与帧长，不再接受调用方另交一组布尔值。成功凭证拥有 codec mode、companding、可达性与帧长身份，`FullAjocDecoder` 在入队及控制到期时都与实际元素复核。三条生产基线命令现在都由 Scene Session 事务提交结果：失败时不交出 Scene 或核心带侧车，并把 selection、合法未支持、内部不变量与 parse/DSP 失败映射为不同 CLI 诊断码。trace 的 `commit_aspx_support`/`commit_aspx_frame` 只映射同一 engine observation 的既有 census 字段，不执行 DSP 或累积 PCM。

**旧迁移 oracle 已删除。** `CoreBand`/`BandExtended`/`Reconstructed` 整文件 sink、`finish_collection`、旧场景事件队列、CLI parser workspace 与 IMDCT overlap 不再由生产或测试构造。raw/MP4 生产命令都先由 batch adapter 定界 AU，再进入同一个 Session 事务；trace 也只调用统一 engine 一次。

**失败不得产出部分 WAVE。** `decode_access_unit` 的解析、驱动、路由、合成或非有限值失败都不返回半帧，并清空对应语法、A-SPX、A-JOC、控制 FIFO 与终端合成历史；整文件 batch 只有全部 AU 成功并完成 edit 投影后才交给既有原子 WAVE writer。Scene/batch 构造测试锁定事务和错误分类，真实 `--stage aspx` 门禁固定 MP4 形状、来源和 SHA-256。

**写出语义不把 LFE 冒充成 A-JOC 输入。** artifact adapter 的 `PcmTrackSource` 分别携带 transport channel、A-JOC input 与对象命名空间内的下标；`PcmTrack::output_index` 只表示最终交织位置。带宽扩展响应写出 `channel_order = "ajoc_input_then_lfe"`，A-JOC 输入路带 `role = "ajoc_input"` 及 `ajoc_input`，LFE 只带 `role = "lfe"`。`cli-result-v1` schema 已覆盖该命令与 `channel_order`，构造用例另锁定 LFE 不获得伪 A-JOC 下标。

标签由 SceneFrame 的 `SceneElementSource` 在 `frame_tracks` 中一次性投影：对象生成 `PcmTrackSource::AjocInput { input_index }`，原生 LFE 生成 `PcmTrackSource::Lfe`，不再从交织位置反推来源。该接缝的判据落在 `decode_check.py` 的 aspx 段（来源串含 `role`，见 5.19d）；实测八条流每条都有 LFE 且排在最后，另有 Rust 构造用例锁定 LFE 不获得伪 A-JOC input 下标。

**这份带宽扩展基线于 2026-08-19 随 P1 `5.3` 联合声道矩阵重新冻结**，当时九条流逐位一致，当前已扩到十二条、两条 channel-based 具名跳过；核心带基线也因矩阵发生在 IMDCT 前而同步更新。脚本固定使用 release 构建，避免本地媒体重复走未优化解码。`probe_ramp_control` 与 `probe_ramp_lengths` 摘要仍相同，与核心带那份的情形一致——两者只在元数据上不同，音频完全相同。

### 5.42 MDCT 联合声道矩阵（`5.3`）

`sf_data()` 解出的输入轨**没有声道含义**。P1 `5.3.1` 明确要求先按声道元素做矩阵，矩阵输出才携带声道顺序并进入 IMDCT。旧链虽然解析并保存了 `b_enable_mdct_stereo_proc`、`sap_mode`、`ms_used`、`sap_data` 与 `chel_matsel`，却逐声道执行「缩放 → 解组 → IMDCT」，这些参数没有任何消费者。全带 M/S 的 `I0/I1` 因而被直接当成 L/R；这既解释了真实节目 core APAC 的明显左偏，也解释了早期多频探针里 R 与 L 同落 `q0` 的异常结果。

生产顺序现改为「同一 `ChannelElement` 全部声道缩放 → `5.3` 逐窗口组/逐标度因子带矩阵 → 各输出声道解组与 IMDCT」。两声道覆盖 identity、选择性 M/S、全带 M/S 与完整 SAP；SAP 的偶/奇频带复用、频率差分和跨窗口组时间差分按 `Pseudocode 59` 还原 alpha。三声道把两份 2×2 参数按表 178 的十二个 `chel_matsel` 组合为 3×3 矩阵，`12…15` 作为保留值在改写谱线前拒绝。任一联合处理失败会让该元素全部声道的 overlap 状态失效，不能只推进其中一半。

判据分三层。第一层用构造谱线钉住 mode 2 的 `O0 = I0 + I1`、`O1 = I0 - I1`，明确没有隐藏归一化；mode 1 用交替掩码证明只改写选中频带；mode 3 用 alpha 差值 `0/-1` 同时覆盖相邻带复用与 `0.1` 反量化。第二层给两份系数填互不相同的整数，逐项核对表 178 全十二张矩阵，再让选择码 9 实际消费三路谱线。第三层重跑真实向量并重冻 core/A-SPX 两份逐位基线：纯 BED 恢复为 L/R=`q0/q1`、右侧与右后=`q4`，移动对象的 X-min/X-max 恢复为 `q3/q4`；所有九条 A-JOC 向量形状不变，仅 PCM 摘要按预期改变。

外部 A/B 只作方向平衡参照，不冒充逐样本 oracle。同一 157.5 秒 768K 节目按 5.1.4、SAF、Apple 几何渲染并编码 APAC 后，方向左/右能量比由修复前 `+3.419072 dB` 降至 `+0.299882 dB`；DRP 导出的 5.1.4 APAC 为 `+0.469452 dB`，残差 `0.170 dB`。同一 core 未压缩 render 与 APAC 解码的逐声道 RMS 相差不超过约 `0.006 dB`，因此原偏移不来自 APAC、CAF 布局或 AudioToolbox。输入哈希、逐声道 RMS 与产物摘要见 [core APAC 平衡实验](experiments/core_apac_balance_observation.json)。

### 5.43 固定动态 core 网格到扬声器 CAF 的严格直接映射

`export-core-caf` 只对可证明的固定 core 网格提供直接扬声器映射。规范只保证
`b_static_dmx = 0` 的 core 是带第一份 OAMD 的对象；它没有声道模式。因此实现不按码率，
也不只按 `Qin_AJOC` 路数命名布局，而是先证明当前对象图与 9.2k 实测模板等价：所选
presentation 只引用一条动态 A-JOC core substream，存在唯一 `object 0 = LFE BED`，
非 LFE 对象为连续 `1…N = Dynamic`，且 N 只能是 5/7/9/11。每个对象在应用 MP4 edit
后的完整时间线中都必须活动、使用 `ObjectGainState::Default`，标准精度整数坐标逐事件
匹配模板；扩展坐标、非零 width、非默认 zone/channel lock、screen、depth、distance、
infinity 或 divergence 任一出现即返回 `mapping.unsupported`。这是合法对象场景但当前
不能证明直接写声道等价，不属于 `internal.invariant_failed`。活动 core DE、静态下混、
channel-based 与 A-SPX 未实现分支仍在更早层返回 `unsupported.coding_path`。

逐事件门禁还验证插值的来源，不只看目标端点。若可见区起点恰有非零 ramp 的事件，
`project_metadata_events` 会以该事件取代边界状态；CAF 出口因此按同一
`SceneElementId` 回查最后一份 preroll 前态，
并要求它也满足相同位置、活动和增益判据。两端完全相同才可把 ramp 视为恒等插值；缺少
前态或从另一状态渐变即拒绝。presentation 级 OAMD common 也必须逐对象一致且时间线无
冲突；P2 `6.3.9.10` 的自定义 trim、screen 与 bed render/distribution 均未固化，不能在
成功结果里伪称 `unmapped = []`。当前实测编码链固定写表 113 保留的
`warp_mode = 0b11`，同时九份 trim 全是无自定义系数的固定 default/disabled 组合；由于
本出口本来就只对白名单编码器网格开放，实现只对白名单中的完整组合保留既有正向支持，
不赋予保留码任何规范语义，其他 warp/profile 组合全部拒绝。耳机字段不作用于扬声器出口。

通过门禁后，5/7/9/11 路分别使用公开的 CoreAudio layout tag
`MPEG_5_1_A`、`Atmos_5_1_2`、`Atmos_5_1_4`、`Atmos_7_1_4`。前三档按
`q0 q1 q2 LFE q3 q4 [q5 q6 [q7 q8]]` 写出；7.1.4 的 tag 顺序是
`L R C LFE Ls Rs Rls Rrs Vhl Vhr Ltr Rtr`，因此写
`q0 q1 q2 LFE q3 q4 q9 q10 q5 q6 q7 q8`，不能沿 q 顺序直接 interleave。
LFE 没有静音补位：OAMD 或 PCM 任一侧缺失都拒绝。

CAF writer 不依赖 AudioToolbox：file/chunk header、`desc` 与 `chan` 字段按 CAF 写大端，
`lpcm` 数据用 `IsFloat | IsLittleEndian` 标志和 Float32 LE；`finish` 回填 64 位 `data`
chunk 大小并传播 seek/flush 错误。写出在同目录临时文件完成后原子发布，零采样率、半帧、
非有限值和全部长度溢出均在发布前失败。样本只乘精确的 `2^-15`，不扫描 sample peak，
更不估计 true peak；不归一化、不限幅、不削波，有限的 `|sample| > 1` 原样保留。结果
`scale` 固定记录该政策，外部响度/真峰值工具负责后处理并必须保留或重写 `chan` tag。
发布使用同目录 hard link 作为原子 no-clobber 提交：目标名已存在时由文件系统直接拒绝，
不存在 `symlink_metadata` 与覆盖式 `rename` 之间的 TOCTOU 窗口。

判据分三层：writer 单元测试逐偏移核对四个 tag、大小端、chunk size、over-range f32 与
失败传播；构造事件覆盖四套整数网格、起点 ramp 前态、common profile、原子 no-clobber，
以及位置、active、gain、width、zone、distance 等拒绝；
真实 768K 端到端固定 10 声道、288 000 帧、`Atmos_5_1_4` tag、逐轨来源与全部 q 轨有声。
本机 release smoke 又覆盖 256/448/768/1500K，`afinfo` 分别报告正确的 5.1、5.1.2、
5.1.4、7.1.4 顺序。CAF writer 自身不另做重建；当前共享的上游 A-SPX 数值路径还执行
5.44 的 frame alignment。

### 5.44 PCM frame alignment 与 QMF 控制数据对齐（`5.6`、`5.7.2`）

P1 `5.6` 的 frame alignment 不是容器时间线，也不是 A-SPX 自身的
`ts_offset_hfgen`。IMDCT 后的 `pinFA` 必须先按表 188 延迟 `d_pcm` 个样本；同一
raw AC-4 frame 中的 QMF 控制虽然属于该帧的 `sf_data`，解码器却要把它暂存
`d_ctrl` 个完整 codec frame，直到对应的 spectral frontend 信号走到 QMF 工具才应用。
旧链两项都未执行，直接把当前 `VarChannelElement`/A-SPX 控制与当前 IMDCT PCM 配对。
对本项目实测的 48 kHz 音乐档，`frame_len_base = 2 048`、`d_pcm = 352`、
`d_ctrl = 1`；因此高频控制相对 frame-aligned PCM 早了 `2 048 − 352 = 1 696`
samples。这与频谱图中 14 kHz 以上事件相对低频左移的量级一致。

实现把两条延迟分开保存。`frame_alignment` 按表 188 覆盖全部八种合法 codec frame
length，以逐声道固定容量环形缓冲连续执行 `d_pcm`；Full engine 按物理 substream
持有统一 QMF 控制 FIFO，保存同一 raw frame 的 `VarChannelElement`、A-SPX 数据、有效配置、
采样率标志、A-JOC/object controls/矩阵、full 支持凭证与 LFE 插回位置，在 1/2/4 帧后才
把整份快照交给对应的 frame-aligned 信号。解析工作区会在下一 raw frame 被覆盖，所以
FIFO 必须持有所有权，不能保存借用，也不能在到期时混入当前 TOC/矩阵。帧长对应的对齐档
若在未 reset 时变化则 fail-closed；substream、seek 或错误 reset 同时清掉 PCM 环形历史、
控制 FIFO、QMF/A-SPX、A-JOC 与两组终端合成历史。

首份控制到期前不能消费 envelope、chirp、noise、tone、sine 或 HF carry-over，但也不能
让 QMF 滤波器从节目起点的全零历史突然启动。预热入口因此只推进 QMF 分析、低带延迟、
TNA 输入历史、A-SPX 输出延迟和终端 QMF 合成；第一份到期控制仍以
`first_frame = true` 执行并在成功后才提交 `master_reset`。短帧仍由原门禁拒绝：表 188
的 frame alignment 已有明确答案，但第 6 节记录的「终端 QMF 全局 6 时隙与
`ts_offset_hfgen = 3` 如何组成绝对输出时间轴」是另一项未决问题，不能因本修复而顺带放开。

判据分三层。单元测试逐行核对八档 `d_pcm`/`d_ctrl`，覆盖跨任意调用边界与延迟大于
单帧的连续 PCM、事务失败/reset，以及 1/2/4 帧 FIFO 的释放顺序；通路判据证明预热只
推进信号侧历史。九条真实 A-SPX 基线按这次规范性数值变化重冻，形状与逐轨来源不变。
另以 一份本地私有 768K 音乐参考素材的中置/q2 做受控观察：1024 点 Hann STFT、64 samples
hop，分别汇总 200 Hz–13 kHz 与 14–22 kHz 的对数能量并做归一化互相关。修复前相对
DRP 的最佳 lag 分别为 2 432/4 096 samples，带间残差 1 664；修复后两段均为 2 048，
带间残差为 0。修复前后自身的低/高频移动量为 320/2 048，在 64-sample 分辨率内分别
吻合表 188 的 352/2 048。剩余共同的 2 048-sample 起点差不再是频带错位，本观察不据此
裁决 DRP 与 core 导出的容器 priming 原点。输入、算法、相关系数与摘要见
[A-SPX 帧对齐实验](experiments/aspx_frame_alignment_observation.json)。

### 5.45 A-SPX crossover 的 Core PCM/QMF 增益边界

本节首先记录规范风险：P1 `Pseudocode 62`/`64` 与 `5.5.3` 对 2 倍因子的落位不自洽，`5.7.3`/`5.7.6.1` 又没有定义单独的 QMF 接口换算。故以下 A/B 只能与 P65/P66、P82 和连续性一起裁决本实现的内部表示，不能把参考解码器当规范，也不能声称 `5.5.3` 已明文授权 `/2`。决策边界见 [ADR-0006](decisions/0006-core-pcm-qmf-gain-boundary.md)。

同一份 768K 节目按 4 096 点 Hann FFT 汇总除 LFE 外的全带轨，13.125–13.5 kHz 与 13.5–13.875 kHz 的功率比在旧本地 core 直接 5.1.4 为 `-7.976 dB`，DRP 直接 5.1.4 为 `-2.512 dB`；DRP 的 OAR 前 20 路重建对象、OAR 后 9.1.6 与直接 5.1.4 分别为 `-2.524/-2.506/-2.512 dB`。DAP、DRC、limiter 与 IEQ 均关闭，差异在对象 renderer 前已经存在。

M6 full 对象出口落地后完成了隔离 A/B：只在 Core PCM 进入 QMF 时撤销 IMDCT PCM 出口的 `×2`，并在终端 QMF 合成后补回同一因子；不改 QMF 原型窗、A-SPX patch、P90–P104、A-JOC 矩阵或容器。完整节目 core 直接的交界比由 `-7.976367 dB` 变为 `-2.660284 dB`，与 DRP 直接的差只剩 `-0.148687 dB`；本地 20 对象聚合由 `-8.105 dB` 变为 `-2.897883 dB`，与 DRP OAR 前的 `-2.524 dB` 相差约 `-0.374 dB`。13.875 kHz 以上相对交界下方的提升稳定在 `5.962–5.965 dB`，而 13.5 kHz 以下各带的相对谱形只共同移动约 `-0.056 dB`（高带抬升后的 Hann 窗泄漏）。这组“低带端到端电平不变、高带恢复、core/full 同时贴近参考”的结果把根因锁定在 PCM/QMF 接口归一化，而非固定高架 EQ。

当前流是 object audio substream 中的 `var_channel_element`（`var_codec_mode = ASPX`、`b_ajoc = 1`），不是 `immersive_channel_element / ASPX_SCPL`；`4.8.3.11.2` 的 `g = 2` 仍不适用。本修复也没有把该分支增益误套到高带，而是把本实现已用于终端 Core PCM 的 2 倍表示移出绝对标度的 QMF 工具链。A-SPX 与对象两份真实 PCM 基线已随有意数值变化重冻；纯 Core 基线保持不变。

DRP 还表现出独立的输出带宽限制：约 18.0 kHz 开始陡降，18.0–18.375 kHz 相对 13.125–13.5 kHz 已低约 `22.18 dB`，约 18.375 kHz 以上进入约 `-79 dB` 的近停止带，而本地 core 仍延伸到约 21 kHz。故 DRP 的 `>=18 kHz` 频谱不作为 A-SPX 带宽或幅度 oracle；尤其不得用其 18.375 kHz 以上的硬切裁决本地输出。低于这条边界的参照也必须先满足解码层级一致。完整记录见 [A-SPX crossover 实验](experiments/aspx_crossover_normalization_observation.json)。

### 5.46 A-JOC 参数频带到 QMF 子带的映射（`5.7.3.1`）

表 28 把 64 个 QMF 子带摊到 `ajoc_num_bands` 个参数频带，即 `Pseudocode 18` 的 `sb_to_pb()`。生成器从 PDF 的 23 行区间抽取并验证连续覆盖，再展开成 `[列][64 子带]` 的本地 Rust 表；仓库源码只保留查表逻辑。

**结构判据挡不住数值抽取错误。** 单元判据只能验每列单调非降、自 0 起、逐频带满射、末值为 `num_bands − 1`；某个内部值改变后仍可能全部成立。因此 `scripts/check_ajoc_tables.py` 重新从 PDF 抽取表 28，再与本地生成结果逐格核对。

**注入归属分成两边，谁都不能单独覆盖：**

| 注入 | 单元判据 | PDF 脚本 |
|---|---|---|
| 内部值改变但仍保持单调/满射 | **沉默** | 失败 |
| 单调回退、跳过频带 | 失败 | 失败 |
| PDF 区间不连续、列值越界 | 生成失败 | 失败 |
| 运行期列选择逻辑被改坏 | 失败 | **沉默** |

末一行是脚本的固有边界：它核对表**数据**，不替代 Rust 查表逻辑测试。

**表 197 相等是复用的前提，不是顺带的观察。** 表 28 的 15/12/9/7 四列与 P1 表 197 的 A-CPL 映射逐格相同，两份 PDF 独立排印。`5.7.3.5` 的 transient ducker 借用 A-CPL 的 `acpl_max_num_param_bands = 15` 频带划分（P1 `5.7.7.4.3`），`bands.rs` 据此不另存一张表 197。这条相等一旦被推翻，ducker 必须改取独立的表，因此脚本把它作为第二组核对长期锁住。

脚本需要 PDF 与锁定版本的 `pdfplumber`，与 `check_sfb_tables.py`、`check_aspx_tables.py` 同类；CI 的 feature job 会安装依赖、生成并执行全部三项审核。

### 5.47 A-JOC dry/wet 矩阵参数反量化（`5.7.3.3`）

表 29–32 是四张以零为中心的均匀量化表。生成器从 PDF 证明每张表可由统一的 `(levels, midpoint, step_numerator)` 表示；运行期按 `(q − midpoint) × step_numerator / 2 048` 计算，不保存 PDF 中位数不一的十进制近似值。整数分子可由 `f32` 精确表示，分母又是二的幂，因此生成值不会继承 PDF 排印的十进制舍入。

四种入口分别只接受 `0..51`、`0..101`、`0..21`、`0..41` 的半开区间，越界返回结构化错误。单元判据逐项核对公式的 `f32` 位模式，并单独锁定四组端点、中点和两侧越界。

`scripts/check_ajoc_tables.py` 同时从本地生成文件解析四组 `(levels, midpoint, step_numerator)`，再与 PDF 表 29–32 的 214 个排印值逐项比较；容差只允许各单元格最后一个十进制位的四舍五入。这样既不让单元测试用实现常量证明自己，也不把规范为排版而截短的小数误当成精确常量。

### 5.48 A-JOC 矩阵参数差分解码（`5.7.3.2`）

`Pseudocode 16` 的频率方向首项是 F0 绝对量化值，其余项在上一频带上累加并模 `nquant`；时间方向则在上一数据点或上一 AC-4 帧的同一系数上作普通加法，不执行模运算。DT 码本的 `2 × nquant − 1` 个有符号符号恰好覆盖任意两个合法量化索引之间的全部差值；某个符号本身可解码，不代表它与任意历史值的组合都合法。实现因此只对频率方向使用 `rem_euclid(nquant)`，时间累加越出表 29–32 时返回结构化错误，不把负越界循环到量化表另一端。

伪码 wet sparse 分支中的 `mtx_wet_q[o][dp][ch][pb]` 按其声明维度和同节其余行修正为 `[de]`；`ajoc_sparse_select` 与 `ajoc_sparse_mask_wet` 补回对象下标 `[o]`。未传输的 sparse 行明确写入量化中点，并像传输行一样成为后续 DT 历史；inactive 对象和零数据点帧则不销毁最后一次传输历史。

每个对象由调用方持有一份无分配 `DiffState`。入口先复制状态并演算整帧，预检所有 F0、原始索引、DT 兼容性和时间累加范围，成功后才进入不会再失败的提交阶段；因此即便后一个对象或 wet 行失败，先前对象的状态与调用方输出也保持原样。量化模式或频带数变化后，只有先以 DF/sparse 建立新历史才允许 DT，未定义的跨网格映射一律 fail-closed。

### 5.49 A-JOC 参数跨帧时间插值（`5.7.3.4`）

`Pseudocode 17` 的逐系数 `prev_value`/`delta_inc` 与 `Pseudocode 18` 末尾的 `curr_ramp_len`/`target_ramp_len` 属于两个层级：前者分别保存在每个 dry、wet、pre 系数的 rolling 状态中，后者是整个 A-JOC substream 共享的一份时隙游标。实现因此不保存规范注释所写的 `[num_qmf_timeslots][num_qmf_subbands][…]` 展开历史；每个系数只保留 `current`、`target`、`delta`，唯一的时隙入口统一推进三组系数后才把共享游标增加一次。

每个时隙先执行进入该时隙时已有的增量；若 `ts == ajoc_start_pos[dp]`，当前时隙的输出仍是旧 ramp 的结果，随后才以这个 current 计算 `(target - current) / ajoc_ramp_len[dp]`，供下一时隙使用。`Pseudocode 18` 在同一位置把游标重置为零，故一个长度为 `N` 的 ramp 应执行恰好 `N` 次增量；实现以 `completed < ramp_len` 判定，并在第 `N` 次后钉到 target，避免伪码 `<=` 字面产生第 `N + 1` 次增量，也消除重复浮点加法留下的终点尾差。语法将 `ajoc_ramp_len_minus1` 加一，但内部日程构造仍显式拒绝零和大于 64 的长度。

日程绑定当前帧由帧长推导的实际 `num_qmf_timeslots`；5 位 `ajoc_start_pos` 所给的 32 只是上界，不是固定帧长。当前 full 支持集的 1 536/1 920/2 048 样本帧分别有 24/30/32 个 QMF 时隙，构造日程时会拒绝落在本帧末尾之外的起点，逐时隙入口也在改写状态前拒绝不存在的时隙，避免丢掉目标或提前推进跨帧 ramp。

测试中的独立展开 oracle 为每个 QMF 时隙实际保存完整系数快照，且不调用 production rolling helper；它与 production 在 24/30/32 时隙帧、多对象、多参数带、dry/wet/pre 三组、0/1/2 数据点、连续帧以及 `start_pos=0, ramp_len=32` 的跨帧终点上逐个比较 `f32::to_bits()`。该判读的 start-pos 顺序与 ramp 次数另登记在第 7 节，待对象 PCM oracle 裁决。

### 5.50 A-JOC 去相关器与瞬态抑制（P2 `5.7.3.5`；P1 `5.7.7.4`）

表 198 的三个子带区域分别为 `0–6: delay 7/order 7`、`7–22: 10/4`、`23–63: 12/2`，三者都恰好需要 14 个历史复数。实现使用规范等价的 canonical direct-form II：每个 `(decorrelator, subband)` 只保存这 14 个状态，不同时保存 `Pseudocode 111` 的输入与输出历史。QMF 接口保持 `f32`，递归状态和每一步滤波累加使用 `f64`，再在输出边界收窄；测试另有不调用 production helper 的 `Pseudocode 111` 直写递推，在 D0/D1/D2、三个区域的首尾边界和 192 个连续样本上逐项要求 `f32::to_bits()` 相同。

表 198–201 的区域、48 个系数和 A-JOC 七路循环均写入用户本地生成文件。`scripts/check_ajoc_tables.py` 直接从两份 PDF 重新抽取并与生成结果逐项比较；单元测试只负责区域覆盖、14 状态约束和循环边界，不能代替这份外部审核。

`Pseudocode 112`–`114` 按每个 QMF 时隙更新。每一路 decorrelator 独立保存 15 个 `peak_decay/smooth/smooth_peak_diff` 状态，帧边界不重置；输入能量以当前输入 QMF 复数的模平方、按表 197（已由 5.46 证明与表 28 的 15 带列相同）累加，gain 才施加到 all-pass 输出。帧级入口只接受实际提供的 `1..=32` 个时隙，不假定每帧恒为 32；24 与 30 时隙两帧拼接后和连续逐时隙处理的输出及状态逐位相同。瞬态夹具要求 delay 后的混响尾部确实衰减，另覆盖静音、三类 decorrelator、独立状态、有限值和 reset。

形状和输入非有限值在任何状态改写之前拒绝。IIR 推进后的意外数值失败可能已改写候选状态，因此后续对象矩阵重建必须像差分入口一样在整帧状态副本上调用，只有全部对象和时隙成功才提交；这一事务边界留给 5.51 的统一帧入口，而不是让底层逐时隙 helper 为每个子带复制 64 × 14 个状态。

### 5.51 A-JOC 对象 QMF 矩阵重建（P2 `5.7.3.6`）

`Pseudocode 18` 现由一个无分配帧级入口完整串接：先在候选状态上执行差分解码和反量化，再为每个数据点计算 pre target；每个 QMF 时隙统一推进 dry/wet/pre rolling 系数，依次执行 `u = pre × x`、七路以内的去相关，以及 `z = dry × x + wet × y`。输入、rolling 系数、pre 累加、decorrelator 和最终输出均检查有限性；只有全部对象和实际 `1..=32` 个时隙成功后，候选的量化历史、ramp 游标、三组系数和 decorrelator 状态才一起提交。失败时调用方状态逐位不变，输出由上层按帧丢弃。

`ajoc_decorr_enable[de]` 是最终 wet 混合的显式门控，不能由 sparse mask 代替：P2 `6.2.5.3` 的 dense 语法即使该路禁用仍携带 wet 段。禁用路不进入 `z`，但量化、rolling、all-pass 与 ducker 候选状态继续推进，使同一下标重新启用时保留连续历史；dense 非零 wet 的回归夹具同时要求禁用输出等于零 wet 输出、候选状态等于启用路状态。

pre 不是在某个共享参数带网格上近似。dry/wet 的参数带数按对象定义，故实现对每个 `(dp, de, ch, sb)` 直接求 `Σo abs(wet[o,de,sb]) × dry[o,ch,sb]`：每个对象先用自己的表 28 `AjocBandMap` 把该 QMF 子带映回参数带，再以固定对象顺序在 `f64` 中累加并收窄到 `f32` target。这与 `5.7.3.6.2` 的 `D(ts,sb) = |Csub2(ts,sb)^T| × Csub1(ts,sb)` 闭合。绝对值只用于 pre；最终 `wet × y` 保留 wet 的原始正负号。

状态按 QMF 子带保存，所以同一拓扑内允许对象参数频带网格逐帧变化；对象数、下混信号数或 decorrelator 数变化则结构化要求 reset。实现固定支持最多 20 个重建对象，这是当前 CLI full 支持凭证和真实验证矩阵的产品边界，不宣称为规范上限。活动 dialogue enhancement 仍由该凭证拒绝，本入口不会把未实现 DE 分支降级成普通矩阵乘法。每帧在候选计算前清空全部 pre target 和调用方输出工作区，避免上一帧或多余对象槽残留。

单元判据覆盖 dry-only 复数 QMF、非零 wet、正负等幅 wet 的 pre 状态相同而最终输出反号、inactive 对象目标归零但量化历史保留、1/23 带对象共存时各自映射、逐帧 pre/输出完整清零、维度/频带映射/拓扑错误，以及矩阵输出溢出发生在候选状态已推进之后仍不提交状态。真实信号的控制 FIFO、LFE 插回和终端 QMF 合成由 5.52 的产品驱动层覆盖，不由这些构造测试冒充。

### 5.52 A-JOC full QMF 驱动与 LFE 插回（P1 `5.7.2` 表 188；P2 `5.7.2.1`、`5.7.2.3`）

P1 `5.7.2` 要求同一 raw frame 的所有 QMF tool control 在 `d_ctrl` 后才作用于同源 spectral frontend 信号，范围不止 A-SPX。实现因此把 5.44 的 FIFO 扩成单一所有权快照：element/A-SPX/config、采样率与物理拓扑、A-JOC control/矩阵、full 支持凭证及 `SubstreamInfoAjoc::lfe_reinsertion_position()` 一起排队、一起到期。`SupportedAjocFullFrame::check` 只接受同一份 `Ac4SubstreamAjoc` 与 `AjocSubstreamContext`，并把 A-SPX、A-JOC、采样率、物理 substream 数及 dialogue 对象数固化进不可构造的帧身份；原始 `u8` 数据点数不再是公共发证参数，内部防御检查同时拒绝保留值 3 和超出 2 位字段范围的值。入队与到期两处都把凭证身份同实际快照复核。到期后先由 `element_drive` 按 P2 `Pseudocode 14a` 产出 `Qin_AJOC`，再消费 full 凭证进入 5.51 的帧级矩阵事务；当前 raw frame 的矩阵绝不用于已经到期的旧信号。到期快照与当前 frame-alignment 的输入拓扑或对齐档不同会结构化失败并要求 reset。

M5 的无 sink 入口又把 `AudioDataState`、ASF/A-SPX 元素、A-JOC control/矩阵及 core/full OAMD block 六组工作区并入同一个 `FullAjocDecoder`。`decode_syntax_frame` 只返回裁到本帧实际写入长度的借用视图，并固化解析后当前生效的 `AspxConfig`，使不传配置的非 I 帧仍能把继承值交给后续 QMF 入口；A-SPX 与 full 支持凭证也从同一解析摘要生成。截断或语法失败同时失效该物理 substream 的语法与既有 QMF 历史。相同配置连续两帧逐一锁定六组工作区地址不变，容量外 substream 下标在扩容前拒绝。

同一 decoder 现在也拥有 ASF 标度因子还原、反量化、MDCT 联合声道矩阵、解组、IMDCT 工作区与逐物理 substream overlap。`decode_frontend_frame` 在一次所有权事务中返回语法/OAMD 快照、借用的核心带 planar PCM 及逐路数值/守恒 observation；配置期预留 27 路最大元素形状和帧缓冲，稳定帧不更换 PCM 地址。帧长无 reset 改变、任一路重建失败或非有限值都会清除语法、ASF 与下游 QMF/Full 历史，不交出半帧。显式启用核心带诊断时，Scene Session 才把当前 AU 的这份 pre-A-SPX PCM 归一化到可复用侧车，并与 A-SPX/Core、Full Scene 结果一同受 engine 事务约束；普通 renderer 默认不复制它。`export-core-pcm`、A-SPX 与 Full CLI batch 因而都不再自持第二套前端状态。

诊断 census 还需要覆盖「语法/ASF 已成功、A-SPX 或 Full 下游随后拒绝」的帧，不能因原子 DSP 出口返回 `Err` 就丢掉已经验证的前端事实。`FullAjocDecoder::last_syntax_observation()` 与 `last_asf_observation()` 因此借用最近一次调用写入的同一工作区：前者包含解析摘要、有效 A-SPX 配置、A-JOC control/矩阵及两批 raw OAMD，后者包含核心带 PCM 与逐路数值 observation；语法或 ASF 自身失败时对应视图为空。decoder 的每个公共可变入口（包括预分配）都在改写共享工作区前清空两份视图，不会让旧帧长描述符重新借用已缩短的 PCM plane。`ObserveFull` 在前端成功后遇到下游 blocker 时只回滚 QMF/Full 历史，语法配置与 ASF overlap 保留给后续非 I 依赖帧；其他模式仍按完整帧原子失败。feature trace 现在只调用一次 `decode_audio_frame()`，成功帧直接消费 `frontend()`，下游失败帧消费上述只读视图，再由 CLI 聚合既有 JSON 计数、OAMD 轨迹与稳定诊断类别。CLI 自持的 `AudioDataState`、六组 parser workspace、`ScaledStats` 数值执行和 IMDCT overlap 已删除；14 条本地 M4A 的完整 `result.validation.ajoc` 与迁移前逐字段一致。

迁移后的 `export-aspx-pcm` 通过 `DecodeMode::Core` 从同一个 `Q_out,ASPX`/`Qin_AJOC` 出口做终端 QMF 合成，Scene batch adapter 再恢复旧声道顺序和浮点位模式；full 分支另持有 A-JOC 差分/rolling/decorrelator 状态及逐对象终端合成状态。表 188 预热期只推进 frame alignment、QMF/A-SPX 信号历史和两组终端合成：对象侧按已确认输出拓扑合成全零 QMF，不调用矩阵入口，因此不会提前推进差分、ramp 或 decorrelator。A-SPX 诊断 PCM 和对象 PCM 都先暂存在整帧缓冲；任一路重建、插回或合成失败时不向 Scene 提交半帧，Session 随后同时 reset A-SPX、A-JOC、控制 FIFO 和对象合成历史。

P2 `Pseudocode 15` 的 `pos_lfe` 取 full/upmix assignment 派生值。若有 LFE，必须满足 `pos_lfe <= num_umx_signals`，在该位置插入已经过 5.41 相对对齐的 LFE QMF；位置可为 0、对象序列中间或尾端。无 LFE 时对象序列原样通过。输出对象数、LFE 有无/位置或输入声道拓扑无 reset 改变均 fail-closed，避免终端合成状态换路。

构造判据逐项覆盖 `pos_lfe=0/2/3`、无 LFE 原序、越界/控制不一致、输出拓扑变化、full-only reset 与整条 substream reset，并把 element/A-SPX/A-JOC/matrix/LFE 身份组合成一份探针，在表 188 的 1/2/4 帧 FIFO 后核对所有字段仍来自同一 raw frame。真实 `probe_bed_only` 256/448 kbps 各 49 帧均观察到 1 帧零对象预热、48 帧 full 重建和 48 帧启用 wet/去相关执行；`audio_check.sh` 直接读取这三项 DSP 计数，不以矩阵 census 推断执行。两者的既有 A-SPX WAVE SHA-256 保持不变；公开对象 sink 与基线见 5.53。

### 5.53 full A-JOC 对象 PCM 出口与第三份基线

旧 `PcmStage::CoreBand`/`BandExtended`/`Reconstructed` sink 已随整文件迁移 oracle 删除。`DecodeMode::Full` 当前或到期控制没有 `SupportedAjocFullFrame` 时会 reset 整条 substream 并返回 `unsupported.coding_path`，不会退回 A-SPX 诊断 PCM。成功 SceneFrame 以 `SceneElementSource::AjocSpatialGroup` 或 `NativeLfe` 保留来源，CLI 再投影成 `PcmTrackSource::AjocObject { object_index }` 或 `Lfe`；`output_index` 保留 `Pseudocode 15` 插回后的 WAVE 交织位置。表 188 预热仍不推进矩阵、ramp 或 decorrelator；对象 QMF 为零，但已对齐的真实 LFE 继续进入终端合成并保存历史。

`export-objects-pcm` 复用既有 WAVE_FORMAT_EXTENSIBLE Float32 writer：DIRECTOUT、内部 `±32768` 量级、MP4 edit list、目标拒绝覆盖与同目录原子发布均不另起分支。v1 artifact 固定为 `objects_pcm_wave`，`bandwidth = "aspx"`，`channel_order = "ajoc_objects_with_lfe_reinserted"`；对象轨带 `role/ajoc_object/output_channel`，LFE 轨只带 `role/output_channel`。

迁到 Scene Session 后，公开 PCM 是唯一的 normalized planar `f32`，不再为 CLI 保存第二份
内部尺度 plane。batch adapter 在 Scene 外乘精确的 `2^15` 恢复既有 WAVE 量级，受支持真实
媒体继续以 core、A-SPX 和 objects PCM 基线锁定位型；次正规数等无法经过 normalized `f32`
往返的合成内部位型不属于 Scene 或制品契约。

full 驱动错误不靠中文消息分类。`QmfDriveError` 显式区分合法未支持、矩阵重建、对象非有限与对象形状；后三类分别累加 `ajoc_reconstruction_failures`、`objects_nonfinite`、`object_shape_mismatches`，进入同一 `ReconstructionInvariant` 清单。对象导出在通用 `aspx_failures` 总括门禁之前读取具体分类：合法未支持映射 `unsupported.coding_path`，内部数值/形状破坏映射 `internal.invariant_failed`，容器或语法损坏仍为 `parse.failed`。

`scripts/decode_check.py` 现默认连续报告 core/aspx/objects 三段；`--stage objects --update` 只原子更新 `vectors/objects_baseline.json`。对象基线包含 12 条真实 A-JOC 媒体，另两条 channel-based 媒体具名跳过；生成后立即复跑逐位一致。它只证明当前对象输出没有意外变化，不单独证明正确，外部参考解码器逐对象差分留到下一轮。

### 5.54 full A-JOC 对象 ADM 与 Logic RF64/dbmd

P2 `4.8.3.1`/表 6 把 A-JOC 上混限定在 full decoding mode；`4.8.3.4.2` 又在
`b_static_dmx = 0` 时把第一份 OAMD 分给 core、第二份分给 full。因此
`export-full-adm-bwf` 只消费公共 Scene `DecodeMode::Full`，对象 PCM 与 full OAMD 由
Scene Session batch adapter 在**同一趟**解码中采集，不能把 core OAMD 或另一趟解析的
控制时间线套到对象 PCM。
当前 full 支持凭证仍在控制入表 188 FIFO 前检查；末帧命中 blocker 会立即失败，不会等一个
永远不会到期的控制快照，也不会留下输出文件。

配对门禁把 OAMD identity 与 PCM 交织位置分开。P2 `6.3.2.8.1` 的 full assignment 在
存在 LFE 时必须是 `object 0 = Bed/LFE` 和连续 `1…N = Dynamic`，无 LFE 时则是连续
`0…N−1 = Dynamic`；所选 presentation 只能指向一个物理 A-JOC substream。PCM 侧
必须各有唯一 `AjocObject { object_index: 0…N−1 }`，LFE 只按
`PcmTrackSource::Lfe` 定位。P2 `5.7.2.3`/Pseudocode 15 允许 LFE 插到对象序列不同位置，
该位置只记录为 `source_output_channel`；ADM essence 始终使用 `RC_LFE` DirectSpeakers 的
第 4 轨，不把 LFE 冒充 Objects。缺失、重复、错误来源、非连续输出位置、采样率/长度/有限值
不一致全部在 writer 前失败，其中合法未支持分支为 `unsupported.coding_path`，对象数值或
形状破坏为 `internal.invariant_failed`，容器/语法失败为 `parse.failed`。

ADM writer 继续固定 24-bit PCM、节目级单一增益、MP4 edit/preroll 和原子 no-clobber
发布。前十轨是 7.1.2 compatibility bed，仅第 4 轨承载 LFE；full Objects 按 OAMD
`1…N` 从第 11 轨开始。AXML 沿用 active、gain、位置、width、priority、ramp 等映射，
无法等价表示的字段进入 `unmapped`，`--strict-mapping` 将其升级为失败。时间线没有加入
尚未证实的 2400-sample DRP 偏移。

Standard 配置写 BW64、九位 ADM 时钟且无 dbmd；Logic 配置写 RF64、五位时钟及现有 Dolby
dbmd，`interpolationLength` 始终保留纳秒来源的完整精度。dbmd segment 10 使用真实总轨数
`10 + N`，第 4 轨写 LFE 标志，其余轨保持 scene-relative/binaural 配置；`ds64.sampleCount`
在 Logic 中写呈现帧数。768K 真实夹具固定为 20 个 full Objects、30 轨、288000 帧、30 条
CHNA，Logic dbmd 为 564 字节；segment 7/9/10 校验和全部闭合，`29.97df` 的帧率码已覆盖。
Standard/Logic 的 PCM data 和轨序逐字节相同。

### 5.55 真实 full A-JOC DAMF 0.5.1/home 与 0.6.0/3DoF

`export-full-damf` 沿用 5.54 的规范判读与门禁：P2 `4.8.3.1`/表 6 限定 full A-JOC
上混，P2 `4.8.3.4.2` 选择第二份/full OAMD，P2 `6.3.2.8.1` 约束可选 LFE 与连续动态
对象，P2 `5.7.2.3`/Pseudocode 15 决定 LFE 可变输出位置。场景与对象 PCM 必须由
同一趟 Scene Session batch 解码取得；只接受单一物理 A-JOC substream，并按
`PcmTrackSource::AjocObject { object_index: 0…N−1 }`/`Lfe` 来源标签闭合配对。因而 core OAMD、
缺失或重复对象、错误来源、非连续输出、采样率/长度/有限值破坏都不能进入 package writer。

full ADM 与 DAMF 共用唯一节目级增益 `G = min(1, 32768/source_peak)` 和 S24LE 流式
writer。前十轨为 7.1.2 compatibility bed，可选 LFE 只进第 4 轨，其余九轨静音；full
Objects 按 full OAMD 顺序从第 11 轨开始。DAMF CAF 头固定 48 kHz、24-bit signed
little-endian，audio payload 与 Standard/Logic full ADM 的 `data` payload 逐字节相同。
writer 泛化前后的既有 `export-damf` manifest/metadata/CAF 哈希保持不变；只把此前 CLI
结果错误标注的 `caf_s24be` 修正为 `caf_s24le`。

presentation type 是 package 声明，不是对象跟踪状态。home 写
`version: 0.5.1`/`type: home`，3DoF 写 `version: 0.6.0`/`type: 3dof`；两者只允许这两行
manifest 不同，metadata、CAF、对象 ID/顺序、节目增益和时间线必须相同。逐对象
`headTrackMode` 仍从 full OAMD 的 headphone/common 字段映射为 `scene relative` 或
`head relative`，选择 3DoF 不强制改写，也不凭空生成 6DoF 或独立头部姿态轨。

768K 集成夹具固定产出 20 个 full Objects、30 轨、288000 帧，bed ID 为 `0…9`、对象
ID 为 `10…29`。home/3DoF metadata 与 CAF 逐字节一致，manifest 归一化 version/type
后相同；macOS `afinfo` 将 CAF 识别为 30 路、48 kHz、24-bit little-endian signed
integer。本机 ADM normalizer 接受 home package；DME 随附 `atmos_info --validate 1`
分别识别为 DAMF 0.5.1/home theatre 与 DAMF 0.6.0/3dof，且都报告 30 轨、288000 samples。
映射 warning 在 `--strict-mapping` 下于创建目标目录前升级为
`mapping.unsupported`；未支持编码路径、内部 PCM/拓扑破坏和语法失败分别保持
`unsupported.coding_path`、`internal.invariant_failed`、`parse.failed`。

## 6. 未决规范问题

在实现前需要形成明确结论：

- 目标 AC-4 bitstream/presentation version 的最小集合。
- full decode 与 core decode 的产品支持边界。
- ~~direct-object 测试流是否能由现有编码链稳定生成。~~ **已关闭（M2）**：不能。TOC 拓扑显示 `b_channel_coded = 0` 且 `b_ajoc = 1`，两个案例全部 143 帧均为 A-JOC。见测试向量策略 9.2a。取得能产生 direct-object 的编码器前，该路径只有构造码流的分支覆盖。
- A-JOC 输出 identity 的规范语义和稳定性。M2 已查明输出槽位总数即码流声明的 `n_fullband_upmix_signals` 加 `b_lfe`（P2 `6.2.1.9`），与创作对象无关，且单个孤立对象不被摊分（测试向量策略 9.2、9.2a）；但规范层面 identity 的定义、跨帧稳定性要求，以及槽位接近上限时是否仍保持分离，均需核对条款并补充实验。
- **规范内部不一致（P1 `4.3.3.2.7` 与 P2 `6.3.2.1.3`）**：同一个 `b_iframe_global`，Part 1 表述为「所有 presentation 中的所有 substream 的 `b_iframe` 为真」，Part 2 表述为「每个 presentation 的**第一个** substream 独立编码」。二者对随机访问的强度要求不同。本实现按 Part 2 处理（它针对当前覆盖的 `bitstream_version = 2`），并要求全部 ndot 标志为真才判定为完整随机访问点，见测试向量策略 9.2b。
- **规范内部不一致（P2 `6.2.8.5` 与 `6.2.8.10`）**：`add_per_object_md()` 的定义写作 `add_per_object_md(b_object_not_active, b_dynamic_object)`，而 `object_info_block()` 中的调用写作 `add_per_object_md(b_dynamic_object, b_object_not_active)`，两者形参顺序相反。函数体用 `if (b_object_not_active == 0) { if (b_dynamic_object) { b_ext_prec_pos; …` 决定是否读取扩展精度位置，按调用处的顺序绑定，这个条件几乎恰好相反。

**该差异不会被任何比特级门禁发现**：`object_info_block()` 里 `remain_bits = 8 * atd_size - used_bits`，总消耗恒为 `8 × atd_size`，`add_per_object_md()` 内部读多读少都被 `add_table_data` 吸收。实测确认——把绑定顺序换过来，八条流 568 帧的落点判据**照常全部通过**。

差异只在内容层面可见：绑反后 `b_ext_prec_pos` 不再被读取，扩展精度修正整个丢失，ADM 导出的对象 Z 坐标从 `−0.013333` 变成 `0`。本实现按定义的形参名绑定（对象活跃且为动态对象时才读），依据是函数体自身的逻辑自洽，以及该修正在实测流中确实存在且取值合理——**不是比特级证据**。记录在此以备向 ETSI 确认。
- **规范内部不一致（P2 `6.2.1.11` 与表 60）**：`ac4_substream_info_obj()` 的语法表把对象数写作 `[0, 1, 2, 3, 5, 7][n_objects_code]` 且不加 `b_lfe`，而表 60 给出 `[0, 1, 2, 3, 5][code] + b_lfe` 且 `code ≥ 5` 为保留。两者在 `n_objects_code = 4` 且 `b_lfe = 1` 时给出不同结果。差异不影响比特消耗，当前按表 60 实现；需在取得 direct-object 样本后核实，或向 ETSI 确认。
- `ac4_dsi_version = 1` 的处理。表 E.5 要求该字段为 `0b000`，但 `E.4a` 的 `ac4_presentation_v0_dsi` 又带 `if (ac4_dsi_version == 0) … else …` 分支。当前两个探针样本实测均为版本 1；固定头已解析，presentation DSI 仍保留原始字节，该分支语义待核对。
- **`ramp_duration` 的范围表述（P2 `6.3.9.3.8` 与表 95）**：条款声明 `ramp_duration` 的范围是 `[0, 2 047]`，而表 95 的 `ramp_duration_table` 末项为 2 048。本实现理解为该范围只约束 11 比特直接编码的元素，不约束查表值，两者不矛盾。差异不影响比特消耗，因此不会被任何比特级门禁发现，记录在此以备核对。
- **`bed_dyn_obj_assignment()` 不写出 DYN 条目（P2 `6.2.1.10`）**：该语法只向 `obj_type[]` 追加 BED 与 ISF，并对每个条目置 `b_lfe = 0`、`b_ajoc_coded = 1`；DYN 对象由剩余信号数隐含，其 `b_ajoc_coded` 由调用方决定。A-JOC 路径下全部为真，直接编码路径下为假。当前无 direct-object 样本，该推导只有构造码流的分支覆盖。
- **短帧下 `5.7.1` 的全局 6 时隙与表 192 的 `ts_offset_hfgen` 未能调和（P1）**：`5.7.1` 说「6 QMF time slots of history are kept in between blocks. This means that the QMF synthesis works on QMF data that is delayed by 6 QMF time slots, or 6 × num_qmf_subbands time domain samples」，即 384 个样本，且不随帧长变化；表 192 的 `ts_offset_hfgen` 在 `frame_len_base` 取 2 048/1 920/1 536 时恰为 6 个时隙、384 个样本，取 1 024 及以下时只有 3 个时隙、192 个样本。长帧上两者数值重合，短帧上差 3 个时隙，而规范没有说终端 QMF 调度是另加固定 6 个时隙、吸收已有的 A-SPX 延迟，还是只补齐两者的差值。**这已不再决定 LFE 的相对对齐量**：`Q_out,ASPX` 中的直达输入分量相对 `Q_in,ASPX` 明确带 `δ_ASPX`，任何施加给全部输出的公共延迟都不会消掉 LFE 旁路少掉的这段延迟。未决的是短帧的**绝对输出时间轴**及其落点。当前编码链 `frame_rate_index = 13` 恒给出 `frame_len_base = 2 048`，无法观察短帧分歧；`export-aspx-pcm` 只放行两个延迟数值重合的长帧 A-SPX 通路，对 SIMPLE 和 1 024 及以下短帧 fail-closed，没有把该缺口暗自裁决掉。
- **`var_codec_mode` 跨帧变化时 A-SPX 历史如何衔接未定义（P2 `6.2.4.4`、P1 `5.7.6`）**：该位每帧传输，语法没有限制只能在随机访问点变化；A-SPX 各工具却只把来源写成「上一个 A-SPX interval」，没有说明 SIMPLE 帧插在中间时延迟线、预测器与游标是冻结、重置还是随旁路输入推进。三种做法都会改变恢复 A-SPX 后的 PCM，且现有实测流全部恒为 A-SPX，无法裁决。当前元素驱动显式拒绝模式变化，要求调用方在随机访问点整体重置；取得可控切换向量或 ETSI 说明后再放开。
- 渲染前接口相关附录的精确要求。
- PCM 的规范参考精度、舍入和允许容差。
- 错误 concealment 哪些行为是规范要求，哪些属于产品策略。

每个问题关闭时应更新本文件、相关 ADR 和路线图，而不是只保留在讨论记录中。

## 7. 已判读、待外部裁决

与第 6 节不同：这里的每一条都**已经做出选择并写进实现**，只是依据不足以自证。规范在这些地方要么自相矛盾、要么字面与其他条款冲突、要么根本没说。差分参考解码器接上后应逐条核对；在此之前它们是实现里风险最集中的位置。

**判据无法覆盖这一类。** 注入实验只能证明「实现与我的判读一致」，不能证明判读本身对。因此这张表的价值不在于提醒去写更多判据，而在于：**接上 oracle 后按图索骥，而不是靠回忆**。

「逐位可见」指该判读的对错是否会体现在解码 PCM 上；「触发条件」是要让差异显形所需的码流特征——不满足时即使判错也对不出来。

| 条款 | 判读 | 依据强度 | 逐位可见 | 触发条件 |
|---|---|---|---|---|
| P2 `5.7.3.4` `Pseudocode 17` / `18` | 当前时隙先应用旧 delta，再安装命中 `start_pos` 的 target；共享游标在末尾推进一次；长度 `N` 恰好执行 `N` 次增量并钉到 target | 两段伪码的调用/游标更新顺序支持先旧后新；`<=` 与重置为 0 合读会多执行一次，和“ramp length in QMF time slots”矛盾 | 是 | `start_pos=0, ramp_len=32` 会把终点推到下一帧首时隙；非二进制可整除 delta 可区分是否钉终点 |
| P2 `5.7.3.6.1` `Pseudocode 18`、`5.7.3.6.2` | pre target 不共享任一对象的参数带下标；在每个 QMF 子带上用各对象自己的 `sb_to_pb` 映射后求 `Σo abs(wet[o,de]) × dry[o,ch]` | dry/wet 的参数带维度明确带 `[o]`，而 `5.7.3.6.2` 又把等价矩阵 `D` 定义在每个 `(ts,sb)`；伪码把 pre 单写成 `[pb]`，没有定义对象频带网格不同时该 `pb` 属于谁 | 是 | 至少两个 active 对象使用不同 `ajoc_num_bands`，并令窄带对象与宽带对象在高低子带的 dry 或 wet 不同 |
| P1 `5.7.7.4.3` `Pseudocode 112`–`114`；P2 `5.7.3.5` | ducker 的 `*_prev` 取前一个 QMF 时隙并跨 AC-4 帧连续，每个并行 decorrelator 独立持有状态 | **偏离 P1 紧随伪码的「previous frame」字面。** P113 的 `x[sb]` 是当前 QMF subsample，P114 又把 gain 施加到当前 decorrelator 输出；若整帧只更新一次，帧内瞬态不会抑制同帧稍后的 7/10/12 时隙混响尾部，也与该节处理 fast time-envelopes 的目的冲突。P2 只说每一路输出都执行同一 ducker，没有给更粗时间轴 | 是 | 单帧首时隙放脉冲、后续静音；逐时隙读法会衰减同帧 delay 后的尾部，逐帧读法不会 |
| P2 `5.7.3.2` `Pseudocode 16` | wet sparse 的 `[ch]` 按维度修为 `[de]`，两处 sparse 选择补对象下标 `[o]` | 三处均按变量声明维度修正，否则会索引错误或引用错误对象 | 是 | 多对象 sparse wet 数据，且对象、downmix 声道与 decorrelator 下标不相同 |
| `5.7.6.3.1.5` | 把 `sbz` 与 patch 接缝同列为不可合并锚点 | **偏离伪码字面**，依据是 `5.7.6.3.1` 的通则与 `Pseudocode 96`/`100` 的全子带映射 | 是 | 904 组配置里 32 组会命中 |
| `5.7.6.4.2.2` | `aspx_limiter = 0` 时只执行 `Pseudocode 95`，噪声与正弦保持 `Pseudocode 94`，boost 取 1 | 表 122 加 `sbg_lim` 的分组结构；clause 5 全文不提该标志 | 是 | 该标志逐区间变化，实测材料上必然命中。备选读法是「只去掉上限、保留 boost」 |
| `5.7.6.4.2.1` | `Pseudocode 90` 的分母补上 `num_ts_in_ats`，取实际 QMF 时隙数 | **偏离伪码字面，但有较强跨条款证据。** 本节定义与正文都称 QMF 区域的平均能量；P85 明确按 ATS 跨度乘倍率平均；P94/P95 又把该量作为逐 QMF 样本的能量配平项。伪码分母漏倍率会在倍率 2 时凭帧长配置平白增加一倍 | 是 | 倍率为 2 的帧长配置，整条包络差 2 倍 |
| `5.7.6.4.3` | 表 D.2 的 `[0]` 为实部、`[1]` 为虚部 | C 惯例。规范声称的「随机相位」与「平均能量为 1」在实虚互换下都成立，数据判不出来 | 是 | **只有逐位对照能定**，无其他手段 |
| `5.7.6.4.3`、`5.7.6.4.4` | `Pseudocode 103`/`105` 都减去**未乘**倍率的 `atsg_sig[0]` | 纯字面，量纲不一致；取模使字面读法不越界，故无旁证 | 是 | 倍率为 2 **且** `atsg_sig[0] ≠ 0`，即 VARFIX/VARVAR 的 I 帧带非零 `aspx_var_bord_left` |
| `5.7.6.5.3`；P2 `4.8.3.11.1`、`5.7.2.3`、`5.7.3.6.1` | LFE 并回 A-JOC 输出之前，先在 QMF 域延迟 `ts_offset_hfgen` 个时隙 | **较强的跨条款闭合，但规范未直接写出 LFE 延迟。** `5.7.6.5.3` 称 `δ_ASPX = ts_offset_hfgen` 是 A-SPX 引入的总延迟，`4.8.3.11.1` 与 `6.2.10` 又把 LFE 排除在 A-SPX 之外；`Pseudocode 15` 对 LFE 只有直接赋值，说明 `Q'_in,AJOC` 在进入接口前就应对齐。`5.7.3.6.1` 的 dry 分支在同一 `ts` 直接使用输入；decorrelator 的 7/10/12 时隙延迟只属于 wet 分量，不是整个 A-JOC 输出的统一延迟。`5.7.1` 的公共 QMF 时间轴不会消除这段相对差值 | 是 | 任一同时带 LFE 与 A-SPX 全频带信号、且两者含可关联瞬态的流都能区分「补 `δ_ASPX`」与「原样插回」；长帧的 `δ_ASPX = 6` 已足够。短帧只用于另行裁决第 6 节的全局绝对时间轴 |
| P2 `5.7.2.3`、`6.2.1.9`、`6.3.2.8`、`6.3.2.10.8.2`（表 63–66） | `b_reconstruction_contains_Left/Right/Centre_channel` 从 full-decode 的第二份 `bed_dyn_obj_assignment(n_fullband_upmix_signals)` 派生；分别表示上混床分配是否含 L/R/C，`pos_lfe` 是三者的真值数 | **较强的跨条款闭合，但同名标志全文没有定义，属编辑脱漏。** `Qout_AJOC` 的对象数是 `num_umx_signals`，故“corresponding A-JOC reconstruction parameter”只能落到完整解码侧的上混分配，而不是 core/downmix 分配；`6.3.2.8.1` 明说 core/full 的对象类型与位置分别传输，`6.3.2.10.8.2` 又规定静态对象先于动态对象、床对象按表中声道顺序排列。表 63 固定码、表 64/65 标志与表 66 逐声道值都给出唯一的 L/R/C 映射；动态对象与 ISF 不带这些扬声器锚定标签 | 是 | 需要 `b_lfe = 1` 且 full/upmix 侧含床对象；2.0.0 给 `pos_lfe = 2`，其余固定床码给 3。要区分误取 downmix 分配，还需 core/full 两侧床布局不同。现有八条流均为 dynamic-only，只覆盖三者全假、`pos_lfe = 0` |
| `5.7.6.5.3` | `aspx_fic_present` 与 `aspx_tic_present` 都为 0 时，按频率交织那条**相加**式合并 | 正文只给了两种交织各自的分支，未写都不存在的情形。依据是替换式会丢掉整个 `Y` | 是 | 每帧都命中，但两条读法的差异只在 `Q_in` 高带非零时可见，即 QMF 过渡带泄漏的量级 |
| `5.7.6.4.5` | `Pseudocode 107`/`108` 的叠加起点取**乘过**倍率的 `atsg_sig[0]·num_ts_in_ats`，与同节 `106` 及生成侧 `102`/`104` 一致 | 偏离伪码字面。旁证有二：`102`/`104` 从未写过那一段；`106` 第一段放的是上一帧的成品，再叠加即二次添加 | 是 | 倍率为 2 **且** `atsg_sig[0] ≠ 0`。本实现的生成器不产出该区间，字面读法无从表达 |
| `5.7.6.4.4` | `first_frame` 只在编解码初始化时为真，配置变化不重置音调相位 | 正文明写「1 only at the codec initialization stage」，但未说明与 `master_reset` 的关系 | 是 | 配置变化而非初始化的帧；与噪声的 `master_reset` 对照才显形 |
| `5.7.6.4.1.3` | `NEW_CHIRP` 取表 195 的转置 | 伪码下标顺序与表的行列标注相反；表不对称故方向可判定 | 是 | `aspx_tna_mode` 发生变化的区间 |
| `5.7.6.4.2.2` | `Pseudocode 96` 的 `nom` 从 `0` 起、`99` 从 `EPSILON0` 起 | 纯字面，无旁证 | 名义上是 | **实际大概率裁决不了**：差异约 `1e-12`，只在 `scf_sig` 远小于该值时显著，而实测标度因子域不会那么小 |
| `5.7.6.4.2.1`、`5.7.6.4.3`、`5.7.6.4.4`、`5.7.6.4.1.3`、`5.7.6.3.1.5`、`5.7.6.4.2.2` | 七处排印脱漏的补正：`Pseudocode 91` 的 `atsg_noise` 兼作数组名、`Pseudocode 103`/`105` 的 `noise_idx_prev`/`sine_idx_prev` 取标量（后者另有「`sb` 形参未被使用」的旁证）、`Pseudocode 87` 的 `abs(cov[1][2])` 补 `[sb]`、`Pseudocode 72` 行尾分号、`Pseudocode 101` 缺左括号、`Pseudocode 106` 首个双重循环缺左花括号却多一个右花括号（无论补哪一侧，语义都是同一个双重循环） | 较强：各有上下文或越界证据 | 是 | 风险低于上表其余各条，但仍属判读 |

**本地与 oracle 已到达同类对象 PCM 层，但本轮没有执行差分裁决。** 外部参考解码器已可调用，元数据侧的对照也已做过一轮：DRP 4.3.0.19350 arm64 runtime 经本地适配后产出的两份 AXML 与记录哈希一致；上混侧逐对象位置的 664 个点、1992 个坐标全部通过逐轴绝对差 `< 1e-9` 的容差判据，实测最大绝对差为 `4.838629497072588e-13`（见 [ROADMAP](ROADMAP.md) M3 与[实验记录](experiments/reference_decoder_position_crosscheck.json)）。个人适配工具的名称与调用路径不入库。5.53 已公开本地对象 PCM，消除了原先的层级缺口；但逐对象 identity、公共延迟、量级及参考端导出顺序仍须先建立对齐凭证，因此 `objects_baseline.json` 只冻结本地行为，不能据此改动上表判读。

因此旧条目的状态没有变化；等待项从「接通对象路径」变成了「建立外部逐对象对齐后实际比较」。表里标着「只有逐位对照能定」的项目仍然只有那一条路。

**裁决时的规矩**（`docs/TEST_VECTOR_STRATEGY.md` 9.2）：参考解码器是参照物而非真理，三方不一致时以规范裁决。上表任一条与参考解码器冲突时，正确处理是回到条款重新论证，而不是直接改成参考解码器的行为——否则等于把某个实现的行为当成规范。

**新增判读要同步登记此表。** 判读散落在各模块文档里时，接 oracle 的那一步只能靠回忆，而回忆会漏。
