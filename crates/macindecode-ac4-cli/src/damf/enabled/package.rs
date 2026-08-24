//! DAMF 三件套原子 no-clobber 提交与 CAF 探针生成。

use super::*;

pub(super) fn write_package(
    output: &Path,
    stem: &str,
    manifest: &str,
    metadata: &str,
    duration: u64,
    selected: &[SelectedObject],
    level_dbfs: f64,
) -> Result<(), CliError> {
    write_package_with_audio(
        super::super::COMMAND,
        output,
        stem,
        manifest,
        metadata,
        |path| {
            write_caf(path, duration, selected, level_dbfs)
                .map_err(|message| cli_error(DiagnosticCode::OutputWriteFailed, message))
        },
    )
}

pub(super) fn write_package_with_audio<F>(
    command: &'static str,
    output: &Path,
    stem: &str,
    manifest: &str,
    metadata: &str,
    write_audio: F,
) -> Result<(), CliError>
where
    F: FnOnce(&Path) -> Result<(), CliError>,
{
    let parent = output_parent(output);
    let base = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("damf");
    let mut temp = None;
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(".{base}.tmp-{}-{attempt}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                temp = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(package_error(
                    command,
                    DiagnosticCode::OutputCreateFailed,
                    format!("Failed to create temporary DAMF directory: {error}"),
                ));
            }
        }
    }
    let temp = temp.ok_or_else(|| {
        package_error(
            command,
            DiagnosticCode::OutputCreateFailed,
            "Failed to allocate a unique temporary DAMF directory",
        )
    })?;
    let result = (|| {
        fs::write(temp.join(format!("{stem}.atmos")), manifest).map_err(|error| {
            package_error(
                command,
                DiagnosticCode::OutputWriteFailed,
                format!("Failed to write manifest: {error}"),
            )
        })?;
        fs::write(temp.join(format!("{stem}.atmos.metadata")), metadata).map_err(|error| {
            package_error(
                command,
                DiagnosticCode::OutputWriteFailed,
                format!("Failed to write metadata: {error}"),
            )
        })?;
        write_audio(&temp.join(format!("{stem}.atmos.audio")))?;
        match renamore::rename_exclusive(&temp, output) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(package_error(
                    command,
                    DiagnosticCode::OutputExists,
                    format!(
                        "Output path was created while writing: {}",
                        output.display()
                    ),
                ));
            }
            Err(error) => {
                return Err(package_error(
                    command,
                    DiagnosticCode::OutputCommitFailed,
                    format!("Failed to commit DAMF package atomically without clobbering: {error}"),
                ));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}

fn package_error(
    command: &'static str,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> CliError {
    CliError::new(command, code, message)
}

pub(super) fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(super) fn write_caf(
    path: &Path,
    frames: u64,
    selected: &[SelectedObject],
    level_dbfs: f64,
) -> Result<(), String> {
    let channels = BED_CHANNELS
        .len()
        .checked_add(selected.len())
        .ok_or("CAF channel-count overflow")?;
    write_caf_with_audio(path, frames, channels, |writer| {
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
                writer.write_all(&chunk).map_err(io_error)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            writer.write_all(&chunk).map_err(io_error)?;
        }
        Ok(())
    })
}

pub(super) fn write_caf_with_audio<F>(
    path: &Path,
    frames: u64,
    channels: usize,
    write_audio: F,
) -> Result<(), String>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<(), String>,
{
    let bytes_per_packet = u64::try_from(channels)
        .map_err(|_| "CAF channel count exceeds u64")?
        .checked_mul(BYTES_PER_SAMPLE)
        .ok_or("CAF packet-byte-count overflow")?;
    let payload_bytes = frames
        .checked_mul(bytes_per_packet)
        .ok_or("CAF payload-size overflow")?;
    let data_chunk_size = payload_bytes
        .checked_add(4)
        .ok_or("CAF data-chunk overflow")?;
    let mut writer = BufWriter::new(
        File::create(path).map_err(|error| format!("Failed to create CAF: {error}"))?,
    );
    writer.write_all(b"caff").map_err(io_error)?;
    writer.write_all(&1u16.to_be_bytes()).map_err(io_error)?;
    writer.write_all(&0u16.to_be_bytes()).map_err(io_error)?;
    writer.write_all(b"desc").map_err(io_error)?;
    writer.write_all(&32i64.to_be_bytes()).map_err(io_error)?;
    writer
        .write_all(&f64::from(OUTPUT_SAMPLE_RATE).to_bits().to_be_bytes())
        .map_err(io_error)?;
    writer.write_all(b"lpcm").map_err(io_error)?;
    writer.write_all(&2u32.to_be_bytes()).map_err(io_error)?;
    writer
        .write_all(
            &u32::try_from(bytes_per_packet)
                .map_err(|_| "CAF packet size exceeds u32")?
                .to_be_bytes(),
        )
        .map_err(io_error)?;
    writer.write_all(&1u32.to_be_bytes()).map_err(io_error)?;
    writer
        .write_all(
            &u32::try_from(channels)
                .map_err(|_| "CAF channel count exceeds u32")?
                .to_be_bytes(),
        )
        .map_err(io_error)?;
    writer.write_all(&24u32.to_be_bytes()).map_err(io_error)?;
    writer.write_all(b"data").map_err(io_error)?;
    writer
        .write_all(
            &i64::try_from(data_chunk_size)
                .map_err(|_| "CAF data chunk exceeds i64")?
                .to_be_bytes(),
        )
        .map_err(io_error)?;
    writer.write_all(&0u32.to_be_bytes()).map_err(io_error)?;
    write_audio(&mut writer)?;
    writer.flush().map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PACKAGE_ROOT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn package_publish_never_replaces_a_target_created_during_audio_write() {
        let serial = NEXT_PACKAGE_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "macinac4-damf-publish-{}-{serial}",
            std::process::id()
        ));
        let output = root.join("package");
        fs::create_dir(&root).expect("应能创建 DAMF 发布测试目录");

        let result = write_package_with_audio(
            super::super::super::FULL_COMMAND,
            &output,
            "race",
            "manifest",
            "metadata",
            |audio| {
                fs::write(audio, b"audio").map_err(|error| {
                    package_error(
                        super::super::super::FULL_COMMAND,
                        DiagnosticCode::OutputWriteFailed,
                        format!("写竞态测试音频失败：{error}"),
                    )
                })?;
                fs::create_dir(&output).expect("应能在提交前创建竞态目标目录");
                Ok(())
            },
        )
        .expect_err("原子发布不得替换写入期间出现的目标目录");

        assert_eq!(result.command, super::super::super::FULL_COMMAND);
        assert_eq!(result.code, DiagnosticCode::OutputExists);
        assert!(
            output
                .read_dir()
                .expect("竞态目标目录应保留")
                .next()
                .is_none(),
            "失败发布不得把临时包移入竞态目标"
        );
        assert_eq!(
            root.read_dir()
                .expect("测试根目录应可读取")
                .filter_map(Result::ok)
                .count(),
            1,
            "失败发布应清理同级临时包"
        );

        fs::remove_dir_all(root).expect("应能清理 DAMF 发布测试目录");
    }
}
