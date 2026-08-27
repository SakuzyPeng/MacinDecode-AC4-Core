//! substream index table、拓扑模型与逐帧解析。

use super::*;

/// 表 56 中含 LFE 的声道模式。
const fn channel_mode_contains_lfe(mode: u8) -> bool {
    matches!(mode, 4 | 6 | 8 | 10 | 12 | 14 | 15)
}

/// 把表 56 的声道模式拆为无 LFE 的 layout family 与 LFE presence。
///
/// 3–14 从 5.X 开始成对排列：奇数是不含 LFE 的 family，紧随的偶数是
/// 同一 family 的含 LFE 变体。22.2 只有含 LFE 的 mode 15。
fn channel_mode_parts(mode: u8) -> Option<(u8, bool)> {
    match mode {
        0..=3 | 5 | 7 | 9 | 11 | 13 => Some((mode, false)),
        4 | 6 | 8 | 10 | 12 | 14 => mode.checked_sub(1).map(|base| (base, true)),
        15 => Some((15, true)),
        _ => None,
    }
}

/// P2 `6.3.3.1.27` 的 `superset()`。
///
/// 先在无 LFE 的 layout family 上取最低共同 family，再只要任一输入含 LFE
/// 就选择该 family 的含 LFE 变体。这样合并满足交换律、结合律和幂等性，且不会像
/// 旧参考查表的矛盾项那样因 substream 顺序不同而改变结果或丢失 LFE。
fn superset_channel_mode(left: Option<u8>, right: Option<u8>) -> Option<u8> {
    let (Some(left), Some(right)) = (left, right) else {
        return left.or(right);
    };
    let (left_base, left_lfe) = channel_mode_parts(left)?;
    let (right_base, right_lfe) = channel_mode_parts(right)?;
    let base = left_base.max(right_base);
    if base == 15 || !(left_lfe || right_lfe) {
        return Some(base);
    }
    base.checked_add(1)
}

#[derive(Debug, Clone, Copy, Default)]
struct PresentationChannelAccumulator {
    presentation_channel_mode: Option<u8>,
    core_channel_mode: Option<u8>,
    object_or_ajoc: bool,
    adaptive_object_or_ajoc: bool,
    four_back_channels_present: bool,
    top_channel_pairs: u8,
}

impl PresentationChannelAccumulator {
    fn include(&mut self, substream: &SubstreamInfo) -> Option<()> {
        match *substream {
            SubstreamInfo::Chan(ref info) => {
                let channel_mode = u8::try_from(info.channel_mode.ch_mode).ok()?;
                self.presentation_channel_mode =
                    superset_channel_mode(self.presentation_channel_mode, Some(channel_mode));

                let core_channel_mode = match channel_mode {
                    11 | 13 => Some(5),
                    12 | 14 => Some(6),
                    _ => None,
                };
                self.core_channel_mode =
                    superset_channel_mode(self.core_channel_mode, core_channel_mode);
                self.four_back_channels_present |= info.four_back_channels_present.unwrap_or(false);
                self.top_channel_pairs = match info.top_channels_present {
                    Some(3) => 2,
                    Some(1 | 2) => self.top_channel_pairs.max(1),
                    _ => self.top_channel_pairs,
                };
            }
            SubstreamInfo::Ajoc(ref info) => {
                self.object_or_ajoc = true;
                if info.static_dmx {
                    let core_channel_mode = if info.b_lfe { 4 } else { 3 };
                    self.core_channel_mode =
                        superset_channel_mode(self.core_channel_mode, Some(core_channel_mode));
                } else {
                    self.adaptive_object_or_ajoc = true;
                }
            }
            SubstreamInfo::Obj(_) => {
                self.object_or_ajoc = true;
                self.adaptive_object_or_ajoc = true;
            }
        }
        Some(())
    }

    fn finish(self) -> PresentationChannelContext {
        let presentation_channel_mode = if self.object_or_ajoc {
            None
        } else {
            self.presentation_channel_mode
        };
        let mut core_channel_mode = if self.adaptive_object_or_ajoc {
            None
        } else {
            self.core_channel_mode
        };
        if core_channel_mode == presentation_channel_mode {
            core_channel_mode = None;
        }
        let has_lfe = presentation_channel_mode.map_or_else(
            || matches!(core_channel_mode, Some(4 | 6)),
            channel_mode_contains_lfe,
        );
        PresentationChannelContext::new(
            presentation_channel_mode,
            core_channel_mode,
            self.four_back_channels_present,
            self.top_channel_pairs,
            has_lfe,
        )
    }
}

/// `substream_index_table()` 中的一条 substream 尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstreamSize {
    /// 以字节计的 substream 尺寸。
    pub bytes: u32,
}

/// `substream_index_table()`，见 `TS103190-1:v1.4.1:4.2.3.11`（表 14）。
///
/// `n_substreams == 1` 时尺寸可以省略，此时 substream 一直延伸到帧尾；
/// 其余情况必须逐条给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstreamIndexTable {
    /// substream 数量。
    pub n_substreams: u32,
    /// 是否传输了各 substream 的尺寸。
    pub size_present: bool,
    sizes: [SubstreamSize; MAX_SUBSTREAMS],
    written: usize,
}

impl SubstreamIndexTable {
    /// 已记录的 substream 尺寸。
    ///
    /// `size_present` 为假时为空切片。
    #[must_use]
    pub fn sizes(&self) -> &[SubstreamSize] {
        self.sizes.get(..self.written).unwrap_or(&[])
    }

    /// 解析 `substream_index_table()`。
    ///
    /// # Errors
    ///
    /// 读取越界或 substream 数超过 [`MAX_SUBSTREAMS`] 时返回错误。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, TopologyError> {
        let mut n_substreams = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
        if n_substreams == 0 {
            n_substreams = reader.variable_bits_scaled_u32(2, 4, 0)?;
        }

        // 只有单 substream 时尺寸才可省略：此时帧内没有需要跳过的边界。
        let size_present = if n_substreams == 1 {
            reader.read_flag()?
        } else {
            true
        };

        let mut table = Self {
            n_substreams,
            size_present,
            sizes: [SubstreamSize::default(); MAX_SUBSTREAMS],
            written: 0,
        };
        if !size_present {
            return Ok(table);
        }

        let count = usize::try_from(n_substreams).unwrap_or(usize::MAX);
        if count > MAX_SUBSTREAMS {
            return Err(TopologyError::CapacityExceeded {
                what: Capacity::Substreams,
                declared: n_substreams,
                limit: MAX_SUBSTREAMS,
            });
        }

        for _ in 0..count {
            // b_more_bits 在 substream_size 之前传输
            let more = reader.read_flag()?;
            let mut bytes = u32::try_from(reader.read_bits(10)?).unwrap_or(u32::MAX);
            if more {
                bytes = reader.variable_bits_scaled_u32(2, bytes, 10)?;
            }
            let slot =
                table
                    .sizes
                    .get_mut(table.written)
                    .ok_or(TopologyError::CapacityExceeded {
                        what: Capacity::Substreams,
                        declared: n_substreams,
                        limit: MAX_SUBSTREAMS,
                    })?;
            *slot = SubstreamSize { bytes };
            table.written = table.written.saturating_add(1);
        }
        Ok(table)
    }
}

/// 一帧内所有 group 共同确定的编码路径。
///
/// 这是 M2 要回答的问题：整条编码链究竟产生了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenePath {
    /// 全部 group 都是声道编码。
    ChannelBased,
    /// 至少一个 group 使用 A-JOC，且没有 direct-object。
    Ajoc,
    /// 至少一个 group 使用 direct-coded object。
    DirectObject,
    /// 同帧内同时出现 A-JOC 与 direct-object。
    Mixed,
    /// 帧内没有任何 substream group。
    Empty,
}

impl ScenePath {
    /// 用于序列化的稳定名称。
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            ScenePath::ChannelBased => "channel_based",
            ScenePath::Ajoc => "ajoc",
            ScenePath::DirectObject => "direct_object",
            ScenePath::Mixed => "mixed",
            ScenePath::Empty => "empty",
        }
    }
}

impl fmt::Display for ScenePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 帧级节目标识，见 `TS103190-2:v1.3.1:6.3.2.1.4`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramId {
    /// 16 比特短标识。
    pub short_program_id: u16,
    /// 可选的 16 字节 UUID。
    pub program_uuid: Option<[u8; 16]>,
}

/// 一帧的完整 TOC 拓扑。
#[derive(Debug, Clone)]
pub struct Ac4Topology {
    /// TOC 前置字段。
    pub toc: Ac4Toc,
    /// 节目标识，未传输时为 `None`。
    pub program_id: Option<ProgramId>,
    presentations: [Ac4PresentationV1Info; MAX_PRESENTATIONS],
    n_presentations: usize,
    groups: [Ac4SubstreamGroupInfo; MAX_SUBSTREAM_GROUPS],
    n_groups: usize,
    /// substream 索引表。
    pub index_table: SubstreamIndexTable,
    /// TOC 解析结束时的比特偏移，即 `byte_align` 之前的位置。
    pub bits_consumed: u64,
}

impl Ac4Topology {
    /// 本帧的 presentation。
    #[must_use]
    pub fn presentations(&self) -> &[Ac4PresentationV1Info] {
        self.presentations
            .get(..self.n_presentations)
            .unwrap_or(&[])
    }

    /// 本帧的 substream group，下标即 `group_index`。
    #[must_use]
    pub fn groups(&self) -> &[Ac4SubstreamGroupInfo] {
        self.groups.get(..self.n_groups).unwrap_or(&[])
    }

    /// 取得某个 presentation 的 alternative selection 前缀解析上下文。
    ///
    /// `n_substreams_in_presentation` 按 `TS103190-2:v1.3.1:6.3.3.1.13` 的
    /// `ac4_sgi_specifier()` 外层顺序与 group 内层顺序计算，包含 dialogue-enhancement
    /// substream，且不按物理 substream index 去重。因此 config 1/4 中额外引用的 DE
    /// group 也会进入计数，不能只使用 `n_substream_groups`。
    ///
    /// presentation 下标越界、该 presentation 没有 presentation substream，或内部 group
    /// 引用不存在时返回 `None`。从 [`Self::parse`] 返回的 topology 已通过 group 引用校验，
    /// 最后一种情况只用于防守内部不变量。
    #[must_use]
    pub fn presentation_substream_selection_context(
        &self,
        presentation_index: usize,
    ) -> Option<PresentationSubstreamSelectionContext> {
        self.presentation_substream_context(presentation_index)
            .map(PresentationSubstreamContext::selection_context)
    }

    /// 取得 presentation selection、common metadata 与 downmix helper 的完整解析上下文。
    ///
    /// 除 `n_substreams_in_presentation` 外，本方法按 P2 `6.3.3.1.27`–
    /// `6.3.3.1.31` 的 Pseudocode 25/26 与 helper 规则，派生 `pres_ch_mode`、
    /// `pres_ch_mode_core`、four-back、top-pairs 与 LFE。任一对象/A-JOC
    /// substream 会令 `pres_ch_mode = -1`；adaptive A-JOC 或 direct-object 还会令
    /// `pres_ch_mode_core = -1`。同时直接保留 presentation 语法声明的
    /// `n_substream_groups`，不能用可能包含额外 dialogue-enhancement SGI 的
    /// `group_indices()` 长度替代。
    #[must_use]
    pub fn presentation_substream_context(
        &self,
        presentation_index: usize,
    ) -> Option<PresentationSubstreamContext> {
        let presentation = self.presentations().get(presentation_index)?;
        let substream = presentation.substream?;
        let mut n_audio_substreams = 0u32;
        let mut channel = PresentationChannelAccumulator::default();
        for &group_index in presentation.group_indices() {
            let group = self.groups().get(usize::try_from(group_index).ok()?)?;
            n_audio_substreams =
                n_audio_substreams.checked_add(u32::try_from(group.substreams().len()).ok()?)?;
            for info in group.substreams() {
                channel.include(info)?;
            }
        }
        Some(PresentationSubstreamContext::new(
            substream.alternative,
            substream.ndot,
            n_audio_substreams,
            presentation.n_substream_groups,
            channel.finish(),
        ))
    }

    /// 字节对齐后的 `ac4_toc` 长度，即 substream 载荷区的计算基准。
    ///
    /// `payload_base` 按 `TS103190-1:v1.4.1:4.3.3.2.11` 相对该位置计。
    #[must_use]
    pub const fn toc_bytes(&self) -> u64 {
        self.bits_consumed.div_ceil(8)
    }

    /// 定位单个 substream 的载荷字节。
    ///
    /// 实现 `TS103190-1:v1.4.1:4.3.3.12` 的 Pseudocode 1：偏移由
    /// `payload_base` 加上所有更小下标的 `substream_size` 累加得到，全部相对
    /// 字节对齐的 `ac4_toc` 末尾。`frame` 必须是完整的 `raw_ac4_frame`，即
    /// 从 `ac4_toc` 首字节开始。
    ///
    /// # Errors
    ///
    /// 索引越界返回 [`TopologyError::SubstreamIndexOutOfRange`]；除可延伸到帧尾的
    /// 单 substream 外，索引表未传输尺寸返回
    /// [`TopologyError::SubstreamSizesAbsent`]；区间超出帧长返回
    /// [`TopologyError::SubstreamPayloadOutOfFrame`]。
    pub fn substream_payload<'a>(
        &self,
        frame: &'a [u8],
        index: u32,
    ) -> Result<&'a [u8], TopologyError> {
        let sizes = self.index_table.sizes();
        let wanted = usize::try_from(index).unwrap_or(usize::MAX);
        if index >= self.index_table.n_substreams {
            return Err(TopologyError::SubstreamIndexOutOfRange {
                index,
                total: self.index_table.n_substreams,
            });
        }
        let mut start = self
            .toc_bytes()
            .saturating_add(u64::from(self.toc.payload_base));
        for earlier in sizes.get(..wanted).unwrap_or(&[]) {
            start = start.saturating_add(u64::from(earlier.bytes));
        }
        let frame_len = frame.len() as u64;
        let end = match sizes.get(wanted) {
            Some(size) => start.saturating_add(u64::from(size.bytes)),
            // 单 substream 可省略尺寸；它是载荷区内唯一元素，边界即帧尾。
            None if self.index_table.n_substreams == 1 && !self.index_table.size_present => {
                frame_len
            }
            None => return Err(TopologyError::SubstreamSizesAbsent),
        };
        let range = usize::try_from(start)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(start, end)| frame.get(start..end));
        range.ok_or(TopologyError::SubstreamPayloadOutOfFrame {
            index,
            start,
            end,
            frame_len,
        })
    }

    /// 综合所有 group 得到的编码路径。
    #[must_use]
    pub fn scene_path(&self) -> ScenePath {
        let mut has_ajoc = false;
        let mut has_direct = false;
        let mut any = false;
        for group in self.groups() {
            any = true;
            has_ajoc |= group.has_ajoc();
            has_direct |= group.has_direct_object();
        }
        match (any, has_ajoc, has_direct) {
            (false, _, _) => ScenePath::Empty,
            (true, true, true) => ScenePath::Mixed,
            (true, true, false) => ScenePath::Ajoc,
            (true, false, true) => ScenePath::DirectObject,
            (true, false, false) => ScenePath::ChannelBased,
        }
    }

    /// 本帧的解码器配置指纹。
    #[must_use]
    pub fn config_fingerprint(&self) -> ConfigFingerprint {
        let mut presentations = [Ac4PresentationV1Info::EMPTY; MAX_PRESENTATIONS];
        for (target, source) in presentations.iter_mut().zip(self.presentations()) {
            *target = source.configuration_copy();
        }

        let mut groups = [Ac4SubstreamGroupInfo::EMPTY; MAX_SUBSTREAM_GROUPS];
        for (target, source) in groups.iter_mut().zip(self.groups()) {
            *target = source.configuration_copy();
        }

        ConfigFingerprint {
            bitstream_version: self.toc.bitstream_version,
            fs_index: self.toc.fs_index,
            frame_rate_index: self.toc.frame_rate_index,
            n_presentations: u32::try_from(self.presentations().len()).unwrap_or(u32::MAX),
            n_groups: u32::try_from(self.groups().len()).unwrap_or(u32::MAX),
            scene_path: self.scene_path(),
            total_objects: self.total_objects(),
            n_substreams: self.configuration_substream_span(),
            program_id: self.program_id,
            presentations,
            groups,
        }
    }

    /// 固定配置所引用的 substream 下标跨度。
    ///
    /// EMDF payload substream 是逐帧路由：没有 payload 的帧可以不声明它，
    /// 因此既不在这里计数，也不应触发整个解码会话重置。每帧的 index table
    /// 仍由引用校验与 payload 定位独立验证。
    fn configuration_substream_span(&self) -> u32 {
        let mut span = 0u32;
        let mut include = |index: u32| {
            span = span.max(index.saturating_add(1));
        };

        for presentation in self.presentations() {
            if let Some(substream) = presentation.substream {
                include(substream.substream_index);
            }
        }
        for group in self.groups() {
            if let Some(index) = group
                .oamd_substream
                .and_then(|substream| substream.substream_index)
            {
                include(index);
            }
            for substream in group.substreams() {
                if let Some(first) = substream.substream_index() {
                    for offset in 0..group.frame_rate_factor {
                        include(first.saturating_add(offset));
                    }
                }
            }
            for &index in group.hsf_substream_indices().iter().flatten() {
                include(index);
            }
        }
        span
    }

    /// 本帧作为随机访问点的可用程度。
    ///
    /// `b_iframe_global` 为假时直接判定不可起解；为真时还要求全部
    /// substream、OAMD 与 presentation substream 的 ndot 标志均为真，
    /// 才算完整的场景重建起点。
    #[must_use]
    pub fn random_access(&self) -> RandomAccess {
        if !self.toc.iframe_global {
            return RandomAccess::None;
        }
        let audio_independent = self
            .groups()
            .iter()
            .all(|group| group.substreams().iter().all(SubstreamInfo::audio_ndot));
        let oamd_independent = self
            .groups()
            .iter()
            .all(|group| group.oamd_substream.is_none_or(|oamd| oamd.ndot));
        let presentation_independent = self
            .presentations()
            .iter()
            .all(|item| item.substream.is_none_or(|substream| substream.ndot));

        if audio_independent && oamd_independent && presentation_independent {
            RandomAccess::Full
        } else {
            RandomAccess::AudioOnly
        }
    }

    /// 全部 group 中的对象总数。
    #[must_use]
    pub fn total_objects(&self) -> u32 {
        self.groups()
            .iter()
            .fold(0u32, |acc, group| acc.saturating_add(group.n_objects()))
    }

    /// 解析 `raw_ac4_frame()` 的完整 TOC。
    ///
    /// 会先解析 TOC 前置字段，再按 `bitstream_version` 选择 presentation 语法。
    ///
    /// # Errors
    ///
    /// 读取越界、结构超出固定容量，或遇到未覆盖的语法分支时返回错误。
    pub fn parse(raw_frame: &[u8]) -> Result<Self, TopologyError> {
        let toc = Ac4Toc::parse(raw_frame).map_err(|error| match error {
            crate::toc::TocError::Read(read) => TopologyError::Read(read),
        })?;

        let mut reader = BitReader::new(raw_frame);
        reader.skip_bits(toc.bits_consumed)?;

        // bitstream_version ≤ 1 走 ac4_presentation_info()，当前工具链不产生
        // 这类码流，实现它无法验证。当前规范也只定义到版本 2，
        // 未来版本不能沿用 v2 语法猜测性解析。
        if toc.bitstream_version <= 1 {
            return Err(TopologyError::Unsupported {
                what: Unsupported::LegacyPresentationInfo {
                    bitstream_version: toc.bitstream_version,
                },
                bit_position: reader.bit_position(),
            });
        }
        if toc.bitstream_version > 2 {
            return Err(TopologyError::Unsupported {
                what: Unsupported::FutureBitstreamVersion {
                    bitstream_version: toc.bitstream_version,
                },
                bit_position: reader.bit_position(),
            });
        }

        let program_id = if reader.read_flag()? {
            let short_program_id = u16::try_from(reader.read_bits(16)?).unwrap_or(u16::MAX);
            let program_uuid = if reader.read_flag()? {
                let mut uuid = [0u8; 16];
                for slot in &mut uuid {
                    *slot = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
                }
                Some(uuid)
            } else {
                None
            };
            Some(ProgramId {
                short_program_id,
                program_uuid,
            })
        } else {
            None
        };

        let declared = toc.n_presentations;
        let n_presentations = usize::try_from(declared).unwrap_or(usize::MAX);
        if n_presentations > MAX_PRESENTATIONS {
            return Err(TopologyError::CapacityExceeded {
                what: Capacity::Presentations,
                declared,
                limit: MAX_PRESENTATIONS,
            });
        }

        let mut presentations = [Ac4PresentationV1Info::EMPTY; MAX_PRESENTATIONS];
        let mut written = 0usize;
        for _ in 0..n_presentations {
            let info = Ac4PresentationV1Info::parse(
                &mut reader,
                toc.bitstream_version,
                toc.frame_rate_index,
            )?;
            let slot = presentations
                .get_mut(written)
                .ok_or(TopologyError::CapacityExceeded {
                    what: Capacity::Presentations,
                    declared,
                    limit: MAX_PRESENTATIONS,
                })?;
            *slot = info;
            written = written.saturating_add(1);
        }

        // total_n_substream_groups = 1 + max(group_index)，见 6.3.2.1.8。
        // group 的数量不由某个字段直接给出，必须先读完全部 presentation。
        let total_groups = presentations
            .get(..written)
            .unwrap_or(&[])
            .iter()
            .flat_map(Ac4PresentationV1Info::group_indices)
            .fold(None::<u32>, |acc, index| {
                Some(acc.map_or(*index, |current| current.max(*index)))
            })
            .map_or(0usize, |max| {
                usize::try_from(max.saturating_add(1)).unwrap_or(usize::MAX)
            });
        if total_groups > MAX_SUBSTREAM_GROUPS {
            return Err(TopologyError::CapacityExceeded {
                what: Capacity::SubstreamGroups,
                declared: u32::try_from(total_groups).unwrap_or(u32::MAX),
                limit: MAX_SUBSTREAM_GROUPS,
            });
        }

        let mut groups = [Ac4SubstreamGroupInfo::EMPTY; MAX_SUBSTREAM_GROUPS];
        let mut groups_written = 0usize;
        for index in 0..total_groups {
            // frame_rate_factor 由 presentation 携带，而 group 在 TOC 级共享。
            // 取第一个引用该 group 的 presentation 的取值；frame_rate_index 13
            // 下该值恒为 1，本项目的向量不区分这两种取法。
            let factor = presentations
                .get(..written)
                .unwrap_or(&[])
                .iter()
                .find(|presentation| {
                    presentation
                        .group_indices()
                        .iter()
                        .any(|&referenced| usize::try_from(referenced) == Ok(index))
                })
                .map_or(1, Ac4PresentationV1Info::frame_rate_factor);

            let info = Ac4SubstreamGroupInfo::parse(
                &mut reader,
                toc.bitstream_version,
                toc.fs_index,
                factor,
            )?;
            let slot = groups
                .get_mut(groups_written)
                .ok_or(TopologyError::CapacityExceeded {
                    what: Capacity::SubstreamGroups,
                    declared: u32::try_from(total_groups).unwrap_or(u32::MAX),
                    limit: MAX_SUBSTREAM_GROUPS,
                })?;
            *slot = info;
            groups_written = groups_written.saturating_add(1);
        }

        let index_table = SubstreamIndexTable::parse(&mut reader)?;

        Ok(Self {
            toc,
            program_id,
            presentations,
            n_presentations: written,
            groups,
            n_groups: groups_written,
            index_table,
            bits_consumed: reader.bit_position(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superset_channel_mode_matches_normative_examples_and_preserves_lfe() {
        assert_eq!(superset_channel_mode(None, None), None);
        assert_eq!(superset_channel_mode(Some(0), Some(1)), Some(1));
        assert_eq!(superset_channel_mode(Some(4), Some(11)), Some(12));

        // 7.0.4 已经是 7.1.4 的无 LFE family；旧参考表在这个顺序下误得
        // 9.0.4，既不是最低 family，还会丢失右侧的 LFE。
        assert_eq!(superset_channel_mode(Some(11), Some(12)), Some(12));
        assert_eq!(superset_channel_mode(Some(12), Some(11)), Some(12));
    }

    #[test]
    fn superset_channel_mode_is_stable_for_every_defined_mode() {
        for left in 0..=15u8 {
            assert_eq!(superset_channel_mode(None, Some(left)), Some(left));
            assert_eq!(superset_channel_mode(Some(left), None), Some(left));
            assert_eq!(superset_channel_mode(Some(left), Some(left)), Some(left));

            for right in 0..=15u8 {
                let combined = superset_channel_mode(Some(left), Some(right));
                assert_eq!(
                    combined,
                    superset_channel_mode(Some(right), Some(left)),
                    "superset must be commutative for modes {left} and {right}"
                );
                if channel_mode_contains_lfe(left) || channel_mode_contains_lfe(right) {
                    assert!(
                        combined.is_some_and(channel_mode_contains_lfe),
                        "superset of modes {left} and {right} lost the LFE"
                    );
                }

                for third in 0..=15u8 {
                    assert_eq!(
                        superset_channel_mode(combined, Some(third)),
                        superset_channel_mode(
                            Some(left),
                            superset_channel_mode(Some(right), Some(third)),
                        ),
                        "superset must be associative for modes {left}, {right}, and {third}"
                    );
                }
            }
        }
    }
}
