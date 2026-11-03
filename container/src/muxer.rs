//! Multi-track MP4 and MOV muxer.
//!
//! Sample payloads stream into one `mdat` as they are written; the sample
//! tables, edit lists and codec configuration are assembled into a trailing
//! `moov` by [`Muxer::finish`]. The box layer is `transmux`'s typed ISO BMFF
//! model (the same one the demuxer parses with), so sample entries and
//! configuration boxes are produced by a maintained writer; this module owns
//! the sample tables and the file layout.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use broadcast_common::{Parse, Serialize};
use mediax_core::{Error, VideoColorInfo};
use transmux::OpaqueBox as EntryBox;
use transmux::box_types::box_iter;
use transmux::mp4esds::{
    DecoderConfigDescriptor, DecoderSpecificInfo, ESDescriptor, EsdsBox, SLConfigDescriptor,
};
use transmux::sample_entries::{OpaqueBox, SampleEntryChild};
use transmux::{
    AVCConfigurationBox, Av1ConfigurationBox, ChunkLargeOffsetBox, ChunkOffsetBox, CodecConfig,
    CompositionOffsetBox, CttsEntry, EditBox, EditListBox, EditListEntry, FileTypeBox,
    HEVCConfigurationBox, MovieBox, SampleEntryVariant, SampleSizeBox, SampleToChunkBox, StblChild,
    StscEntry, SttsEntry, SyncSampleBox, TimeToSampleBox, TrackSpec, build_init_segment,
};

use crate::stream::{Codec, EncodedSample, TrackId, TrackInfo, TrackKind};

/// Ticks per second of the movie header; track timelines keep their own
/// timescales.
const MOVIE_TIMESCALE: u32 = 1_000;

/// `mdat` header: `size == 1` marks a 64-bit `largesize`, patched by
/// [`Muxer::finish`] once the payload length is known.
const MDAT_HEADER_LEN: u64 = 16;

/// Offset of the `largesize` field inside the `mdat` header.
const MDAT_LARGESIZE_OFFSET: u64 = 8;

/// `elst` `media_time` of an empty edit.
const EMPTY_EDIT: i64 = -1;

/// ISO/IEC 14496-1 object type indication of MPEG-4 audio.
const OBJECT_TYPE_MPEG4_AUDIO: u8 = 0x40;

/// ISO/IEC 14496-1 stream type of audio.
const STREAM_TYPE_AUDIO: u8 = 0x05;

/// Container flavour a [`Muxer`] writes.
///
/// The format selects the `ftyp` brand: ISO base media (`isom`) for MP4,
/// `QuickTime` (`qt  `) for MOV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerFormat {
    /// MP4 (ISO/IEC 14496-14).
    #[default]
    Mp4,
    /// MOV (Apple `QuickTime`).
    Mov,
}

impl ContainerFormat {
    fn file_type(self) -> FileTypeBox {
        match self {
            Self::Mp4 => FileTypeBox {
                major_brand: *b"isom",
                minor_version: 512,
                compatible_brands: vec![*b"isom", *b"iso2", *b"mp41"],
            },
            Self::Mov => FileTypeBox {
                major_brand: *b"qt  ",
                minor_version: 20_050_300,
                compatible_brands: vec![*b"qt  "],
            },
        }
    }
}

/// Writer of MP4 and MOV files with any number of H.264, H.265 or AV1 video
/// tracks and AAC audio tracks.
///
/// Tracks are declared with [`add_track`](Self::add_track) from a
/// [`TrackInfo`]; samples are [`EncodedSample`]s in the form the container
/// stores them: length-prefixed NAL units for H.264 and H.265, temporal units
/// without their temporal delimiter for AV1, raw access units for AAC. Samples
/// of one track are written in decode order and the call order across tracks
/// is the interleave order in the file.
///
/// A track's timeline starts at its first sample's decode time. The container
/// stores that start through an edit list, so a track that begins later than
/// another keeps its offset, but a reader that ignores edit lists, like this
/// crate's [`ProgressiveTrackReader`](crate::ProgressiveTrackReader), reports
/// decode times relative to it. A sample's duration is the gap to the next
/// sample's decode time; the declared duration only applies to a track's last
/// sample.
///
/// In a MOV, AAC tracks carry the `QuickTime` variable-bitrate marker and a
/// `chan` layout, without which `AVFoundation` refuses to open the file.
///
/// The writer buffers no payload, only a few words of table data per sample.
#[derive(Debug)]
pub struct Muxer<W> {
    out: W,
    format: ContainerFormat,
    /// File offset of the `mdat` box.
    mdat_start: u64,
    /// File offset the next sample is written at.
    position: u64,
    tracks: BTreeMap<TrackId, MuxTrack>,
    /// The track the open chunk belongs to; a sample of another track opens a
    /// new chunk.
    open_chunk: Option<TrackId>,
}

impl Muxer<BufWriter<File>> {
    /// Creates `path` and starts a file of `format` in it.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be created or written.
    pub fn create(path: impl AsRef<Path>, format: ContainerFormat) -> Result<Self, Error> {
        Self::new(BufWriter::new(File::create(path)?), format)
    }
}

impl<W: Write + Seek> Muxer<W> {
    /// Starts a file of `format` at the writer's current position.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file header cannot be written.
    pub fn new(mut out: W, format: ContainerFormat) -> Result<Self, Error> {
        let ftyp = format.file_type();
        let mut header = vec![0; ftyp.serialized_len()];
        ftyp.serialize_into(&mut header).map_err(container_error)?;
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.extend_from_slice(b"mdat");
        header.extend_from_slice(&0_u64.to_be_bytes());

        let start = out.stream_position()?;
        out.write_all(&header)?;
        let mdat_start = start + header.len() as u64 - MDAT_HEADER_LEN;
        Ok(Self {
            out,
            format,
            mdat_start,
            position: mdat_start + MDAT_HEADER_LEN,
            tracks: BTreeMap::new(),
            open_chunk: None,
        })
    }

    /// Returns the container format being written.
    #[must_use]
    pub const fn format(&self) -> ContainerFormat {
        self.format
    }

    /// Declares a track; its [`TrackInfo::id`] addresses its samples.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is already declared, the codec is not
    /// H.264, H.265, AV1 or AAC, the configuration cannot be parsed, or the
    /// track needs signaling the muxer cannot write (Dolby Vision, Common
    /// Encryption).
    pub fn add_track(&mut self, info: TrackInfo) -> Result<(), Error> {
        let id = info.id();
        if self.tracks.contains_key(&id) {
            return Err(Error::Container(format!(
                "track {} is already declared",
                id.get()
            )));
        }
        if info.protection().is_some() {
            return Err(Error::Unsupported(String::from(
                "the muxer does not write Common Encryption tracks",
            )));
        }
        if info.video_color_info().is_some_and(|c| c.dolby_vision) {
            return Err(Error::Unsupported(String::from(
                "the muxer cannot signal Dolby Vision",
            )));
        }
        let spec = TrackSpec::new(id.get(), info.timescale().get(), codec_config(&info)?);
        self.tracks.insert(
            id,
            MuxTrack {
                info,
                spec,
                samples: Vec::new(),
                chunks: Vec::new(),
            },
        );
        Ok(())
    }

    /// Appends a sample to its track.
    ///
    /// # Errors
    ///
    /// Returns an error when the sample's track is not declared, its
    /// timestamps are not in the track's timescale, its decode time does not
    /// advance, its composition offset or duration do not fit the container,
    /// it is empty or encrypted, or the write fails.
    pub fn write_sample(&mut self, sample: &EncodedSample) -> Result<(), Error> {
        let id = sample.track_id();
        let track = self
            .tracks
            .get_mut(&id)
            .ok_or_else(|| Error::Container(format!("sample for undeclared track {}", id.get())))?;
        if sample.encryption().is_some() {
            return Err(Error::Unsupported(String::from(
                "the muxer does not write encrypted samples",
            )));
        }
        let size = u32::try_from(sample.data().len())
            .map_err(|_| Error::Container(String::from("sample exceeds the u32 size field")))?;
        if size == 0 {
            return Err(Error::Container(format!(
                "track {} has an empty sample",
                id.get()
            )));
        }
        let record = track.record(sample, size)?;

        if self.open_chunk != Some(id) {
            track.chunks.push(Chunk {
                offset: self.position,
                samples: 0,
            });
            self.open_chunk = Some(id);
        }
        if let Some(chunk) = track.chunks.last_mut() {
            chunk.samples += 1;
        }
        track.samples.push(record);
        self.out.write_all(sample.data())?;
        self.position += u64::from(size);
        Ok(())
    }

    /// Completes the file: finalizes the `mdat` size and writes the `moov`.
    /// Returns the underlying writer.
    ///
    /// # Errors
    ///
    /// Returns an error when no track was declared, a track has no samples, a
    /// value does not fit its container field, or the write fails.
    pub fn finish(mut self) -> Result<W, Error> {
        let moov = self.moov()?;

        self.out
            .seek(SeekFrom::Start(self.mdat_start + MDAT_LARGESIZE_OFFSET))?;
        self.out
            .write_all(&(self.position - self.mdat_start).to_be_bytes())?;
        self.out.seek(SeekFrom::Start(self.position))?;
        self.out.write_all(&moov)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn moov(&self) -> Result<Vec<u8>, Error> {
        if self.tracks.is_empty() {
            return Err(Error::Container(String::from(
                "a file needs at least one track",
            )));
        }
        let spans = self
            .tracks
            .values()
            .map(MuxTrack::span)
            .collect::<Result<Vec<_>, _>>()?;
        let movie_start = spans
            .iter()
            .map(|span| span.movie_start)
            .min()
            .unwrap_or_default();

        let specs: Vec<TrackSpec> = self.tracks.values().map(|t| t.spec.clone()).collect();
        let init = build_init_segment(&specs, MOVIE_TIMESCALE).map_err(container_error)?;
        let mut moov = box_iter(&init)
            .scan(0_usize, |offset, step| {
                let start = *offset;
                Some(step.map(|(boxed, consumed)| {
                    *offset += consumed;
                    (boxed.header.box_type, start..*offset)
                }))
            })
            .find_map(|step| match step {
                Ok((box_type, range)) if box_type.is(b"moov") => Some(range),
                _ => None,
            })
            .ok_or_else(|| Error::Container(String::from("initialization has no moov")))
            .and_then(|range| MovieBox::parse(&init[range]).map_err(container_error))?;
        moov.mvex = None;

        let mut movie_duration = 0;
        for ((trak, track), span) in moov.tracks.iter_mut().zip(self.tracks.values()).zip(&spans) {
            let presentation = span.presentation(movie_start)?;
            movie_duration = movie_duration.max(presentation.duration);
            trak.tkhd.version = header_version(presentation.duration);
            trak.tkhd.duration = presentation.duration;
            trak.edts = presentation
                .edits
                .map(|elst| EditBox::new(Some(elst), Vec::new()));

            let mdia = trak
                .mdia
                .as_mut()
                .ok_or_else(|| Error::Container(String::from("track skeleton has no mdia")))?;
            let mdhd = mdia
                .mdhd
                .as_mut()
                .ok_or_else(|| Error::Container(String::from("track skeleton has no mdhd")))?;
            mdhd.version = header_version(span.media_duration);
            mdhd.duration = span.media_duration;
            let stbl = mdia
                .minf
                .as_mut()
                .and_then(|minf| minf.stbl.as_mut())
                .ok_or_else(|| Error::Container(String::from("track skeleton has no stbl")))?;
            let stsd = stbl
                .children
                .iter()
                .position(|child| matches!(child, StblChild::Stsd(_)))
                .map(|index| stbl.children.swap_remove(index))
                .ok_or_else(|| Error::Container(String::from("track skeleton has no stsd")))?;
            stbl.children = track.sample_table(stsd, span, self.format)?;
        }
        moov.mvhd.version = header_version(movie_duration);
        moov.mvhd.duration = movie_duration;

        let mut bytes = vec![0; moov.serialized_len()];
        let written = moov.serialize_into(&mut bytes).map_err(container_error)?;
        bytes.truncate(written);
        Ok(bytes)
    }
}

/// One declared track and the sample tables collected for it.
#[derive(Debug)]
struct MuxTrack {
    info: TrackInfo,
    spec: TrackSpec,
    samples: Vec<SampleRecord>,
    chunks: Vec<Chunk>,
}

/// The table data of one written sample.
#[derive(Debug, Clone, Copy)]
struct SampleRecord {
    decode_time: i64,
    composition_offset: i32,
    /// Declared duration, in track ticks.
    duration: u32,
    size: u32,
    sync: bool,
}

/// Consecutive samples of one track stored contiguously in `mdat`.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    offset: u64,
    samples: u32,
}

/// A track's extent on its own and on the movie timeline.
#[derive(Debug)]
struct TrackSpan {
    timescale: u32,
    /// Decode time of the first sample.
    first_decode: i64,
    /// Earliest presentation time.
    start: i64,
    /// Latest presentation end.
    end: i64,
    /// `stts` deltas, one per sample.
    deltas: Vec<u32>,
    media_duration: u64,
    /// `start` on the movie timeline.
    movie_start: i64,
}

/// A track's placement on the movie timeline.
#[derive(Debug)]
struct Presentation {
    /// `tkhd` duration in movie ticks.
    duration: u64,
    edits: Option<EditListBox>,
}

impl MuxTrack {
    /// Validates `sample` against the track and the sample before it.
    fn record(&self, sample: &EncodedSample, size: u32) -> Result<SampleRecord, Error> {
        let timescale = self.info.timescale();
        let id = self.info.id().get();
        let decode = sample.decode_time();
        let presentation = sample.presentation_time();
        let duration = sample.duration();
        for time in [decode, presentation, duration] {
            if time.timescale() != timescale {
                return Err(Error::Container(format!(
                    "track {id} has timescale {timescale} but a sample is timed in {}",
                    time.timescale()
                )));
            }
        }
        if let Some(previous) = self.samples.last()
            && decode.ticks() <= previous.decode_time
        {
            return Err(Error::Container(format!(
                "track {id} decode time {} does not advance past {}",
                decode.ticks(),
                previous.decode_time
            )));
        }
        let composition_offset = presentation
            .ticks()
            .checked_sub(decode.ticks())
            .and_then(|offset| i32::try_from(offset).ok())
            .ok_or_else(|| {
                Error::Container(format!(
                    "track {id} composition offset does not fit the ctts field"
                ))
            })?;
        let duration = u32::try_from(duration.ticks()).map_err(|_| {
            Error::Container(format!("track {id} sample duration does not fit u32"))
        })?;
        Ok(SampleRecord {
            decode_time: decode.ticks(),
            composition_offset,
            duration,
            size,
            sync: sample.is_keyframe(),
        })
    }

    fn span(&self) -> Result<TrackSpan, Error> {
        let id = self.info.id().get();
        let timescale = self.info.timescale().get();
        let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
            return Err(Error::Container(format!("track {id} has no samples")));
        };
        let mut deltas = Vec::with_capacity(self.samples.len());
        for pair in self.samples.windows(2) {
            let delta = u32::try_from(pair[1].decode_time - pair[0].decode_time).map_err(|_| {
                Error::Container(format!(
                    "track {id} decode delta does not fit the stts field"
                ))
            })?;
            deltas.push(delta);
        }
        deltas.push(last.duration);

        let mut start = i64::MAX;
        let mut end = i64::MIN;
        for (record, delta) in self.samples.iter().zip(&deltas) {
            let presentation = record.decode_time + i64::from(record.composition_offset);
            start = start.min(presentation);
            end = end.max(presentation + i64::from(*delta));
        }
        if start < first.decode_time {
            return Err(Error::Container(format!(
                "track {id} presents a sample before its first decode time"
            )));
        }
        Ok(TrackSpan {
            timescale,
            first_decode: first.decode_time,
            start,
            end,
            media_duration: deltas.iter().map(|&delta| u64::from(delta)).sum(),
            deltas,
            movie_start: rescale(start, timescale, MOVIE_TIMESCALE)?,
        })
    }

    /// Builds the `stbl` children of the track around its sample entry.
    fn sample_table(
        &self,
        mut stsd: StblChild,
        span: &TrackSpan,
        format: ContainerFormat,
    ) -> Result<Vec<StblChild>, Error> {
        if let (ContainerFormat::Mov, StblChild::Stsd(entries)) = (format, &mut stsd)
            && self.info.kind() == TrackKind::Audio
        {
            let entry = entries
                .entries
                .first_mut()
                .ok_or_else(|| Error::Container(String::from("track has no sample entry")))?;
            quicktime_sound_description(entry)?;
        }
        if let (StblChild::Stsd(entries), Some(color)) = (&mut stsd, self.info.video_color_info())
            && self.info.kind() == TrackKind::Video
        {
            let entry = entries
                .entries
                .first_mut()
                .ok_or_else(|| Error::Container(String::from("track has no sample entry")))?;
            signal_color(entry, color)?;
        }

        let mut children = vec![stsd, StblChild::Stts(time_to_sample(&span.deltas))];
        if let Some(sync) = self.sync_samples()? {
            children.push(StblChild::Stss(sync));
        }
        if let Some(offsets) = self.composition_offsets() {
            children.push(StblChild::Ctts(offsets));
        }
        children.push(StblChild::Stsc(self.sample_to_chunk()));
        children.push(StblChild::Stsz(SampleSizeBox {
            version: 0,
            flags: 0,
            sample_size: 0,
            sample_count: count_u32(self.samples.len())?,
            entries: self.samples.iter().map(|record| record.size).collect(),
        }));
        children.push(self.chunk_offsets());
        Ok(children)
    }

    /// The `stss` table, or `None` when every sample is a sync sample.
    fn sync_samples(&self) -> Result<Option<SyncSampleBox>, Error> {
        if self.samples.iter().all(|record| record.sync) {
            return Ok(None);
        }
        let entries = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, record)| record.sync)
            .map(|(index, _)| count_u32(index + 1))
            .collect::<Result<_, _>>()?;
        Ok(Some(SyncSampleBox {
            version: 0,
            flags: 0,
            entries,
        }))
    }

    /// The `ctts` table, or `None` when no sample is reordered. Signed offsets
    /// need version 1.
    fn composition_offsets(&self) -> Option<CompositionOffsetBox> {
        if self
            .samples
            .iter()
            .all(|record| record.composition_offset == 0)
        {
            return None;
        }
        let mut entries: Vec<CttsEntry> = Vec::new();
        for record in &self.samples {
            match entries.last_mut() {
                Some(last) if last.sample_offset == record.composition_offset => {
                    last.sample_count += 1;
                }
                _ => entries.push(CttsEntry {
                    sample_count: 1,
                    sample_offset: record.composition_offset,
                }),
            }
        }
        let signed = self
            .samples
            .iter()
            .any(|record| record.composition_offset < 0);
        Some(CompositionOffsetBox {
            version: u8::from(signed),
            flags: 0,
            entries,
        })
    }

    fn sample_to_chunk(&self) -> SampleToChunkBox {
        let mut entries: Vec<StscEntry> = Vec::new();
        for (index, chunk) in self.chunks.iter().enumerate() {
            if entries
                .last()
                .is_none_or(|last| last.samples_per_chunk != chunk.samples)
            {
                entries.push(StscEntry {
                    first_chunk: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    samples_per_chunk: chunk.samples,
                    sample_description_index: 1,
                });
            }
        }
        SampleToChunkBox {
            version: 0,
            flags: 0,
            entries,
        }
    }

    /// `stco`, or `co64` once an offset passes 32 bits.
    fn chunk_offsets(&self) -> StblChild {
        let offsets: Result<Vec<u32>, _> = self
            .chunks
            .iter()
            .map(|chunk| u32::try_from(chunk.offset))
            .collect();
        offsets.map_or_else(
            |_| {
                StblChild::Co64(ChunkLargeOffsetBox {
                    version: 0,
                    flags: 0,
                    entries: self.chunks.iter().map(|chunk| chunk.offset).collect(),
                })
            },
            |entries| {
                StblChild::Stco(ChunkOffsetBox {
                    version: 0,
                    flags: 0,
                    entries,
                })
            },
        )
    }
}

impl TrackSpan {
    /// Places the track on the movie timeline that starts at `movie_start`.
    ///
    /// A late start becomes an empty edit and a composition lead-in (the
    /// presentation of the first sample after its decode time) becomes the
    /// start of the media edit.
    fn presentation(&self, movie_start: i64) -> Result<Presentation, Error> {
        let empty = u64::try_from(self.movie_start - movie_start)
            .map_err(|_| Error::Container(String::from("track starts before the movie")))?;
        let media_time = self.start - self.first_decode;
        if empty == 0 && media_time == 0 {
            return Ok(Presentation {
                duration: rescale_unsigned(self.media_duration, self.timescale)?,
                edits: None,
            });
        }
        let segment = rescale_unsigned(
            u64::try_from(self.end - self.start)
                .map_err(|_| Error::Container(String::from("track ends before it starts")))?,
            self.timescale,
        )?;
        let mut entries = Vec::with_capacity(2);
        if empty > 0 {
            entries.push(EditListEntry {
                segment_duration: empty,
                media_time: EMPTY_EDIT,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            });
        }
        entries.push(EditListEntry {
            segment_duration: segment,
            media_time,
            media_rate_integer: 1,
            media_rate_fraction: 0,
        });
        let wide = entries.iter().any(|entry| {
            u32::try_from(entry.segment_duration).is_err()
                || i32::try_from(entry.media_time).is_err()
        });
        Ok(Presentation {
            duration: empty + segment,
            edits: Some(EditListBox {
                version: u8::from(wide),
                flags: 0,
                entries,
            }),
        })
    }
}

/// Run-length codes per-sample decode deltas.
fn time_to_sample(deltas: &[u32]) -> TimeToSampleBox {
    let mut entries: Vec<SttsEntry> = Vec::new();
    for &delta in deltas {
        match entries.last_mut() {
            Some(last) if last.sample_delta == delta => last.sample_count += 1,
            _ => entries.push(SttsEntry {
                sample_count: 1,
                sample_delta: delta,
            }),
        }
    }
    TimeToSampleBox {
        version: 0,
        flags: 0,
        entries,
    }
}

/// The sample entry and configuration box for `info`'s codec.
pub fn codec_config(info: &TrackInfo) -> Result<CodecConfig, Error> {
    let expected = match info.codec() {
        Codec::H264 | Codec::H265 | Codec::Av1 => TrackKind::Video,
        Codec::Aac => TrackKind::Audio,
        other => {
            return Err(Error::Unsupported(format!(
                "the muxer writes H.264, H.265, AV1 and AAC tracks, not {other:?}"
            )));
        }
    };
    if info.kind() != expected {
        return Err(Error::Container(format!(
            "{:?} track declared as {:?}",
            info.codec(),
            info.kind()
        )));
    }
    let config = info.decoder_configuration();
    // H.264, H.265 and AV1 configurations are always the whole box.
    let record = || {
        config.get(8..).ok_or_else(|| {
            Error::Container(String::from(
                "codec configuration shorter than a box header",
            ))
        })
    };
    match info.codec() {
        Codec::H264 => {
            let (width, height) = video_size(info)?;
            Ok(CodecConfig::Avc {
                config: AVCConfigurationBox::parse_body(record()?).map_err(container_error)?,
                width,
                height,
            })
        }
        Codec::H265 => {
            let (width, height) = video_size(info)?;
            Ok(CodecConfig::Hevc {
                config: HEVCConfigurationBox::parse_body(record()?).map_err(container_error)?,
                width,
                height,
            })
        }
        Codec::Av1 => {
            let (width, height) = video_size(info)?;
            Ok(CodecConfig::Av1 {
                config: Av1ConfigurationBox::parse(record()?).map_err(container_error)?,
                width,
                height,
            })
        }
        _ => {
            let layout = info.audio_layout().ok_or_else(|| {
                Error::Container(String::from("an audio track needs its audio layout"))
            })?;
            let channel_count = u16::try_from(layout.channels.get()).map_err(|_| {
                Error::Container(String::from("channel count does not fit the u16 field"))
            })?;
            let specific_info = DecoderSpecificInfo::new(config.to_vec());
            let decoder = DecoderConfigDescriptor::new(
                OBJECT_TYPE_MPEG4_AUDIO,
                STREAM_TYPE_AUDIO,
                false,
                0,
                0,
                0,
                Some(specific_info),
            );
            Ok(CodecConfig::Aac {
                esds: EsdsBox::new(ESDescriptor::new(
                    0,
                    0,
                    Some(decoder),
                    Some(SLConfigDescriptor::predefined_two()),
                )),
                channel_count,
                sample_rate: layout.sample_rate.get(),
                sample_size: layout.sample_size,
            })
        }
    }
}

fn video_size(info: &TrackInfo) -> Result<(u16, u16), Error> {
    let dimensions = info
        .video_dimensions()
        .ok_or_else(|| Error::Container(String::from("a video track needs its dimensions")))?;
    let field = |pixels: std::num::NonZeroU32| {
        u16::try_from(pixels.get()).map_err(|_| {
            Error::Container(format!("{pixels} pixels exceed the u16 sample-entry field"))
        })
    };
    Ok((field(dimensions.width)?, field(dimensions.height)?))
}

/// Appends `colr` (and `clli` when the content light level is known) to a
/// video sample entry.
fn signal_color(entry: &mut SampleEntryVariant, color: VideoColorInfo) -> Result<(), Error> {
    let cicp = color.cicp();
    let mut nclx = Vec::with_capacity(11);
    nclx.extend_from_slice(b"nclx");
    nclx.extend_from_slice(&u16::from(cicp.primaries).to_be_bytes());
    nclx.extend_from_slice(&u16::from(cicp.transfer).to_be_bytes());
    nclx.extend_from_slice(&u16::from(cicp.matrix).to_be_bytes());
    nclx.push(u8::from(cicp.full_range) << 7);
    let mut boxes = vec![(*b"colr", nclx)];
    if let Some(light) = color.content_light_level {
        let mut clli = Vec::with_capacity(4);
        clli.extend_from_slice(&light.max_content_light_level().to_be_bytes());
        clli.extend_from_slice(&light.max_frame_average_light_level().to_be_bytes());
        boxes.push((*b"clli", clli));
    }
    match entry {
        SampleEntryVariant::Avc1(entry) => entry.extra_boxes.extend(
            boxes
                .into_iter()
                .map(|(box_type, data)| OpaqueBox { box_type, data }),
        ),
        SampleEntryVariant::Hevc1(entry) => entry.extra_boxes.extend(
            boxes
                .into_iter()
                .map(|(box_type, data)| OpaqueBox { box_type, data }),
        ),
        SampleEntryVariant::Av01(entry) => {
            for (box_type, data) in boxes {
                let size = u32::try_from(data.len() + 8)
                    .map_err(|_| Error::Container(String::from("color box too large")))?;
                let mut bytes = size.to_be_bytes().to_vec();
                bytes.extend_from_slice(&box_type);
                bytes.extend_from_slice(&data);
                entry.children.push(SampleEntryChild::Other(bytes));
            }
        }
        _ => {
            return Err(Error::Container(String::from(
                "unexpected video sample entry",
            )));
        }
    }
    Ok(())
}

/// Marks an ISO `mp4a` entry as `QuickTime` variable-bitrate audio
/// (`compression_ID` -2) and adds the `chan` channel layout, which is what
/// `AVFoundation` requires of a MOV's audio.
///
/// The full `QuickTime` sound description (version 1 with a `wave` atom) also
/// opens in `AVFoundation`, but the `mp4` crate this crate demuxes with cannot
/// parse it, so the entry keeps the ISO layout.
fn quicktime_sound_description(entry: &mut SampleEntryVariant) -> Result<(), Error> {
    /// `compression_ID` marking variable-bitrate audio, then a zero packet size.
    const VBR_COMPRESSION_ID: [u8; 4] = [0xff, 0xfe, 0, 0];

    let SampleEntryVariant::Mp4a(mp4a) = entry else {
        return Err(Error::Container(String::from(
            "unexpected audio sample entry",
        )));
    };
    mp4a.compression_id_and_packet_size = VBR_COMPRESSION_ID;
    let layout = match mp4a.channelcount {
        1 => Some(CHANNEL_LAYOUT_MONO),
        2 => Some(CHANNEL_LAYOUT_STEREO),
        _ => None,
    };
    if let Some(layout) = layout {
        let mut chan = vec![0; 4];
        chan.extend_from_slice(&layout.to_be_bytes());
        chan.extend_from_slice(&[0; 8]);
        mp4a.config_boxes.push(EntryBox::new(*b"chan", chan));
    }
    Ok(())
}

/// `kAudioChannelLayoutTag_Mono`.
const CHANNEL_LAYOUT_MONO: u32 = 0x0064_0001;
/// `kAudioChannelLayoutTag_Stereo`.
const CHANNEL_LAYOUT_STEREO: u32 = 0x0065_0002;

/// Version of a movie, track or media header whose duration is `duration`:
/// 32-bit fields at version 0, 64-bit at version 1.
fn header_version(duration: u64) -> u8 {
    u8::from(u32::try_from(duration).is_err())
}

/// Converts `ticks` between timescales, rounding toward negative infinity.
fn rescale(ticks: i64, from: u32, to: u32) -> Result<i64, Error> {
    let scaled = (i128::from(ticks) * i128::from(to)).div_euclid(i128::from(from));
    i64::try_from(scaled)
        .map_err(|_| Error::Container(String::from("timestamp exceeds the movie timeline")))
}

fn rescale_unsigned(ticks: u64, timescale: u32) -> Result<u64, Error> {
    let scaled = u128::from(ticks) * u128::from(MOVIE_TIMESCALE) / u128::from(timescale);
    u64::try_from(scaled)
        .map_err(|_| Error::Container(String::from("duration exceeds the movie timeline")))
}

fn count_u32(count: usize) -> Result<u32, Error> {
    u32::try_from(count)
        .map_err(|_| Error::Container(String::from("sample count exceeds the u32 table fields")))
}

fn container_error(error: impl std::fmt::Display) -> Error {
    Error::Container(error.to_string())
}

#[cfg(test)]
pub mod tests;
