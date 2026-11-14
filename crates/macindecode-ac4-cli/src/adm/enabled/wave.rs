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
    write_pcm: F,
) -> Result<(), String>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<(), String>,
{
    macin_atmos::containers::write_adm_wave_payload(
        file,
        macin_atmos::containers::AdmWaveSpec {
            frames: spec.frames,
            channels: spec.channels,
            sample_rate: spec.sample_rate,
            axml: spec.axml,
            chna: spec.chna,
            dbmd: spec.dbmd,
            compatibility: spec.compatibility.into(),
        },
        write_pcm,
    )
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
