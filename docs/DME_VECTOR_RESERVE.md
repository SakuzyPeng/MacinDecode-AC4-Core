# DME 解码器向量储备

记录日期为 2026-11-08。本次保存后续解码器验证所需的外部编码产物。向量储备与已验证的 PCM 支持范围分别记账，不改写既有三阶段
PCM 基线。

三个案例以 `pcm_baseline_status = "reserve"` 声明储备状态。默认 PCM 检查器按完整冻结条目集执行回归，储备媒体由独立向量门禁核对。需要冻结储备 PCM 时，向 `decode_check.py --update` 显式传入相应媒体；已登记条目始终属于默认必检集合。

## 已完成的第一批

| 案例 | 输入与用途 | 编码作业 | 数量 |
| --- | --- | --- | ---: |
| [`probe_dme_dense_prbs`](../vectors/probe_dme_dense_prbs/case.json) | 8 秒，8 路独立固定种子宽带对象、低电平 bed；对象分离、交叉、重合、再分离 | L3 320/448/768；L4 128/256/448/768/1500 kbps | 8 |
| [`probe_dme_transient_edges`](../vectors/probe_dme_transient_edges/case.json) | 6 秒，静音中的 PRBS 短猝发与独立脉冲；首尾和候选帧边界两侧 | L3 320；L4 128/256/768/1500 kbps | 5 |
| [`probe_dme_native_highband`](../vectors/probe_dme_native_highband/case.json) | 6 秒，逐声道高频正弦与独立 40 Hz LFE | stereo 48/128/768；5.1 96/256/768；5.1.4 192/448/768 kbps | 9 |
| 同上 | 同一 5.1 WAVE 的 general/music 对照 | 两种模式各 64/144/320 kbps | 6 |

共 **28 条**。母版、规范化输入、case 和码流合计约 **77.6 MiB**，其中编码媒体约
**10.0 MiB**。媒体位于各案例的 `encoded/`，保持 Git 忽略；`case.json`、
`provenance.json`、`hashes.sha256` 与[覆盖记录](experiments/dme_reserve_20261108.json)
用于追溯。相同信号的不同码率是不同作业，不等于新增相同数量的语法分支。

### 实际新增的证据

- 高频 stereo 的三个码率均出现 **非零 DE body**：每条总计 81 bit，单帧最大 45 bit；
  `channel_config` 同时观察到 0 与 6。每条 143 帧有 10 次 New、133 次 KeepPrevious，
  而完整随机访问帧与 EMDF 路由各为 8 次。原先只有配置、没有 body 的真实样本缺口得到补充。
  这是原始 body 与配置的证据，尚未验证这些 body 的 Huffman 参数、跨帧还原和 DE 数值处理。
- 密集 PRBS 场景同时产生 dense/sparse A-JOC 参数，低码率 L4 使用 7 个参数带，L3 和
  中码率 L4 使用 12 个参数带，L4 1500 kbps 使用 23 个参数带。保留复杂对象混合下的
  矩阵与 wet/去相关输入，便于后续差分。
- 13 条 A-JOC 共 **2,235 帧**完成音频解析，**2,222 帧**执行 full wet 路径，
  重建不变量没有违例；A-SPX variable framing 累计观察到 135 帧。
- channel 编码的 6 秒输入产生 143 个 native 2048-sample 帧；IMS general 则产生
  145 个有效帧时长为 2000 samples 的 24 fps 帧，music 为 141 个 2048-sample 帧。
  旧 2 秒案例的 channel/general 恰好同为 49 帧，曾掩盖检查器的错误假设。

### 检查器调整

`scripts/dme_native_check.py` 分开 channel 与 IMS general 的帧数预期；EMDF 数量与
完整随机访问帧比较，不再与 DE New 数量绑定。dependent frame 也能更新 DE 配置。

只有显式写入 `case.json.dme_native_expectations` 的文件才采用非零 DE census；
预期完整记录 presence、new/keep、body 长度和配置计数，任何统计变化都失败。
既有案例仍要求零 body。该字段冻结观察，不执行 DE，不替代独立解码参考。

新增 `--bin` 可固定检查所用的 CLI，避免在有并发源码改动时不断重编译。此次使用既有
release 二进制的快照，SHA-256 记入覆盖记录；没有声称它与记录时工作树的每个文件一致。

已完成检查：28 条容器/拓扑/metadata 解析与全部溯源哈希；上述 A-JOC 重建不变量；
包含 6 条既有样本的 **21 条 DME native 门禁**；DME 两组 Python 测试 **25 项通过**。
channel-based/IMS 的 PCM 重建仍未验证，本批没有独立参考解码 PCM。

## 1500 kbps 与原生声道的能力核验

用户要求再加 1500 kbps，并准备 channel-based。对当前本机工具核验后，矩阵为：

| 编码路径 | 当前工具最高档 | 储备状态 |
| --- | ---: | --- |
| A-JOC Level 4 | 1500 kbps | 宽带多对象、瞬态边界各已有一条；已重新核对文件哈希 |
| A-JOC Level 3 | 768 kbps | 多对象案例已保留 768 kbps |
| 原生 channel-based：2.0、5.1、5.1.4 | 768 kbps | 各已有三档、合计九条；三个布局均含 768 kbps |
| Native IMS：general、music | 320 kbps | 各已有三档、合计六条；两种模式均含 320 kbps |

除查看 CLI 帮助外，还使用确定性的 1 秒、48 kHz/24-bit WAVE 实际请求三个声道布局
和两种 IMS 模式的 1500 kbps。五次均返回退出码 1，明确报
`Invalid 'data-rate' option value: '1500'.`，未生成媒体。这是编码器自身的限制，
并非仓库脚本的码率白名单造成；没有把失败作业加入正式 `case.json`。

完整输入配方、工具指纹、请求结果及已有 1500 kbps 文件哈希见
[码率能力记录](experiments/dme_bitrate_capabilities_20261108.json)。结论仅限当前 DME，
不表示 AC-4 规范或其他编码器不能支持更高码率。channel-based 1500 仍是工具能力缺口，
不能由 A-JOC 1500 的向量替代。

### 进一步的后端核验

同日通过本地运行时观察与独立配置对照，交叉核对所测构建的配置接受性。
三种已验证原生配置的结果一致，768 kbps 可接受，1024/1500 kbps 均被拒绝。

PCM 入口另做了四条 48 帧探针：5.1 配置下的 6/8 路输入得到逐字节相同的输出，
5.1.4 配置下的 10/12 路输入也逐字节相同；每对共同的前缀声道 PCM 完全相同，
追加声道均有独立非零信号。官方 muxer 对 12 路输入产物生成的 DSI 仍为
`four_back_channels_present=false`、`channel_groups=0x77`，没有变成完整 7.1.4。

这些结果支持“所测构建的原生配置路径存在限制”，不支持“只需增加界面选项就能工作”；
也不据此声称更通用的 DSP 算法绝不存在。原始编码器文件未修改，普通编码与加入观察后的
编码产物哈希相同。摘要及产物哈希见[后端能力记录](experiments/dme_backend_capabilities_20261108.json)。
原始实验程序、运行时分析和配置状态由本地受限研究档案保存。
正式解码回归的覆盖范围以案例声明与既有基线为准。

## 后续生产顺序

建议在 11 月 16 日前完成剩余候选的筛选和归档，按实际新增分支决定是否扩批：

1. **先消费新 DE 样本。** 冻结其逐帧原始 body，运行现有 DE Huffman/状态 API 并与
   独立参考核对；再用语音与背景声的干湿对照寻找更长、持续更新的参数。
2. **补不同素材类型。** 同一组合法自有素材取语音、打击乐、高频音调和稠密音乐的短片段，
   各选少量低/高码率。PRBS 已能增加矩阵密度，但本批仍未触发 A-SPX harmonic、balance
   或 FIC/TIC，继续复制相同噪声的码率档位价值有限。
3. **补对象与元数据交互。** 16/32 对象混合、增益与 size 变化、静音期间的位置变化，
   以及混合 SceneRelative/HeadRelative 的成对输入。按 OAMD 原值和信号相关性判断结果，
   不用创作对象 ID 直接匹配解码槽位。
4. **补时间与参数控制。** 检查编码器公开支持的 IMS target-fps、I-frame 间隔、DRC
   profile 与 downmix metadata；每次只改变一个可控参数，并保存完整配置与 timing。
   当前生产脚本只开放其中一部分，扩展前须明确输入、命名和时序契约。
5. **保存独立参考输出。** 对有新增行为的码流保存参考对象/声道 PCM、元数据、延迟与
   工具指纹。高成本参考优先覆盖新 DE、低码率多对象和瞬态边界。

本批未命中活动 companding、A-SPX FIC/TIC，也未取得 1024 及以下短帧、direct-object
或静态 core 的真实证据。DME 无法控制或候选素材无法触发的分支继续保留缺口，不以文件
数量代替覆盖。

## 重跑与保留

```bash
./scripts/build_vector.sh --profile dme_ac4 vectors/probe_dme_dense_prbs/case.json
./scripts/build_vector.sh --profile dme_ac4 vectors/probe_dme_transient_edges/case.json
./scripts/build_vector.sh --profile dme_native vectors/probe_dme_native_highband/case.json
python3 scripts/record_provenance.py vectors/probe_dme_dense_prbs
python3 scripts/record_provenance.py vectors/probe_dme_transient_edges
python3 scripts/record_provenance.py vectors/probe_dme_native_highband
python3 scripts/dme_native_check.py --bin target/release/macinac4
python3 -m unittest scripts/test_dme_native.py scripts/test_dme_ac4.py
```

后续验证使用已保存的媒体及其固定摘要，信号配方用于追溯输入。
本机案例目录保留主副本，外置 `cache` 卷另存完整归档与 SHA-256；外置盘断开不影响
本机使用。归档包含输入、码流、溯源、完整 trace、检查器快照与相关脚本，不包含外部
专有编码器或客户媒体。
