//! AC-4 audio track discovery in ISO BMFF containers.

use core::fmt;

use crate::{BoxError, BoxIter, Mp4Box, SampleInfo, find_box, find_path};

/// Byte length of the fixed `AudioSampleEntry` fields before child boxes.
///
/// ISO/IEC 14496-12 and `TS103190-1:v1.4.1` Table E.2 define this as
///
/// ```text
/// reserved[6] + data_reference_index(2) + reserved[2](8) + channel_count(2)
/// + sample_size(2) + reserved(4) + sampling_frequency(2) + reserved(2)
/// ```
pub const AUDIO_SAMPLE_ENTRY_LEN: usize = 28;

/// Failure while locating or validating an AC-4 track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackError {
    /// The FullBox header or `entry_count` is truncated.
    StsdHeaderTruncated {
        /// Available payload bytes.
        available: usize,
    },
    /// A declared sample entry is not a valid bounded ISO BMFF box.
    StsdEntryInvalid {
        /// One-based sample-entry index.
        index: u32,
        /// Box framing failure.
        source: BoxError,
    },
    /// Fewer sample entries are present than declared by `entry_count`.
    StsdEntryCountMismatch {
        /// Declared entry count.
        declared: u32,
        /// Completely parsed entry count.
        parsed: u32,
    },
    /// Bytes remain after every declared sample entry has been consumed.
    StsdTrailingBytes {
        /// Declared entry count.
        declared: u32,
        /// Undeclared trailing byte count.
        trailing: usize,
    },
    /// A sample references a different `stsd` entry than the selected AC-4 entry.
    SampleDescriptionMismatch {
        /// Zero-based sample index.
        sample: u32,
        /// Selected one-based AC-4 entry index.
        selected: u32,
        /// One-based entry index referenced through `stsc`.
        referenced: u32,
    },
}

impl fmt::Display for TrackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::StsdHeaderTruncated { available } => write!(
                formatter,
                "Truncated stsd header; only {available} payload bytes are available"
            ),
            Self::StsdEntryInvalid { index, ref source } => {
                write!(formatter, "Invalid stsd sample entry {index}: {source}")
            }
            Self::StsdEntryCountMismatch { declared, parsed } => write!(
                formatter,
                "stsd declares {declared} sample entries, but only {parsed} are present"
            ),
            Self::StsdTrailingBytes { declared, trailing } => write!(
                formatter,
                "stsd has {trailing} undeclared trailing bytes after {declared} sample entries"
            ),
            Self::SampleDescriptionMismatch {
                sample,
                selected,
                referenced,
            } => write!(
                formatter,
                "MP4 sample {sample} references sample description {referenced}, but selected AC-4 entry is {selected}"
            ),
        }
    }
}

impl core::error::Error for TrackError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::StsdEntryInvalid { source, .. } => Some(source),
            Self::StsdHeaderTruncated { .. }
            | Self::StsdEntryCountMismatch { .. }
            | Self::StsdTrailingBytes { .. }
            | Self::SampleDescriptionMismatch { .. } => None,
        }
    }
}

/// A validated `ac-4` entry and its one-based position in `stsd`.
#[derive(Debug, Clone)]
pub struct Ac4SampleEntry<'a> {
    /// One-based sample-description index used by `stsc`.
    pub index: u32,
    /// The selected `ac-4` sample-entry box.
    pub entry: Mp4Box<'a>,
}

impl<'a> Ac4SampleEntry<'a> {
    /// Locate the `dac4` child box in this `ac-4` sample entry.
    #[must_use]
    pub fn dac4(&self) -> Option<Mp4Box<'a>> {
        self.entry
            .payload
            .get(AUDIO_SAMPLE_ENTRY_LEN..)
            .and_then(|tail| find_box(tail, b"dac4"))
    }

    /// Confirm that a sample references this `stsd` entry.
    ///
    /// # Errors
    ///
    /// Returns [`TrackError::SampleDescriptionMismatch`] when `stsc` selects a
    /// different entry. Mixed-description tracks must be handled explicitly by
    /// callers instead of interpreting every sample as AC-4.
    pub fn validate_sample(&self, sample: &SampleInfo) -> Result<(), TrackError> {
        if sample.sample_description_index == self.index {
            Ok(())
        } else {
            Err(TrackError::SampleDescriptionMismatch {
                sample: sample.index,
                selected: self.index,
                referenced: sample.sample_description_index,
            })
        }
    }
}

/// The container boxes belonging to the first discovered AC-4 audio track.
#[derive(Debug, Clone)]
pub struct Ac4Track<'a> {
    /// Zero-based `trak` index within `moov`.
    pub index: u32,
    /// The selected `trak` box.
    pub trak: Mp4Box<'a>,
    /// The selected track's `mdia` box.
    pub mdia: Mp4Box<'a>,
    /// The selected track's `stbl` box.
    pub stbl: Mp4Box<'a>,
    /// The selected `ac-4` sample entry.
    pub sample_entry: Ac4SampleEntry<'a>,
}

impl<'a> Ac4Track<'a> {
    /// Locate the `dac4` child box in this track's `ac-4` sample entry.
    #[must_use]
    pub fn dac4(&self) -> Option<Mp4Box<'a>> {
        self.sample_entry.dac4()
    }
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}

/// Locate the first `ac-4` entry in an `stsd` payload.
///
/// Every entry declared by `entry_count` is validated even after the first
/// `ac-4` entry is found, and undeclared trailing bytes are rejected.
///
/// # Errors
///
/// Returns [`TrackError`] when the FullBox header, declared entry boxes, count,
/// or trailing boundary is invalid.
pub fn find_ac4_sample_entry(
    stsd_payload: &[u8],
) -> Result<Option<Ac4SampleEntry<'_>>, TrackError> {
    let entry_count = read_u32(stsd_payload, 4).ok_or(TrackError::StsdHeaderTruncated {
        available: stsd_payload.len(),
    })?;
    let entries = stsd_payload
        .get(8..)
        .ok_or(TrackError::StsdHeaderTruncated {
            available: stsd_payload.len(),
        })?;
    let mut iterator = BoxIter::new(entries);
    let mut parsed = 0u32;
    let mut consumed = 0usize;
    let mut found = None;

    while parsed < entry_count {
        let index = parsed.saturating_add(1);
        let item = match iterator.next() {
            Some(Ok(item)) => item,
            Some(Err(source)) => {
                return Err(TrackError::StsdEntryInvalid { index, source });
            }
            None => {
                return Err(TrackError::StsdEntryCountMismatch {
                    declared: entry_count,
                    parsed,
                });
            }
        };
        consumed = item.offset.saturating_add(item.total_len);
        parsed = index;
        if found.is_none() && item.is(b"ac-4") {
            found = Some(Ac4SampleEntry { index, entry: item });
        }
    }

    let trailing = entries.len().saturating_sub(consumed);
    if trailing != 0 {
        return Err(TrackError::StsdTrailingBytes {
            declared: entry_count,
            trailing,
        });
    }
    Ok(found)
}

/// Locate the first track whose sample description contains an `ac-4` entry.
///
/// # Errors
///
/// Returns [`TrackError`] when a visited `stsd` table is malformed.
pub fn find_ac4_track(moov_payload: &[u8]) -> Result<Option<Ac4Track<'_>>, TrackError> {
    let mut track_index = 0u32;
    for item in BoxIter::new(moov_payload).flatten() {
        if !item.is(b"trak") {
            continue;
        }
        let index = track_index;
        track_index = track_index.saturating_add(1);

        let Some(mdia) = find_box(item.payload, b"mdia") else {
            continue;
        };
        let Some(stbl) = find_path(mdia.payload, &[*b"minf", *b"stbl"]) else {
            continue;
        };
        let Some(stsd) = find_box(stbl.payload, b"stsd") else {
            continue;
        };
        let Some(sample_entry) = find_ac4_sample_entry(stsd.payload)? else {
            continue;
        };
        return Ok(Some(Ac4Track {
            index,
            trak: item,
            mdia,
            stbl,
            sample_entry,
        }));
    }
    Ok(None)
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "tests construct fixed, minimal stsd payloads"
)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = u32::try_from(payload.len().checked_add(8).unwrap()).unwrap();
        let mut bytes = Vec::from(size.to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn locates_first_ac4_track_and_dac4() {
        let dac4 = boxed(b"dac4", &[1, 2, 3]);
        let mut sample_payload = vec![0; AUDIO_SAMPLE_ENTRY_LEN];
        sample_payload.extend_from_slice(&dac4);
        let sample_entry = boxed(b"ac-4", &sample_payload);
        let mut stsd_payload = vec![0; 8];
        stsd_payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd_payload.extend_from_slice(&sample_entry);
        let stsd = boxed(b"stsd", &stsd_payload);
        let stbl = boxed(b"stbl", &stsd);
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &minf);
        let trak = boxed(b"trak", &mdia);

        let track = find_ac4_track(&trak)
            .expect("stsd should be valid")
            .expect("AC-4 track should be found");
        assert_eq!(track.index, 0);
        assert_eq!(track.sample_entry.index, 1);
        assert_eq!(
            track.dac4().expect("dac4 should be found").payload,
            [1, 2, 3]
        );
    }

    #[test]
    fn skips_non_ac4_tracks_and_counts_track_indices() {
        let empty_stsd = boxed(b"stsd", &[0; 8]);
        let first_stbl = boxed(b"stbl", &empty_stsd);
        let first_minf = boxed(b"minf", &first_stbl);
        let first_mdia = boxed(b"mdia", &first_minf);
        let first_trak = boxed(b"trak", &first_mdia);

        let ac4_entry = boxed(b"ac-4", &[0; AUDIO_SAMPLE_ENTRY_LEN]);
        let mut second_stsd_payload = vec![0; 8];
        second_stsd_payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        second_stsd_payload.extend_from_slice(&ac4_entry);
        let second_stsd = boxed(b"stsd", &second_stsd_payload);
        let second_stbl = boxed(b"stbl", &second_stsd);
        let second_minf = boxed(b"minf", &second_stbl);
        let second_mdia = boxed(b"mdia", &second_minf);
        let second_trak = boxed(b"trak", &second_mdia);

        let mut moov_payload = first_trak;
        moov_payload.extend_from_slice(&second_trak);
        assert_eq!(
            find_ac4_track(&moov_payload)
                .unwrap()
                .expect("second track should be selected")
                .index,
            1
        );
    }

    #[test]
    fn reports_one_based_sample_entry_index() {
        let mp4a_entry = boxed(b"mp4a", &[0; AUDIO_SAMPLE_ENTRY_LEN]);
        let ac4_entry = boxed(b"ac-4", &[0; AUDIO_SAMPLE_ENTRY_LEN]);
        let mut payload = vec![0; 8];
        payload[4..8].copy_from_slice(&2u32.to_be_bytes());
        payload.extend_from_slice(&mp4a_entry);
        payload.extend_from_slice(&ac4_entry);

        let entry = find_ac4_sample_entry(&payload)
            .expect("stsd should be valid")
            .expect("AC-4 entry should be found");
        assert_eq!(entry.index, 2);
        assert!(entry.entry.is(b"ac-4"));
    }

    #[test]
    fn rejects_entries_beyond_declared_count() {
        let ac4_entry = boxed(b"ac-4", &[0; AUDIO_SAMPLE_ENTRY_LEN]);
        let mut payload = vec![0; 8];
        payload.extend_from_slice(&ac4_entry);

        assert_eq!(
            find_ac4_sample_entry(&payload).unwrap_err(),
            TrackError::StsdTrailingBytes {
                declared: 0,
                trailing: ac4_entry.len(),
            }
        );
    }

    #[test]
    fn rejects_missing_declared_entry() {
        let mut payload = vec![0; 8];
        payload[4..8].copy_from_slice(&1u32.to_be_bytes());

        assert_eq!(
            find_ac4_sample_entry(&payload).unwrap_err(),
            TrackError::StsdEntryCountMismatch {
                declared: 1,
                parsed: 0,
            }
        );
    }

    #[test]
    fn rejects_truncated_stsd_header() {
        assert_eq!(
            find_ac4_sample_entry(&[0; 7]).unwrap_err(),
            TrackError::StsdHeaderTruncated { available: 7 }
        );
    }

    #[test]
    fn rejects_invalid_declared_entry_box() {
        let mut payload = vec![0; 8];
        payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&64u32.to_be_bytes());
        payload.extend_from_slice(b"ac-4");

        assert!(matches!(
            find_ac4_sample_entry(&payload),
            Err(TrackError::StsdEntryInvalid { index: 1, .. })
        ));
    }
}
