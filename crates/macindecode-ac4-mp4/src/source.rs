//! Shared, bounded AC-4 track and access-unit view for complete MP4 inputs.

use core::{fmt, ops::Range};

use crate::{
    Ac4Dsi, Ac4Track, DsiError, EditListEntry, HeaderTiming, PresentationSampleSpan,
    PresentationTiming, SampleBoundsError, SampleInfo, SampleIter, SampleTable, SampleTableError,
    TimelineError, TrackError, find_ac4_track, find_box, find_path, media_time_to_presentation,
    parse_edit_list, parse_header_timing, presentation_media_span, presentation_sample_shift,
    presentation_timing, rescale_i64_round, rescale_u64_round,
};

/// Failure while preparing or iterating the shared AC-4 MP4 view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ac4Mp4Error {
    /// A required ISO BMFF box is absent.
    MissingBox {
        /// Missing four-character box type.
        box_type: [u8; 4],
    },
    /// No track contains an `ac-4` sample entry.
    NoAc4Track,
    /// The selected track or a sample-description reference is invalid.
    Track(TrackError),
    /// The selected track's `dac4` payload is invalid.
    Dsi(DsiError),
    /// The selected track's sample table is invalid.
    SampleTable(SampleTableError),
    /// A declared sample byte range is not bounded by the complete input.
    SampleBounds(SampleBoundsError),
    /// Movie/media timing or the edit list is invalid.
    Timeline(TimelineError),
}

impl fmt::Display for Ac4Mp4Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingBox { box_type } if box_type == *b"dac4" => {
                formatter.write_str("ac-4 sample entry has no dac4 box")
            }
            Self::MissingBox { box_type } => {
                for byte in box_type {
                    let character = if byte.is_ascii_graphic() {
                        char::from(byte)
                    } else {
                        '.'
                    };
                    write!(formatter, "{character}")?;
                }
                formatter.write_str(" box not found")
            }
            Self::NoAc4Track => formatter.write_str("No track with an ac-4 sample entry was found"),
            Self::Track(error) => error.fmt(formatter),
            Self::Dsi(error) => error.fmt(formatter),
            Self::SampleTable(error) => error.fmt(formatter),
            Self::SampleBounds(error) => error.fmt(formatter),
            Self::Timeline(error) => error.fmt(formatter),
        }
    }
}

impl core::error::Error for Ac4Mp4Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Track(error) => Some(error),
            Self::Dsi(error) => Some(error),
            Self::SampleTable(error) => Some(error),
            Self::SampleBounds(error) => Some(error),
            Self::Timeline(error) => Some(error),
            Self::MissingBox { .. } | Self::NoAc4Track => None,
        }
    }
}

impl From<TrackError> for Ac4Mp4Error {
    fn from(value: TrackError) -> Self {
        Self::Track(value)
    }
}

impl From<DsiError> for Ac4Mp4Error {
    fn from(value: DsiError) -> Self {
        Self::Dsi(value)
    }
}

impl From<SampleTableError> for Ac4Mp4Error {
    fn from(value: SampleTableError) -> Self {
        Self::SampleTable(value)
    }
}

impl From<SampleBoundsError> for Ac4Mp4Error {
    fn from(value: SampleBoundsError) -> Self {
        Self::SampleBounds(value)
    }
}

impl From<TimelineError> for Ac4Mp4Error {
    fn from(value: TimelineError) -> Self {
        Self::Timeline(value)
    }
}

/// A selected AC-4 track, DSI, sample table, and complete bounded MP4 input.
///
/// Construction deliberately does not require `mvhd` or an edit list. Read-only
/// metadata consumers can inspect a valid media track without opting into the
/// presentation timeline requirements used by decoders and exporters.
#[derive(Debug, Clone)]
pub struct Ac4Mp4<'a> {
    input: &'a [u8],
    moov: crate::Mp4Box<'a>,
    track: Ac4Track<'a>,
    media: HeaderTiming,
    dac4: crate::Mp4Box<'a>,
    dsi: Ac4Dsi<'a>,
    samples: SampleTable<'a>,
}

impl<'a> Ac4Mp4<'a> {
    /// Parse the first AC-4 track and all state required to iterate its samples.
    ///
    /// # Errors
    ///
    /// Returns [`Ac4Mp4Error`] when the required track boxes, `dac4`, or sample
    /// table are missing or invalid.
    pub fn parse(input: &'a [u8]) -> Result<Self, Ac4Mp4Error> {
        let moov =
            find_box(input, b"moov").ok_or(Ac4Mp4Error::MissingBox { box_type: *b"moov" })?;
        let track = find_ac4_track(moov.payload)?.ok_or(Ac4Mp4Error::NoAc4Track)?;
        let mdhd = find_box(track.mdia.payload, b"mdhd")
            .ok_or(Ac4Mp4Error::MissingBox { box_type: *b"mdhd" })?;
        let media = parse_header_timing(*b"mdhd", mdhd.payload)?;
        let dac4 = track
            .dac4()
            .ok_or(Ac4Mp4Error::MissingBox { box_type: *b"dac4" })?;
        let dsi = Ac4Dsi::parse(dac4.payload)?;
        let samples = SampleTable::parse(track.stbl.payload)?;
        Ok(Self {
            input,
            moov,
            track,
            media,
            dac4,
            dsi,
            samples,
        })
    }

    /// Selected track and its validated `ac-4` sample entry.
    #[must_use]
    pub const fn track(&self) -> &Ac4Track<'a> {
        &self.track
    }

    /// Parsed media-header timing for the selected track.
    #[must_use]
    pub const fn media_timing(&self) -> HeaderTiming {
        self.media
    }

    /// Selected sample entry's bounded `dac4` box.
    #[must_use]
    pub const fn dac4(&self) -> &crate::Mp4Box<'a> {
        &self.dac4
    }

    /// Parsed `dac4` DSI for the selected track.
    #[must_use]
    pub const fn dsi(&self) -> &Ac4Dsi<'a> {
        &self.dsi
    }

    /// Number of samples declared by `stsz`.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.samples.sample_count()
    }

    /// Iterate sample descriptors while enforcing the selected `stsd` entry.
    #[must_use]
    pub fn sample_infos(&self) -> Ac4SampleInfoIter<'a> {
        Ac4SampleInfoIter {
            inner: self.samples.iter(),
            selected_sample_description: self.track.sample_entry.index,
            failed: false,
        }
    }

    /// Iterate fully bounded AC-4 access units.
    #[must_use]
    pub fn access_units(&self) -> Ac4AccessUnitIter<'a> {
        Ac4AccessUnitIter {
            input: self.input,
            samples: self.sample_infos(),
        }
    }

    /// Validate one sample descriptor and bind it to the complete input buffer.
    ///
    /// # Errors
    ///
    /// Returns [`Ac4Mp4Error::Track`] for a mixed sample-description reference or
    /// [`Ac4Mp4Error::SampleBounds`] for a range outside the input.
    pub fn access_unit(&self, info: SampleInfo) -> Result<Ac4AccessUnit<'a>, Ac4Mp4Error> {
        self.track.sample_entry.validate_sample(&info)?;
        bound_access_unit(self.input, info).map_err(Into::into)
    }

    /// Parse movie timing and edit-list state with caller-selected fixed capacity.
    ///
    /// # Errors
    ///
    /// Returns [`Ac4Mp4Error`] for a missing/invalid `mvhd`, edit list, or derived
    /// presentation timeline.
    pub fn presentation_timeline<const EDITS: usize>(
        &self,
    ) -> Result<Ac4Mp4Timeline<EDITS>, Ac4Mp4Error> {
        let mvhd = find_box(self.moov.payload, b"mvhd")
            .ok_or(Ac4Mp4Error::MissingBox { box_type: *b"mvhd" })?;
        let movie = parse_header_timing(*b"mvhd", mvhd.payload)?;
        let mut edit_storage = [EditListEntry {
            segment_duration: 0,
            media_time: 0,
            media_rate: (0, 0),
        }; EDITS];
        let edit_count = find_path(self.track.trak.payload, &[*b"edts", *b"elst"])
            .map(|box_| parse_edit_list(box_.payload, &mut edit_storage))
            .transpose()?
            .unwrap_or(0);
        let edits = edit_storage
            .get(..edit_count)
            .ok_or(TimelineError::TimeOverflow)?;
        let presentation = presentation_timing(self.media, movie.timescale, edits)?;
        Ok(Ac4Mp4Timeline {
            movie,
            media: self.media,
            edits: edit_storage,
            edit_count,
            presentation,
        })
    }
}

/// One validated and file-bounded AC-4 access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ac4AccessUnit<'a> {
    /// Sample-table descriptor and integer media timing.
    pub info: SampleInfo,
    /// Checked range within the complete MP4 byte buffer.
    pub range: Range<usize>,
    /// Bytes selected by [`Self::range`].
    pub payload: &'a [u8],
}

fn bound_access_unit(
    input: &[u8],
    info: SampleInfo,
) -> Result<Ac4AccessUnit<'_>, SampleBoundsError> {
    let range = info.checked_range(input.len())?;
    let payload = input
        .get(range.clone())
        .ok_or(SampleBoundsError::RangeExceedsInput {
            start: range.start,
            end: range.end,
            input_len: input.len(),
        })?;
    Ok(Ac4AccessUnit {
        info,
        range,
        payload,
    })
}

/// Iterator over sample-table descriptors validated against the selected `ac-4` entry.
#[derive(Debug, Clone)]
pub struct Ac4SampleInfoIter<'a> {
    inner: SampleIter<'a>,
    selected_sample_description: u32,
    failed: bool,
}

impl Iterator for Ac4SampleInfoIter<'_> {
    type Item = Result<SampleInfo, Ac4Mp4Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let info = match self.inner.next()? {
            Ok(info) => info,
            Err(error) => {
                self.failed = true;
                return Some(Err(error.into()));
            }
        };
        if info.sample_description_index != self.selected_sample_description {
            self.failed = true;
            return Some(Err(TrackError::SampleDescriptionMismatch {
                sample: info.index,
                selected: self.selected_sample_description,
                referenced: info.sample_description_index,
            }
            .into()));
        }
        Some(Ok(info))
    }
}

/// Iterator over validated and file-bounded AC-4 access units.
#[derive(Debug, Clone)]
pub struct Ac4AccessUnitIter<'a> {
    input: &'a [u8],
    samples: Ac4SampleInfoIter<'a>,
}

impl<'a> Iterator for Ac4AccessUnitIter<'a> {
    type Item = Result<Ac4AccessUnit<'a>, Ac4Mp4Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let info = match self.samples.next()? {
            Ok(info) => info,
            Err(error) => return Some(Err(error)),
        };
        Some(bound_access_unit(self.input, info).map_err(Into::into))
    }
}

/// Movie/media/edit state derived on demand from an [`Ac4Mp4`] source.
#[derive(Debug, Clone)]
pub struct Ac4Mp4Timeline<const EDITS: usize> {
    movie: HeaderTiming,
    media: HeaderTiming,
    edits: [EditListEntry; EDITS],
    edit_count: usize,
    presentation: PresentationTiming,
}

impl<const EDITS: usize> Ac4Mp4Timeline<EDITS> {
    /// Parsed movie-header timing.
    #[must_use]
    pub const fn movie_timing(&self) -> HeaderTiming {
        self.movie
    }

    /// Parsed media-header timing.
    #[must_use]
    pub const fn media_timing(&self) -> HeaderTiming {
        self.media
    }

    /// Validated edit-list entries in file order.
    #[must_use]
    pub fn edits(&self) -> &[EditListEntry] {
        self.edits.get(..self.edit_count).unwrap_or(&[])
    }

    /// Derived media duration, presentation duration, and priming.
    #[must_use]
    pub const fn presentation_timing(&self) -> PresentationTiming {
        self.presentation
    }

    /// Number of edit entries that reference media rather than inserting silence.
    #[must_use]
    pub fn media_edit_count(&self) -> usize {
        self.edits()
            .iter()
            .filter(|entry| !entry.is_empty_edit())
            .count()
    }

    /// Convert a media PTS to the edited presentation timeline in media ticks.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for invalid edit state or arithmetic overflow.
    pub fn presentation_time(&self, media_time: i64) -> Result<Option<i64>, TimelineError> {
        media_time_to_presentation(
            media_time,
            self.media.timescale,
            self.movie.timescale,
            self.edits(),
        )
    }

    /// Convert a media timestamp to nearest integer samples without applying edits.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for zero scales or arithmetic overflow.
    pub fn media_time_samples(
        &self,
        media_time: i64,
        sample_rate: u32,
    ) -> Result<i64, TimelineError> {
        rescale_i64_round(media_time, self.media.timescale, sample_rate)
    }

    /// Convert the affine presentation shift to integer samples.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for invalid edit state or arithmetic overflow.
    pub fn presentation_sample_shift(
        &self,
        sample_rate: u32,
    ) -> Result<Option<i64>, TimelineError> {
        presentation_sample_shift(
            sample_rate,
            self.media.timescale,
            self.movie.timescale,
            self.edits(),
        )
    }

    /// Presentation duration rounded to the nearest output sample.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for zero scales or arithmetic overflow.
    pub fn presentation_duration_samples(&self, sample_rate: u32) -> Result<u64, TimelineError> {
        rescale_u64_round(
            self.presentation.presented_duration,
            self.media.timescale,
            sample_rate,
        )
    }

    /// Container-declared priming rounded to the nearest output sample.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for zero scales or arithmetic overflow.
    pub fn priming_samples(&self, sample_rate: u32) -> Result<u64, TimelineError> {
        rescale_u64_round(self.presentation.priming, self.media.timescale, sample_rate)
    }

    /// Media-backed interval on the edited presentation sample timeline.
    ///
    /// No edit list represents the complete presentation; an all-empty edit list
    /// returns `None`.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError`] for invalid edit state or arithmetic overflow.
    pub fn media_span_samples(
        &self,
        sample_rate: u32,
    ) -> Result<Option<PresentationSampleSpan>, TimelineError> {
        if self.edits().is_empty() {
            return Ok(Some(PresentationSampleSpan {
                start_sample: 0,
                end_sample: self.presentation_duration_samples(sample_rate)?,
            }));
        }
        presentation_media_span(
            sample_rate,
            self.media.timescale,
            self.movie.timescale,
            self.edits(),
        )
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "tests construct fixed, minimal ISO BMFF layouts"
)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

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

    fn header_timing(duration: u32) -> Vec<u8> {
        let mut payload = vec![0u8; 20];
        payload[12..16].copy_from_slice(&48_000u32.to_be_bytes());
        payload[16..20].copy_from_slice(&duration.to_be_bytes());
        payload
    }

    fn track(chunk_offset: u32, size: u32, sample_description_index: u32) -> Vec<u8> {
        let mut entry_payload = vec![0u8; 28];
        entry_payload.extend_from_slice(&mp4_box(b"dac4", &[0x00, 0xBA, 0x01]));
        let entry = mp4_box(b"ac-4", &entry_payload);
        let mut stsd_payload = vec![0u8; 8];
        stsd_payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd_payload.extend_from_slice(&entry);

        let mut stbl_payload = mp4_box(b"stsd", &stsd_payload);
        stbl_payload.extend_from_slice(&full_box_table(b"stts", &[1, 1, 2_048]));
        stbl_payload.extend_from_slice(&full_box_table(
            b"stsc",
            &[1, 1, 1, sample_description_index],
        ));
        stbl_payload.extend_from_slice(&full_box_table(b"stsz", &[0, 1, size]));
        stbl_payload.extend_from_slice(&full_box_table(b"stco", &[1, chunk_offset]));
        let stbl = mp4_box(b"stbl", &stbl_payload);
        let minf = mp4_box(b"minf", &stbl);
        let mut mdia_payload = mp4_box(b"mdhd", &header_timing(2_048));
        mdia_payload.extend_from_slice(&minf);
        mp4_box(b"trak", &mp4_box(b"mdia", &mdia_payload))
    }

    fn moov(
        chunk_offset: u32,
        size: u32,
        sample_description_index: u32,
        movie_header: bool,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        if movie_header {
            payload.extend_from_slice(&mp4_box(b"mvhd", &header_timing(2_048)));
        }
        payload.extend_from_slice(&track(chunk_offset, size, sample_description_index));
        mp4_box(b"moov", &payload)
    }

    fn minimal_mp4(payload: &[u8], sample_description_index: u32, movie_header: bool) -> Vec<u8> {
        let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0isom");
        let size = u32::try_from(payload.len()).unwrap();
        let placeholder = moov(0, size, sample_description_index, movie_header);
        let chunk_offset = u32::try_from(ftyp.len() + placeholder.len() + 8).unwrap();
        let moov = moov(chunk_offset, size, sample_description_index, movie_header);
        [ftyp, moov, mp4_box(b"mdat", payload)].concat()
    }

    #[test]
    fn shared_source_yields_one_bounded_access_unit_and_timeline() {
        let data = minimal_mp4(&[1, 2, 3, 4], 1, true);
        let source = Ac4Mp4::parse(&data).expect("minimal AC-4 track should parse");
        assert_eq!(source.sample_count(), 1);
        assert_eq!(source.track().index, 0);
        assert_eq!(source.dsi().base_sampling_frequency.hz(), 48_000);

        let mut access_units = source.access_units();
        let access_unit = access_units
            .next()
            .expect("one access unit")
            .expect("sample range should be bounded");
        assert_eq!(access_unit.payload, [1, 2, 3, 4]);
        assert_eq!(&data[access_unit.range], [1, 2, 3, 4]);
        assert!(access_units.next().is_none());

        let timeline = source
            .presentation_timeline::<8>()
            .expect("mvhd should provide a presentation timeline");
        assert_eq!(timeline.presentation_duration_samples(48_000), Ok(2_048));
        assert_eq!(timeline.priming_samples(48_000), Ok(0));
        assert_eq!(
            timeline.media_span_samples(48_000),
            Ok(Some(PresentationSampleSpan {
                start_sample: 0,
                end_sample: 2_048,
            }))
        );
    }

    #[test]
    fn metadata_source_does_not_require_movie_timing_until_requested() {
        let data = minimal_mp4(&[1, 2], 1, false);
        let source = Ac4Mp4::parse(&data).expect("media track alone should remain inspectable");
        assert_eq!(source.access_units().count(), 1);
        assert_eq!(
            source.presentation_timeline::<8>().unwrap_err(),
            Ac4Mp4Error::MissingBox { box_type: *b"mvhd" }
        );
    }

    #[test]
    fn source_rejects_mixed_descriptions_and_truncated_file_ranges() {
        let mixed = minimal_mp4(&[1, 2], 2, true);
        assert!(matches!(
            Ac4Mp4::parse(&mixed)
                .unwrap()
                .sample_infos()
                .next()
                .expect("one descriptor"),
            Err(Ac4Mp4Error::Track(TrackError::SampleDescriptionMismatch {
                sample: 0,
                ..
            }))
        ));

        let mut truncated = minimal_mp4(&[1, 2], 1, true);
        truncated.pop();
        assert!(matches!(
            Ac4Mp4::parse(&truncated)
                .unwrap()
                .access_units()
                .next()
                .expect("one descriptor"),
            Err(Ac4Mp4Error::SampleBounds(
                SampleBoundsError::RangeExceedsInput { .. }
            ))
        ));
    }
}
