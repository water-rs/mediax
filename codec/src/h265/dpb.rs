//! The decoded picture buffer of H.265 streams.
//!
//! This is the picture management of ITU-T H.265 clause 8.3 and the output
//! order conformance of annex C.5.2: picture order count (8.3.1), the
//! reference picture set (8.3.2), reference list construction (8.3.4) and the
//! "bumping" process that outputs pictures (C.5.2.2 to C.5.2.4). It knows
//! nothing of the decoder that consumes it: pictures are identified by a
//! [`FrameId`], and what the caller must do with a frame is reported as an
//! [`Event`].
//!
//! All temporal sub-layers are decoded, so `HighestTid` is the highest
//! sub-layer of the sequence. Pictures a stream needs but lost are an error
//! rather than being generated (8.3.3), except for the pictures only the
//! skipped leading pictures of a random access point refer to, which are
//! never needed.

use super::{error::DpbError, nal::NalUnitType};

/// The most pictures the buffer holds (`MaxDpbSize`).
pub const MAX_DPB_SIZE: usize = 16;

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

/// What picture management needs of a sequence parameter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceParams {
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4`.
    pub log2_max_poc_lsb: u8,
    /// `sps_max_dec_pic_buffering_minus1 + 1` of the highest sub-layer: the
    /// frames the buffer holds.
    pub max_dec_pic_buffering: usize,
    /// `sps_max_num_reorder_pics` of the highest sub-layer.
    pub max_num_reorder: usize,
    /// `sps_max_latency_increase_plus1` of the highest sub-layer.
    pub max_latency_increase_plus1: u32,
}

impl SequenceParams {
    /// `SpsMaxLatencyPictures`, when the sequence limits the latency.
    fn max_latency_pictures(&self) -> Option<u64> {
        let plus1 = u64::from(self.max_latency_increase_plus1);
        (plus1 != 0).then(|| {
            u64::try_from(self.max_num_reorder).expect("a usize fits in 64 bits") + (plus1 - 1)
        })
    }

    const fn max_poc_lsb(&self) -> i64 {
        1_i64 << self.log2_max_poc_lsb
    }
}

/// One picture of a short-term reference picture set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpsEntry {
    /// The order count relative to the current picture.
    pub delta_poc: i32,
    /// Whether the current picture may refer to it.
    pub used_by_curr: bool,
}

/// A short-term reference picture set (7.4.8), the pictures nearest to the
/// current one first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortTermRps {
    /// The pictures that precede the current one in output order.
    pub negative: Vec<RpsEntry>,
    /// The pictures that follow it.
    pub positive: Vec<RpsEntry>,
}

impl ShortTermRps {
    /// The pictures the current picture may refer to.
    #[must_use]
    pub fn used_count(&self) -> usize {
        self.negative
            .iter()
            .chain(&self.positive)
            .filter(|entry| entry.used_by_curr)
            .count()
    }
}

/// One long-term entry of a slice header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongTermRef {
    /// `PocLsbLt`.
    pub poc_lsb: u32,
    /// `DeltaPocMsbCycleLt` (7-52): the running total, which an entry
    /// without `delta_poc_msb_present_flag` carries on unchanged.
    pub msb_cycle: u32,
    /// `delta_poc_msb_present_flag`: the picture is named by its full order
    /// count rather than by the LSBs.
    pub msb_present: bool,
    /// `UsedByCurrPicLt`.
    pub used_by_curr: bool,
}

/// The reference picture set a slice header codes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferencePictureSet {
    /// The short-term pictures.
    pub short_term: ShortTermRps,
    /// The long-term pictures.
    pub long_term: Vec<LongTermRef>,
}

impl ReferencePictureSet {
    /// `NumPicTotalCurr`.
    #[must_use]
    pub fn num_pic_total_curr(&self) -> usize {
        self.short_term.used_count()
            + self
                .long_term
                .iter()
                .filter(|entry| entry.used_by_curr)
                .count()
    }
}

/// The slice header fields that picture management reads, of the first slice
/// segment of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureHeader {
    /// `nal_unit_type`.
    pub nal_unit_type: NalUnitType,
    /// `TemporalId`.
    pub temporal_id: u8,
    /// `slice_pic_order_cnt_lsb`, 0 for an IDR picture.
    pub poc_lsb: u32,
    /// `no_output_of_prior_pics_flag`, 0 unless the picture is an IRAP
    /// picture.
    pub no_output_of_prior_pics: bool,
    /// `pic_output_flag`.
    pub pic_output: bool,
    /// The reference picture set, empty for an IDR picture.
    pub rps: ReferencePictureSet,
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
    /// `PicOrderCntVal`.
    pub poc: i32,
    /// Whether the frame is marked as used for long-term reference.
    pub long_term: bool,
}

/// The pictures of a reference picture set the current picture refers to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpsLists {
    /// `RefPicSetStCurrBefore`.
    pub st_curr_before: Vec<FrameId>,
    /// `RefPicSetStCurrAfter`.
    pub st_curr_after: Vec<FrameId>,
    /// `RefPicSetLtCurr`.
    pub lt_curr: Vec<FrameId>,
}

impl RpsLists {
    const fn total(&self) -> usize {
        self.st_curr_before.len() + self.st_curr_after.len() + self.lt_curr.len()
    }
}

/// The result of starting a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Began {
    /// The frame the picture is decoded into.
    pub id: FrameId,
    /// What to do with the frames the picture's arrival output or released.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    Unused,
    ShortTerm,
    LongTerm,
}

#[derive(Debug, Clone)]
struct Frame {
    id: FrameId,
    poc: i32,
    reference: Reference,
    needed_for_output: bool,
    /// `PicLatencyCount`.
    latency: u64,
}

impl Frame {
    const fn view(&self) -> FrameView {
        FrameView {
            id: self.id,
            poc: self.poc,
            long_term: matches!(self.reference, Reference::LongTerm),
        }
    }

    const fn is_reference(&self) -> bool {
        !matches!(self.reference, Reference::Unused)
    }
}

#[derive(Debug)]
struct Current {
    frame: Frame,
    header: PictureHeader,
    /// `PicOrderCntMsb`.
    poc_msb: i64,
    sets: RpsLists,
}

/// The reference picture set matched against the buffer.
struct Derived {
    marking: Vec<Reference>,
    sets: RpsLists,
}

/// The decoded picture buffer.
#[derive(Debug)]
pub struct Dpb {
    seq: SequenceParams,
    frames: Vec<Frame>,
    current: Option<Current>,
    next_id: u64,
    /// No picture was decoded since the start or the last drain.
    first: bool,
    /// `NoRaslOutputFlag` of the last IRAP picture.
    irap_no_rasl_output: bool,
    /// `slice_pic_order_cnt_lsb` and `PicOrderCntMsb` of `prevTid0Pic`.
    prev_tid0: Option<(i64, i64)>,
}

fn to_i32(field: &'static str, value: i64) -> Result<i32, DpbError> {
    i32::try_from(value).map_err(|_| DpbError::OutOfRange { field, value })
}

impl Dpb {
    /// Creates an empty buffer for a sequence.
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when the parameters are out of range.
    pub fn new(seq: SequenceParams) -> Result<Self, DpbError> {
        if seq.max_dec_pic_buffering == 0 || seq.max_dec_pic_buffering > MAX_DPB_SIZE {
            return Err(DpbError::OutOfRange {
                field: "sps_max_dec_pic_buffering_minus1",
                value: i64::try_from(seq.max_dec_pic_buffering).expect("a usize fits in 63 bits")
                    - 1,
            });
        }
        if seq.max_num_reorder >= seq.max_dec_pic_buffering {
            return Err(DpbError::BufferTooSmall {
                needed: seq.max_num_reorder + 1,
                capacity: seq.max_dec_pic_buffering,
            });
        }
        if !(4..=16).contains(&seq.log2_max_poc_lsb) {
            return Err(DpbError::OutOfRange {
                field: "log2_max_pic_order_cnt_lsb",
                value: i64::from(seq.log2_max_poc_lsb),
            });
        }
        Ok(Self {
            seq,
            frames: Vec::new(),
            current: None,
            next_id: 0,
            first: true,
            irap_no_rasl_output: false,
            prev_tid0: None,
        })
    }

    /// The frame being decoded.
    #[must_use]
    pub fn current(&self) -> Option<FrameView> {
        self.current.as_ref().map(|current| current.frame.view())
    }

    /// The pictures of the reference picture set of the frame being decoded.
    #[must_use]
    pub fn current_sets(&self) -> Option<&RpsLists> {
        self.current.as_ref().map(|current| &current.sets)
    }

    /// Looks a frame up, the one being decoded included.
    #[cfg(test)]
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
            .filter(|frame| frame.is_reference())
            .map(Frame::view)
    }

    const fn fresh_id(&mut self) -> FrameId {
        let id = FrameId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Whether the prior pictures are discarded without being output when
    /// the picture of `header` starts a sequence (C.5.2.2). A CRA picture
    /// starts a sequence only after the end of the stream was handled, so
    /// nothing is left to discard then.
    #[must_use]
    pub const fn discards_prior_pictures(&self, header: &PictureHeader) -> bool {
        !self.first && header.no_output_of_prior_pics
    }

    /// Whether `kind` starts a coded video sequence here: an IRAP picture
    /// with `NoRaslOutputFlag` equal to 1.
    #[must_use]
    pub const fn starts_sequence(&self, kind: NalUnitType) -> bool {
        kind.is_irap() && (kind.is_idr() || kind.is_bla() || self.first)
    }

    /// Starts a picture: derives its order count, applies its reference
    /// picture set, outputs and removes frames to make room and allocates the
    /// current frame. Returns `None` for a random access skipped leading
    /// picture whose random access point starts the sequence, which is not
    /// decoded.
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when the header is inconsistent with the
    /// sequence or the buffer.
    pub fn begin_picture(&mut self, header: PictureHeader) -> Result<Option<Began>, DpbError> {
        if self.current.is_some() {
            return Err(DpbError::PictureInProgress);
        }
        let kind = header.nal_unit_type;
        let irap = kind.is_irap();
        if !irap && self.first {
            return Err(DpbError::InconsistentHeader(
                "a sequence does not start with an IRAP picture",
            ));
        }
        let no_rasl_output = self.starts_sequence(kind);
        if irap {
            self.irap_no_rasl_output = no_rasl_output;
        }
        if kind.is_rasl() && self.irap_no_rasl_output {
            return Ok(None);
        }
        if kind.is_idr() && header.poc_lsb != 0 {
            return Err(DpbError::InconsistentHeader(
                "an IDR picture has a picture order count lsb",
            ));
        }
        let max_lsb = self.seq.max_poc_lsb();
        let lsb = i64::from(header.poc_lsb);
        if lsb >= max_lsb {
            return Err(DpbError::OutOfRange {
                field: "slice_pic_order_cnt_lsb",
                value: lsb,
            });
        }
        let poc_msb = if no_rasl_output {
            0
        } else {
            let (earlier, cycle_start) = self.prev_tid0.ok_or(DpbError::InconsistentHeader(
                "no earlier picture to derive the order count from",
            ))?;
            if lsb < earlier && earlier - lsb >= max_lsb / 2 {
                cycle_start + max_lsb
            } else if lsb > earlier && lsb - earlier > max_lsb / 2 {
                cycle_start - max_lsb
            } else {
                cycle_start
            }
        };
        let poc = to_i32("PicOrderCntVal", poc_msb + lsb)?;
        let derived = self.derive_rps(poc, &header.rps, no_rasl_output)?;
        let mut events = Vec::new();
        self.apply_marking(&derived.marking);
        if no_rasl_output {
            if !self.first {
                let discard = self.discards_prior_pictures(&header);
                self.end_sequence_into(discard, &mut events);
            }
        } else {
            self.reap(&mut events);
            while self.must_bump_before_decoding() {
                self.bump(&mut events)?;
            }
        }
        let id = self.fresh_id();
        self.current = Some(Current {
            frame: Frame {
                id,
                poc,
                reference: Reference::Unused,
                needed_for_output: header.pic_output,
                latency: 0,
            },
            header,
            poc_msb,
            sets: derived.sets,
        });
        Ok(Some(Began { id, events }))
    }

    /// Drops the picture being decoded, as when its decoding failed.
    pub fn abort_picture(&mut self) -> Option<FrameId> {
        self.current.take().map(|current| current.frame.id)
    }

    /// 8.3.2: matches the reference picture set against the buffer. Nothing
    /// changes; the new marking is applied by the caller.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the steps of 8.3.2, each checked where it is made"
    )]
    fn derive_rps(
        &self,
        poc: i32,
        rps: &ReferencePictureSet,
        starts_sequence: bool,
    ) -> Result<Derived, DpbError> {
        let total =
            rps.short_term.negative.len() + rps.short_term.positive.len() + rps.long_term.len();
        if total >= self.seq.max_dec_pic_buffering {
            return Err(DpbError::TooManyReferences {
                references: total,
                limit: self.seq.max_dec_pic_buffering - 1,
            });
        }
        let mut marking: Vec<Reference> = self
            .frames
            .iter()
            .map(|frame| {
                if starts_sequence {
                    Reference::Unused
                } else {
                    frame.reference
                }
            })
            .collect();
        let poc = i64::from(poc);
        let max_lsb = self.seq.max_poc_lsb();
        let mut sets = RpsLists::default();
        let mut keep = vec![false; self.frames.len()];

        // Long-term pictures first: they may be any reference picture.
        for entry in &rps.long_term {
            let matches_full_poc = entry.msb_present;
            let target = if matches_full_poc {
                i64::from(entry.poc_lsb) + poc
                    - i64::from(entry.msb_cycle) * max_lsb
                    - (poc & (max_lsb - 1))
            } else {
                i64::from(entry.poc_lsb)
            };
            let mut found = None;
            for (index, frame) in self.frames.iter().enumerate() {
                if marking[index] == Reference::Unused {
                    continue;
                }
                let frame_poc = i64::from(frame.poc);
                let equal = if matches_full_poc {
                    frame_poc == target
                } else {
                    frame_poc & (max_lsb - 1) == target
                };
                if equal {
                    if found.is_some() {
                        return Err(DpbError::AmbiguousLongTerm(target));
                    }
                    found = Some(index);
                }
            }
            match found {
                Some(index) => {
                    marking[index] = Reference::LongTerm;
                    keep[index] = true;
                    if entry.used_by_curr {
                        sets.lt_curr.push(self.frames[index].id);
                    }
                }
                None if entry.used_by_curr => return Err(DpbError::MissingReference(target)),
                // A picture only a skipped or dropped picture refers to.
                None => {}
            }
        }

        let short_term = |delta: i32| -> Option<usize> {
            let target = poc + i64::from(delta);
            self.frames.iter().enumerate().position(|(index, frame)| {
                marking[index] == Reference::ShortTerm && i64::from(frame.poc) == target
            })
        };
        let mut found_short_term = Vec::new();
        for (entries, before) in [
            (&rps.short_term.negative, true),
            (&rps.short_term.positive, false),
        ] {
            for entry in entries {
                match short_term(entry.delta_poc) {
                    Some(index) => {
                        found_short_term.push(index);
                        if entry.used_by_curr {
                            let id = self.frames[index].id;
                            if before {
                                sets.st_curr_before.push(id);
                            } else {
                                sets.st_curr_after.push(id);
                            }
                        }
                    }
                    None if entry.used_by_curr => {
                        return Err(DpbError::MissingReference(poc + i64::from(entry.delta_poc)));
                    }
                    None => {}
                }
            }
        }
        for index in found_short_term {
            keep[index] = true;
        }
        for (mark, keep) in marking.iter_mut().zip(&keep) {
            if !keep {
                *mark = Reference::Unused;
            }
        }
        Ok(Derived { marking, sets })
    }

    fn apply_marking(&mut self, marking: &[Reference]) {
        for (frame, &mark) in self.frames.iter_mut().zip(marking) {
            frame.reference = mark;
        }
    }

    /// Releases the frames that are neither references nor waiting for
    /// output.
    fn reap(&mut self, events: &mut Vec<Event>) {
        self.frames.retain(|frame| {
            let keep = frame.is_reference() || frame.needed_for_output;
            if !keep {
                events.push(Event::Release(frame.id));
            }
            keep
        });
    }

    fn waiting(&self) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame.needed_for_output)
            .count()
    }

    fn exceeds_reorder_or_latency(&self) -> bool {
        self.waiting() > self.seq.max_num_reorder
            || self.seq.max_latency_pictures().is_some_and(|limit| {
                self.frames
                    .iter()
                    .any(|frame| frame.needed_for_output && frame.latency >= limit)
            })
    }

    /// C.5.2.2: whether a frame is output before the picture is decoded.
    fn must_bump_before_decoding(&self) -> bool {
        self.exceeds_reorder_or_latency() || self.frames.len() >= self.seq.max_dec_pic_buffering
    }

    /// C.5.2.4: outputs the waiting frame that is first in output order.
    fn bump(&mut self, events: &mut Vec<Event>) -> Result<(), DpbError> {
        let first = self
            .frames
            .iter_mut()
            .filter(|frame| frame.needed_for_output)
            .min_by_key(|frame| frame.poc)
            .ok_or(DpbError::BufferOverflow)?;
        first.needed_for_output = false;
        events.push(Event::Output(first.id));
        self.reap(events);
        Ok(())
    }

    /// Empties the buffer at the start of a sequence, outputting what waits
    /// unless `discard`.
    fn end_sequence_into(&mut self, discard: bool, events: &mut Vec<Event>) {
        if !discard {
            let mut waiting: Vec<&Frame> = self
                .frames
                .iter()
                .filter(|frame| frame.needed_for_output)
                .collect();
            waiting.sort_by_key(|frame| frame.poc);
            events.extend(waiting.iter().map(|frame| Event::Output(frame.id)));
        }
        events.extend(self.frames.drain(..).map(|frame| Event::Release(frame.id)));
    }

    /// Ends the stream: outputs every frame still waiting, in output order,
    /// unless `discard`, and empties the buffer. The next picture must be an
    /// IRAP picture and starts a new sequence.
    pub fn end_sequence(&mut self, discard: bool) -> Vec<Event> {
        let mut events = Vec::new();
        self.end_sequence_into(discard, &mut events);
        self.first = true;
        self.prev_tid0 = None;
        events
    }

    /// Outputs every frame still waiting, at the end of the stream.
    pub fn drain(&mut self) -> Vec<Event> {
        self.end_sequence(false)
    }

    /// Finishes the picture (C.5.2.3): stores it as a short-term reference
    /// and outputs the frames the output order rules ask for.
    ///
    /// # Errors
    ///
    /// Returns [`DpbError::NoPicture`] when no picture is being decoded.
    pub fn finish_picture(&mut self) -> Result<Vec<Event>, DpbError> {
        let Current {
            mut frame,
            header,
            poc_msb,
            ..
        } = self.current.take().ok_or(DpbError::NoPicture)?;
        let kind = header.nal_unit_type;
        if header.temporal_id == 0
            && !kind.is_rasl()
            && !kind.is_radl()
            && !kind.is_sub_layer_non_reference()
        {
            self.prev_tid0 = Some((i64::from(header.poc_lsb), poc_msb));
        }
        self.first = false;
        let mut events = Vec::new();
        if frame.needed_for_output {
            for other in &mut self.frames {
                if other.needed_for_output && other.poc > frame.poc {
                    other.latency += 1;
                }
            }
        }
        frame.reference = Reference::ShortTerm;
        self.frames.push(frame);
        while self.exceeds_reorder_or_latency() {
            self.bump(&mut events)?;
        }
        Ok(events)
    }

    /// 8.3.4: the initial reference picture lists of the current picture,
    /// cut to `num_active` entries and reordered by the `list_entry` values
    /// of `modifications`.
    ///
    /// # Errors
    ///
    /// Returns a [`DpbError`] when there is no picture to refer to or an
    /// entry is out of range.
    pub fn reference_lists(
        &self,
        kind: ListKind,
        num_active: [usize; 2],
        modifications: [Option<&[u32]>; 2],
    ) -> Result<[Vec<FrameId>; 2], DpbError> {
        let sets = self
            .current
            .as_ref()
            .map(|current| &current.sets)
            .ok_or(DpbError::NoPicture)?;
        let total = sets.total();
        if total == 0 {
            return Err(DpbError::EmptyReferenceSet);
        }
        let build =
            |first: &[FrameId], second: &[FrameId], count: usize, entries: Option<&[u32]>| {
                let temporary: Vec<FrameId> = first
                    .iter()
                    .chain(second)
                    .chain(&sets.lt_curr)
                    .copied()
                    .cycle()
                    .take(count.max(total))
                    .collect();
                (0..count)
                    .map(|index| {
                        let position = match entries {
                            Some(entries) => {
                                let entry =
                                    entries.get(index).copied().ok_or(DpbError::ListEntry {
                                        entry: u32::MAX,
                                        len: total,
                                    })?;
                                let position = usize::try_from(entry)
                                    .map_err(|_| DpbError::ListEntry { entry, len: total })?;
                                if position >= total {
                                    return Err(DpbError::ListEntry { entry, len: total });
                                }
                                position
                            }
                            None => index,
                        };
                        Ok(temporary[position])
                    })
                    .collect::<Result<Vec<_>, DpbError>>()
            };
        let list0 = build(
            &sets.st_curr_before,
            &sets.st_curr_after,
            num_active[0],
            modifications[0],
        )?;
        let list1 = match kind {
            ListKind::P => Vec::new(),
            ListKind::B => build(
                &sets.st_curr_after,
                &sets.st_curr_before,
                num_active[1],
                modifications[1],
            )?,
        };
        Ok([list0, list1])
    }
}

#[cfg(test)]
mod tests;
