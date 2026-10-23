# OAMD 偏移单位与实际消费边界

日期：2026-10-23；起点 `6b55ce7`。本轮只增加测试观察、独立检查器和证据。
生产 DSP、公共 API、CLI、默认选项与三层 PCM 基线保持不变。
固定构造清单见 [机器清单](experiments/oamd_consumer_cases.json)，
结果和来源摘要见 [机器记录](experiments/oamd_consumer_comparison.json)。

## 结论

在已锁定参考版本和这 16 个配置上，OAMDI getter 返回的是原始 block offset factor；
OAR 实际消费者将其乘 **32**，再加内部 sample offset。ramp 字段直接以采样数使用。
独立 OAMDI 解析、OAR 内部解析、实际入队、查询游标与剩余时长形成了连续的证据链。
本地 Scene 也按相同的原始字段单位发布事件，没有发现新的生产错误反例。

**旧参考 scene 导出桥接存在独立反例：漏乘 32。** 12 个非零偏移配置都能复现；
旧坐标比 OAR 输入 PCM 坐标下的实际事件早 `31 × factor` 个采样。原值对照和三个
仅改 A-JOC 的配置偏移为零，因而不会暴露这个错误。旧文件与旧桥接保留；新实验另外
生成明确标注 `oar_input_pcm` 坐标的规范化记录，不改写历史捕获。
随后已将单位修复应用到供后续实验使用的私有适配器，见末节的修复记录。

本轮关闭的是字段单位和参考元数据消费边界。不同实现的音频坐标仍须独立解释，
不能据此改动 A-JOC 的第一步、终点钉住或本地 PCM 延迟。

## 固定输入与规范依据

锁定 Part 2 V1.3.1 的正文页 98（`5.9.2`）独立给出：

```text
source_event = source_sample_start + sample_offset + 32 × block_offset_factor
```

正文页 186 的 `6.3.9.3.11`／表 95 用于独立抄录 ramp 表，包含 32 和 1001。
这两个页码已进行视觉核对，PDF 摘要沿用 manifest；旧代码注释中的 `4.8.3.4.1`
不作为本轮公式的条款定位依据。

| 材料 | 配置 |
| --- | --- |
| 前轮音乐 raw AU 99 → 消费 AU 100 | 原值对照；(1,1)、(15,2)、(31,32) 各自 A-JOC 单改、OAMD 单改及同时修改，共 10 个 |
| 同一母流新增 OAMD 单改 | factor 1／31／63 × ramp 32／1001，共 6 个 |

新构造使用自己的因子／采样时长协议，没有重解释旧 38 配置。所有改动等长、只触及
已定界的白名单时间字段；旧值、位置、宽度、源摘要和改写范围逐项核对。10 个复用配置
与历史文件逐字节相同。新流全部完整解析到 EOF；完整 Core PCM、A-SPX QMF、对象 PCM
和选定窗口的矩阵输入／目标保持母流。独立 A-SPX PCM 导出另行核对。

当前真实帧仍为无 CRC 的 AC40；CRC 重算和错误拒绝由自有 AC41 包络覆盖。
不把这些受控构造计作编码器原生覆盖。

## 原生消费者证据与门禁

参考继续使用 DRP 4.3.0.19350 arm64，并分别锁定 metalib、oarlib 和新私有适配器。
完整解码结束后，OAR 使用自有 PCM／payload 缓冲及独立零状态重放。每个配置有
无观察器基准、独立确认和两个独立观察进程，共 **64 份完整 OAR 运行**，每份 400 块。
双进程的完整 PCM 摘要和规范化时间记录逐字节重复一致。

全程记录每块内部解析和实际入队，详细窗口为调用 99–102。每轮 16 个配置合计
6,400 次解析／入队和 **4,474 次元数据查询**；每个配置的四个窗口都具有完整的
256 次有效消费，另外的查询是新事件尚未生效时对较旧 payload 的回退。
两次捕获都独立满足这些数量与身份要求。

旧适配器有两个不能沿用的成功判据：包络解析会覆盖重复序号并容忍不完整尾部；
处理函数被声明为返回整数，但实际执行路径没有定义可用的成功状态码。新协议严格
检查版本、长度、顺序、唯一性和 EOF；OAR 处理入口按无状态返回调用，分别要求内部
解析返回 0、实际入队一次、对象数正确、处理完成、完整非零有限输出及重复摘要一致。
首个 payload 是 10 对象预热描述，随后为 21；不能把所有返回机械地要求成 21。

观察点由锁定库的指令签名约束。最终观察器使用固定硬件断点，并准确执行断点位置
的整数加载／分支语义，不改写库文件、不改浮点数据或 DSP 参数。早期动态切换断点的
捕获出现异常通知并被排除；Frida 附加超时、错误 ABI 试探和其他失败也保留为本地日志，
不计覆盖。最终捕获由完整无观察器 PCM 一致性及独立进程重复性共同约束。

10 种不同 OAMD 序列全部使用逐字节相同的对象 PCM 完成独立状态重放。
相同 payload、不同 A-JOC 音频的配对组具有相同的全部时间记录，元数据结果不依赖
矩阵日程的变化，也没有用渲染 PCM 分数选择时间解释。

## 时间坐标、跨块与中断

OAR 入队时保留 API offset；内部队列使用带自身原点的减一坐标。检查器先核对该表示，
再由实际消费者的调用参数核对以下等式，而不是把减一坐标直接当事件时间：

```text
oar_event = pcm_call_start + api_offset + internal_offset + 32 × factor
query_relative_to_payload = pcm_query − pcm_call_start − api_offset
remaining = max(0, ramp_duration − (query_relative_to_payload − event_offset))
```

本批 API offset 恒为 352；所用 OAR 初始化配置的 latency getter 为 0。这不等同于
完整解码链无延迟。OAR 在 32 样本处理块的中点查询元数据，每块查询位置为
16、48、…、2032；奇数 factor 证明它没有把时间字段先取整到 64 样本。
剩余时长由实际返回值验证；这里没有进一步声称完整空间增益曲线或逐采样滤波响应
都已验证。

| factor | 字段定义的偏移 | 首次有效查询相对 payload 的位置 | 旧桥接坐标误差 |
| ---: | ---: | ---: | ---: |
| 0 | 0 | 16 | 0 |
| 1 | 32 | 48 | −31 |
| 2 | 64 | 80 | −62 |
| 30 | 960 | 976 | −930 |
| 31 | 992 | 1008 | −961 |
| 62 | 1984 | 2000 | −1922 |
| 63 | 2016 | 2032 | −1953 |

factor 62／63 的事件加上 API offset 后跨越调用块边界，仍由正确的旧 payload 提供。
下一 payload 的事件生效后，消费者转向新来源；factor 63、ramp 1001 在被替换前只
观察到剩余 985 的一次有效查询，不能宣称其无中断终点已覆盖。factor 1／31、ramp
1001 则都能观察到剩余时长归零。检查器核对每个中点的最新有效来源、回退、完整数量
与顺序，因此丢掉跨块历史或提前绑定下一 AU 都会失败。

本地公共 Scene 驱动连续核对全部 16 × 400 AU，共 134,064 条逐对象事件。
首帧只预热，raw AU n 的来源随控制 FIFO 在 n+1 消费。对于 raw AU 99：

| 坐标域 | 本批事件位置 |
| --- | --- |
| 原始来源样本 | 200704 + 32 × factor |
| 本地 Scene codec 坐标 | 202752 + 32 × factor |
| 参考 OAR 输入 PCM 坐标 | 203104 + 32 × factor |

这些是具名坐标转换，不是三个对同一物理采样的竞争猜测。352 的坐标差没有被用来
修改本地时间轴；与 ASF／QMF 物理支持区间的对应、A-JOC／OAMD 同步裁决继续独立保留。

## 反例、验收与复现

旧桥接的最小已测反例为 factor 2、ramp 64：第 100 条旧坐标为 203106，实际消费者
坐标为 203168。新检查器提供明确返回非零的复现入口；建议修复未来使用的私有桥接，
为块因子补一次乘 32，并显式标明其 PCM 坐标域。历史数据不能静默重算或重冻。

新增 20 项无媒体测试接入 CI，覆盖时间编码、等长保护、CRC、包络完整性、整数边界、
跨帧队列、reset、拆分等价、首步／终点／中断及旧桥接反例。实际临时生产故障注入的
漏乘、重复乘、64 样本取整、错源 AU 和丢弃未来队列均被捕获；等价移位实现及恢复后的
完整流通过。所有实验性生产改动已恢复。API offset、捕获丢失和解析失败伪成功另由
检查器记录故障注入覆盖，不冒充原生库实现变异。

默认／audio-decode／spec-tables 分别通过 516／1,331／107 项 Rust 测试；相关 Python
193 项通过。三套 Clippy、两套 no_std、格式、分层、规范来源／表、分发与工作流审核
通过。原有三层 36 条 PCM 基线通过，三份原始完整对象流共 25,460,736 个样本逐位保持；
六份新增流的 A-SPX PCM 导出也逐位等于母流。release 候选摘要与前轮相同，正式二进制
恢复原摘要。完整命令、来源与结果保存于机器记录。

大文件、原生代码及失败日志只留在核验身份后的 cache 卷。48 份重复 OAR PCM 在摘要
确认后删除，节省约 2.52 GB；已验证的新输出另经硬链接去重节省约 2.12 GB。没有新增
独立 Cargo 缓存，没有链接或改写原媒体、旧证据及正式二进制。

```bash
python3 scripts/check_oamd_consumer_timing.py /absolute/path/previous-manifest.json \
  --prepare /absolute/path/new-variants
MACINAC4_OAMD_INPUT=/absolute/path/variant.ac4 \
MACINAC4_OAMD_OUTPUT=/absolute/path/new-scene \
  cargo test -p macindecode-ac4-scene --features audio-decode --release \
  capture_oamd_scene_timing -- --ignored
python3 scripts/check_oamd_consumer_timing.py /absolute/path/new-variants/manifest.json \
  --captures /absolute/path/oar --scene /absolute/path/scene \
  --output /absolute/path/comparison.json
# 预期失败；必须保留对应的完整、已验证参考捕获。
python3 scripts/check_oamd_consumer_timing.py /absolute/path/new-variants/manifest.json \
  --captures /absolute/path/oar --scene /absolute/path/scene \
  --legacy-case music_99_oamd_s01_r01 --legacy-scene /absolute/path/old-capture.scene.bin
python3 -m unittest scripts/test_check_oamd_consumer_timing.py
```

覆盖仍限于单块、implicit sample offset、32 时隙帧、该对象拓扑和参考版本。
其它 sample-offset 编码、多块及短帧只有自有检查器／既有 Rust 边界测试，未获得新的
原生消费者覆盖。OAR 全部空间渲染数值和创作节目绝对时间也不属于本轮结论。

## 2026-10-23 修复应用：后续参考桥接

用户确认应用后，私有参考桥接发布为 `oamd-offset32-v1`。相对冻结源文件，数值修改
只有一行：将 `sample += block_offset` 改为 `sample += (int64_t)block_offset * 32`。
先提升到有符号 64 位再换算；API offset、内部 offset、ramp 和对象属性保持原处理。
本地后续实验的适配器索引已指向验证后的新版本，旧实验目录继续作为冻结证据。
MASC v1 的字节格式保持，版本侧车明确坐标为 OAR 输入 PCM；不能再对输出坐标乘 32。

16 个配置各完成两个独立进程的完整解码，共 32 次、每次 400 AU。修复后的全部事件
通过同一消费者模型，12 个非零配置各只改变第 100 条事件的时间，改变量恰为
`31 × factor`；另外 4 个配置的 scene 文件逐字节保持。所有配置的对象属性及其他
非时间字节、原始 OAMD、完整对象 PCM 都保持原摘要，两次新 scene 导出也逐字节一致。
原来的 12 个失败文件仍被原判据拒绝，没有修改预期或重冻任何 PCM 基线。

补跑的 36 项相关 Python 检查通过。生产代码未改，正式二进制、三层基线及参考库
摘要再次核对保持；本次没有重复不受影响的 Rust 构建。源码、适配器、安装索引、
完整调用门禁与失败尝试摘要见 [修复机器记录](experiments/oamd_bridge_fix.json)。
