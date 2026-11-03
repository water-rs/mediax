//! The errors of H.265 parsing and picture management.

use thiserror::Error;

/// A stream feature the decoder does not implement.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Unsupported {
    /// The profile is not Main, Main 10 or Main Still Picture.
    #[error("H.265 general_profile_idc {0}; only Main and Main 10 are decoded")]
    Profile(u8),
    /// The stream codes fields.
    #[error("interlaced coding; only progressive streams are decoded")]
    Interlaced,
    /// The chroma format is not 4:2:0.
    #[error("chroma_format_idc {0}; only 4:2:0 is decoded")]
    ChromaFormat(u32),
    /// The sample bit depths are not both 8 or both 10.
    #[error("{luma}-bit luma and {chroma}-bit chroma samples; only 8-bit and 10-bit are decoded")]
    BitDepth {
        /// The luma bit depth.
        luma: u32,
        /// The chroma bit depth.
        chroma: u32,
    },
    /// A parameter set carries an extension beyond version 1 of the standard.
    #[error("the {0} extension")]
    Extension(&'static str),
}

/// Why a picture could not be managed in the decoded picture buffer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DpbError {
    /// A header value does not fit the sequence.
    #[error("{field} {value} is out of range")]
    OutOfRange {
        /// The syntax element.
        field: &'static str,
        /// Its value.
        value: i64,
    },
    /// The flags of a header disagree with its NAL unit type.
    #[error("the header is inconsistent with its NAL unit type: {0}")]
    InconsistentHeader(&'static str),
    /// A picture is being managed already.
    #[error("a picture is being decoded already")]
    PictureInProgress,
    /// No picture is being managed.
    #[error("no picture is being decoded")]
    NoPicture,
    /// The buffer is smaller than the sequence needs.
    #[error("the sequence needs {needed} frames in a buffer of {capacity}")]
    BufferTooSmall {
        /// The frames the sequence keeps.
        needed: usize,
        /// The buffer's capacity.
        capacity: usize,
    },
    /// Every frame buffer holds a reference and none waits for output.
    #[error("the decoded picture buffer is full of reference frames")]
    BufferOverflow,
    /// The reference picture set names a picture that is not in the buffer.
    #[error("the reference picture set names the missing picture with order count {0}")]
    MissingReference(i64),
    /// A long-term entry matches more than one picture.
    #[error("a long-term reference picture is ambiguous: order count (lsb) {0}")]
    AmbiguousLongTerm(i64),
    /// A reference list modification names an entry the list does not have.
    #[error("a reference list entry {entry} is outside the {len} pictures of the set")]
    ListEntry {
        /// The `list_entry`.
        entry: u32,
        /// `NumPicTotalCurr`.
        len: usize,
    },
    /// A slice needs a reference list but the reference picture set has no
    /// picture it may refer to.
    #[error("a P or B slice has no picture to refer to")]
    EmptyReferenceSet,
    /// More pictures are references than the sequence allows.
    #[error("{references} reference pictures exceed the buffer size {limit}")]
    TooManyReferences {
        /// The pictures marked as references.
        references: usize,
        /// `sps_max_dec_pic_buffering_minus1`.
        limit: usize,
    },
}

/// Why an H.265 stream could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum H265Error {
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
    #[error("the active sequence parameter set changed outside an IRAP picture")]
    SequenceChange,
    /// The stream uses a feature the decoder does not implement.
    #[error("unsupported: {0}")]
    Unsupported(#[from] Unsupported),
    /// Picture management failed.
    #[error(transparent)]
    Dpb(#[from] DpbError),
    /// A NAL unit that the stream cannot contain, or a value out of its
    /// range.
    #[error("invalid stream: {0}")]
    Invalid(&'static str),
}

impl H265Error {
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
