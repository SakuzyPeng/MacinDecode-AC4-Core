//! AXML 与 CHNA 生成。

use super::*;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

#[derive(Debug, Clone, Copy)]
enum AdmEssence {
    Probe,
    Full,
}

impl AdmEssence {
    const fn bed_object_name(self) -> &'static str {
        match self {
            Self::Probe => "Synthetic silent 7.1.2 bed",
            Self::Full => "AC-4 full 7.1.2 bed (LFE essence; other channels silent)",
        }
    }

    const fn bed_pack_name(self) -> &'static str {
        match self {
            Self::Probe => "Synthetic 7.1.2 bed",
            Self::Full => "AC-4 full 7.1.2 compatibility bed",
        }
    }

    fn object_name(self, selector: &str) -> String {
        match self {
            Self::Probe => format!("AC-4 {selector} pink probe"),
            Self::Full => format!("AC-4 full object {selector}"),
        }
    }
}

struct AxmlWriter {
    inner: Writer<Vec<u8>>,
}

impl AxmlWriter {
    fn new() -> Result<Self, String> {
        let mut inner = Writer::new_with_indent(Vec::new(), b' ', 2);
        inner
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .map_err(xml_write_error)?;
        Ok(Self { inner })
    }

    fn start(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), String> {
        let mut event = BytesStart::new(name);
        for &(key, value) in attributes {
            // `(&str, &str)` 的 Attribute 转换由 quick-xml 执行 XML 转义。
            event.push_attribute((key, value));
        }
        self.inner
            .write_event(Event::Start(event))
            .map_err(xml_write_error)
    }

    fn end(&mut self, name: &str) -> Result<(), String> {
        self.inner
            .write_event(Event::End(BytesEnd::new(name)))
            .map_err(xml_write_error)
    }

    fn element(
        &mut self,
        name: &str,
        attributes: &[(&str, &str)],
        text: &str,
    ) -> Result<(), String> {
        self.start(name, attributes)?;
        self.inner
            .write_event(Event::Text(BytesText::new(text)))
            .map_err(xml_write_error)?;
        self.end(name)
    }

    fn finish(self) -> Result<String, String> {
        let mut bytes = self.inner.into_inner();
        bytes.push(b'\n');
        String::from_utf8(bytes).map_err(|error| format!("AXML 不是 UTF-8：{error}"))
    }
}

fn xml_write_error(error: std::io::Error) -> String {
    format!("AXML 事件写出失败：{error}")
}

pub(super) fn build_axml(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    compatibility: AdmCompatibility,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    build_axml_with_essence(
        name,
        metadata,
        selected,
        duration,
        OUTPUT_SAMPLE_RATE,
        compatibility,
        AdmEssence::Probe,
        warnings,
    )
}

pub(super) fn build_full_axml(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    build_axml_with_essence(
        name,
        metadata,
        selected,
        duration,
        sample_rate,
        compatibility,
        AdmEssence::Full,
        warnings,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_axml_with_essence(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    essence: AdmEssence,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    let start = format_sample_time_at(0, sample_rate, compatibility)?;
    let end = format_sample_time_at(duration, sample_rate, compatibility)?;
    let mut xml = AxmlWriter::new()?;
    xml.start(
        "ebuCoreMain",
        &[("xmlns", "urn:ebu:metadata-schema:ebuCore_2017")],
    )?;
    xml.start("coreMetadata", &[])?;
    xml.start("format", &[])?;
    xml.start("audioFormatExtended", &[("version", "ITU-R_BS.2076-2")])?;

    xml.start(
        "audioProgramme",
        &[
            ("audioProgrammeID", "APR_1001"),
            ("audioProgrammeName", name),
            ("start", &start),
            ("end", &end),
        ],
    )?;
    xml.element("audioContentIDRef", &[], "ACO_1001")?;
    xml.end("audioProgramme")?;

    let content_name = format!("{name} Content");
    xml.start(
        "audioContent",
        &[
            ("audioContentID", "ACO_1001"),
            ("audioContentName", &content_name),
        ],
    )?;
    xml.element("audioObjectIDRef", &[], "AO_1001")?;
    for object in selected {
        xml.element("audioObjectIDRef", &[], &object_id(object.track_index))?;
    }
    xml.end("audioContent")?;

    append_bed_object(&mut xml, duration, sample_rate, compatibility, essence)?;
    for object in selected {
        append_object(
            &mut xml,
            object,
            duration,
            sample_rate,
            compatibility,
            essence,
        )?;
    }
    append_bed_pack(&mut xml, essence)?;
    for object in selected {
        append_object_pack(&mut xml, object, essence)?;
    }
    append_bed_channels(&mut xml)?;
    for object in selected {
        append_object_channel(
            &mut xml,
            metadata,
            object,
            duration,
            sample_rate,
            compatibility,
            essence,
            warnings,
        )?;
    }
    append_stream_and_track_formats(&mut xml, selected, essence)?;
    append_track_uids(&mut xml, selected, sample_rate)?;

    xml.end("audioFormatExtended")?;
    xml.end("format")?;
    xml.end("coreMetadata")?;
    xml.end("ebuCoreMain")?;
    xml.finish()
}

fn append_bed_object(
    xml: &mut AxmlWriter,
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    essence: AdmEssence,
) -> Result<(), String> {
    let start = format_sample_time_at(0, sample_rate, compatibility)?;
    let duration = format_sample_time_at(duration, sample_rate, compatibility)?;
    xml.start(
        "audioObject",
        &[
            ("audioObjectID", "AO_1001"),
            ("audioObjectName", essence.bed_object_name()),
            ("start", &start),
            ("duration", &duration),
        ],
    )?;
    xml.element("audioPackFormatIDRef", &[], "AP_00011001")?;
    for track in 1..=BED_CHANNELS.len() {
        let track = u16::try_from(track).unwrap_or(u16::MAX);
        xml.element("audioTrackUIDRef", &[], &track_uid(track))?;
    }
    xml.end("audioObject")
}

fn append_object(
    xml: &mut AxmlWriter,
    object: &SelectedObject,
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    essence: AdmEssence,
) -> Result<(), String> {
    let start = format_sample_time_at(0, sample_rate, compatibility)?;
    let duration = format_sample_time_at(duration, sample_rate, compatibility)?;
    let object_id = object_id(object.track_index);
    let object_name = essence.object_name(&scene_selector(&object.scene));
    xml.start(
        "audioObject",
        &[
            ("audioObjectID", &object_id),
            ("audioObjectName", &object_name),
            ("start", &start),
            ("duration", &duration),
        ],
    )?;
    xml.element("audioPackFormatIDRef", &[], &object_pack_id(object.ordinal))?;
    xml.element("audioTrackUIDRef", &[], &track_uid(object.track_index))?;
    xml.end("audioObject")
}

fn append_bed_pack(xml: &mut AxmlWriter, essence: AdmEssence) -> Result<(), String> {
    xml.start(
        "audioPackFormat",
        &[
            ("audioPackFormatID", "AP_00011001"),
            ("audioPackFormatName", essence.bed_pack_name()),
            ("typeLabel", "0001"),
            ("typeDefinition", "DirectSpeakers"),
        ],
    )?;
    for index in 1..=BED_CHANNELS.len() {
        let index = u16::try_from(index).unwrap_or(u16::MAX);
        xml.element("audioChannelFormatIDRef", &[], &bed_channel_id(index))?;
    }
    xml.end("audioPackFormat")
}

fn append_object_pack(
    xml: &mut AxmlWriter,
    object: &SelectedObject,
    essence: AdmEssence,
) -> Result<(), String> {
    let pack_id = object_pack_id(object.ordinal);
    let pack_name = essence.object_name(&scene_selector(&object.scene));
    xml.start(
        "audioPackFormat",
        &[
            ("audioPackFormatID", &pack_id),
            ("audioPackFormatName", &pack_name),
            ("typeLabel", "0003"),
            ("typeDefinition", "Objects"),
        ],
    )?;
    xml.element(
        "audioChannelFormatIDRef",
        &[],
        &object_channel_id(object.ordinal),
    )?;
    xml.end("audioPackFormat")
}

fn append_bed_channels(xml: &mut AxmlWriter) -> Result<(), String> {
    for (zero_index, channel) in BED_CHANNELS.iter().enumerate() {
        let index = u16::try_from(zero_index.saturating_add(1)).unwrap_or(u16::MAX);
        let channel_id = bed_channel_id(index);
        xml.start(
            "audioChannelFormat",
            &[
                ("audioChannelFormatID", &channel_id),
                ("audioChannelFormatName", channel.name),
                ("typeLabel", "0001"),
                ("typeDefinition", "DirectSpeakers"),
            ],
        )?;
        xml.start(
            "audioBlockFormat",
            &[("audioBlockFormatID", &block_id(&channel_id, 1))],
        )?;
        xml.element("speakerLabel", &[], channel.label)?;
        xml.element("position", &[("coordinate", "X")], &number(channel.x))?;
        xml.element("position", &[("coordinate", "Y")], &number(channel.y))?;
        xml.element("position", &[("coordinate", "Z")], &number(channel.z))?;
        xml.element("cartesian", &[], "1")?;
        xml.end("audioBlockFormat")?;
        xml.end("audioChannelFormat")?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_object_channel(
    xml: &mut AxmlWriter,
    metadata: &MetadataBatch,
    object: &SelectedObject,
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    essence: AdmEssence,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let channel_id = object_channel_id(object.ordinal);
    let channel_name = essence.object_name(&scene_selector(&object.scene));
    xml.start(
        "audioChannelFormat",
        &[
            ("audioChannelFormatID", &channel_id),
            ("audioChannelFormatName", &channel_name),
            ("typeLabel", "0003"),
            ("typeDefinition", "Objects"),
        ],
    )?;
    let events = output_events_at(metadata, object, sample_rate, duration)?;
    for (index, event) in events.iter().copied().enumerate() {
        let next_sample = events
            .get(index.saturating_add(1))
            .map_or(duration, |next| next.sample);
        let block_duration = next_sample.saturating_sub(event.sample);
        append_object_block(
            xml,
            &channel_id,
            index,
            event,
            block_duration,
            &scene_selector(&object.scene),
            sample_rate,
            compatibility,
            warnings,
        )?;
    }
    xml.end("audioChannelFormat")
}

#[allow(clippy::too_many_arguments)]
fn append_object_block(
    xml: &mut AxmlWriter,
    channel_id: &str,
    index: usize,
    event: OutputEvent,
    block_duration: u64,
    selector: &str,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let state = event.state;
    let render = state.render.ok_or_else(|| {
        format!(
            "对象 {selector} 在 sample {} 缺少完整 render 状态",
            event.sample
        )
    })?;
    let basic = state.basic.ok_or_else(|| {
        format!(
            "对象 {selector} 在 sample {} 缺少完整 basic 状态",
            event.sample
        )
    })?;
    let block_number = u32::try_from(index.saturating_add(1))
        .map_err(|_| format!("对象 {selector} 的 audioBlockFormat 数量超出 u32"))?;
    let rtime = format_sample_time_at(event.sample, sample_rate, compatibility)?;
    let block_end = event
        .sample
        .checked_add(block_duration)
        .ok_or("ADM block 结束采样位置溢出")?;
    let duration = format_sample_span_at(event.sample, block_end, sample_rate, compatibility)?;
    let block_id = block_id(channel_id, block_number);
    xml.start(
        "audioBlockFormat",
        &[
            ("audioBlockFormatID", &block_id),
            ("rtime", &rtime),
            ("duration", &duration),
        ],
    )?;

    let (x, y, z) = position(render.position, event.additional);
    for (axis, value) in [("X", x), ("Y", y), ("Z", z)] {
        xml.element("position", &[("coordinate", axis)], &number(value))?;
    }

    let other = render.other_properties;
    match other.width {
        Some(WidthUpdate::Uniform(code)) if code <= 31 => {
            let value = f64::from(code) / 31.0;
            let value = number(value);
            xml.element("width", &[], &value)?;
            xml.element("height", &[], &value)?;
            xml.element("depth", &[], &value)?;
        }
        Some(WidthUpdate::Cartesian { x, y, z }) if x <= 31 && y <= 31 && z <= 31 => {
            xml.element("width", &[], &number(f64::from(x) / 31.0))?;
            xml.element("height", &[], &number(f64::from(z) / 31.0))?;
            xml.element("depth", &[], &number(f64::from(y) / 31.0))?;
        }
        Some(_) => {
            return Err(format!(
                "对象 {selector} 在 sample {} 使用超出五比特范围的 width",
                event.sample
            ));
        }
        None => {}
    }
    xml.element("cartesian", &[], "1")?;

    let gain = effective_gain(state.active, basic.gain);
    xml.element("gain", &[], &number(gain))?;

    let (snap, elevation, zone_mask) = zone_fields(render.zone);
    if snap {
        xml.element("channelLock", &[], "1")?;
    }
    if !elevation {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "elevation",
            "OAMD elevation 约束没有通用 ADM Objects 等价值",
        );
    }
    if zone_mask != 0 {
        if zone_mask > 6 {
            return Err(format!("对象 {selector} 使用保留 zone_mask {zone_mask}"));
        }
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "zones",
            "OAMD 离散渲染区域没有经验证的 ADM zoneExclusion 几何等价值",
        );
    }

    let mut ramp = event.ramp;
    if ramp > block_duration {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "ramp",
            "OAMD ramp 跨过下一事件，ADM interpolationLength 截断到当前 block duration",
        );
        ramp = block_duration;
    }
    let ramp_end = event
        .sample
        .checked_add(ramp)
        .ok_or("ADM interpolation 结束采样位置溢出")?;
    let interpolation_length = format_seconds_span_at(event.sample, ramp_end, sample_rate)?;
    xml.element(
        "jumpPosition",
        &[("interpolationLength", &interpolation_length)],
        "1",
    )?;

    if other.screen_factor_code.is_some() {
        xml.element("screenRef", &[], "1")?;
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "screen_factor",
            "OAMD 连续 screen factor 在 ADM 中只能降为 screenRef 布尔值",
        );
    }

    let importance = importance(basic.priority);
    if let ObjectPriorityState::Quantized(code) = basic.priority {
        if u16::from(code).saturating_mul(10) != u16::from(importance).saturating_mul(31) {
            warnings.push(
                selector,
                i64::try_from(event.sample).ok(),
                "importance",
                "OAMD 5 比特 priority 已舍入到 ADM 0..10 importance",
            );
        }
    }
    xml.element("importance", &[], &importance.to_string())?;

    append_unmapped_event_warnings(selector, event, other, warnings)?;
    xml.end("audioBlockFormat")
}

pub(super) fn append_unmapped_event_warnings(
    selector: &str,
    event: OutputEvent,
    other: OtherPropertiesUpdate,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let sample = i64::try_from(event.sample).ok();
    if other.depth_factor.is_some() {
        warnings.push(
            selector,
            sample,
            "depth_factor",
            "OAMD depth factor 不是 ADM Objects 的声源 depth extent，未伪映射",
        );
    }
    if other.object_at_infinity == Some(true) || other.distance_factor_code.is_some() {
        warnings.push(
            selector,
            sample,
            "distance",
            "Cartesian ADM position 没有 OAMD distance/infinity 的无损等价值",
        );
    }
    if other.divergence_mode.is_some()
        || other.divergence_table.is_some()
        || other.divergence_code.is_some()
    {
        if other.divergence_mode == Some(3) {
            return Err(format!(
                "对象 {selector} 在 sample {} 使用保留 object_div_mode 3",
                event.sample
            ));
        }
        if other.divergence_code == Some(0) {
            return Err(format!(
                "对象 {selector} 在 sample {} 使用保留 object_div_code 0",
                event.sample
            ));
        }
        warnings.push(
            selector,
            sample,
            "divergence",
            "OAMD divergence 量化表尚未验证，未伪映射为 ADM objectDivergence",
        );
    }
    if event.additional.trim_disabled {
        warnings.push(
            selector,
            sample,
            "trim",
            "逐对象 trim bypass 没有通用 ADM 等价值",
        );
    }
    if event.additional.headphone.is_some() {
        warnings.push(
            selector,
            sample,
            "headphone",
            "OAMD 近/远与头部跟踪模式不能无损转换为 ADM headphoneVirtualise",
        );
    }
    Ok(())
}

fn append_stream_and_track_formats(
    xml: &mut AxmlWriter,
    selected: &[SelectedObject],
    essence: AdmEssence,
) -> Result<(), String> {
    for (zero_index, channel) in BED_CHANNELS.iter().enumerate() {
        let index = u16::try_from(zero_index.saturating_add(1)).unwrap_or(u16::MAX);
        append_stream_and_track_format(xml, &bed_channel_id(index), channel.name)?;
    }
    for object in selected {
        let selector = essence.object_name(&scene_selector(&object.scene));
        append_stream_and_track_format(xml, &object_channel_id(object.ordinal), &selector)?;
    }
    Ok(())
}

fn append_stream_and_track_format(
    xml: &mut AxmlWriter,
    channel_id: &str,
    name: &str,
) -> Result<(), String> {
    let stream_id = channel_id.replacen("AC_", "AS_", 1);
    let track_id = format!("{}_01", channel_id.replacen("AC_", "AT_", 1));
    let format_name = format!("PCM_{name}");
    xml.start(
        "audioStreamFormat",
        &[
            ("audioStreamFormatID", &stream_id),
            ("audioStreamFormatName", &format_name),
            ("formatLabel", "0001"),
            ("formatDefinition", "PCM"),
        ],
    )?;
    xml.element("audioChannelFormatIDRef", &[], channel_id)?;
    xml.element("audioTrackFormatIDRef", &[], &track_id)?;
    xml.end("audioStreamFormat")?;

    xml.start(
        "audioTrackFormat",
        &[
            ("audioTrackFormatID", &track_id),
            ("audioTrackFormatName", &format_name),
            ("formatLabel", "0001"),
            ("formatDefinition", "PCM"),
        ],
    )?;
    xml.element("audioStreamFormatIDRef", &[], &stream_id)?;
    xml.end("audioTrackFormat")
}

fn append_track_uids(
    xml: &mut AxmlWriter,
    selected: &[SelectedObject],
    sample_rate: u32,
) -> Result<(), String> {
    for index in 1..=BED_CHANNELS.len() {
        let index = u16::try_from(index).unwrap_or(u16::MAX);
        append_track_uid(
            xml,
            index,
            &bed_channel_id(index),
            &bed_pack_id(),
            sample_rate,
        )?;
    }
    for object in selected {
        append_track_uid(
            xml,
            object.track_index,
            &object_channel_id(object.ordinal),
            &object_pack_id(object.ordinal),
            sample_rate,
        )?;
    }
    Ok(())
}

fn append_track_uid(
    xml: &mut AxmlWriter,
    track: u16,
    channel_id: &str,
    pack_id: &str,
    sample_rate: u32,
) -> Result<(), String> {
    let track_format = format!("{}_01", channel_id.replacen("AC_", "AT_", 1));
    let uid = track_uid(track);
    let sample_rate = sample_rate.to_string();
    xml.start(
        "audioTrackUID",
        &[
            ("UID", &uid),
            ("bitDepth", "24"),
            ("sampleRate", &sample_rate),
        ],
    )?;
    xml.element("audioTrackFormatIDRef", &[], &track_format)?;
    xml.element("audioPackFormatIDRef", &[], pack_id)?;
    xml.end("audioTrackUID")
}

fn output_events_at(
    metadata: &MetadataBatch,
    selected: &SelectedObject,
    sample_rate: u32,
    duration: u64,
) -> Result<Vec<OutputEvent>, String> {
    project_metadata_events(metadata, &selected.scene, sample_rate, duration)
}

pub(super) fn zone_fields(zone: ZoneUpdate) -> (bool, bool, u8) {
    zone_components(zone)
}

pub(super) fn effective_gain(active: bool, gain: ObjectGainState) -> f64 {
    if !active {
        return 0.0;
    }
    let db = match gain {
        ObjectGainState::Default => return 1.0,
        ObjectGainState::NegativeInfinity => return 0.0,
        ObjectGainState::Quantized(code) if code <= 14 => f64::from(15u8.saturating_sub(code)),
        ObjectGainState::Quantized(code) => 14.0 - f64::from(code),
    };
    10.0f64.powf(db / 20.0)
}

pub(super) fn importance(priority: ObjectPriorityState) -> u8 {
    match priority {
        ObjectPriorityState::Default => 10,
        ObjectPriorityState::Minimum => 0,
        ObjectPriorityState::Quantized(code) => {
            let scaled = u16::from(code).saturating_mul(10).saturating_add(15) / 31;
            u8::try_from(scaled).unwrap_or(10).min(10)
        }
    }
}

pub(super) fn build_chna(selected: &[SelectedObject]) -> Result<Vec<u8>, String> {
    let tracks = BED_CHANNELS
        .len()
        .checked_add(selected.len())
        .ok_or("CHNA track 数量溢出")?;
    let tracks = u16::try_from(tracks).map_err(|_| "CHNA track 数量超出 u16")?;
    let capacity = 4usize
        .checked_add(usize::from(tracks).saturating_mul(40))
        .ok_or("CHNA payload 容量溢出")?;
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(&tracks.to_le_bytes());
    out.extend_from_slice(&tracks.to_le_bytes());
    for track in 1..=u16::try_from(BED_CHANNELS.len()).unwrap_or(0) {
        append_chna_entry(&mut out, track, &bed_channel_id(track), &bed_pack_id())?;
    }
    for object in selected {
        append_chna_entry(
            &mut out,
            object.track_index,
            &object_channel_id(object.ordinal),
            &object_pack_id(object.ordinal),
        )?;
    }
    Ok(out)
}

pub(super) fn append_chna_entry(
    out: &mut Vec<u8>,
    track: u16,
    channel_id: &str,
    pack_id: &str,
) -> Result<(), String> {
    let track_format = format!("{}_01", channel_id.replacen("AC_", "AT_", 1));
    out.extend_from_slice(&track.to_le_bytes());
    out.extend_from_slice(&fixed_ascii::<12>(&track_uid(track), "audioTrackUID")?);
    out.extend_from_slice(&fixed_ascii::<14>(&track_format, "audioTrackFormatIDRef")?);
    out.extend_from_slice(&fixed_ascii::<11>(pack_id, "audioPackFormatIDRef")?);
    out.push(0);
    Ok(())
}

pub(super) fn fixed_ascii<const N: usize>(value: &str, field: &str) -> Result<[u8; N], String> {
    if !value.is_ascii() || value.len() > N {
        return Err(format!("{field} 无法写入 {N} 字节定长字段：{value:?}"));
    }
    let mut out = [0u8; N];
    let target = out
        .get_mut(..value.len())
        .ok_or_else(|| format!("{field} 长度超出定长字段"))?;
    target.copy_from_slice(value.as_bytes());
    Ok(out)
}

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn event_writer_escapes_attributes_and_text() {
        let mut writer = AxmlWriter::new().expect("writer 应可创建");
        writer
            .start("root", &[("id", "1"), ("name", "a<&\"'")])
            .expect("起始事件应可写出");
        writer
            .element("value", &[], "x<&>")
            .expect("文本事件应可写出");
        writer.end("root").expect("结束事件应可写出");
        let xml = writer.finish().expect("AXML 应为 UTF-8");

        assert!(xml.contains("<root id=\"1\" name=\"a&lt;&amp;&quot;&apos;\">"));
        assert!(xml.contains("<value>x&lt;&amp;&gt;</value>"));
        assert!(xml.ends_with('\n'));
    }
}
