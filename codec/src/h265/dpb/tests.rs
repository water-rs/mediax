use super::*;
use std::collections::HashMap;

const TRAIL_R: u8 = 1;
const TRAIL_N: u8 = 0;
const RADL_R: u8 = 7;
const RASL_R: u8 = 9;
const BLA_W_LP: u8 = 16;
const IDR_W_RADL: u8 = 19;
const CRA: u8 = 21;

fn params(log2_lsb: u8, buffering: usize, reorder: usize, latency_plus1: u32) -> SequenceParams {
    SequenceParams {
        log2_max_poc_lsb: log2_lsb,
        max_dec_pic_buffering: buffering,
        max_num_reorder: reorder,
        max_latency_increase_plus1: latency_plus1,
    }
}

fn st(negative: &[(i32, bool)], positive: &[(i32, bool)]) -> ReferencePictureSet {
    let entries = |list: &[(i32, bool)]| {
        list.iter()
            .map(|&(delta_poc, used_by_curr)| RpsEntry {
                delta_poc,
                used_by_curr,
            })
            .collect()
    };
    ReferencePictureSet {
        short_term: ShortTermRps {
            negative: entries(negative),
            positive: entries(positive),
        },
        long_term: Vec::new(),
    }
}

fn header(kind: u8, poc_lsb: u32, rps: ReferencePictureSet) -> PictureHeader {
    PictureHeader {
        nal_unit_type: NalUnitType(kind),
        temporal_id: 0,
        poc_lsb,
        no_output_of_prior_pics: false,
        pic_output: true,
        rps,
    }
}

/// A buffer and the record of what it reported.
struct Run {
    dpb: Dpb,
    pocs: HashMap<FrameId, i32>,
    /// The order counts of the frames output, in order.
    output: Vec<i32>,
    released: Vec<FrameId>,
}

impl Run {
    fn new(seq: SequenceParams) -> Self {
        Self {
            dpb: Dpb::new(seq).unwrap(),
            pocs: HashMap::new(),
            output: Vec::new(),
            released: Vec::new(),
        }
    }

    fn apply(&mut self, events: &[Event]) {
        for event in events {
            match *event {
                Event::Output(id) => self.output.push(self.pocs[&id]),
                Event::Release(id) => self.released.push(id),
            }
        }
    }

    /// Starts a picture, returning its id and the events of the start.
    fn begin(&mut self, header: PictureHeader) -> Result<Option<(FrameId, Vec<Event>)>, DpbError> {
        let Some(began) = self.dpb.begin_picture(header)? else {
            return Ok(None);
        };
        self.pocs.insert(began.id, self.dpb.current().unwrap().poc);
        self.apply(&began.events);
        Ok(Some((began.id, began.events)))
    }

    fn finish(&mut self) -> Vec<Event> {
        let events = self.dpb.finish_picture().unwrap();
        self.apply(&events);
        events
    }

    /// Decodes a whole picture and returns its order count.
    fn decode(&mut self, header: PictureHeader) -> i32 {
        let (id, _) = self.begin(header).unwrap().unwrap();
        let poc = self.pocs[&id];
        self.finish();
        poc
    }

    fn drain(&mut self) {
        let events = self.dpb.drain();
        self.apply(&events);
    }
}

fn idr() -> PictureHeader {
    header(IDR_W_RADL, 0, ReferencePictureSet::default())
}

#[test]
fn order_counts_wrap_up_and_down() {
    let mut run = Run::new(params(4, 3, 0, 0));
    assert_eq!(run.decode(idr()), 0);
    let mut previous = 0;
    for lsb in [4, 8, 12] {
        let rps = st(&[(-(lsb - previous), true)], &[]);
        assert_eq!(run.decode(header(TRAIL_R, lsb.cast_unsigned(), rps)), lsb);
        previous = lsb;
    }
    // lsb 0 after 12: the most significant part advances.
    assert_eq!(run.decode(header(TRAIL_R, 0, st(&[(-4, true)], &[]))), 16);
    // lsb 14 after lsb 0 (order count 16): it goes back.
    assert_eq!(
        run.decode(header(TRAIL_R, 14, st(&[(-2, true)], &[(2, false)]))),
        14
    );
    // lsb 1 after lsb 14 advances again.
    assert_eq!(run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[]))), 17);
}

#[test]
fn a_leading_picture_before_the_previous_one_goes_back_a_cycle() {
    let mut run = Run::new(params(4, 3, 2, 0));
    run.decode(idr());
    // lsb 10 is more than half a cycle after lsb 0: order count -6.
    let poc = run.decode(header(TRAIL_R, 10, ReferencePictureSet::default()));
    assert_eq!(poc, -6);
}

#[test]
fn an_order_count_outside_i32_is_an_error() {
    let mut run = Run::new(params(16, 2, 0, 0));
    run.decode(idr());
    // Each step of 32768 advances the cycle by one; the count overflows
    // i32 only after 65536 steps, so the check is on the lsb.
    let error = run
        .dpb
        .begin_picture(header(TRAIL_R, 1 << 16, ReferencePictureSet::default()))
        .unwrap_err();
    assert!(matches!(
        error,
        DpbError::OutOfRange {
            field: "slice_pic_order_cnt_lsb",
            ..
        }
    ));
}

#[test]
fn an_idr_picture_resets_the_count_and_unmarks_everything() {
    let mut run = Run::new(params(8, 4, 2, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[])));
    run.decode(header(TRAIL_R, 2, st(&[(-1, true), (-2, true)], &[])));
    assert_eq!(run.dpb.reference_frames().count(), 3);
    assert_eq!(run.decode(idr()), 0);
    assert_eq!(run.dpb.reference_frames().count(), 1);
    assert_eq!(run.output, [0, 1, 2]);
    run.drain();
    assert_eq!(run.output, [0, 1, 2, 0]);
}

#[test]
fn the_reference_picture_set_unmarks_pictures_it_leaves_out() {
    let mut run = Run::new(params(8, 4, 0, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[])));
    run.decode(header(TRAIL_R, 2, st(&[(-1, true)], &[])));
    let pocs: Vec<i32> = run.dpb.reference_frames().map(|frame| frame.poc).collect();
    // Pictures 0 and 1: 2 was added after the set of picture 2 was applied.
    assert_eq!(pocs, [1, 2]);
    // A picture that names no reference removes them all.
    run.decode(header(TRAIL_R, 3, ReferencePictureSet::default()));
    let pocs: Vec<i32> = run.dpb.reference_frames().map(|frame| frame.poc).collect();
    assert_eq!(pocs, [3]);
}

#[test]
fn short_term_lists_are_ordered_before_then_after() {
    let mut run = Run::new(params(8, 6, 4, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
    run.decode(header(TRAIL_R, 4, st(&[(-4, true)], &[(4, true)])));
    // Picture 6 sees 0, 4 before it and 8 after; 0 is only kept.
    let (_, _) = run
        .begin(header(
            TRAIL_R,
            6,
            st(&[(-2, true), (-6, false)], &[(2, true)]),
        ))
        .unwrap()
        .unwrap();
    let sets = run.dpb.current_sets().unwrap();
    let poc = |id: &FrameId| run.dpb.frame(*id).unwrap().poc;
    assert_eq!(sets.st_curr_before.iter().map(poc).collect::<Vec<_>>(), [4]);
    assert_eq!(sets.st_curr_after.iter().map(poc).collect::<Vec<_>>(), [8]);
    assert_eq!(sets.lt_curr, []);
    assert_eq!(run.dpb.reference_frames().count(), 3);
}

fn long_term(poc_lsb: u32, msb_cycle: Option<u32>, used_by_curr: bool) -> LongTermRef {
    LongTermRef {
        poc_lsb,
        msb_cycle: msb_cycle.unwrap_or(0),
        msb_present: msb_cycle.is_some(),
        used_by_curr,
    }
}

#[test]
fn a_carried_msb_cycle_names_the_picture_one_cycle_back() {
    // Pictures 0 and 16 share lsb 0; the picture being decoded is 20 (lsb 4).
    // A running total of 1 (as an entry coded with cycle 0 after one coded
    // with cycle 1 has) names the picture one MSB cycle back: picture 0.
    let mut run = Run::new(params(4, 6, 0, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
    run.decode(header(TRAIL_R, 0, st(&[(-8, true), (-16, true)], &[])));
    let mut rps = st(&[(-4, true)], &[]);
    rps.long_term.push(LongTermRef {
        poc_lsb: 0,
        msb_cycle: 1,
        msb_present: true,
        used_by_curr: false,
    });
    let began = run.dpb.begin_picture(header(TRAIL_R, 4, rps));
    assert!(began.is_ok(), "{began:?}");
    let marks: Vec<_> = run
        .dpb
        .reference_frames()
        .map(|frame| (frame.poc, frame.long_term))
        .collect();
    assert!(marks.contains(&(0, true)), "{marks:?}");
    assert!(marks.contains(&(16, false)), "{marks:?}");
}

#[test]
fn long_term_pictures_are_found_by_lsb_or_with_the_msb_cycle() {
    let mut run = Run::new(params(4, 5, 0, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 5, st(&[(-5, true)], &[])));
    // Keep picture 0 as long-term by its lsb (0) and picture 5 short-term.
    let mut rps = st(&[(-1, true)], &[]);
    rps.long_term.push(long_term(0, None, true));
    run.decode(header(TRAIL_R, 6, rps));
    let marks: Vec<(i32, bool)> = run
        .dpb
        .reference_frames()
        .map(|frame| (frame.poc, frame.long_term))
        .collect();
    assert_eq!(marks, [(0, true), (5, false), (6, false)]);
    // Advance the count far enough that lsb 0 is ambiguous without the msb:
    // pictures 16 and 32 carry lsb 0 too, so name picture 0 by cycle.
    let mut previous = 6;
    for poc in [12, 18, 24, 30] {
        let mut rps = st(&[(-(poc - previous), true)], &[]);
        rps.long_term
            .push(long_term(0, Some(u32::try_from(poc / 16).unwrap()), false));
        run.decode(header(TRAIL_R, u32::try_from(poc % 16).unwrap(), rps));
        previous = poc;
    }
    assert!(
        run.dpb
            .reference_frames()
            .any(|frame| frame.poc == 0 && frame.long_term)
    );
}

#[test]
fn an_ambiguous_long_term_picture_is_an_error() {
    let mut run = Run::new(params(4, 6, 0, 0));
    run.decode(idr());
    // Picture 16 shares lsb 0 with picture 0; both stay as short-term.
    run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
    run.decode(header(TRAIL_R, 0, st(&[(-8, true), (-16, true)], &[])));
    // By lsb alone, 0 names both picture 0 and picture 16.
    let mut rps = st(&[(-8, true), (-24, true)], &[]);
    rps.long_term.push(long_term(0, None, false));
    let error = run.dpb.begin_picture(header(TRAIL_R, 8, rps)).unwrap_err();
    assert_eq!(error, DpbError::AmbiguousLongTerm(0));
}

#[test]
fn a_missing_picture_is_an_error_only_when_the_picture_uses_it() {
    let mut run = Run::new(params(8, 4, 0, 0));
    run.decode(idr());
    let error = run
        .dpb
        .begin_picture(header(TRAIL_R, 2, st(&[(-1, true)], &[])))
        .unwrap_err();
    assert_eq!(error, DpbError::MissingReference(1));
    assert!(run.dpb.current().is_none());
    // Not used by the current picture: ignored.
    run.decode(header(TRAIL_R, 2, st(&[(-2, false)], &[])));
}

#[test]
fn too_many_references_are_an_error() {
    let mut run = Run::new(params(8, 2, 0, 0));
    run.decode(idr());
    let error = run
        .dpb
        .begin_picture(header(TRAIL_R, 2, st(&[(-1, false), (-2, false)], &[])))
        .unwrap_err();
    assert!(matches!(error, DpbError::TooManyReferences { .. }));
}

#[test]
fn hierarchical_b_pictures_are_output_in_order() {
    let mut run = Run::new(params(8, 5, 2, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 4, st(&[(-4, true)], &[])));
    assert_eq!(run.output, []);
    run.decode(header(TRAIL_R, 2, st(&[(-2, true)], &[(2, true)])));
    assert_eq!(run.output, [0]);
    run.decode(header(
        TRAIL_N,
        1,
        st(&[(-1, true)], &[(1, true), (3, false)]),
    ));
    assert_eq!(run.output, [0, 1]);
    run.decode(header(TRAIL_N, 3, st(&[(-1, true)], &[(1, true)])));
    assert_eq!(run.output, [0, 1, 2]);
    run.drain();
    assert_eq!(run.output, [0, 1, 2, 3, 4]);
}

#[test]
fn a_full_buffer_outputs_before_the_next_picture() {
    let mut run = Run::new(params(8, 2, 1, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 2, st(&[(-2, true)], &[])));
    assert_eq!(run.output, [0]);
    // Picture 1 keeps only picture 0, so picture 2 would be dropped while
    // waiting for output: the full buffer outputs it first.
    let (_, events) = run
        .begin(header(TRAIL_R, 1, st(&[(-1, true)], &[])))
        .unwrap()
        .unwrap();
    assert_eq!(run.output, [0, 2]);
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], Event::Output(_)));
    assert!(matches!(events[1], Event::Release(_)));
}

#[test]
fn the_latency_limit_outputs_a_picture_that_waits_too_long() {
    let run_stream = |latency_plus1| {
        let mut run = Run::new(params(8, 6, 2, latency_plus1));
        run.decode(idr());
        run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
        run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[(7, true)])));
        run.decode(header(TRAIL_R, 2, st(&[(-1, true)], &[(6, true)])));
        run.decode(header(TRAIL_R, 3, st(&[(-1, true)], &[(5, true)])));
        run.output
    };
    // SpsMaxLatencyPictures is 3: picture 8 has seen three pictures that
    // precede it by the time picture 3 is stored.
    assert_eq!(run_stream(2), [0, 1, 2, 3, 8]);
    assert_eq!(run_stream(0), [0, 1, 2]);
}

#[test]
fn pictures_with_pic_output_flag_zero_are_never_output() {
    let mut run = Run::new(params(8, 4, 0, 0));
    run.decode(idr());
    let mut hidden = header(TRAIL_R, 1, st(&[(-1, true)], &[]));
    hidden.pic_output = false;
    run.decode(hidden);
    run.decode(header(TRAIL_R, 2, st(&[(-1, true)], &[])));
    run.drain();
    assert_eq!(run.output, [0, 2]);
}

#[test]
fn an_idr_picture_outputs_the_prior_pictures_unless_told_not_to() {
    for no_output in [false, true] {
        let mut run = Run::new(params(8, 4, 2, 0));
        run.decode(idr());
        run.decode(header(TRAIL_R, 2, st(&[(-2, true)], &[])));
        assert_eq!(run.output, []);
        let mut second = idr();
        second.no_output_of_prior_pics = no_output;
        let (_, events) = run.begin(second).unwrap().unwrap();
        if no_output {
            assert_eq!(run.output, []);
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event, Event::Release(_)))
            );
        } else {
            assert_eq!(run.output, [0, 2]);
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Release(_)))
                .count(),
            2
        );
    }
}

#[test]
fn a_cra_picture_at_the_start_skips_its_rasl_pictures_and_decodes_radl_ones() {
    let mut run = Run::new(params(8, 5, 2, 0));
    // CRA with lsb 8; its set names pictures a RASL picture would need.
    let cra = header(CRA, 8, st(&[(-8, false)], &[]));
    assert_eq!(run.decode(cra), 8);
    let rasl = header(RASL_R, 6, st(&[(-6, true)], &[(2, true)]));
    assert!(run.begin(rasl).unwrap().is_none());
    assert!(run.dpb.current().is_none());
    let decodable = header(RADL_R, 7, st(&[], &[(1, true)]));
    assert_eq!(run.decode(decodable), 7);
    run.drain();
    assert_eq!(run.output, [7, 8]);
}

#[test]
fn a_cra_picture_in_the_middle_keeps_its_rasl_pictures() {
    let mut run = Run::new(params(8, 5, 2, 0));
    run.decode(idr());
    run.decode(header(CRA, 8, st(&[(-8, true)], &[])));
    // The RASL picture refers to picture 0, from before the CRA picture.
    let rasl = header(RASL_R, 4, st(&[(-4, true)], &[(4, true)]));
    assert_eq!(run.decode(rasl), 4);
    run.drain();
    assert_eq!(run.output, [0, 4, 8]);
}

#[test]
fn a_bla_picture_starts_a_sequence_and_skips_its_rasl_pictures() {
    let mut run = Run::new(params(8, 5, 2, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 3, st(&[(-3, true)], &[])));
    let (_, _) = run
        .begin(header(BLA_W_LP, 40, ReferencePictureSet::default()))
        .unwrap()
        .unwrap();
    // The prior pictures were output; the order count restarts from the lsb.
    assert_eq!(run.output, [0, 3]);
    assert_eq!(run.dpb.current().unwrap().poc, 40);
    run.finish();
    let rasl = header(RASL_R, 38, st(&[(-8, true)], &[(2, true)]));
    assert!(run.begin(rasl).unwrap().is_none());
}

#[test]
fn a_stream_must_start_with_an_irap_picture() {
    let mut run = Run::new(params(8, 4, 0, 0));
    let error = run
        .dpb
        .begin_picture(header(TRAIL_R, 0, ReferencePictureSet::default()))
        .unwrap_err();
    assert!(matches!(error, DpbError::InconsistentHeader(_)));
    run.decode(idr());
    run.drain();
    let error = run
        .dpb
        .begin_picture(header(TRAIL_R, 1, ReferencePictureSet::default()))
        .unwrap_err();
    assert!(matches!(error, DpbError::InconsistentHeader(_)));
}

#[test]
fn an_idr_picture_with_an_order_count_lsb_is_an_error() {
    let mut run = Run::new(params(8, 4, 0, 0));
    let error = run
        .dpb
        .begin_picture(header(IDR_W_RADL, 3, ReferencePictureSet::default()))
        .unwrap_err();
    assert!(matches!(error, DpbError::InconsistentHeader(_)));
}

#[test]
fn sub_layer_non_reference_and_leading_pictures_do_not_anchor_the_count() {
    let mut run = Run::new(params(4, 4, 2, 0));
    run.decode(idr());
    // These do not become prevTid0Pic: the count stays derived from the IDR.
    run.decode(header(TRAIL_N, 6, st(&[(-6, true)], &[])));
    let mut upper = header(TRAIL_R, 7, st(&[(-1, true)], &[]));
    upper.temporal_id = 1;
    run.decode(upper);
    // lsb 9 relative to prevTid0Pic (lsb 0) is 9 > 8: it would be -7.
    // Relative to lsb 6 or 7 it would be 9. It is -7 because neither moved
    // the anchor.
    let poc = run.decode(header(TRAIL_R, 9, ReferencePictureSet::default()));
    assert_eq!(poc, -7);
}

#[test]
fn reference_lists_start_with_before_then_after_then_long_term_and_repeat() {
    let mut run = Run::new(params(8, 6, 4, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
    run.decode(header(TRAIL_R, 4, st(&[(-4, true)], &[(4, true)])));
    let mut rps = st(&[(-2, true)], &[(2, true)]);
    rps.long_term.push(long_term(0, None, true));
    let (id, _) = run.begin(header(TRAIL_R, 6, rps)).unwrap().unwrap();
    let poc = |id: &FrameId| run.dpb.frame(*id).unwrap().poc;
    let pocs = |list: &Vec<FrameId>| list.iter().map(poc).collect::<Vec<_>>();
    let [list0, list1] = run
        .dpb
        .reference_lists(ListKind::B, [4, 2], [None, None])
        .unwrap();
    // Before: 4; after: 8; long-term: 0.
    assert_eq!(pocs(&list0), [4, 8, 0, 4]);
    assert_eq!(pocs(&list1), [8, 4]);
    assert!(run.dpb.frame(id).is_some());
    let [list0, list1] = run
        .dpb
        .reference_lists(ListKind::P, [2, 0], [None, None])
        .unwrap();
    assert_eq!(pocs(&list0), [4, 8]);
    assert_eq!(list1, []);
}

#[test]
fn reference_lists_can_be_modified_and_entries_are_checked() {
    let mut run = Run::new(params(8, 6, 4, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 8, st(&[(-8, true)], &[])));
    run.begin(header(TRAIL_R, 4, st(&[(-4, true)], &[(4, true)])))
        .unwrap();
    let poc = |id: &FrameId| run.dpb.frame(*id).unwrap().poc;
    let [list0, _] = run
        .dpb
        .reference_lists(ListKind::P, [3, 0], [Some(&[1, 1, 0]), None])
        .unwrap();
    assert_eq!(list0.iter().map(poc).collect::<Vec<_>>(), [8, 8, 0]);
    let error = run
        .dpb
        .reference_lists(ListKind::P, [1, 0], [Some(&[2]), None])
        .unwrap_err();
    assert_eq!(error, DpbError::ListEntry { entry: 2, len: 2 });
}

#[test]
fn a_slice_with_nothing_to_refer_to_is_an_error() {
    let mut run = Run::new(params(8, 4, 0, 0));
    run.begin(idr()).unwrap();
    let error = run
        .dpb
        .reference_lists(ListKind::P, [1, 0], [None, None])
        .unwrap_err();
    assert_eq!(error, DpbError::EmptyReferenceSet);
}

#[test]
fn picture_state_errors_do_not_panic() {
    let mut run = Run::new(params(8, 4, 0, 0));
    assert_eq!(run.dpb.finish_picture().unwrap_err(), DpbError::NoPicture);
    assert!(run.dpb.abort_picture().is_none());
    assert_eq!(
        run.dpb
            .reference_lists(ListKind::P, [1, 0], [None, None])
            .unwrap_err(),
        DpbError::NoPicture
    );
    run.begin(idr()).unwrap();
    assert_eq!(
        run.dpb.begin_picture(idr()).unwrap_err(),
        DpbError::PictureInProgress
    );
    let aborted = run.dpb.abort_picture().unwrap();
    assert!(run.dpb.frame(aborted).is_none());
    // The sequence can still start.
    run.decode(idr());
}

#[test]
fn a_set_of_the_most_references_always_leaves_room_for_the_picture() {
    let mut run = Run::new(params(8, 3, 0, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[])));
    for lsb in 2..40 {
        run.decode(header(TRAIL_R, lsb, st(&[(-1, true), (-2, true)], &[])));
        assert_eq!(run.dpb.reference_frames().count(), 3);
    }
    assert_eq!(run.output.len(), 40);
}

#[test]
fn the_buffer_size_must_be_sane() {
    assert!(Dpb::new(params(8, 0, 0, 0)).is_err());
    assert!(Dpb::new(params(8, 17, 0, 0)).is_err());
    assert!(Dpb::new(params(8, 2, 2, 0)).is_err());
    assert!(Dpb::new(params(3, 2, 0, 0)).is_err());
    assert!(Dpb::new(params(17, 2, 0, 0)).is_err());
    assert!(Dpb::new(params(16, 16, 15, 0)).is_ok());
}

#[test]
fn frames_that_are_neither_referenced_nor_waiting_are_released() {
    let mut run = Run::new(params(8, 4, 0, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 1, ReferencePictureSet::default()));
    // The first frame left the reference set when the second began.
    assert_eq!(run.released.len(), 1);
    run.decode(header(TRAIL_N, 2, ReferencePictureSet::default()));
    // The sub-layer non-reference picture is released at the next one.
    run.decode(header(TRAIL_R, 3, ReferencePictureSet::default()));
    assert_eq!(run.released.len(), 3);
}

#[test]
fn drain_outputs_in_order_and_empties_the_buffer() {
    let mut run = Run::new(params(8, 6, 5, 0));
    run.decode(idr());
    run.decode(header(TRAIL_R, 3, st(&[(-3, true)], &[])));
    run.decode(header(TRAIL_R, 1, st(&[(-1, true)], &[(2, true)])));
    run.decode(header(TRAIL_R, 2, st(&[(-1, true)], &[(1, true)])));
    assert_eq!(run.output, []);
    run.drain();
    assert_eq!(run.output, [0, 1, 2, 3]);
    assert_eq!(run.dpb.reference_frames().count(), 0);
    assert_eq!(run.dpb.drain(), Vec::new());
}
