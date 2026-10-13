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

2026-10-12 在 macOS ARM64、Rust 1.98.0、Python 3.13 上，同一份 PDF 的快速生成约
**1.67 秒**，全量扫描约 **48.66 秒**，9036 字节的结果完全相同。该比较仅反映本地提取过程，
不能当作 GitHub 全流程耗时。首轮本地 release 一致性构建约 18.77 秒，后续修改后的测量
以实际运行日志为准。

改动前最近一次 GitHub CI 约 4 分 18 秒，六平台发布约 11 分 46 秒；其中 Linux 音频 job
的规范准备与审计约 2 分 23 秒，Intel macOS 发布的规范准备约 5 分 23 秒。
新流程每个 PR 增加六平台实际解码，比较速度时须同时记录整体等待时间、各 job／step 时间
和总 runner 分钟，区分覆盖增加与同等工作的提速。规范准备、release 编译和测试的分段秒数
写入 Actions job summary；云端冷／暖缓存的完整对比需要该提交在 GitHub 实跑后补录。
