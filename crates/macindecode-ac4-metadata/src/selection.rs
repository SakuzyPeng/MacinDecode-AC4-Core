//! 只根据 topology 选择 presentation，与音频重建能力无关。
use crate::PresentationSelection;
use core::fmt;
/// presentation 选择失败。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSelectionError {
    /// 没有携带音频 group 的 eligible presentation。
    NoEligiblePresentation { declared: u32 },
    /// `AutoUnique` 遇到多个 eligible presentation。
    Ambiguous { eligible: u32 },
    /// 零基下标超出码流声明范围。
    IndexOutOfRange { requested: u32, declared: u32 },
    /// `presentation_id` 在当前配置中不存在。
    IdNotFound { requested: u32 },
    /// 同一 `presentation_id` 在当前配置中出现多次。
    IdNotUnique { requested: u32, matches: u32 },
    /// 显式选择到了只携带数据或不引用音频 group 的 presentation。
    NotEligible { index: u32 },
}

impl fmt::Display for PresentationSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NoEligiblePresentation { declared } => write!(
                formatter,
                "Input declares {declared} presentations but has no eligible audio presentation"
            ),
            Self::Ambiguous { eligible } => write!(
                formatter,
                "Input has {eligible} eligible presentations; AutoUnique cannot make a unique selection"
            ),
            Self::IndexOutOfRange {
                requested,
                declared,
            } => write!(
                formatter,
                "Presentation index {requested} is out of range; input declares {declared} presentations"
            ),
            Self::IdNotFound { requested } => {
                write!(formatter, "presentation_id {requested} does not exist")
            }
            Self::IdNotUnique { requested, matches } => write!(
                formatter,
                "presentation_id {requested} occurs {matches} times and cannot be selected uniquely"
            ),
            Self::NotEligible { index } => {
                write!(
                    formatter,
                    "Presentation {index} has no selectable audio group"
                )
            }
        }
    }
}

impl core::error::Error for PresentationSelectionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationCandidate {
    pub index: u32,
    pub id: Option<u32>,
    pub eligible: bool,
}

impl PresentationCandidate {
    pub const EMPTY: Self = Self {
        index: 0,
        id: None,
        eligible: false,
    };
}

pub fn select_candidate(
    candidates: &[PresentationCandidate],
    selection: PresentationSelection,
) -> Result<PresentationCandidate, PresentationSelectionError> {
    let declared = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let selected = match selection {
        PresentationSelection::AutoUnique => {
            let mut selected = None;
            let mut eligible = 0u32;
            for candidate in candidates.iter().copied().filter(|item| item.eligible) {
                eligible = eligible.saturating_add(1);
                selected = Some(candidate);
            }
            match (eligible, selected) {
                (0, _) => {
                    return Err(PresentationSelectionError::NoEligiblePresentation { declared });
                }
                (1, Some(candidate)) => candidate,
                (count, _) => {
                    return Err(PresentationSelectionError::Ambiguous { eligible: count });
                }
            }
        }
        PresentationSelection::Index(requested) => {
            let index = usize::try_from(requested).unwrap_or(usize::MAX);
            candidates
                .get(index)
                .copied()
                .ok_or(PresentationSelectionError::IndexOutOfRange {
                    requested,
                    declared,
                })?
        }
        PresentationSelection::Id(requested) => {
            let mut selected = None;
            let mut matches = 0u32;
            for candidate in candidates
                .iter()
                .copied()
                .filter(|item| item.id == Some(requested))
            {
                matches = matches.saturating_add(1);
                selected = Some(candidate);
            }
            match (matches, selected) {
                (0, _) => return Err(PresentationSelectionError::IdNotFound { requested }),
                (1, Some(candidate)) => candidate,
                (count, _) => {
                    return Err(PresentationSelectionError::IdNotUnique {
                        requested,
                        matches: count,
                    });
                }
            }
        }
    };

    if !selected.eligible {
        return Err(PresentationSelectionError::NotEligible {
            index: selected.index,
        });
    }
    Ok(selected)
}
