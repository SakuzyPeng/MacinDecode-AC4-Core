//! Logic 兼容 dbmd 生成。

use super::*;

pub(super) fn build_logic_dbmd(
    channel_count: usize,
    frame_rate: MasterFrameRate,
) -> Result<Vec<u8>, String> {
    let channel_count =
        u16::try_from(channel_count).map_err(|_| "DBMD channel count exceeds u16")?;
    if channel_count > 128 {
        return Err("DBMD Atmos supplemental data supports at most 128 channels".to_owned());
    }

    let mut out = Vec::new();
    out.extend_from_slice(&DBMD_VERSION);

    // EBU Tech 3285 Supplement 6 定义 segment 头、终止标记和校验和；7/9/10
    // 是 Logic 互操作配置所需的私有负载。这里只写项目夹具与结构判据已覆盖的
    // 字段，不复制母版块，也不声明生成物来自任何厂商设备。
    let mut ddplus = [0u8; 96];
    for (offset, value) in [
        (1usize, 0x47u8),
        (5, 0x60),
        (8, 0x24),
        (9, 0x24),
        (14, 0x02),
        (15, 0x02),
    ] {
        set_dbmd_byte(&mut ddplus, offset, value)?;
    }
    append_dbmd_segment(&mut out, 7, &ddplus)?;

    let mut atmos = [0u8; 248];
    copy_dbmd_ascii(&mut atmos, 0, 32, "Created by MacinDecode")?;
    copy_dbmd_ascii(&mut atmos, 32, 64, "MacinDecode AC-4 Core")?;
    for (offset, value) in [
        (97usize, 0x01u8), // content creation tool 0.1.0
        (111, frame_rate.dbmd_code()),
        (112, 0xff),
        (118, 0x03),
        (134, 0xf0),
        (135, 0x08),
        (139, 0x10),
        (152, 0x83), // home content, Lo/Ro warp
        (168, 0xf0),
        (170, 0x08),
        (184, 0xf0),
        (186, 0x08),
        (200, 0xf0),
        (202, 0x08),
    ] {
        set_dbmd_byte(&mut atmos, offset, value)?;
    }
    append_dbmd_segment(&mut out, 9, &atmos)?;

    let channel_count_usize = usize::from(channel_count);
    let mut supplemental =
        Vec::with_capacity(142usize.saturating_add(channel_count_usize.saturating_mul(2)));
    supplemental.extend_from_slice(&DBMD_ATMOS_SUPPLEMENTAL_SYNC);
    supplemental.extend_from_slice(&channel_count.to_le_bytes());
    supplemental.push(0); // reserved
    for config in 0..9 {
        let auto_trim = matches!(config, 0 | 3 | 5 | 6 | 8);
        supplemental.push(u8::from(auto_trim));
        supplemental.extend_from_slice(&[0; 12]);
        if auto_trim {
            supplemental.extend_from_slice(&[0; 2]);
        } else {
            supplemental.extend_from_slice(&[0x80, 0x80]);
        }
    }
    supplemental.resize(supplemental.len().saturating_add(channel_count_usize), 0); // trim bypass
    for channel in 0..channel_count_usize {
        // lower三位是 binauralRenderMode；0x40 表示 scene-relative。
        supplemental.push(if channel == 3 { 0x40 } else { 0x44 });
    }
    append_dbmd_segment(&mut out, 10, &supplemental)?;

    out.push(0); // segment terminator
    if out.len() & 1 != 0 {
        out.push(0); // RIFF payload alignment
    }
    Ok(out)
}

pub(super) fn set_dbmd_byte<const N: usize>(
    target: &mut [u8; N],
    offset: usize,
    value: u8,
) -> Result<(), String> {
    let byte = target
        .get_mut(offset)
        .ok_or("Internal DBMD field offset is out of bounds")?;
    *byte = value;
    Ok(())
}

pub(super) fn copy_dbmd_ascii<const N: usize>(
    target: &mut [u8; N],
    offset: usize,
    width: usize,
    value: &str,
) -> Result<(), String> {
    if !value.is_ascii() || value.len() > width {
        return Err("Internal DBMD ASCII field is invalid".to_owned());
    }
    let end = offset
        .checked_add(value.len())
        .ok_or("Internal DBMD ASCII field-offset overflow")?;
    let field = target
        .get_mut(offset..end)
        .ok_or("Internal DBMD ASCII field is out of bounds")?;
    field.copy_from_slice(value.as_bytes());
    Ok(())
}

pub(super) fn append_dbmd_segment(out: &mut Vec<u8>, id: u8, payload: &[u8]) -> Result<(), String> {
    let size = u16::try_from(payload.len()).map_err(|_| "DBMD segment exceeds u16")?;
    out.push(id);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(payload);
    let size_low = size.to_le_bytes().first().copied().unwrap_or(0);
    let sum = payload
        .iter()
        .fold(size_low, |sum, byte| sum.wrapping_add(*byte));
    out.push(0u8.wrapping_sub(sum));
    Ok(())
}
