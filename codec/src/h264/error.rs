//! The errors of H.264 parsing and picture management.

use thiserror::Error;

/// A stream feature the decoder does not implement.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Unsupported {
    /// The profile is not Baseline, Main or High.
    #[error("H.264 profile_idc {0}; only Baseline, Main and High are decoded")]
    Profile(u8),
    /// The stream codes fields or macroblock-adaptive frame/field pairs.
    #[error("interlaced coding; only progressive streams are decoded")]
    Interlaced,
    /// The chroma format is not 4:2:0.
    #[error("chroma_format_idc {0}; only 4:2:0 is decoded")]
    ChromaFormat(u32),
    /// The sample bit depth is not 8.
    #[error("{luma}-bit luma and {chroma}-bit chroma samples; only 8-bit is decoded")]
    BitDepth {
        /// The luma bit depth.
        luma: u8,
        /// The chroma bit depth.
        chroma: u8,
    },
    /// The picture parameter set uses slice groups.
    #[error("flexible macroblock ordering (slice groups)")]
    SliceGroups,
    /// A slice is an SP or SI slice.
    #[error("SP and SI slices")]
    SwitchingSlices,
    /// The stream is partitioned into data partitions A, B and C.
    #[error("data partitioning")]
    DataPartitioning,
}

/// Why a picture could not be managed in the decoded picture buffer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DpbError {
    /// `frame_num` skips values although the sequence does not allow it.
    #[error("frame_num {found} follows {previous} and the sequence allows no gaps")]
    FrameNumGap {
        /// `PrevRefFrameNum`.
        previous: u32,
        /// The `frame_num` of the picture.
        found: u32,
    },
    /// An IDR picture has a `frame_num` other than 0.
    #[error("an IDR picture has frame_num {0}")]
    IdrFrameNum(u32),
    /// A header value does not fit the sequence.
    #[error("{field} {value} is out of range")]
    OutOfRange {
        /// The syntax element.
        field: &'static str,
        /// Its value.
        value: i64,
    },
    /// The reference marking of a header disagrees with `nal_ref_idc`.
    #[error("reference marking is present exactly for reference pictures")]
    InconsistentMarking,
    /// A picture is being managed already.
    #[error("a picture is being decoded already")]
    PictureInProgress,
    /// No picture is being managed.
    #[error("no picture is being decoded")]
    NoPicture,
    /// The sequence keeps more reference frames than its buffer holds.
    #[error("the sequence keeps {references} reference frames in a buffer of {capacity}")]
    BufferTooSmall {
        /// `max_num_ref_frames`.
        references: usize,
        /// The buffer's frame capacity.
        capacity: usize,
    },
    /// The sliding window has no short-term frame to drop.
    #[error("the sliding window found no short-term reference frame to remove")]
    SlidingWindowEmpty,
    /// After marking, more frames are references than the sequence allows.
    #[error("{references} reference frames exceed max_num_ref_frames {limit}")]
    TooManyReferences {
        /// The frames marked as references.
        references: usize,
        /// `max_num_ref_frames`.
        limit: usize,
    },
    /// Every frame buffer holds a reference and none waits for output.
    #[error("the decoded picture buffer is full of reference frames")]
    BufferOverflow,
    /// A memory management control operation names no such frame, or an
    /// index the sequence does not allow.
    #[error("invalid memory management control operation: {0}")]
    InvalidMmco(&'static str),
    /// A reference list modification names a frame that is not a reference.
    #[error("reference list modification names missing picture number {0}")]
    MissingReference(i64),
    /// A reference list modification has more operations than entries.
    #[error("a reference list modification has more operations than the list has entries")]
    TooManyModifications,
}

/// Why an H.264 stream could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum H264Error {
    /// A NAL unit does not parse.
    #[error("malformed {unit}: {detail}")]
    Malformed {
        /// The kind of NAL unit.
        unit: &'static str,
        /// The parser's description.
        detail: String,
    },
    /// A slice refers to a parameter set that was never received.
    #[error("a slice refers to the missing {0}")]
    MissingParameterSet(&'static str),
    /// A parameter set changed while its sequence was active.
    #[error("the active sequence parameter set changed outside an IDR picture")]
    SequenceChange,
    /// The stream uses a feature the decoder does not implement.
    #[error("unsupported: {0}")]
    Unsupported(#[from] Unsupported),
    /// Picture management failed.
    #[error(transparent)]
    Dpb(#[from] DpbError),
    /// A NAL unit that the stream cannot contain.
    #[error("invalid stream: {0}")]
    Invalid(&'static str),
}

impl H264Error {
    /// Builds [`Self::Malformed`] from a parser error.
    pub fn malformed(unit: &'static str, error: &impl std::fmt::Debug) -> Self {
        Self::Malformed {
            unit,
            detail: format!("{error:?}"),
        }
    }

    /// Whether the stream is fine and only uses a feature that is not
    /// implemented.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}
