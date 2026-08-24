//! 已选 presentation 中对象 selector 的统一解析。

use crate::metadata_batch::{MetadataBatch, MetadataElement, MetadataElementKind};
use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionError {
    NoDynamicObjects,
    InvalidSelector(String),
    InvalidSubstream(String),
    InvalidObject(String),
    NoMatch(String),
    Ambiguous { selector: String, choices: String },
    Duplicate(String),
    ReservedGlobalTrim { selector: String, value: u8 },
    ReservedHeadphoneMode { selector: String, value: u8 },
}

impl fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDynamicObjects => {
                formatter.write_str("Selected presentation has no dynamic full-range objects")
            }
            Self::InvalidSelector(raw) => write!(formatter, "Invalid object selector {raw:?}"),
            Self::InvalidSubstream(raw) => write!(formatter, "Invalid substream index: {raw:?}"),
            Self::InvalidObject(raw) => write!(formatter, "Invalid object index: {raw:?}"),
            Self::NoMatch(raw) => {
                write!(
                    formatter,
                    "Object selector {raw:?} matches no dynamic full-range object"
                )
            }
            Self::Ambiguous { selector, choices } => write!(
                formatter,
                "Object selector {selector:?} is ambiguous; use substream:object: {choices}"
            ),
            Self::Duplicate(selector) => write!(formatter, "Object {selector} was selected twice"),
            Self::ReservedGlobalTrim { selector, value } => write!(
                formatter,
                "OAMD common for object {selector} uses reserved global_trim_mode {value}"
            ),
            Self::ReservedHeadphoneMode { selector, value } => write!(
                formatter,
                "OAMD common for object {selector} uses reserved hp_operation_mode {value}"
            ),
        }
    }
}

pub(crate) fn select_metadata_elements(
    batch: &MetadataBatch,
    selectors: &[String],
    all: bool,
) -> Result<Vec<MetadataElement>, SelectionError> {
    let mut candidates = batch
        .elements
        .iter()
        .copied()
        .filter(|item| item.kind == MetadataElementKind::DynamicObject)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|item| (item.substream_index, item.object_index));
    if candidates.is_empty() {
        return Err(SelectionError::NoDynamicObjects);
    }
    if all {
        return Ok(candidates);
    }

    let mut chosen = Vec::new();
    let mut seen = HashSet::new();
    for raw in selectors {
        let (substream, object) = parse_selector(raw)?;
        let matching = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                candidate.object_index == object
                    && substream.is_none_or(|value| candidate.substream_index == value)
            })
            .collect::<Vec<_>>();
        let selected = match matching.as_slice() {
            [one] => *one,
            [] => return Err(SelectionError::NoMatch(raw.clone())),
            _ => {
                return Err(SelectionError::Ambiguous {
                    selector: raw.clone(),
                    choices: matching
                        .iter()
                        .map(scene_selector)
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        };
        if !seen.insert(selected.element_id) {
            return Err(SelectionError::Duplicate(scene_selector(&selected)));
        }
        chosen.push(selected);
    }
    Ok(chosen)
}

pub(crate) fn parse_selector(raw: &str) -> Result<(Option<u32>, u8), SelectionError> {
    let mut parts = raw.split(':');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    if parts.next().is_some() || first.is_empty() {
        return Err(SelectionError::InvalidSelector(raw.to_owned()));
    }
    match second {
        Some(object) if !object.is_empty() => Ok((
            Some(
                first
                    .parse::<u32>()
                    .map_err(|_| SelectionError::InvalidSubstream(first.to_owned()))?,
            ),
            object
                .parse::<u8>()
                .map_err(|_| SelectionError::InvalidObject(object.to_owned()))?,
        )),
        Some(_) => Err(SelectionError::InvalidSelector(raw.to_owned())),
        None => Ok((
            None,
            first
                .parse::<u8>()
                .map_err(|_| SelectionError::InvalidObject(first.to_owned()))?,
        )),
    }
}

pub(crate) fn validate_selected_common(selected: &[MetadataElement]) -> Result<(), SelectionError> {
    for scene in selected {
        let Some(common) = scene.common else {
            continue;
        };
        let selector = scene_selector(scene);
        if common.trim.present && common.trim.global_trim_mode > 2 {
            return Err(SelectionError::ReservedGlobalTrim {
                selector,
                value: common.trim.global_trim_mode,
            });
        }
        if common.headphone.present && common.headphone.hp_operation_mode > 3 {
            return Err(SelectionError::ReservedHeadphoneMode {
                selector,
                value: common.headphone.hp_operation_mode,
            });
        }
    }
    Ok(())
}

pub(crate) fn scene_selector(scene: &MetadataElement) -> String {
    format!("{}:{}", scene.substream_index, scene.object_index)
}
