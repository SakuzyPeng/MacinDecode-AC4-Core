//! 已解析的耳机内容策略；不应用设备或用户的播放设置。

use core::fmt;
#[cfg(any(feature = "audio-decode", test))]
use macindecode_ac4_bitstream::{
    oamd::{AdditionalObjectMetadata, Headphone},
    topology::MAX_SUBSTREAM_GROUPS,
};

/// 内容指定的耳机渲染方式。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadphoneRenderMode {
    Bypass,
    Near,
    Mid,
    Far,
}

/// 内容的参照系要求，不表示设备已经启用头追。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadTrackingPolicy {
    SceneRelative,
    HeadRelative,
}

/// 已合并模式、全局与逐对象控制的内容策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadphonePolicy {
    render_mode: HeadphoneRenderMode,
    head_tracking: HeadTrackingPolicy,
}

impl HeadphonePolicy {
    #[must_use]
    pub const fn new(render_mode: HeadphoneRenderMode, head_tracking: HeadTrackingPolicy) -> Self {
        Self {
            render_mode,
            head_tracking,
        }
    }

    #[must_use]
    pub const fn render_mode(self) -> HeadphoneRenderMode {
        self.render_mode
    }

    #[must_use]
    pub const fn head_tracking(self) -> HeadTrackingPolicy {
        self.head_tracking
    }
}

/// 原始信息已保留，但不能给出唯一耳机语义的原因。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadphonePolicyIssue {
    ReservedOperationMode(u8),
    ReservedObjectMode(u8),
    MissingGlobalControl,
    UnboundSource,
    ConflictingGroups,
}

impl fmt::Display for HeadphonePolicyIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedOperationMode(value) => {
                write!(formatter, "Reserved hp_operation_mode {value}")
            }
            Self::ReservedObjectMode(value) => {
                write!(formatter, "Reserved hp_render_mode_obj {value}")
            }
            Self::MissingGlobalControl => {
                formatter.write_str("Headphone mode lacks its global tracking control")
            }
            Self::UnboundSource => {
                formatter.write_str("Scene element has no associated audio group")
            }
            Self::ConflictingGroups => {
                formatter.write_str("Audio groups specify conflicting headphone policies")
            }
        }
    }
}

/// 合法未指定与尚不能解释的值分开表达；warm-up 的对象状态仍为 `None`。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeadphonePolicyState {
    Resolved(HeadphonePolicy),
    #[default]
    Unspecified,
    Unsupported(HeadphonePolicyIssue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(feature = "audio-decode", test))]
pub(crate) struct HeadphoneContext {
    pub(crate) group_mask: u8,
    pub(crate) groups: [Option<Headphone>; MAX_SUBSTREAM_GROUPS],
}

#[cfg(any(feature = "audio-decode", test))]
impl HeadphoneContext {
    pub(crate) const EMPTY: Self = Self {
        group_mask: 0,
        groups: [None; MAX_SUBSTREAM_GROUPS],
    };

    pub(crate) fn changed_groups(self, previous: Self) -> u8 {
        let mut changed = self.group_mask ^ previous.group_mask;
        for (index, (next, before)) in self.groups.iter().zip(previous.groups).enumerate() {
            let bit = 1u8
                .checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
                .unwrap_or(0);
            if self.group_mask & bit != 0 && *next != before {
                changed |= bit;
            }
        }
        changed
    }

    pub(crate) fn resolve(self, additional: AdditionalObjectMetadata) -> HeadphonePolicyState {
        let mut resolved = None;
        for (index, common) in self.groups.iter().enumerate() {
            let bit = 1u8
                .checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
                .unwrap_or(0);
            if self.group_mask & bit == 0 {
                continue;
            }
            let next = resolve_group(*common, additional);
            match resolved {
                Some(previous) if previous != next => {
                    return HeadphonePolicyState::Unsupported(
                        HeadphonePolicyIssue::ConflictingGroups,
                    );
                }
                None => resolved = Some(next),
                _ => {}
            }
        }
        resolved.unwrap_or(HeadphonePolicyState::Unsupported(
            HeadphonePolicyIssue::UnboundSource,
        ))
    }
}

/// TS103190-2:v1.3.1:6.3.9.10a–11，表 120b 按操作模式选择控制源。
#[cfg(any(feature = "audio-decode", test))]
fn resolve_group(
    common: Option<Headphone>,
    additional: AdditionalObjectMetadata,
) -> HeadphonePolicyState {
    use HeadphonePolicyState::{Resolved, Unspecified, Unsupported};
    let Some(common) = common.filter(|value| value.present) else {
        return Unspecified;
    };
    let tracking = |disabled| {
        if disabled {
            HeadTrackingPolicy::HeadRelative
        } else {
            HeadTrackingPolicy::SceneRelative
        }
    };
    let (mode, disabled) = match common.hp_operation_mode {
        0 => (HeadphoneRenderMode::Bypass, true),
        1 | 2 => {
            let Some(disabled) = common.head_track_disable_all else {
                return Unsupported(HeadphonePolicyIssue::MissingGlobalControl);
            };
            (
                if common.hp_operation_mode == 1 {
                    HeadphoneRenderMode::Near
                } else {
                    HeadphoneRenderMode::Far
                },
                disabled,
            )
        }
        3 => {
            let Some(object) = additional.headphone else {
                return Unspecified;
            };
            let mode = match object.render_mode {
                0 => HeadphoneRenderMode::Bypass,
                1 => HeadphoneRenderMode::Near,
                2 => HeadphoneRenderMode::Far,
                3 => HeadphoneRenderMode::Mid,
                value => return Unsupported(HeadphonePolicyIssue::ReservedObjectMode(value)),
            };
            (mode, object.head_tracking_disabled)
        }
        value => return Unsupported(HeadphonePolicyIssue::ReservedOperationMode(value)),
    };
    Resolved(HeadphonePolicy::new(mode, tracking(disabled)))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "固定容量测试夹具使用已知下标")]
mod tests {
    use super::*;
    use macindecode_ac4_bitstream::oamd::ObjectHeadphone;

    fn context(mode: u8, disabled: Option<bool>) -> HeadphoneContext {
        let mut result = HeadphoneContext::EMPTY;
        result.group_mask = 1;
        result.groups[0] = Some(Headphone {
            present: true,
            hp_operation_mode: mode,
            head_track_disable_all: disabled,
        });
        result
    }

    fn object(mode: u8, disabled: bool) -> AdditionalObjectMetadata {
        AdditionalObjectMetadata {
            headphone: Some(ObjectHeadphone {
                render_mode: mode,
                head_tracking_disabled: disabled,
            }),
            ..Default::default()
        }
    }

    fn resolved(mode: HeadphoneRenderMode, tracking: HeadTrackingPolicy) -> HeadphonePolicyState {
        HeadphonePolicyState::Resolved(HeadphonePolicy::new(mode, tracking))
    }

    #[test]
    fn operation_mode_selects_controls_instead_of_object_presence() {
        use HeadTrackingPolicy::{HeadRelative, SceneRelative};
        use HeadphoneRenderMode::{Bypass, Far, Near};
        assert_eq!(
            context(0, None).resolve(object(2, false)),
            resolved(Bypass, HeadRelative)
        );
        for (mode, render) in [(1, Near), (2, Far)] {
            for disabled in [false, true] {
                assert_eq!(
                    context(mode, Some(disabled)).resolve(object(3, !disabled)),
                    resolved(
                        render,
                        if disabled {
                            HeadRelative
                        } else {
                            SceneRelative
                        }
                    )
                );
            }
        }
        for (mode, render) in [
            (0, Bypass),
            (1, Near),
            (2, Far),
            (3, HeadphoneRenderMode::Mid),
        ] {
            for disabled in [false, true] {
                assert_eq!(
                    context(3, None).resolve(object(mode, disabled)),
                    resolved(
                        render,
                        if disabled {
                            HeadRelative
                        } else {
                            SceneRelative
                        }
                    )
                );
            }
        }
    }

    #[test]
    fn absent_controls_are_not_disabled_or_guessed() {
        let mut absent = context(1, Some(false));
        absent.groups[0] = None;
        assert_eq!(
            absent.resolve(object(2, true)),
            HeadphonePolicyState::Unspecified
        );
        absent.groups[0] = Some(Headphone::default());
        assert_eq!(
            absent.resolve(object(2, true)),
            HeadphonePolicyState::Unspecified
        );
        assert_eq!(
            context(3, None).resolve(Default::default()),
            HeadphonePolicyState::Unspecified
        );
        assert_eq!(
            context(1, None).resolve(Default::default()),
            HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::MissingGlobalControl)
        );
        assert_eq!(
            HeadphoneContext::EMPTY.resolve(Default::default()),
            HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::UnboundSource)
        );
    }

    #[test]
    fn reserved_modes_remain_explicit() {
        for mode in 4..=7 {
            assert_eq!(
                context(mode, None).resolve(object(1, false)),
                HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::ReservedOperationMode(
                    mode
                ))
            );
        }
        assert_eq!(
            context(3, None).resolve(object(4, false)),
            HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::ReservedObjectMode(4))
        );
    }

    #[test]
    fn shared_groups_compare_effective_policies_including_unspecified() {
        let mut groups = context(1, Some(false));
        groups.group_mask = 3;
        groups.groups[1] = context(3, None).groups[0];
        assert_eq!(
            groups.resolve(object(1, false)),
            context(1, Some(false)).resolve(object(1, false))
        );
        assert_eq!(
            groups.resolve(object(1, true)),
            HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::ConflictingGroups)
        );
        groups.groups[1] = None;
        assert_eq!(
            groups.resolve(Default::default()),
            HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::ConflictingGroups)
        );
        groups.groups[0] = None;
        assert_eq!(
            groups.resolve(Default::default()),
            HeadphonePolicyState::Unspecified
        );
    }

    #[test]
    fn change_detection_tracks_each_group_without_changing_semantic_equality() {
        let initial = context(1, Some(false));
        assert_eq!(initial.changed_groups(initial), 0);
        assert_eq!(context(1, Some(true)).changed_groups(initial), 1);
        let manual = context(3, None);
        assert_eq!(manual.changed_groups(initial), 1);
        assert_eq!(
            manual.resolve(object(1, false)),
            initial.resolve(object(1, false))
        );
    }
}
