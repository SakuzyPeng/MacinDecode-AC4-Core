//! CLI 身份字段适配；DBMD 序列化由通用母版库负责。
use super::*;
pub(super) fn build_logic_dbmd(
    channels: usize,
    frame_rate: MasterFrameRate,
) -> Result<Vec<u8>, String> {
    macin_atmos::dbmd::build_logic_dbmd(
        channels,
        frame_rate.into(),
        "Created by MacinDecode",
        "MacinDecode AC-4 Core",
    )
}
