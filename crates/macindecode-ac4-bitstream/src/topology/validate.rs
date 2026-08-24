//! group 与 substream 引用完整性验证。

use super::*;

/// presentation 引用的 group 下标全部落在 TOC 实际解析出的范围内。
///
/// # Errors
///
/// 存在越界引用时返回 [`TopologyError::GroupIndexOutOfRange`]。
pub fn validate_group_references(topology: &Ac4Topology) -> Result<(), TopologyError> {
    let total = topology.groups().len();
    for presentation in topology.presentations() {
        for &index in presentation.group_indices() {
            if usize::try_from(index).unwrap_or(usize::MAX) >= total {
                return Err(TopologyError::GroupIndexOutOfRange {
                    group_index: index,
                    total,
                });
            }
        }
    }
    Ok(())
}

/// TOC 中的显式 substream 引用与索引表精确匹配。
///
/// 同一 substream 可被多个 presentation 共享，因此重复引用合法；但每个
/// 引用都必须在表内，且表内不能留有无主条目。帧率因子大于 1 的音频
/// info 会覆盖以声明下标为起点的连续 substream。
///
/// # Errors
///
/// 引用越界时返回 [`TopologyError::SubstreamIndexOutOfRange`]；存在未被
/// 引用条目时返回 [`TopologyError::UnreferencedSubstream`]。
pub fn validate_substream_references(topology: &Ac4Topology) -> Result<(), TopologyError> {
    let total = topology.index_table.n_substreams;
    let mut referenced = 0u32;

    for presentation in topology.presentations() {
        if let Some(substream) = presentation.substream {
            mark_substream(&mut referenced, substream.substream_index, total)?;
        }
        if let Some(index) = presentation
            .emdf
            .and_then(|emdf| emdf.payloads_substream_index)
        {
            mark_substream(&mut referenced, index, total)?;
        }
        for emdf in presentation.additional_emdf() {
            if let Some(index) = emdf.payloads_substream_index {
                mark_substream(&mut referenced, index, total)?;
            }
        }
    }

    for group in topology.groups() {
        if let Some(index) = group
            .oamd_substream
            .and_then(|substream| substream.substream_index)
        {
            mark_substream(&mut referenced, index, total)?;
        }
        for info in group.substreams() {
            if let Some(first) = info.substream_index() {
                for offset in 0..group.frame_rate_factor {
                    let index = first.saturating_add(offset);
                    mark_substream(&mut referenced, index, total)?;
                }
            }
        }
        for &index in group.hsf_substream_indices().iter().flatten() {
            mark_substream(&mut referenced, index, total)?;
        }
    }

    for index in 0..total {
        let mask = 1u32.checked_shl(index).unwrap_or(0);
        if referenced & mask == 0 {
            return Err(TopologyError::UnreferencedSubstream { index });
        }
    }
    Ok(())
}

pub(super) fn mark_substream(
    referenced: &mut u32,
    index: u32,
    total: u32,
) -> Result<(), TopologyError> {
    if index >= total || index >= u32::BITS {
        return Err(TopologyError::SubstreamIndexOutOfRange { index, total });
    }
    *referenced |= 1u32.checked_shl(index).unwrap_or(0);
    Ok(())
}
