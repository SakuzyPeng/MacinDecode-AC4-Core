# CI 与跨平台逐位回归

每个 PR、符合路径规则的 main push 和手动 CI 都运行六个平台的一致性测试：
Linux musl、macOS、Windows MSVC，各含 x86_64 与 ARM64。平台矩阵使用原生 runner；
测试实际执行 release 二进制，不以交叉编译或 `--version` 代替解码。

## 必需检查

- `quality`：默认 feature 的格式、Clippy、测试、打包、无表 `no_std`、数学和分层审计、Python 测试。
- `audio-decode`：汇总 `audio_checks`、`spec_audit` 和六平台 `consistency`；只有所有依赖
  成功才通过。完整音频／metadata／spec-tables 检查、Rustdoc、有表 `no_std` 和打包仍在 Linux 执行。
- `msrv`：要求默认与音频完整测试成功，并检查实际 rustc、固定工具链、workspace MSRV 和各 crate
  的继承关系。当前三者是同一版本，因此不再单独重复编译；版本分离前须重新设计此检查。

三个名称保持原分支保护上下文。汇总检查使用 `always()`，失败、取消、缺失和意外跳过均不能
视为成功。发布工作流在打包前执行同一套 release 一致性测试。

## 夹具和证据边界

`platform_consistency` 集成测试的三组源码夹具各有 8 帧、每帧 1920 样本、48 kHz。
它们覆盖奇数 dry、奇数 wet、偶数 wet 加 LFE；频谱、噪声包络和 TNA 均为非零，包含 I/P 帧。
测试通过 Full engine 获取 core、A-SPX、objects 三阶段，并运行三个实际 CLI 导出命令，
逐位比较 WAVE PCM、通道来源和整数时间线。每条输出通道都必须有限且有非零样本。

输入流与三阶段 PCM 的 SHA-256 固定在 CLI 测试夹具清单中。PCM 按帧、样本、通道顺序，
以 `f32::to_bits().to_le_bytes()` 编码；不归一化零、不使用误差容限、不按平台放宽。
编码夹具只通过公共码本接口寻找合法码字，不复制规范 C 表；输入摘要防止码本和输出共同漂移。
新增／修改夹具或数值路径时，须审阅原因和六平台结果再人工更新预期值，CI 没有更新基线模式。

反例确认 wet、预测和噪声控制会改变对应 PCM，相同频谱的第二帧继承 overlap；另验证 reset
重放、截断后的依赖帧拒绝和 I 帧恢复。PCM 单位扰动、通道交换、样本减少、非有限／静音输出、
缺失夹具和未执行测试均被拒绝。CLI 错误输入不能留下半份导出文件。

这些合成用例验证受覆盖路径的确定性与回归稳定性，不证明全部 AC-4 码流正确，也不替代
真实媒体的三阶段 `decode_check.py` 本地基线或参考解码器差分。

## 本地执行

```bash
python3 scripts/fetch_specs.py
python3 scripts/generate_spec_tables.py
cargo test --locked -p macindecode-ac4-cli --features audio-decode --test platform_consistency
python3 scripts/ci_support.py consistency
python3 scripts/ci_support.py consistency --target x86_64-unknown-linux-musl
python3 scripts/generate_spec_tables.py --check-full-scan
python3 -m unittest scripts/test_ci_support.py scripts/test_pdf_tables.py
```

release 入口先校验精确测试名单，再要求全部通过且没有 ignored／filtered 测试，避免错误
feature 或筛选条件形成空门禁。`--target` 必须能在当前系统执行；日常复用现有 target 目录。

## 提速与测量

规范准备共用 composite action：按固定依赖安装 Python 包、下载并校验规范摘要、生成并审计本地表。
快速路径仅读取绑定到来源 SHA-256 的页码范围，并校验标题、终止标记和表格完整性；Linux
单独从完整 PDF 扫描生成结果，和快速路径逐字节比较。规范表的冻结摘要不变。

GitHub 缓存仅包含 Cargo 下载和 pip 包，不含规范 PDF、C 表、生成表或 target；缓存未命中
不会跳过检查。Cargo 按系统、架构、feature 用途和锁文件区分，pip 按该 job 实际安装的
requirements 区分。纯规范审计不写 Cargo 缓存，避免先结束的轻量 job 占用键却只保存少量依赖。
各 runner 自行从官方来源生成表，本地不建立另一份缓存。

同一工作流的所有 Cargo 步骤统一设置 `MACINDECODE_AC4_SPEC_DIR`。仅在二进制构建时设置、
测试时撤掉此变量会触发 `build.rs` 重跑，继而重新编译解码器；统一后发布测试复用已有 release 库和二进制。
发布入口通过 `--require-reuse` 检查 Cargo 的产物记录；库或 CLI 发生意外重编译时直接失败。

2026-10-12 在 macOS ARM64、Rust 1.98.0、Python 3.13 上，同一份 PDF 的快速生成约
**1.67 秒**，全量扫描约 **48.66 秒**，9036 字节的结果完全相同。统一规范目录后，本地发布
测试的 Cargo 复用检查约 0.70 秒、7 项测试约 0.13 秒。这些局部数字不代表云端总耗时。

下表取自 GitHub Actions 的已完成成功运行。工作流耗时为 `created_at → updated_at`，包含调度
与收尾；累计 runner 分钟为各非 skipped job 的运行秒数之和除以 60，不做平台计费权重折算。
新版本三个运行均使用代码提交 `baa69f8ceea26f02325e99f2e7a61c81cae314f1`。
逐平台日志确认：冷运行的六个平台均未命中 Cargo／pip 缓存，暖运行均恢复了对应缓存。

| 流程 | 工作流耗时 | 累计 runner 分钟 | 覆盖 |
| --- | ---: | ---: | --- |
| [旧 CI](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/actions/runs/36112018437) | 4 分 18 秒 | 8.28 | Ubuntu 完整检查，独立 MSRV 重复测试 |
| [新 CI 冷缓存](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/actions/runs/38336269608) | 9 分 00 秒 | 29.93 | 完整检查、规范审计、六平台 release PCM |
| [新 CI 暖缓存](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/actions/runs/38337216613) | 8 分 43 秒 | 30.25 | 与冷缓存相同的提交和覆盖 |
| [旧六平台发布](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/actions/runs/36112018477) | 11 分 46 秒 | 36.72 | 六平台构建、版本冒烟和打包 |
| [新六平台发布](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/actions/runs/38336282508) | 8 分 32 秒 | 23.13 | 六平台构建、逐位 PCM、产物复用检查和打包 |

主 CI 新增了六平台实际执行，等待时间和总计算量增加。冷／暖运行的关键路径均为 Intel macOS
release 编译及随后汇总，对应平台 job 分别为 525／507 秒。依赖下载缓存不能消除每次完整编译；
暖缓存的累计 runner 时间也没有下降，不能把单次壁钟差异解释成稳定的缓存加速比例。
例如 Intel macOS 的 Python 依赖准备从 115.2 秒降到 5.4 秒，但 release 编译从 363.2 秒
变为 453.4 秒，抵消了大部分下载缓存收益。

旧记录使用 `a1410f0`，新快照还包含用户已有的 A-SPX／CLI 修改；不同 runner 的负载也有波动，
这些整体数字是运行记录，不是严格隔离变量的 A/B 实验。可单独复核的优化是 PDF 快／全路径
逐字节等价、移除重复 MSRV 测试，以及发布测试要求实际复用已构建产物。

全部六平台的三阶段冻结摘要、CLI PCM／通道语义、重置／截断恢复测试通过。Linux 完整检查、
147 项本地 Python 测试、`no_std`、Clippy、Rustdoc、默认／全 feature 打包和工作流语法检查通过。
真实媒体仍未进入这些 CI 运行。
