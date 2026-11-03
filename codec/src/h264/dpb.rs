//! The decoded picture buffer of progressive H.264 streams.
//!
//! This is the picture management of ITU-T H.264 clause 8.2 and annex C.4:
//! picture order count (8.2.1), `frame_num` gaps (8.2.5.2), reference marking
//! with the sliding window and memory management control operations
//! (8.2.5), reference list construction and modification (8.2.4) and the
//! output order of the bumping process (C.4). It knows nothing of the decoder
//! that consumes it: pictures are identified by a [`FrameId`], and what the
//! caller must do with a frame is reported as an [`Event`].
//!
//! Only frames exist here (`frame_mbs_only_flag` is 1), so `PicNum` is
//! `FrameNumWrap`, `LongTermPicNum` is `LongTermFrameIdx` and the picture
//! order count of a frame is the smaller of its two field order counts.

use super::error::DpbError;

/// Identifies a frame in the buffer for as long as the frame lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameId(u64);

/// What the caller does with a frame, in the order the events are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The frame is output now; its samples must be read before it is
    /// released.
    Output(FrameId),
    /// The frame is neither needed for output nor for reference; its storage
    /// is free.
    Release(FrameId),
}

/// How the picture order count is coded (`pic_order_cnt_type`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PocType {
    /// Type 0: the least significant bits are coded in every slice header.
    Zero {
        /// `log2_max_pic_order_cnt_lsb_minus4 + 4`.
        log2_max_lsb: u8,
    },
    /// Type 1: the count follows from `frame_num` and a cycle of offsets.
    One {
        /// `offset_for_non_ref_pic`.
        offset_for_non_ref_pic: i32,
        /// `offset_for_top_to_bottom_field`.
        offset_for_top_to_bottom_field: i32,
        /// `offset_for_ref_frame`, one per picture of the cycle.
        offsets_for_ref_frame: Vec<i32>,
    },
    /// Type 2: the count is twice `FrameNumOffset + frame_num`.
    Two,
}

/// What picture management needs of a sequence parameter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceParams {
    /// `log2_max_frame_num_minus4 + 4`.
    pub log2_max_frame_num: u8,
    /// `pic_order_cnt_type` with its parameters.
    pub poc: PocType,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: usize,
    /// `gaps_in_frame_num_value_allowed_flag`.
    pub gaps_allowed: bool,
    /// The number of frame buffers (`max_dec_frame_buffering`, or `MaxDpbFrames`
    /// of the level).
    pub capacity: usize,
    /// `max_num_reorder_frames` of the VUI: output starts once more frames
    /// wait than this.
    pub num_reorder_frames: Option<usize>,
}

impl SequenceParams {
    const fn max_frame_num(&self) -> i64 {
        1_i64 << self.log2_max_frame_num
    }
}

/// One memory management control operation (7.4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmco {
    /// Operation 1: a short-term frame becomes unused for reference.
    ShortTermUnused {
        /// `difference_of_pic_nums_minus1`.
        difference_of_pic_nums_minus1: u32,
    },
    /// Operation 2: a long-term frame becomes unused for reference.
    LongTermUnused {
        /// `long_term_pic_num`.
        long_term_pic_num: u32,
    },
    /// Operation 3: a short-term frame becomes long-term.
    ShortToLong {
        /// `difference_of_pic_nums_minus1`.
        difference_of_pic_nums_minus1: u32,
        /// `long_term_frame_idx`.
        long_term_frame_idx: u32,
    },
    /// Operation 4: limits the long-term frame indices.
    MaxLongTermIdx {
        /// `max_long_term_frame_idx_plus1`.
        max_long_term_frame_idx_plus1: u32,
    },
    /// Operation 5: every reference becomes unused.
    AllUnused,
    /// Operation 6: the current frame becomes long-term.
    CurrentToLong {
        /// `long_term_frame_idx`.
        long_term_frame_idx: u32,
    },
}

/// The reference marking syntax of a reference picture (`dec_ref_pic_marking`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marking {
    /// The marking of an IDR picture.
    Idr {
        /// `no_output_of_prior_pics_flag`.
        no_output_of_prior_pics: bool,
        /// `long_term_reference_flag`.
        long_term_reference: bool,
    },
    /// `adaptive_ref_pic_marking_mode_flag` is 0.
    SlidingWindow,
    /// `adaptive_ref_pic_marking_mode_flag` is 1.
    Adaptive(Vec<Mmco>),
}

/// The slice header fields that picture management reads, of the first slice
/// of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureHeader {
    /// Whether this is an IDR picture.
    pub idr: bool,
    /// `nal_ref_idc`.
    pub nal_ref_idc: u8,
    /// `frame_num`.
    pub frame_num: u32,
    /// `pic_order_cnt_lsb`, for type 0.
    pub poc_lsb: u32,
    /// `delta_pic_order_cnt_bottom`, for type 0.
    pub delta_poc_bottom: i32,
    /// `delta_pic_order_cnt`, for types 1 and 2.
    pub delta_poc: [i32; 2],
    /// `dec_ref_pic_marking`, present exactly when `nal_ref_idc` is not 0.
    pub marking: Option<Marking>,
}

/// A reference list modification operation (7.4.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modification {
    /// `modification_of_pic_nums_idc` 0.
    Subtract {
        /// `abs_diff_pic_num_minus1`.
        abs_diff_pic_num_minus1: u32,
    },
    /// `modification_of_pic_nums_idc` 1.
    Add {
        /// `abs_diff_pic_num_minus1`.
        abs_diff_pic_num_minus1: u32,
    },
    /// `modification_of_pic_nums_idc` 2.
    LongTerm {
        /// `long_term_pic_num`.
        long_term_pic_num: u32,
    },
}

/// The kind of slice a reference list is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// A P slice: list 0 only.
    P,
    /// A B slice: lists 0 and 1.
    B,
}

/// What the caller knows of a frame in the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameView {
    /// The frame's identity.
    pub id: FrameId,
    /// The `frame_num` of the frame (0 after memory management operation 5).
    pub frame_num: u32,
    /// `LongTermFrameIdx`, for a long-term reference frame.
    pub long_term_idx: Option<u32>,
    /// Whether the frame is marked as used for reference.
    pub is_reference: bool,
    /// Whether the gap process inferred the frame.
    pub non_existing: bool,
    /// `TopFieldOrderCnt`.
    pub top: i32,
    /// `BottomFieldOrderCnt`.
    pub bottom: i32,
}

/// The result of starting a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Began {
    /// The frame the picture is decoded into.
    pub id: FrameId,
    /// What to do with frames the `frame_num` gap process displaced.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    Unused,
    ShortTerm,
    LongTerm(u32),
}

#[derive(Debug, Clone)]
#[allow(
    clippy::struct_field_names,
    reason = "`frame_num` is the syntax element's name"
)]
struct Frame {
    id: FrameId,
    frame_num: u32,
    top: i32,
    bottom: i32,
    reference: Reference,
    needed_for_output: bool,
    non_existing: bool,
}

impl Frame {
    /// `PicOrderCnt` of the frame.
    fn poc(&self) -> i32 {
        self.top.min(self.bottom)
    }

    fn view(&self) -> FrameView {
        FrameView {
            id: self.id,
            frame_num: self.frame_num,
            long_term_idx: match self.reference {
                Reference::LongTerm(idx) => Some(idx),
                _ => None,
            },
            is_reference: self.reference != Reference::Unused,
            non_existing: self.non_existing,
            top: self.top,
            bottom: self.bottom,
        }
    }

    fn is_short_term(&self) -> bool {
        self.reference == Reference::ShortTerm
    }
}

/// What the picture order count and `frame_num` derivations remember of the
/// previous pictures.
#[derive(Debug, Clone, Copy, Default)]
struct Previous {
    /// Whether any picture was decoded.
    started: bool,
    /// `PrevRefFrameNum`.
    ref_frame_num: u32,
    /// `prevFrameNum`.
    frame_num: u32,
    /// `prevFrameNumOffset`.
    frame_num_offset: i64,
    /// `prevPicOrderCntMsb`.
    poc_msb: i64,
    /// `prevPicOrderCntLsb`.
    poc_lsb: i64,
}

/// The derived order counts of a picture.
#[derive(Debug, Clone, Copy)]
struct Poc {
    top: i32,
    bottom: i32,
    frame_num_offset: i64,
    msb: i64,
}

#[derive(Debug)]
struct Current {
    frame: Frame,
    header: PictureHeader,
    frame_num_offset: i64,
    poc_msb: i64,
}

/// The decoded picture buffer.
#[derive(Debug)]
pub struct Dpb {
    seq: SequenceParams,
    frames: Vec<Frame>,
    current: Option<Current>,
    next_id: u64,
    /// `MaxLongTermFrameIdx`; `None` is "no long-term frame indices".
    max_long_term_idx: Option<u32>,
    previous: Previous,
}

fn to_i32(field: &'static str, value: i64) -> Result<i32, DpbError> {
    i32::try_from(value).map_err(|_| DpbError::OutOfRange { field, value })
}

impl Dpb {
    /// Creates an empty buffer for a sequence.
    ///
    /// # Errors
    ///
    /// Returns [`DpbError::BufferTooSmall`] when the buffer cannot hold the
    /// sequence's reference frames.
    pub fn new(seq: SequenceParams) -> Result<Self, DpbError> {
        if seq.max_num_ref_frames > seq.capacity {
            return Err(DpbError::BufferTooSmall {
                references: seq.max_num_ref_frames,
                capacity: seq.capacity,
            });
        }
        Ok(Self {
            seq,
            frames: Vec::new(),
            current: None,
            next_id: 0,
            max_long_term_idx: None,
            previous: Previous::default(),
        })
    }

    /// The number of frame buffers.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.seq.capacity
    }

    /// The frame being decoded.
    #[must_use]
    pub fn current(&self) -> Option<FrameView> {
        self.current.as_ref().map(|current| current.frame.view())
    }

    /// Looks a frame up, the one being decoded included.
    #[must_use]
    pub fn frame(&self, id: FrameId) -> Option<FrameView> {
        self.frames
            .iter()
            .chain(self.current.iter().map(|current| &current.frame))
            .find(|frame| frame.id == id)
            .map(Frame::view)
    }

    /// The frames marked as used for reference, short-term and long-term.
    pub fn reference_frames(&self) -> impl Iterator<Item = FrameView> + '_ {
        self.frames
            .iter()
            .filter(|frame| frame.reference != Reference::Unused)
            .map(Frame::view)
    }

    const fn fresh_id(&mut self) -> FrameId {
        let id = FrameId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Starts a picture: infers the frames of a `frame_num` gap, derives the
    /// picture order count and allocates the current frame.
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when the header is inconsistent with the
    /// sequence or the buffer.
    pub fn begin_picture(&mut self, header: PictureHeader) -> Result<Began, DpbError> {
        if self.current.is_some() {
            return Err(DpbError::PictureInProgress);
        }
        let consistent = match &header.marking {
            None => header.nal_ref_idc == 0 && !header.idr,
            Some(Marking::Idr { .. }) => header.nal_ref_idc != 0 && header.idr,
            Some(_) => header.nal_ref_idc != 0 && !header.idr,
        };
        if !consistent {
            return Err(DpbError::InconsistentMarking);
        }
        if header.idr && header.frame_num != 0 {
            return Err(DpbError::IdrFrameNum(header.frame_num));
        }
        if i64::from(header.frame_num) >= self.seq.max_frame_num() {
            return Err(DpbError::OutOfRange {
                field: "frame_num",
                value: i64::from(header.frame_num),
            });
        }
        let mut events = Vec::new();
        if !header.idr && self.previous.started {
            self.fill_frame_num_gap(header.frame_num, &mut events)?;
        }
        let poc = self.derive_poc(&header)?;
        let id = self.fresh_id();
        self.current = Some(Current {
            frame: Frame {
                id,
                frame_num: header.frame_num,
                top: poc.top,
                bottom: poc.bottom,
                reference: Reference::Unused,
                needed_for_output: true,
                non_existing: false,
            },
            header,
            frame_num_offset: poc.frame_num_offset,
            poc_msb: poc.msb,
        });
        Ok(Began { id, events })
    }

    /// Drops the picture being decoded, as when its decoding failed.
    pub fn abort_picture(&mut self) -> Option<FrameId> {
        self.current.take().map(|current| current.frame.id)
    }

    /// 8.2.1: the picture order counts of a picture.
    fn derive_poc(&self, header: &PictureHeader) -> Result<Poc, DpbError> {
        let previous = &self.previous;
        let max_frame_num = self.seq.max_frame_num();
        let frame_num = i64::from(header.frame_num);
        let frame_num_offset = if header.idr {
            0
        } else if i64::from(previous.frame_num) > frame_num {
            previous.frame_num_offset + max_frame_num
        } else {
            previous.frame_num_offset
        };
        let (top, bottom, msb) = match &self.seq.poc {
            PocType::Zero { log2_max_lsb } => {
                let max_lsb = 1_i64 << log2_max_lsb;
                let lsb = i64::from(header.poc_lsb);
                if lsb >= max_lsb {
                    return Err(DpbError::OutOfRange {
                        field: "pic_order_cnt_lsb",
                        value: lsb,
                    });
                }
                let (anchor_msb, prior_lsb) = if header.idr {
                    (0, 0)
                } else {
                    (previous.poc_msb, previous.poc_lsb)
                };
                let msb = if lsb < prior_lsb && prior_lsb - lsb >= max_lsb / 2 {
                    anchor_msb + max_lsb
                } else if lsb > prior_lsb && lsb - prior_lsb > max_lsb / 2 {
                    anchor_msb - max_lsb
                } else {
                    anchor_msb
                };
                let top = msb + lsb;
                (top, top + i64::from(header.delta_poc_bottom), msb)
            }
            PocType::One {
                offset_for_non_ref_pic,
                offset_for_top_to_bottom_field,
                offsets_for_ref_frame,
            } => {
                let cycle_length = i64::try_from(offsets_for_ref_frame.len()).unwrap_or(0);
                let mut absolute = if cycle_length == 0 {
                    0
                } else {
                    frame_num_offset + frame_num
                };
                if header.nal_ref_idc == 0 && absolute > 0 {
                    absolute -= 1;
                }
                let mut expected = if absolute > 0 {
                    let cycle_count = (absolute - 1) / cycle_length;
                    let in_cycle = usize::try_from((absolute - 1) % cycle_length).unwrap_or(0);
                    let per_cycle: i64 = offsets_for_ref_frame.iter().copied().map(i64::from).sum();
                    cycle_count.saturating_mul(per_cycle)
                        + offsets_for_ref_frame[..=in_cycle]
                            .iter()
                            .copied()
                            .map(i64::from)
                            .sum::<i64>()
                } else {
                    0
                };
                if header.nal_ref_idc == 0 {
                    expected += i64::from(*offset_for_non_ref_pic);
                }
                let top = expected + i64::from(header.delta_poc[0]);
                let bottom = top
                    + i64::from(*offset_for_top_to_bottom_field)
                    + i64::from(header.delta_poc[1]);
                (top, bottom, 0)
            }
            PocType::Two => {
                let count = if header.idr {
                    0
                } else if header.nal_ref_idc == 0 {
                    2 * (frame_num_offset + frame_num) - 1
                } else {
                    2 * (frame_num_offset + frame_num)
                };
                (count, count, 0)
            }
        };
        Ok(Poc {
            top: to_i32("TopFieldOrderCnt", top)?,
            bottom: to_i32("BottomFieldOrderCnt", bottom)?,
            frame_num_offset,
            msb,
        })
    }

    /// 8.2.5.2: infers the frames a jump of `frame_num` skipped.
    fn fill_frame_num_gap(
        &mut self,
        frame_num: u32,
        events: &mut Vec<Event>,
    ) -> Result<(), DpbError> {
        let max_frame_num = self.seq.max_frame_num();
        let previous = i64::from(self.previous.ref_frame_num);
        let found = i64::from(frame_num);
        if found == previous || found == (previous + 1) % max_frame_num {
            return Ok(());
        }
        if !self.seq.gaps_allowed {
            return Err(DpbError::FrameNumGap {
                previous: self.previous.ref_frame_num,
                found: frame_num,
            });
        }
        let mut unused = (previous + 1) % max_frame_num;
        while unused != found {
            let unused_frame_num = u32::try_from(unused).unwrap_or(0);
            let header = PictureHeader {
                idr: false,
                nal_ref_idc: 1,
                frame_num: unused_frame_num,
                poc_lsb: 0,
                delta_poc_bottom: 0,
                delta_poc: [0; 2],
                marking: Some(Marking::SlidingWindow),
            };
            // Picture order counts of type 0 depend on slice headers the
            // inferred frames do not have.
            let poc = match self.seq.poc {
                PocType::Zero { .. } => Poc {
                    top: 0,
                    bottom: 0,
                    frame_num_offset: 0,
                    msb: 0,
                },
                _ => self.derive_poc(&header)?,
            };
            let id = self.fresh_id();
            Self::sliding_window(&self.seq, &mut self.frames, unused_frame_num)?;
            self.reap(events);
            let frame = Frame {
                id,
                frame_num: unused_frame_num,
                top: poc.top,
                bottom: poc.bottom,
                reference: Reference::ShortTerm,
                needed_for_output: false,
                non_existing: true,
            };
            self.store(frame, events)?;
            self.previous.frame_num = unused_frame_num;
            self.previous.ref_frame_num = unused_frame_num;
            self.previous.frame_num_offset = poc.frame_num_offset;
            unused = (unused + 1) % max_frame_num;
        }
        Ok(())
    }

    /// `FrameNumWrap` of a frame relative to the current `frame_num`.
    fn frame_num_wrap(seq: &SequenceParams, frame_num: u32, current: u32) -> i64 {
        if frame_num > current {
            i64::from(frame_num) - seq.max_frame_num()
        } else {
            i64::from(frame_num)
        }
    }

    /// 8.2.5.3: removes the oldest short-term frame when the window is full.
    fn sliding_window(
        seq: &SequenceParams,
        frames: &mut [Frame],
        current_frame_num: u32,
    ) -> Result<(), DpbError> {
        let references = frames
            .iter()
            .filter(|frame| frame.reference != Reference::Unused)
            .count();
        if references < seq.max_num_ref_frames.max(1) {
            return Ok(());
        }
        let oldest = frames
            .iter_mut()
            .filter(|frame| frame.is_short_term())
            .min_by_key(|frame| Self::frame_num_wrap(seq, frame.frame_num, current_frame_num))
            .ok_or(DpbError::SlidingWindowEmpty)?;
        oldest.reference = Reference::Unused;
        Ok(())
    }

    /// Releases the frames that are neither references nor waiting for output.
    fn reap(&mut self, events: &mut Vec<Event>) {
        self.frames.retain(|frame| {
            let keep = frame.reference != Reference::Unused || frame.needed_for_output;
            if !keep {
                events.push(Event::Release(frame.id));
            }
            keep
        });
    }

    /// C.4.5.3: outputs the waiting frame that is first in output order.
    fn bump(&mut self, events: &mut Vec<Event>) -> Result<(), DpbError> {
        let first = self
            .frames
            .iter_mut()
            .filter(|frame| frame.needed_for_output)
            .min_by_key(|frame| frame.poc())
            .ok_or(DpbError::BufferOverflow)?;
        first.needed_for_output = false;
        events.push(Event::Output(first.id));
        self.reap(events);
        Ok(())
    }

    /// C.4.5.1 and C.4.5.2: stores a decoded frame, outputting frames to make
    /// room. A non-reference frame that precedes everything waiting is output
    /// without being stored.
    fn store(&mut self, frame: Frame, events: &mut Vec<Event>) -> Result<(), DpbError> {
        let is_reference = frame.reference != Reference::Unused;
        while self.frames.len() >= self.seq.capacity {
            if !is_reference && frame.needed_for_output {
                let first_waiting = self
                    .frames
                    .iter()
                    .filter(|other| other.needed_for_output)
                    .map(Frame::poc)
                    .min();
                if first_waiting.is_none_or(|waiting| frame.poc() < waiting) {
                    events.push(Event::Output(frame.id));
                    events.push(Event::Release(frame.id));
                    return Ok(());
                }
            }
            self.bump(events)?;
        }
        self.frames.push(frame);
        if let Some(reorder) = self.seq.num_reorder_frames {
            while self
                .frames
                .iter()
                .filter(|other| other.needed_for_output)
                .count()
                > reorder
            {
                self.bump(events)?;
            }
        }
        Ok(())
    }

    /// Outputs every waiting frame in output order, for an IDR picture or
    /// memory management operation 5 (C.4.4), and releases what is no longer
    /// a reference.
    fn flush(&mut self, output: bool, events: &mut Vec<Event>) {
        if output {
            let mut waiting: Vec<&mut Frame> = self
                .frames
                .iter_mut()
                .filter(|frame| frame.needed_for_output)
                .collect();
            waiting.sort_by_key(|frame| frame.poc());
            for frame in waiting {
                frame.needed_for_output = false;
                events.push(Event::Output(frame.id));
            }
        } else {
            for frame in &mut self.frames {
                frame.needed_for_output = false;
            }
        }
        self.reap(events);
    }

    /// Outputs every frame still waiting, at the end of the stream.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.flush(true, &mut events);
        events
    }

    /// Finishes the picture: marks its references (8.2.5), applies the IDR
    /// and memory management operation 5 output rules (C.4.4) and stores it
    /// or outputs it (C.4.5).
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when the marking is invalid for the buffer. The
    /// buffer then still holds the frames it held before and the picture is
    /// dropped.
    pub fn finish_picture(&mut self) -> Result<Vec<Event>, DpbError> {
        let mut current = self.current.take().ok_or(DpbError::NoPicture)?;
        let snapshot = (self.frames.clone(), self.max_long_term_idx, self.previous);
        match self.finish(&mut current) {
            Ok(events) => Ok(events),
            Err(error) => {
                (self.frames, self.max_long_term_idx, self.previous) = snapshot;
                Err(error)
            }
        }
    }

    fn finish(&mut self, current: &mut Current) -> Result<Vec<Event>, DpbError> {
        let mut events = Vec::new();
        let current_frame_num = current.header.frame_num;
        let mut flush = None;
        let mut mmco5 = false;
        match &current.header.marking {
            None => {}
            Some(Marking::Idr {
                no_output_of_prior_pics,
                long_term_reference,
            }) => {
                for frame in &mut self.frames {
                    frame.reference = Reference::Unused;
                }
                flush = Some(!no_output_of_prior_pics);
                if *long_term_reference {
                    current.frame.reference = Reference::LongTerm(0);
                    self.max_long_term_idx = Some(0);
                } else {
                    current.frame.reference = Reference::ShortTerm;
                    self.max_long_term_idx = None;
                }
            }
            Some(Marking::SlidingWindow) => {
                Self::sliding_window(&self.seq, &mut self.frames, current_frame_num)?;
                current.frame.reference = Reference::ShortTerm;
            }
            Some(Marking::Adaptive(operations)) => {
                current.frame.reference = Reference::ShortTerm;
                for operation in operations {
                    mmco5 |= self.apply_mmco(
                        *operation,
                        current_frame_num,
                        &mut current.frame.reference,
                    )?;
                }
            }
        }
        if mmco5 {
            // 8.2.1: the picture is inferred to have had frame_num 0 and its
            // counts are made relative to itself.
            current.frame.frame_num = 0;
            let base = current.frame.poc();
            current.frame.top -= base;
            current.frame.bottom -= base;
            flush = Some(true);
        }
        if let Some(output) = flush {
            self.flush(output, &mut events);
        }
        self.reap(&mut events);
        let references = self
            .frames
            .iter()
            .filter(|frame| frame.reference != Reference::Unused)
            .count()
            + usize::from(current.frame.reference != Reference::Unused);
        let limit = self.seq.max_num_ref_frames.max(1);
        if references > limit {
            return Err(DpbError::TooManyReferences { references, limit });
        }
        self.update_previous(current, mmco5);
        let stored = current.frame.clone();
        self.store(stored, &mut events)?;
        Ok(events)
    }

    /// Applies an operation to the marking; returns whether it was operation 5.
    fn apply_mmco(
        &mut self,
        operation: Mmco,
        current_frame_num: u32,
        current: &mut Reference,
    ) -> Result<bool, DpbError> {
        let seq = &self.seq;
        match operation {
            Mmco::ShortTermUnused {
                difference_of_pic_nums_minus1,
            } => {
                let target = Self::short_term_target(
                    seq,
                    &self.frames,
                    current_frame_num,
                    difference_of_pic_nums_minus1,
                )?;
                self.frames[target].reference = Reference::Unused;
            }
            Mmco::LongTermUnused { long_term_pic_num } => {
                let frame = self
                    .frames
                    .iter_mut()
                    .find(|frame| frame.reference == Reference::LongTerm(long_term_pic_num))
                    .ok_or(DpbError::InvalidMmco("no such long-term frame"))?;
                frame.reference = Reference::Unused;
            }
            Mmco::ShortToLong {
                difference_of_pic_nums_minus1,
                long_term_frame_idx,
            } => {
                self.check_long_term_idx(long_term_frame_idx)?;
                let target = Self::short_term_target(
                    seq,
                    &self.frames,
                    current_frame_num,
                    difference_of_pic_nums_minus1,
                )?;
                for frame in &mut self.frames {
                    if frame.reference == Reference::LongTerm(long_term_frame_idx) {
                        frame.reference = Reference::Unused;
                    }
                }
                self.frames[target].reference = Reference::LongTerm(long_term_frame_idx);
            }
            Mmco::MaxLongTermIdx {
                max_long_term_frame_idx_plus1,
            } => {
                self.max_long_term_idx = max_long_term_frame_idx_plus1.checked_sub(1);
                let max = self.max_long_term_idx;
                for frame in &mut self.frames {
                    if let Reference::LongTerm(idx) = frame.reference
                        && max.is_none_or(|max| idx > max)
                    {
                        frame.reference = Reference::Unused;
                    }
                }
            }
            Mmco::AllUnused => {
                for frame in &mut self.frames {
                    frame.reference = Reference::Unused;
                }
                self.max_long_term_idx = None;
                return Ok(true);
            }
            Mmco::CurrentToLong {
                long_term_frame_idx,
            } => {
                self.check_long_term_idx(long_term_frame_idx)?;
                for frame in &mut self.frames {
                    if frame.reference == Reference::LongTerm(long_term_frame_idx) {
                        frame.reference = Reference::Unused;
                    }
                }
                *current = Reference::LongTerm(long_term_frame_idx);
            }
        }
        Ok(false)
    }

    const fn check_long_term_idx(&self, idx: u32) -> Result<(), DpbError> {
        match self.max_long_term_idx {
            Some(max) if idx <= max => Ok(()),
            _ => Err(DpbError::InvalidMmco(
                "long_term_frame_idx exceeds MaxLongTermFrameIdx",
            )),
        }
    }

    /// The index of the short-term frame `picNumX` names (8.2.5.4.1).
    fn short_term_target(
        seq: &SequenceParams,
        frames: &[Frame],
        current_frame_num: u32,
        difference_of_pic_nums_minus1: u32,
    ) -> Result<usize, DpbError> {
        let pic_num_x =
            i64::from(current_frame_num) - (i64::from(difference_of_pic_nums_minus1) + 1);
        frames
            .iter()
            .position(|frame| {
                frame.is_short_term()
                    && Self::frame_num_wrap(seq, frame.frame_num, current_frame_num) == pic_num_x
            })
            .ok_or(DpbError::InvalidMmco("no such short-term frame"))
    }

    fn update_previous(&mut self, current: &Current, mmco5: bool) {
        let previous = &mut self.previous;
        previous.started = true;
        previous.frame_num = if mmco5 { 0 } else { current.header.frame_num };
        previous.frame_num_offset = if mmco5 { 0 } else { current.frame_num_offset };
        if current.header.nal_ref_idc != 0 {
            previous.ref_frame_num = previous.frame_num;
            if mmco5 {
                previous.poc_msb = 0;
                previous.poc_lsb = i64::from(current.frame.top);
            } else {
                previous.poc_msb = current.poc_msb;
                previous.poc_lsb = i64::from(current.header.poc_lsb);
            }
        }
    }

    /// 8.2.4: the reference picture lists of a slice of the current picture,
    /// each as long as `num_ref_idx_active` with `None` for "no reference
    /// picture".
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when a modification names a frame that is not
    /// a reference.
    pub fn reference_lists(
        &self,
        kind: ListKind,
        num_active: [usize; 2],
        modifications: [&[Modification]; 2],
    ) -> Result<[Vec<Option<FrameId>>; 2], DpbError> {
        let current = self.current.as_ref().ok_or(DpbError::NoPicture)?;
        let [initial0, initial1] = self.initial_lists(current, kind);
        let list0 = self.modify_list(current, initial0, num_active[0], modifications[0])?;
        let list1 = match kind {
            ListKind::P => Vec::new(),
            ListKind::B => self.modify_list(current, initial1, num_active[1], modifications[1])?,
        };
        Ok([list0, list1])
    }

    /// 8.2.4.2: the initial lists, before they are cut to `num_ref_idx_active`.
    fn initial_lists(&self, current: &Current, kind: ListKind) -> [Vec<FrameId>; 2] {
        let current_frame_num = current.header.frame_num;
        let mut long_terms: Vec<(u32, FrameId)> = self
            .frames
            .iter()
            .filter_map(|frame| match frame.reference {
                Reference::LongTerm(idx) => Some((idx, frame.id)),
                _ => None,
            })
            .collect();
        long_terms.sort_by_key(|&(idx, _)| idx);
        let long_terms = long_terms.into_iter().map(|(_, id)| id);
        let shorts = self.frames.iter().filter(|frame| frame.is_short_term());
        match kind {
            ListKind::P => {
                let mut shorts: Vec<&Frame> = shorts.collect();
                shorts.sort_by_key(|frame| {
                    std::cmp::Reverse(Self::frame_num_wrap(
                        &self.seq,
                        frame.frame_num,
                        current_frame_num,
                    ))
                });
                let list = shorts
                    .into_iter()
                    .map(|frame| frame.id)
                    .chain(long_terms)
                    .collect();
                [list, Vec::new()]
            }
            ListKind::B => {
                // Inferred frames have no order count of their own with
                // type 0 and are not used for B prediction.
                let exclude_inferred = matches!(self.seq.poc, PocType::Zero { .. });
                let current_poc = current.frame.poc();
                let shorts: Vec<&Frame> = shorts
                    .filter(|frame| !(exclude_inferred && frame.non_existing))
                    .collect();
                let mut before: Vec<&Frame> = shorts
                    .iter()
                    .copied()
                    .filter(|frame| frame.poc() < current_poc)
                    .collect();
                before.sort_by_key(|frame| std::cmp::Reverse(frame.poc()));
                let mut after: Vec<&Frame> = shorts
                    .iter()
                    .copied()
                    .filter(|frame| frame.poc() >= current_poc)
                    .collect();
                after.sort_by_key(|frame| frame.poc());
                let long_terms: Vec<FrameId> = long_terms.collect();
                let list0: Vec<FrameId> = before
                    .iter()
                    .chain(&after)
                    .map(|frame| frame.id)
                    .chain(long_terms.iter().copied())
                    .collect();
                let mut list1: Vec<FrameId> = after
                    .iter()
                    .chain(&before)
                    .map(|frame| frame.id)
                    .chain(long_terms.iter().copied())
                    .collect();
                if list1.len() > 1 && list1 == list0 {
                    list1.swap(0, 1);
                }
                [list0, list1]
            }
        }
    }

    /// 8.2.4.2 and 8.2.4.3: cuts or pads an initial list and applies the
    /// slice's modification operations.
    fn modify_list(
        &self,
        current: &Current,
        mut list: Vec<FrameId>,
        num_active: usize,
        modifications: &[Modification],
    ) -> Result<Vec<Option<FrameId>>, DpbError> {
        if !(1..=32).contains(&num_active) {
            return Err(DpbError::OutOfRange {
                field: "num_ref_idx_active",
                value: i64::try_from(num_active).unwrap_or(i64::MAX),
            });
        }
        if modifications.len() > num_active {
            return Err(DpbError::TooManyModifications);
        }
        list.truncate(num_active);
        let mut list: Vec<Option<FrameId>> = list.into_iter().map(Some).collect();
        list.resize(num_active, None);
        let max_pic_num = self.seq.max_frame_num();
        let current_pic_num = i64::from(current.header.frame_num);
        let mut prediction = current_pic_num;
        for (index, modification) in modifications.iter().enumerate() {
            let target = match *modification {
                Modification::Subtract {
                    abs_diff_pic_num_minus1,
                }
                | Modification::Add {
                    abs_diff_pic_num_minus1,
                } => {
                    let difference = i64::from(abs_diff_pic_num_minus1) + 1;
                    if difference > max_pic_num {
                        return Err(DpbError::OutOfRange {
                            field: "abs_diff_pic_num_minus1",
                            value: i64::from(abs_diff_pic_num_minus1),
                        });
                    }
                    let no_wrap = if matches!(modification, Modification::Subtract { .. }) {
                        if prediction - difference < 0 {
                            prediction - difference + max_pic_num
                        } else {
                            prediction - difference
                        }
                    } else if prediction + difference >= max_pic_num {
                        prediction + difference - max_pic_num
                    } else {
                        prediction + difference
                    };
                    prediction = no_wrap;
                    let pic_num = if no_wrap > current_pic_num {
                        no_wrap - max_pic_num
                    } else {
                        no_wrap
                    };
                    self.frames
                        .iter()
                        .find(|frame| {
                            frame.is_short_term()
                                && Self::frame_num_wrap(
                                    &self.seq,
                                    frame.frame_num,
                                    current.header.frame_num,
                                ) == pic_num
                        })
                        .ok_or(DpbError::MissingReference(pic_num))?
                        .id
                }
                Modification::LongTerm { long_term_pic_num } => {
                    self.frames
                        .iter()
                        .find(|frame| frame.reference == Reference::LongTerm(long_term_pic_num))
                        .ok_or_else(|| DpbError::MissingReference(i64::from(long_term_pic_num)))?
                        .id
                }
            };
            list.insert(index, Some(target));
            if let Some(duplicate) = list[index + 1..]
                .iter()
                .position(|entry| *entry == Some(target))
            {
                list.remove(index + 1 + duplicate);
            }
            list.truncate(num_active);
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests;
