//! ADM 与 DAMF 共享的场景选择、映射与试听探针基础设施。

mod core;
mod full;
mod mapping;
mod probe;
mod selection;

pub(crate) use core::{
    CoreMappedPcm, CoreSceneError, CoreSceneErrorKind, CoreSourceSelection, map_core_pcm,
    select_core_sources,
};
#[cfg(test)]
pub(crate) use full::full_s24_sample;
pub(crate) use full::{
    FULL_LFE_BED_CHANNEL, FullSceneError, FullSceneErrorKind, FullSourceSelection, PreparedFullPcm,
    map_full_pcm, prepare_full_pcm, select_full_sources, write_full_s24le,
};
pub(crate) use mapping::{MappingWarning, WarningSet, position, zone_components};
pub(crate) use probe::{
    BYTES_PER_SAMPLE, MAX_PROBE_OBJECTS, OUTPUT_SAMPLE_RATE, PinkNoise, SAMPLE_MAX, rescale_u64,
    selector_seed,
};
#[cfg(test)]
pub(crate) use selection::parse_selector as parse_scene_selector;
pub(crate) use selection::{scene_selector, select_metadata_elements, validate_selected_common};
