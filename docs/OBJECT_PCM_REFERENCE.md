# 对象 PCM 外部差分

本检查比较 `export-objects-pcm` 与外部解码器的渲染前对象 PCM。它独立于
`decode_check.py` 的三层本地逐位基线：前者记录跨实现差异，后者保护既有行为。
参考 runtime、适配器、媒体与 PCM 均不入库；CI 只运行构造信号上的检查器测试。

## 2026-09-23 首轮结果

已使用 DRP 4.3.0.19350 arm64 比较三类输入：单对象轴向探针 768K、含 LFE/wet 的
bed 探针 256K，以及一份真实音乐 768K 的前 400 个完整 sync frame（17.067 秒）。
输入摘要、处理设置、轨道统计与限制见
[`object_pcm_reference_crosscheck.json`](experiments/object_pcm_reference_crosscheck.json)。

| 输入 | 非零轨道 | 已固定尺度和整数延迟后的观察 |
| --- | ---: | --- |
| 单对象 768K | 1/21 | 相关性约 0.99999859；未经增益拟合的 SNR 约 55.48 dB；其他 20 路两侧严格为零 |
| bed 256K | 10/21 | 九路非 LFE 的 SNR 约 45.92–71.25 dB；LFE 约 32.16 dB |
| 真实音乐前 400 AU | 21/21 | 多数承载主要能量的对象高度相关；两个低能量对象在 12–18 kHz 仍有明显残差，未裁决原因 |

这些结果是 **observation，不是逐位一致或全功能通过**。未用观测结果倒推一个刚好能通过的
误差阈值，也未因此关闭规范追踪第 7 节的 17 项判读。相关性高不能证明未激活分支；音乐中的
弱信号仍完整保留，不因能量低而从统计中删除。DRP `>=18 kHz` 的输出继续不作为规范目标。

其中两个弱信号对象的比较尺度 RMS 约为 `8e-6`（约 -102 dBFS）；“相对残差明显”不等于已经
证明可听缺陷。另做了表 D.2 实虚列序的单变量 A/B：交换列序后，20 路对象的 12–18 kHz 误差
均下降，该频带合计误差能量改善约 1.12 dB，但没有消除残差。这为另一种读法提供了线索，尚
不足以裁决该判读。实验后生产源码与 release 二进制均恢复，二进制逐字节相同。

**后续排查更新：** [相位与游标实验](ASPX_PHASE_INVESTIGATION.md)表明，上述换列改善不能
单独作为改表依据。固定 QMF 相位模型并换算噪声区间起点后，留出时间段与独立 768K 输入分别
改善约 10.50、8.81 dB；此时交换列序反而明显恶化。强高频及带低频锚点的合成探针也触发了
差异，故“主要出现在极弱对象”的首轮观察仅限当时的音乐样本，不能外推为全谱保证。

## 比较前必须固定的三个条件

### 输出层级与身份

只取参考端 **decoder 的对象 PCM、OAR 之前** 的 Float32 交织输出，不比较扬声器渲染、
ADM 的 S24 量化副本或母版对象。当前三份参考的 OAMD 均核对为初始化之后的
`LFE + 20 dynamic objects`；参考物理路 0 对应 LFE，1–20 对应本地 `ajoc_object` 0–19。
这只是当前实测布局的映射，不能外推为所有 AC-4 的固定 21 路顺序。

检查器按显式身份配对，拒绝重复、缺失和多物理 substream 的歧义；不会用最高相关性重新排列
对象。静音槽位的数字相同不能验证这些槽位的身份，参考描述符才是其配对依据。

### 参考处理与尺度

仅关闭 DRC 不会关闭参考端的响度调整。本轮显式请求 `target_reference_level = 0`，同时关闭
DRC、limiter、DAP；所有参数设置返回成功。将默认参考输出与该配置比较，三份输入分别改变
约 15.000、21.000、17.006 dB，对应各自 -16/-10/-14 dBFS 的 dialogue normalization。
这些是本轮受控 A/B 的结果，不恢复早期已撤回的“全局固定 15 dB”结论。

本地 CLI 导出值乘 `2^-15` 得到公开 Scene PCM；[ADR-0006](decisions/0006-core-pcm-qmf-gain-boundary.md)
决策 3 另明确记录公开 PCM 与字面 QMF 出口间的全局 2 倍表示。因此本轮对参考 PCM 使用固定
`local_scale = 2^-16`。三个案例及所有轨道共用该值，未按对象、频带或样本拟合增益。
检查器仍报告拟合增益作为诊断，但它不改变误差和通过判定。这个换算不改变生产 DSP、Scene
契约或三份逐位基线，也不把参考输出尺度当成 ETSI 的规范证明。

### 时间轴

同一单对象探针的 MP4 与裸流本地 PCM 在裁去 2048 个采样后逐位相同。参考相对本地的偏移是
MP4 输入 2079、裸流输入 31 samples；差额恰为本次 MP4 edit 的 2048 samples。
真实音乐裸流的三个不同对象在 ±4096 samples 的低频相关性探测中也都以 31 为唯一最大值。
本轮据此固定这些输入的整数延迟，全轨共用，未逐轨校准。31 samples 的内部成因尚未裁决，
不得外推为所有码流或参考版本的常量，也不得沿用早期兼容路径的 4451 samples。

manifest 的 `reference_lag_samples = L` 明确表示比较 `local[t]` 与 `reference[t + L]`。
报告列出两侧未比较的前缀与尾部。检查器不靠削去瞬态或反复搜索延迟降低误差；STFT 不足一个
窗口的尾部仍进入全区间逐样本误差统计。

## 运行

宿主侧需要 Python 3.11 或更新版本；NumPy 只用于差分分析，不进入 Rust 解码器依赖。

```bash
python3 -m pip install -r scripts/requirements-pcm.txt
python3 -m unittest scripts/test_compare_object_pcm.py
cargo build --locked --release -p macindecode-ac4-cli --features audio-decode

target/release/macinac4 export-objects-pcm input.m4a --output tmp/local.wav > tmp/local.json
# 通过本机已配置的外部工具导出同一输入的对象 Float32 PCM 与轨道描述。
# 将实际文件 SHA-256、处理设置、轨道身份、延迟和尺度依据填写到本地 manifest。
python3 scripts/compare_object_pcm.py tmp/case.manifest.json --output tmp/case.report.json
```

manifest 的路径相对其所在目录解析；下面仅展示字段，摘要占位符须替换成真实值。`tracks`
必须覆盖全部实际输出路，每个 `output_channel` 恰好出现一次；本地轨道定义自动读取 CLI JSON。
外部适配器的私有调用路径不写进可公开文档。

```json
{
  "version": 1,
  "case": "probe_axes_single_object_768k",
  "input": {"path": "input.m4a", "sha256": "实际输入 SHA-256"},
  "local_pcm": {"path": "local.wav", "sha256": "实际 WAVE SHA-256"},
  "local_export": {"path": "local.json", "sha256": "实际 CLI JSON SHA-256"},
  "reference_pcm": {
    "path": "reference.f32le.raw",
    "sha256": "实际参考 PCM SHA-256",
    "format": "interleaved_f32le",
    "sample_rate_hz": 48000,
    "channels": 2,
    "tracks": [
      {"output_channel": 0, "role": "lfe"},
      {"output_channel": 1, "role": "ajoc_object", "ajoc_object": 0}
    ],
    "runtime": "填写实际参考 runtime 与版本",
    "identity_evidence": "填写参考描述符与本地对象身份的对应依据",
    "processing": {"target_reference_level": 0, "drc_enable": 0, "limiter_enable": 0}
  },
  "alignment": {"reference_lag_samples": 2079, "evidence": "填写此输入的独立对齐证据"},
  "local_scale": 0.0000152587890625,
  "scale_evidence": "CLI 2^-15 与 ADR-0006 的固定 0.5 表示换算；参考关闭响度调整"
}
```

检查器限定当前协议的 48 kHz、单物理 substream、RIFF Float32 本地 WAVE 与外部 raw Float32。
它校验全部文件摘要、WAVE chunk 边界、CLI 形状、身份唯一性和全文件 PCM 有限性。它不独立
证明外部文件确实由所声明的输入/配置生成；这部分须由本地捕获命令、runtime 状态和 provenance
保证。输入或配置改变后必须重新捕获，不能只更新摘要让旧参考继续使用。

默认退出 0 只表示成功生成 `observation_only`。只有事先有依据地规定误差界后才使用
`--max-abs-error VALUE`：它检查固定尺度与延迟下的**整个共同区间、全部轨道**，不使用拟合
增益，也不排除高频。此界不是 AC-4 通用容差。全静音返回 `insufficient_excitation`，超过误差
界返回 `fail`，两者退出 1；缺件、摘要不符、非有限值等输入错误退出 2。已有报告不覆盖。

检查器有 16 项离线测试；7 类故障注入全部被捕获，2 类等价变体均通过。默认与 `audio-decode`
工作区测试、Clippy、rustfmt 和层审计通过，三层各 12 条真实 PCM 基线逐位不变。

后续已补充相位、噪声游标、音调初相与独立输入实验，见[专项报告](ASPX_PHASE_INVESTIGATION.md)。
规范字面与参考行为仍需分别裁决；不能用整段相关性或个别输入代替逐条触发条件的验证。

**2026-10-16 limiter 独立对照：** 内部增益／组装检查点已覆盖 9/9 次调用及音乐的全部包络。
现有 boost 选项在 limiter=0 强纯音上的全段／12–18 kHz 误差改善分别为 −1.0942／−1.1077 dB，
即误差增大；音乐分别为 −0.000716／−0.000429 dB，基本不变。该 A/B 使用当前生产算法，
没有联合启用相位、游标或预平坦化变体；两份 limiter=1 原流对选项保持逐位不变。增益域的
单独收益、参考的剩余约束及捕获验证见[专项报告](ASPX_LIMITER_OFF_INVESTIGATION.md)。

**2026-10-16 噪声起点单项修复：** 按 [ADR-0019](decisions/0019-correct-aspx-noise-origin-time-unit.md)
应用 ATS→QMF 起点换算，未叠加其它相位或增益变体。固定原比较契约，独立音乐 limiter=1 的
全区间／12–18 kHz 误差分别增加约 2.636／2.893 dB，纯音则增加约 0.00038／0.00378 dB。
原语时钟对照全部通过，但该单项不能称为整体参考精度提升。不同 limiter／boost 组合及基线
变化见[修复记录](experiments/aspx_noise_origin_fix.json)；Core 12 条逐位不变，A-SPX／objects
各 12 条摘要有意更新，形状、时长和轨道身份均不变。
