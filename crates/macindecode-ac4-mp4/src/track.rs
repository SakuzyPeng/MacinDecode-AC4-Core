//! AC-4 audio track discovery in ISO BMFF containers.

use crate::{BoxIter, Mp4Box, find_box, find_path};

/// Byte length of the fixed `AudioSampleEntry` fields before child boxes.
///
/// ISO/IEC 14496-12 and `TS103190-1:v1.4.1` Table E.2 define this as
/// `reserved[6] + data_reference_index(2) + reserved[2](8) + channel_count(2)
/// + sample_size(2) + reserved(4) + sampling_frequency(2) + reserved(2)`.
pub const AUDIO_SAMPLE_ENTRY_LEN: usize = 28;

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
    pub sample_entry: Mp4Box<'a>,
}

impl<'a> Ac4Track<'a> {
    /// Locate the `dac4` child box in this track's `ac-4` sample entry.
    #[must_use]
    pub fn dac4(&self) -> Option<Mp4Box<'a>> {
        self.sample_entry
            .payload
            .get(AUDIO_SAMPLE_ENTRY_LEN..)
            .and_then(|tail| find_box(tail, b"dac4"))
    }
}

/// Locate the first `ac-4` entry in an `stsd` payload.
#[must_use]
pub fn find_ac4_sample_entry(stsd_payload: &[u8]) -> Option<Mp4Box<'_>> {
    let entries = stsd_payload.get(8..)?;
    BoxIter::new(entries)
        .flatten()
        .find(|item| item.is(b"ac-4"))
}

/// Locate the first track whose sample description contains an `ac-4` entry.
#[must_use]
pub fn find_ac4_track(moov_payload: &[u8]) -> Option<Ac4Track<'_>> {
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
        let Some(sample_entry) = find_ac4_sample_entry(stsd.payload) else {
            continue;
        };
        return Some(Ac4Track {
            index,
            trak: item,
            mdia,
            stbl,
            sample_entry,
        });
    }
    None
}

#[cfg(test)]
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
        stsd_payload.extend_from_slice(&sample_entry);
        let stsd = boxed(b"stsd", &stsd_payload);
        let stbl = boxed(b"stbl", &stsd);
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &minf);
        let trak = boxed(b"trak", &mdia);

        let track = find_ac4_track(&trak).expect("AC-4 track should be found");
        assert_eq!(track.index, 0);
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
        second_stsd_payload.extend_from_slice(&ac4_entry);
        let second_stsd = boxed(b"stsd", &second_stsd_payload);
        let second_stbl = boxed(b"stbl", &second_stsd);
        let second_minf = boxed(b"minf", &second_stbl);
        let second_mdia = boxed(b"mdia", &second_minf);
        let second_trak = boxed(b"trak", &second_mdia);

        let mut moov_payload = first_trak;
        moov_payload.extend_from_slice(&second_trak);
        assert_eq!(find_ac4_track(&moov_payload).unwrap().index, 1);
    }
}
