//! OAMD metadata block、上下文与 substream payload。

use super::*;

/// 一个对象在一个时间块内的原始元数据更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OamdMetadataBlock {
    /// 对象在 [`OamdContext::objects`] 中的下标。
    pub object_index: u8,
    /// 本帧 `oamd_timing_data()` 中的块下标。
    pub block_index: u8,
    /// 更新状态与全部已传输码值。
    pub info: ObjectInfoBlock,
}

/// 一次 `oamd_substream()` 解析的结果，见 `6.2.2.4`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OamdSubstreamPayload {
    /// `oamd_common_data()`；未传输时为 `None`。
    pub common: Option<OamdCommonData>,
    /// `oamd_timing_data()`；未传输时为 `None`，此时时间信息沿用前序帧。
    pub timing: Option<OamdTimingData>,
    /// `oamd_dyndata_multi()` 中实际出现的 `object_info_block` 总数。
    ///
    /// A-JOC 路径下全部对象的 `b_ajoc_coded` 为真，该值为 0：逐对象动态数据
    /// 位于 `audio_data_ajoc`，不在本 substream。
    pub dyndata_blocks: u32,
    /// 其中依赖前序状态的块数。
    pub history_dependent_blocks: u32,
    /// `byte_align` 消耗的填充比特，取值 0…7。
    pub align_bits: u32,
    metadata_blocks: [OamdMetadataBlock; MAX_OAMD_METADATA_BLOCKS],
    metadata_blocks_written: usize,
}

/// 解析 `oamd_substream()` 所需的、来自 TOC 的上下文。
#[derive(Debug, Clone, Copy)]
pub struct OamdContext<'a> {
    /// 引用该 OAMD 的 substream group 中的对象描述。
    pub objects: &'a [ObjectDescriptor],
    /// presentation 的 `b_alternative`；为真时动态数据改由 `metadata()` 承载。
    pub b_alternative: bool,
    /// `b_oamd_ndot`，在 `oamd_dyndata_multi` 中充当 `b_iframe`。
    pub b_oamd_ndot: bool,
    /// 前一帧的 `num_obj_info_blocks`，用于本帧未传输时间数据时延续。
    pub previous_num_obj_info_blocks: Option<u8>,
}

impl OamdSubstreamPayload {
    /// `oamd_dyndata_multi()` 中按码流顺序出现的逐对象更新。
    #[must_use]
    pub fn metadata_blocks(&self) -> &[OamdMetadataBlock] {
        self.metadata_blocks
            .get(..self.metadata_blocks_written)
            .unwrap_or(&[])
    }

    /// 解析一个完整的 `oamd_substream()` 载荷。
    ///
    /// `payload` 必须恰好是该 substream 的字节，可由
    /// [`crate::topology::Ac4Topology::substream_payload`] 取得。解析结束后的残余比特必须
    /// 少于 8，否则说明某个可变长字段错位。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`OamdError::Read`]；残余比特过多返回
    /// [`OamdError::Misaligned`]；需要延续但无前序状态返回
    /// [`OamdError::TimingUnavailable`]。
    pub fn parse(payload: &[u8], context: OamdContext<'_>) -> Result<Self, OamdError> {
        if context.objects.len() > MAX_OAMD_OBJECTS {
            return Err(OamdError::TooManyObjects {
                limit: MAX_OAMD_OBJECTS,
            });
        }
        let mut reader = BitReader::new(payload);

        let common = if reader.read_flag()? {
            Some(OamdCommonData::parse(&mut reader)?)
        } else {
            None
        };
        let timing = if reader.read_flag()? {
            Some(OamdTimingData::parse(&mut reader)?)
        } else {
            None
        };

        let mut dyndata_blocks = 0u32;
        let mut history_dependent_blocks = 0u32;
        let mut metadata_blocks = [OamdMetadataBlock::default(); MAX_OAMD_METADATA_BLOCKS];
        let mut metadata_blocks_written = 0usize;
        if !context.b_alternative {
            // 块数只在确有对象需要动态数据时才必须已知：全部对象由 A-JOC 编码
            // 时 oamd_dyndata_multi 为空，缺少时间数据并不构成错误。
            let mut n_blocks = timing.map(|timing| timing.num_obj_info_blocks);
            for (object_index, object) in context.objects.iter().enumerate() {
                if object.b_ajoc_coded {
                    continue;
                }
                let n_blocks = match n_blocks {
                    Some(count) => count,
                    None => {
                        let count = context
                            .previous_num_obj_info_blocks
                            .ok_or(OamdError::TimingUnavailable)?;
                        n_blocks = Some(count);
                        count
                    }
                };
                let dynamic = object.is_dynamic_object();
                for block in 0..n_blocks {
                    let b_no_delta = context.b_oamd_ndot && block == 0;
                    let parsed = ObjectInfoBlock::parse(&mut reader, b_no_delta, dynamic)?;
                    let slot = metadata_blocks.get_mut(metadata_blocks_written).ok_or(
                        OamdError::TooManyMetadataBlocks {
                            limit: MAX_OAMD_METADATA_BLOCKS,
                        },
                    )?;
                    *slot = OamdMetadataBlock {
                        object_index: u8::try_from(object_index).unwrap_or(u8::MAX),
                        block_index: block,
                        info: parsed,
                    };
                    metadata_blocks_written = metadata_blocks_written.saturating_add(1);
                    dyndata_blocks = dyndata_blocks.saturating_add(1);
                    if parsed.depends_on_history() {
                        history_dependent_blocks = history_dependent_blocks.saturating_add(1);
                    }
                }
            }
        }

        let align_bits = reader.byte_align()?;
        let remaining_bits = reader.remaining_bits();
        if remaining_bits >= 8 {
            return Err(OamdError::Misaligned { remaining_bits });
        }

        Ok(Self {
            common,
            timing,
            dyndata_blocks,
            history_dependent_blocks,
            align_bits,
            metadata_blocks,
            metadata_blocks_written,
        })
    }
}
