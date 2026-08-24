//! BW64/RF64 原子写入与 PCM 探针生成。

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn write_atomic_adm(
    output: &Path,
    frames: u64,
    selected: &[SelectedObject],
    level_dbfs: f64,
    axml: &[u8],
    chna: &[u8],
    dbmd: Option<&[u8]>,
    compatibility: AdmCompatibility,
) -> Result<(), CliError> {
    let temp = create_temp_file(output)
        .map_err(|message| cli_error(DiagnosticCode::OutputCreateFailed, message))?;
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temp)
            .map_err(|error| {
                cli_error(
                    DiagnosticCode::OutputCreateFailed,
                    format!("Failed to open temporary ADM BWF: {error}"),
                )
            })?;
        write_adm_wave(
            file,
            frames,
            selected,
            level_dbfs,
            axml,
            chna,
            dbmd,
            compatibility,
        )
        .map_err(|message| cli_error(DiagnosticCode::OutputWriteFailed, message))?;
        if output.exists() {
            return Err(cli_error(
                DiagnosticCode::OutputExists,
                format!(
                    "Output path was created while writing: {}",
                    output.display()
                ),
            ));
        }
        fs::rename(&temp, output).map_err(|error| {
            cli_error(
                DiagnosticCode::OutputCommitFailed,
                format!("Failed to commit ADM BWF: {error}"),
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(super) fn create_temp_file(output: &Path) -> Result<PathBuf, String> {
    let parent = output_parent(output);
    let base = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("adm-bwf.wav");
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(".{base}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Failed to create temporary ADM BWF: {error}")),
        }
    }
    Err("Failed to allocate a unique temporary ADM BWF file".to_owned())
}

pub(super) struct AdmWaveSpec<'a> {
    pub frames: u64,
    pub channels: usize,
    pub sample_rate: u32,
    pub axml: &'a [u8],
    pub chna: &'a [u8],
    pub dbmd: Option<&'a [u8]>,
    pub compatibility: AdmCompatibility,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_adm_wave(
    file: File,
    frames: u64,
    selected: &[SelectedObject],
    level_dbfs: f64,
    axml: &[u8],
    chna: &[u8],
    dbmd: Option<&[u8]>,
    compatibility: AdmCompatibility,
) -> Result<(), String> {
    let channels = BED_CHANNELS
        .len()
        .checked_add(selected.len())
        .ok_or("ADM BWF channel-count overflow")?;
    write_adm_wave_payload(
        file,
        AdmWaveSpec {
            frames,
            channels,
            sample_rate: OUTPUT_SAMPLE_RATE,
            axml,
            chna,
            dbmd,
            compatibility,
        },
        |writer| write_pcm(writer, frames, selected, level_dbfs),
    )
}

pub(super) fn write_adm_wave_payload<F>(
    file: File,
    spec: AdmWaveSpec<'_>,
    write_pcm_payload: F,
) -> Result<(), String>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<(), String>,
{
    let AdmWaveSpec {
        frames,
        channels,
        sample_rate,
        axml,
        chna,
        dbmd,
        compatibility,
    } = spec;
    let channels_u16 = u16::try_from(channels).map_err(|_| "ADM BWF channel count exceeds u16")?;
    let block_align_u64 = u64::from(channels_u16)
        .checked_mul(BYTES_PER_SAMPLE)
        .ok_or("ADM BWF blockAlign overflow")?;
    let block_align =
        u16::try_from(block_align_u64).map_err(|_| "ADM BWF blockAlign exceeds u16")?;
    let bytes_per_second = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or("ADM BWF bytesPerSecond overflow")?;
    let data_size = frames
        .checked_mul(block_align_u64)
        .ok_or("ADM BWF data-size overflow")?;
    let axml_size = u64::try_from(axml.len()).map_err(|_| "AXML size exceeds u64")?;
    let chna_size = u64::try_from(chna.len()).map_err(|_| "CHNA size exceeds u64")?;
    let dbmd_size = dbmd
        .map(|value| u64::try_from(value.len()).map_err(|_| "DBMD size exceeds u64"))
        .transpose()?;
    let dbmd_total = dbmd_size.map(chunk_total).transpose()?.unwrap_or(0);
    let axml_uses_ds64 = axml_size >= u64::from(u32::MAX);
    let ds64_payload_size = 28u64
        .checked_add(if axml_uses_ds64 { 12 } else { 0 })
        .ok_or("ds64 size overflow")?;
    let total_size = 12u64
        .checked_add(chunk_total(ds64_payload_size)?)
        .and_then(|value| value.checked_add(24))
        .and_then(|value| value.checked_add(chunk_total(chna_size).ok()?))
        .and_then(|value| value.checked_add(chunk_total(axml_size).ok()?))
        .and_then(|value| value.checked_add(dbmd_total))
        .and_then(|value| value.checked_add(chunk_total(data_size).ok()?))
        .ok_or("ADM BWF file-size overflow")?;
    let riff_size = total_size
        .checked_sub(8)
        .ok_or("ADM BWF outer-size underflow")?;

    let mut writer = BufWriter::new(file);
    writer
        .write_all(compatibility.container_id())
        .map_err(adm_io_error)?;
    writer
        .write_all(&u32::MAX.to_le_bytes())
        .map_err(adm_io_error)?;
    writer.write_all(b"WAVE").map_err(adm_io_error)?;

    writer.write_all(b"ds64").map_err(adm_io_error)?;
    writer
        .write_all(
            &u32::try_from(ds64_payload_size)
                .map_err(|_| "ds64 payload exceeds u32")?
                .to_le_bytes(),
        )
        .map_err(adm_io_error)?;
    writer
        .write_all(&riff_size.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&data_size.to_le_bytes())
        .map_err(adm_io_error)?;
    let sample_count = match compatibility {
        AdmCompatibility::Standard => 0,
        AdmCompatibility::Logic => frames,
    };
    writer
        .write_all(&sample_count.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&u32::from(axml_uses_ds64).to_le_bytes())
        .map_err(adm_io_error)?;
    if axml_uses_ds64 {
        writer.write_all(b"axml").map_err(adm_io_error)?;
        writer
            .write_all(&axml_size.to_le_bytes())
            .map_err(adm_io_error)?;
    }

    writer.write_all(b"fmt ").map_err(adm_io_error)?;
    writer
        .write_all(&16u32.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&1u16.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&channels_u16.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&sample_rate.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&bytes_per_second.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&block_align.to_le_bytes())
        .map_err(adm_io_error)?;
    writer
        .write_all(&24u16.to_le_bytes())
        .map_err(adm_io_error)?;

    write_chunk(&mut writer, b"chna", chna, false)?;
    write_chunk(&mut writer, b"axml", axml, axml_uses_ds64)?;
    if let Some(dbmd) = dbmd {
        write_chunk(&mut writer, b"dbmd", dbmd, false)?;
    }
    writer.write_all(b"data").map_err(adm_io_error)?;
    writer
        .write_all(&u32::MAX.to_le_bytes())
        .map_err(adm_io_error)?;
    write_pcm_payload(&mut writer)?;
    if data_size & 1 != 0 {
        writer.write_all(&[0]).map_err(adm_io_error)?;
    }
    writer.flush().map_err(adm_io_error)?;
    let file = writer
        .into_inner()
        .map_err(|error| format!("Failed to finish writing ADM BWF: {}", error.error()))?;
    file.sync_all()
        .map_err(|error| format!("Failed to sync ADM BWF: {error}"))
}

pub(super) fn chunk_total(payload: u64) -> Result<u64, String> {
    8u64.checked_add(payload)
        .and_then(|value| value.checked_add(payload & 1))
        .ok_or("RIFF chunk-size overflow".to_owned())
}

pub(super) fn write_chunk<W: Write>(
    writer: &mut W,
    id: &[u8; 4],
    data: &[u8],
    uses_ds64: bool,
) -> Result<(), String> {
    writer.write_all(id).map_err(adm_io_error)?;
    let size = if uses_ds64 {
        u32::MAX
    } else {
        u32::try_from(data.len()).map_err(|_| "RIFF chunk payload exceeds u32")?
    };
    writer
        .write_all(&size.to_le_bytes())
        .map_err(adm_io_error)?;
    writer.write_all(data).map_err(adm_io_error)?;
    if data.len() & 1 != 0 {
        writer.write_all(&[0]).map_err(adm_io_error)?;
    }
    Ok(())
}

pub(super) fn write_pcm<W: Write>(
    writer: &mut W,
    frames: u64,
    selected: &[SelectedObject],
    level_dbfs: f64,
) -> Result<(), String> {
    let channels = BED_CHANNELS.len().saturating_add(selected.len());
    let mut generators = selected
        .iter()
        .map(|object| PinkNoise::new(selector_seed(&object.scene)))
        .collect::<Vec<_>>();
    let amplitude = 10.0f64.powf(level_dbfs / 20.0) * SAMPLE_MAX;
    let fade = u64::from(OUTPUT_SAMPLE_RATE / 100);
    let mut chunk = Vec::with_capacity(channels.saturating_mul(3).saturating_mul(1024));
    for frame in 0..frames {
        for _ in 0..BED_CHANNELS.len() {
            chunk.extend_from_slice(&[0, 0, 0]);
        }
        let edge = frame.min(frames.saturating_sub(1).saturating_sub(frame));
        let fade_gain = if edge < fade {
            edge as f64 / fade as f64
        } else {
            1.0
        };
        for generator in &mut generators {
            let value = (generator.next() * amplitude * fade_gain)
                .round()
                .clamp(-SAMPLE_MAX, SAMPLE_MAX) as i32;
            let bytes = value.to_le_bytes();
            chunk.extend_from_slice(bytes.get(..3).unwrap_or(&[0, 0, 0]));
        }
        if chunk.len() >= channels.saturating_mul(3).saturating_mul(1024) {
            writer.write_all(&chunk).map_err(adm_io_error)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        writer.write_all(&chunk).map_err(adm_io_error)?;
    }
    Ok(())
}
