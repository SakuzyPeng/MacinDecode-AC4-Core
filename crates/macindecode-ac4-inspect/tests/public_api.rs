#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "tests construct fixed, minimal AC-4 and ISO BMFF layouts"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use macindecode_ac4_inspect::{
    InspectError, InspectInputFormat, InspectSourceHint, InspectSourceKind, inspect_bytes,
    inspect_path,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temp_path(extension: &str) -> PathBuf {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "macindecode-ac4-inspect-public-{}-{serial}.{extension}",
        std::process::id()
    ))
}

fn pack_bits(bits: &str) -> Vec<u8> {
    let count = bits
        .chars()
        .filter(|character| matches!(character, '0' | '1'))
        .count();
    let mut output = vec![0u8; count.div_ceil(8)];
    let mut index = 0usize;
    for character in bits.chars() {
        if matches!(character, '0' | '1') {
            if character == '1' {
                output[index / 8] |= 1 << (7 - index % 8);
            }
            index += 1;
        }
    }
    output
}

fn topology_frame(sequence_counter: u16, iframe: bool) -> Vec<u8> {
    let iframe = u8::from(iframe);
    let bits = format!(
        "10{sequence_counter:010b} 0 1 1101 {iframe} 1 0 0 \
         1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 \
         1 0 1 1 10 0 0 1 01 0 \
         10 0 0000000011 0 0000000100"
    );
    let mut output = pack_bits(&bits);
    output.extend_from_slice(&[0x55, 0x04, 0x00]);
    output.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]);
    output
}

fn annex_g(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for frame in frames {
        stream.extend_from_slice(&[0xAC, 0x40]);
        stream.extend_from_slice(&u16::try_from(frame.len()).unwrap().to_be_bytes());
        stream.extend_from_slice(frame);
    }
    stream
}

fn mp4_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).unwrap();
    let mut output = Vec::from(size.to_be_bytes());
    output.extend_from_slice(box_type);
    output.extend_from_slice(payload);
    output
}

fn full_box_table(box_type: &[u8; 4], words: &[u32]) -> Vec<u8> {
    let mut payload = vec![0u8; 4];
    for word in words {
        payload.extend_from_slice(&word.to_be_bytes());
    }
    mp4_box(box_type, &payload)
}

fn header_timing(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    payload
}

fn sample_description(include_second_entry: bool) -> Vec<u8> {
    let mut sample_entry = vec![0u8; 28];
    sample_entry.extend_from_slice(&mp4_box(b"dac4", &[0x00, 0xBA, 0x01]));
    let sample_entry = mp4_box(b"ac-4", &sample_entry);
    let mut payload = vec![0u8; 8];
    let entry_count = if include_second_entry { 2u32 } else { 1u32 };
    payload[4..8].copy_from_slice(&entry_count.to_be_bytes());
    payload.extend_from_slice(&sample_entry);
    if include_second_entry {
        payload.extend_from_slice(&mp4_box(b"mp4a", &[0u8; 28]));
    }
    mp4_box(b"stsd", &payload)
}

fn audio_track(chunk_offset: u32, frames: &[Vec<u8>], sample_description_index: u32) -> Vec<u8> {
    let count = u32::try_from(frames.len()).unwrap();
    let mut stbl_payload = sample_description(sample_description_index != 1);
    stbl_payload.extend_from_slice(&full_box_table(b"stts", &[1, count, 2_048]));
    stbl_payload.extend_from_slice(&full_box_table(
        b"stsc",
        &[1, 1, count, sample_description_index],
    ));
    let mut sizes = vec![0, count];
    sizes.extend(
        frames
            .iter()
            .map(|frame| u32::try_from(frame.len()).unwrap()),
    );
    stbl_payload.extend_from_slice(&full_box_table(b"stsz", &sizes));
    stbl_payload.extend_from_slice(&full_box_table(b"stco", &[1, chunk_offset]));
    let stbl = mp4_box(b"stbl", &stbl_payload);
    let minf = mp4_box(b"minf", &stbl);
    let duration = count.checked_mul(2_048).unwrap();
    let mut mdia_payload = mp4_box(b"mdhd", &header_timing(48_000, duration));
    mdia_payload.extend_from_slice(&minf);
    mp4_box(b"trak", &mp4_box(b"mdia", &mdia_payload))
}

fn moov(chunk_offset: u32, frames: &[Vec<u8>], sample_description_index: u32) -> Vec<u8> {
    let duration = u32::try_from(frames.len())
        .unwrap()
        .checked_mul(2_048)
        .unwrap();
    let mut payload = mp4_box(b"mvhd", &header_timing(48_000, duration));
    payload.extend_from_slice(&audio_track(chunk_offset, frames, sample_description_index));
    mp4_box(b"moov", &payload)
}

fn minimal_mp4(frames: &[Vec<u8>]) -> Vec<u8> {
    minimal_mp4_with_sample_description(frames, 1)
}

fn minimal_mp4_with_sample_description(
    frames: &[Vec<u8>],
    sample_description_index: u32,
) -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0isom");
    let placeholder = moov(0, frames, sample_description_index);
    let chunk_offset = u32::try_from(ftyp.len() + placeholder.len() + 8).unwrap();
    let moov = moov(chunk_offset, frames, sample_description_index);
    let media = frames.iter().flatten().copied().collect::<Vec<_>>();
    [ftyp, moov, mp4_box(b"mdat", &media)].concat()
}

#[test]
fn bytes_auto_and_forced_annex_g_expose_owned_public_report() {
    let data = annex_g(&[topology_frame(0, true)]);
    let automatic = inspect_bytes(&data, InspectSourceHint::default()).unwrap();
    assert_eq!(automatic.source.kind, InspectSourceKind::AnnexG);
    assert_eq!(automatic.source.input, "<memory>");
    assert_eq!(automatic.source.frame_count, 1);
    assert_eq!(
        automatic.stream.sample_rate.value,
        Some(serde_json::json!(48_000))
    );
    assert!(automatic.render_text().ends_with('\n'));

    let named = inspect_bytes(
        &data,
        InspectSourceHint::new(Some("network-packet.ac4"), InspectInputFormat::AnnexG),
    )
    .unwrap();
    assert_eq!(named.source.input, "network-packet.ac4");
    assert_eq!(
        serde_json::to_value(&named).unwrap()["source"]["kind"],
        "annex_g"
    );

    let path = temp_path("ac4");
    fs::write(&path, &data).unwrap();
    let from_path = inspect_path(&path).unwrap();
    let _ = fs::remove_file(&path);
    assert_eq!(from_path.source.kind, InspectSourceKind::AnnexG);
    assert_eq!(from_path.source.frame_count, automatic.source.frame_count);
}

#[test]
fn path_and_memory_mp4_reports_are_identical_with_the_same_name() {
    let data = minimal_mp4(&[topology_frame(0, true), topology_frame(1, false)]);
    let path = temp_path("m4a");
    fs::write(&path, &data).unwrap();

    let from_path = inspect_path(&path).unwrap();
    let display = path.display().to_string();
    let from_memory = inspect_bytes(
        &data,
        InspectSourceHint::new(Some(&display), InspectInputFormat::Mp4),
    )
    .unwrap();
    let _ = fs::remove_file(&path);

    assert_eq!(from_path.source.kind, InspectSourceKind::Mp4);
    assert_eq!(
        serde_json::to_value(&from_path).unwrap(),
        serde_json::to_value(&from_memory).unwrap()
    );
    assert_eq!(from_path.render_text(), from_memory.render_text());
}

#[test]
fn forced_wrong_format_returns_the_resolved_parse_kind() {
    let data = annex_g(&[topology_frame(0, true)]);
    let error = inspect_bytes(
        &data,
        InspectSourceHint::new(Some("forced"), InspectInputFormat::Mp4),
    )
    .unwrap_err();
    match error {
        InspectError::Parse {
            input,
            format,
            cause,
        } => {
            assert_eq!(input, "forced");
            assert_eq!(format, InspectSourceKind::Mp4);
            assert!(cause.contains("moov box not found"));
        }
        other => panic!("expected parse error, got {other:?}"),
    }
}

#[test]
fn mp4_samples_must_reference_the_selected_ac4_entry() {
    let data = minimal_mp4_with_sample_description(&[topology_frame(0, true)], 2);
    let error = inspect_bytes(
        &data,
        InspectSourceHint::new(Some("mixed.m4a"), InspectInputFormat::Mp4),
    )
    .unwrap_err();
    match error {
        InspectError::Parse { cause, .. } => assert!(
            cause.contains(
                "MP4 sample 0 references sample description 2, but selected AC-4 entry is 1"
            ),
            "unexpected parse cause: {cause}"
        ),
        other => panic!("expected parse error, got {other:?}"),
    }
}

#[test]
fn empty_and_missing_inputs_keep_structured_error_context() {
    assert!(matches!(
        inspect_bytes(&[], InspectSourceHint::default()),
        Err(InspectError::EmptyInput { input }) if input == "<memory>"
    ));

    let path = temp_path("missing");
    let _ = fs::remove_file(&path);
    assert!(matches!(
        inspect_path(&path),
        Err(InspectError::Read { path: failed, .. }) if failed == path
    ));
}
