//! Reading a file's samples of every track, in the order a muxer wants them.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use mediax_container::{
    EncodedSample, MediaFile, MediaFormat, MpegTsDemuxer, MpegTsEvent, ProgressiveTrackReader,
    TrackKind,
};

use crate::error::CliError;

/// Bytes of a transport stream fed to the demuxer per read: 512 packets of 188
/// bytes.
const TS_READ_BYTES: usize = 188 * 512;

/// Every sample of a [`MediaFile`], interleaved across its tracks by decode
/// time.
#[derive(Debug)]
pub enum Samples {
    /// MP4, MOV or fragmented MP4: one reader per track, merged by decode time.
    Progressive(Vec<Lane>),
    /// MPEG-TS: the demuxer already emits samples in stream order.
    TransportStream(Box<TransportStream>),
}

impl Samples {
    /// Starts reading the samples of `file`, which was opened from `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read again or a track cannot
    /// be selected.
    pub fn open(path: &Path, file: &MediaFile) -> Result<Self, CliError> {
        match file.format() {
            MediaFormat::Mp4 | MediaFormat::Mov | MediaFormat::FragmentedMp4 => {
                let mut audio_seen = 0;
                let mut video_seen = 0;
                let mut lanes = Vec::new();
                for track in file.tracks() {
                    let kind = track.info().kind();
                    let seen = match kind {
                        TrackKind::Video => &mut video_seen,
                        TrackKind::Audio => &mut audio_seen,
                        TrackKind::Subtitle | TrackKind::Metadata => {
                            return Err(mediax_container::Error::Unsupported(format!(
                                "{kind:?} tracks cannot be read as samples"
                            ))
                            .into());
                        }
                    };
                    let mut reader = ProgressiveTrackReader::open(path, kind)?;
                    reader.select_track(*seen)?;
                    *seen += 1;
                    let next = reader.read_sample()?;
                    lanes.push(Lane { reader, next });
                }
                Ok(Self::Progressive(lanes))
            }
            MediaFormat::MpegTs => Ok(Self::TransportStream(Box::new(TransportStream {
                file: File::open(path)?,
                demuxer: MpegTsDemuxer::new(),
                buffer: vec![0; TS_READ_BYTES],
                pending: VecDeque::new(),
                finished: false,
            }))),
            other => Err(CliError::UnsupportedFormat(other)),
        }
    }
}

impl Iterator for Samples {
    type Item = Result<EncodedSample, CliError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Progressive(lanes) => next_progressive(lanes),
            Self::TransportStream(stream) => stream.next_sample().transpose(),
        }
    }
}

/// One track of a progressive file and its next unread sample.
#[derive(Debug)]
pub struct Lane {
    reader: ProgressiveTrackReader,
    next: Option<EncodedSample>,
}

fn next_progressive(lanes: &mut [Lane]) -> Option<Result<EncodedSample, CliError>> {
    let lane = lanes
        .iter_mut()
        .filter(|lane| lane.next.is_some())
        .min_by(|a, b| {
            let (a, b) = (
                a.next.as_ref().map(EncodedSample::decode_time),
                b.next.as_ref().map(EncodedSample::decode_time),
            );
            // Cross-multiplied so tracks with different timescales compare
            // exactly.
            a.zip(b).map_or(std::cmp::Ordering::Equal, |(a, b)| {
                (i128::from(a.ticks()) * i128::from(b.timescale().get()))
                    .cmp(&(i128::from(b.ticks()) * i128::from(a.timescale().get())))
            })
        })?;
    let sample = lane.next.take()?;
    match lane.reader.read_sample() {
        Ok(next) => {
            lane.next = next;
            Some(Ok(sample))
        }
        Err(error) => Some(Err(error.into())),
    }
}

/// An MPEG-TS file read in chunks.
#[derive(Debug)]
pub struct TransportStream {
    file: File,
    demuxer: MpegTsDemuxer,
    buffer: Vec<u8>,
    pending: VecDeque<EncodedSample>,
    finished: bool,
}

impl TransportStream {
    fn next_sample(&mut self) -> Result<Option<EncodedSample>, CliError> {
        loop {
            if let Some(sample) = self.pending.pop_front() {
                return Ok(Some(sample));
            }
            if self.finished {
                return Ok(None);
            }
            let read = self.file.read(&mut self.buffer)?;
            let events = if read == 0 {
                self.finished = true;
                self.demuxer.finish()?
            } else {
                self.demuxer.feed(&self.buffer[..read])?
            };
            self.pending
                .extend(events.into_iter().filter_map(|event| match event {
                    MpegTsEvent::Sample(sample) => Some(sample),
                    _ => None,
                }));
        }
    }
}
