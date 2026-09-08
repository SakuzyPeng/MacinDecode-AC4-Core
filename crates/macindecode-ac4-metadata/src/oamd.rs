//! 无表 OAMD 目标状态与源时间观察，与 Full engine 共用 bitstream 事务原语。
use crate::{
    MetadataError, MetadataErrorContext, MetadataErrorKind, MetadataObjectState,
    MetadataObjectUpdate, ObjectMetadataDomain, session::MetadataOutput,
};
use macindecode_ac4_bitstream::oamd::{
    MAX_OAMD_METADATA_BLOCKS, MAX_OAMD_OBJECTS, OamdMetadataBlock, OamdState, OamdTimingData,
    ObjectDescriptor,
};
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_side(
    mut state: OamdState,
    blocks: &[OamdMetadataBlock],
    count: u8,
    timing: Option<OamdTimingData>,
    descriptors: &[ObjectDescriptor],
    index: u32,
    domain: ObjectMetadataDomain,
    output: &mut MetadataOutput,
) -> Result<OamdState, MetadataError> {
    let scope = MetadataErrorContext::for_access_unit(output.context.index()).with_substream(index);
    if blocks.len() > MAX_OAMD_METADATA_BLOCKS || descriptors.len() > MAX_OAMD_OBJECTS {
        return Err(MetadataError::new(MetadataErrorKind::Capacity, scope));
    }
    if let Some(t) = timing
        && t.num_obj_info_blocks != count
    {
        return Err(MetadataError::new(
            MetadataErrorKind::OamdTimingConflict {
                expected: count,
                actual: t.num_obj_info_blocks,
            },
            scope,
        ));
    }
    output
        .updates
        .try_reserve(blocks.len())
        .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
    output
        .objects
        .try_reserve(descriptors.len())
        .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
    if blocks
        .iter()
        .any(|b| usize::from(b.object_index) >= descriptors.len())
    {
        return Err(MetadataError::new(MetadataErrorKind::Invariant, scope));
    }
    let before = output.updates.len();
    let result = state.apply_blocks_with_observer(blocks, Some(count), |raw, state, additional| {
        if let Some(&descriptor) = descriptors.get(usize::from(raw.object_index)) {
            let target = MetadataObjectState {
                substream_index: index,
                domain,
                object_index: raw.object_index,
                descriptor,
                state,
                additional,
            };
            let block_timing = timing.and_then(|t| {
                t.blocks()
                    .get(usize::from(raw.block_index))
                    .copied()
                    .map(|b| (t, b))
            });
            output.updates.push(MetadataObjectUpdate {
                target,
                raw: *raw,
                timing,
                offset_samples: block_timing
                    .map(|(t, b)| u32::from(t.sample_offset).saturating_add(b.offset_samples())),
                ramp_duration_samples: block_timing.map(|(_, b)| u32::from(b.ramp_duration)),
            });
        }
    });
    if let Err(e) = result {
        output.updates.truncate(before);
        return Err(MetadataError::new(MetadataErrorKind::OamdState(e), scope));
    }
    for (object_index, &descriptor) in descriptors.iter().enumerate() {
        let index_u8 = u8::try_from(object_index)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        output.objects.push(
            object_state(&state, index, domain, index_u8, descriptor)
                .ok_or(MetadataError::new(MetadataErrorKind::Invariant, scope))?,
        );
    }
    Ok(state)
}
fn object_state(
    state: &OamdState,
    substream_index: u32,
    domain: ObjectMetadataDomain,
    object_index: u8,
    descriptor: ObjectDescriptor,
) -> Option<MetadataObjectState> {
    Some(MetadataObjectState {
        substream_index,
        domain,
        object_index,
        descriptor,
        state: *state.object(usize::from(object_index))?,
        additional: *state.object_additional(usize::from(object_index))?,
    })
}
