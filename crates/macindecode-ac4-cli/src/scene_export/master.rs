//! CLI 配置到通用母版库配置的转换。
impl From<crate::AdmCompatibility> for macin_atmos::Profile {
    fn from(value: crate::AdmCompatibility) -> Self {
        match value {
            crate::AdmCompatibility::Standard => Self::Standard,
            crate::AdmCompatibility::Logic => Self::Logic,
        }
    }
}
impl From<crate::DamfPresentationType> for macin_atmos::Profile {
    fn from(value: crate::DamfPresentationType) -> Self {
        match value {
            crate::DamfPresentationType::Home => Self::DamfHome,
            crate::DamfPresentationType::ThreeDof => Self::DamfThreeDof,
        }
    }
}
impl From<crate::MasterFrameRate> for macin_atmos::FrameRate {
    fn from(value: crate::MasterFrameRate) -> Self {
        match value {
            crate::MasterFrameRate::Fps23976 => Self::Fps23976,
            crate::MasterFrameRate::Fps24 => Self::Fps24,
            crate::MasterFrameRate::Fps25 => Self::Fps25,
            crate::MasterFrameRate::Fps2997 => Self::Fps2997,
            crate::MasterFrameRate::Fps2997Drop => Self::Fps2997Drop,
            crate::MasterFrameRate::Fps30 => Self::Fps30,
        }
    }
}
