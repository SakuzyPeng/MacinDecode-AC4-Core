# A-JOC 插值时间锚点：音频坐标与 OAMD 事件

日期：2026-10-22–23；起点 `c67d540`。本轮只新增测试观察、检查器和证据，生产 DSP、公开接口、
CLI 与三层 PCM 基线不变。机器记录见
[`ajoc_timing_comparison.json`](experiments/ajoc_timing_comparison.json)。

## 结论

已经把 raw AU、控制 FIFO 来源、ASF 输出样本坐标、实际消费 QMF 时隙和 OAMD 原始事件
分别固定；未发现交接错帧、错一时隙或改变输入坐标的生产反例。108 条带样本标签的构造链，
以及三份完整真实流的时间记录通过独立检查。

这仍不能唯一裁决 `start_pos` 命中时隙应使用旧值还是新 ramp 的第一步。两种日程的差异
确实存在，但不能把“参考采用另一日程”或“候选在某个位置更接近参考”当作生产错误。
本轮不修改插值顺序、终点钉住规则、QMF 表示或音调初相。

## 独立音频与元数据坐标

完整流驱动新增 `timeline.jsonl`，记录当前 AU 的输入样本起点、raw 矩阵起点及长度、
group／downmix／upmix timing、derive 标志和实际消费的有效 timing。消费来源从真正的
aligned side information 取得，矩阵日程从真正的 `reconstruct_frame` 观察取得。

Python 检查器不调用生产 FIFO 或 timing 继承 helper：按 raw 记录重新选择显式 timing、
group timing、derive 或前序有效值，再核对到期消费者。OAMD 事件独立计算为：

```text
source_sample_start + sample_offset + 32 × block_offset_factor
```

事件使用有符号 64 位整数范围检查；不会把 OAMD 更新强行放到矩阵更新时隙。首 AU 只能
预热，不得提前消费控制。三份流共 592 AU、589 个实际重建帧全部检查，12 个既有真实
窗口保留具体音频支持区间、矩阵候选起止点和 OAMD 更新／ramp 长度。

音频分析沿用已验证的独立 640 抽头复 FIR。在 ASF PCM 输出流坐标下，消费时隙 `t` 的
低带输入支持区间为：

```text
end   = 64 × (global_qmf_slot − 6) + 63 − pcm_delay
start = end − 639
```

负坐标是零前置历史。三个受支持长帧的 `pcm_delay` 分别为 1,536→96、1,920→288、
2,048→352 样本；分析接口固定除二。重新捕获的真实低带／LFE 与此前独立 FIR 检查一致。
这里的支持区间属于 **ASF 已输出 PCM 的坐标**，不能不经 IMDCT／窗口时间解释就当成
原节目物理采样中心，也不能把终端 PCM 的比较偏移挪到此处。

## 构造连续链

实际生产分析、低带延迟、dry 矩阵和合成器连续处理以下组合：

- 24／30／32 时隙帧；起点为首、中、末时隙；ramp 长度 1／2／31／32／33／64。
- 带固定样本位置的脉冲与阶跃，共 108 条五帧链；包含无新点、两点、跨帧延续和 ramp 中断。
- 共 15,480 个时隙、990,720 个合成 PCM 样本。构造范围不扩大真实媒体或产品支持声明。

输入波形由检查器按配方独立生成。先用绝对样本索引的 FIR 核对实际矩阵 X，再固定真实 X
核对 dry 乘法，最后独立合成真实 Z，避免把两个阶段的误差混在一个终端分数中。
分析和合成的误差界来自既有算子与输入幅度，不按本轮残差选择；最大误差／界分别约为
0.03092 和 0.001873。

单系数事件模型分别计算“命中时隙执行第一步”和“下一时隙执行第一步”。两者使用相同
输入与终点规则，所有 108 条链都能区分日程；后一模型逐位预测当前实际消费系数。这是
受控的时序隔离，并不把前一模型冒充参考浮点累加及终点行为的完整复刻。

## 为什么仍保留解释冲突

P2 `5.7.3.4` 说明插值用于与 OAMD 同步；`6.3.6.4.2` 把 `ajoc_start_pos` 描述为包含在
插值中的第一个 QMF 时隙。P17 先计算并返回旧增量推进后的值，再安装新 delta；P18 的
游标更新也在该时隙输出之后。两处信息需要同时保留，不能只选其中一处当唯一依据。

本批真实流始终使用 `start_pos=0, ramp_len=32`，OAMD 则在源帧起点更新、ramp 为
2,048 样本。把源帧起点记为 S，固定日程的两种目标落点为 S+1,984 和 S+2,048。
这明确了两种读法的差别，但没有单独证明 metadata 端点与 QMF 消费值应如何采样对应。
构造链证明了各自行为，不能循环地把自己选择的日程当规范 oracle。

因此当前结论仍是：参考实际消费者在命中时隙执行第一步，本地保持 P17/P18 的旧增量
后安装顺序。非零起点、非 32 长度及被中断 ramp 的真实参考覆盖没有增加；整体对象 PCM
也不能据此解释为提前／延后 64 样本。

## 门禁与复现

命中时隙提前推进的实际生产故障注入被当前日程核对捕获；无媒体检查器另拒绝错一时隙、
错源 AU、预热提前消费、遗漏矩阵帧、错误 OAMD 事件及整数溢出。该故障注入只说明判据
能区分行为，不作为已解决规范歧义的证据。

```bash
MACINAC4_AJOC_TIMING_OUTPUT=/absolute/path/new-timing \
  cargo test -p macindecode-ac4-decode --features audio-decode \
  replay_ajoc_timing -- --ignored
# 三份完整流使用既有 capture_ajoc_input_boundary，并同时启用矩阵观察。
python3 scripts/check_ajoc_timing.py /absolute/path/experiment-root \
  --output /absolute/path/new-timing-comparison.json
python3 -m unittest scripts/test_check_ajoc_timing.py
```

输入摘要、完整输出、检查命令和失败尝试留在核验身份后的 cache 卷。后续若继续裁决时序，
应增加具有明确 OAMD 事件、非零起点和非 32 ramp 的同源编码材料，保持音频坐标固定；
去相关／ducker 的本轮独立结果见 [专项报告](AJOC_DECORRELATOR_INVESTIGATION.md)。

2026-10-23 后续已完成[38 组受控码流验证](AJOC_TIMING_STREAM_INVESTIGATION.md)，补充
非零起点、非整帧 ramp 及 OAMD 配对的实际参考消费证据。插值行为已进一步分离，
但旧元数据桥接的非零偏移单位尚未验证，绝对时间解释仍保持开放。
