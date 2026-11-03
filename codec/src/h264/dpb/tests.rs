//! Hand-built picture sequences against the picture buffer.

#![allow(
    clippy::assert_is_empty,
    reason = "an emptiness check reads better than a comparison with an empty array"
)]

use super::*;

fn sequence() -> SequenceParams {
    SequenceParams {
        log2_max_frame_num: 4,
        poc: PocType::Zero { log2_max_lsb: 8 },
        max_num_ref_frames: 4,
        gaps_allowed: false,
        capacity: 6,
        num_reorder_frames: None,
    }
}

fn idr(lsb: u32) -> PictureHeader {
    PictureHeader {
        idr: true,
        nal_ref_idc: 3,
        frame_num: 0,
        poc_lsb: lsb,
        delta_poc_bottom: 0,
        delta_poc: [0; 2],
        marking: Some(Marking::Idr {
            no_output_of_prior_pics: false,
            long_term_reference: false,
        }),
    }
}

fn p(frame_num: u32, lsb: u32) -> PictureHeader {
    PictureHeader {
        idr: false,
        nal_ref_idc: 1,
        frame_num,
        poc_lsb: lsb,
        delta_poc_bottom: 0,
        delta_poc: [0; 2],
        marking: Some(Marking::SlidingWindow),
    }
}

fn b(frame_num: u32, lsb: u32) -> PictureHeader {
    PictureHeader {
        nal_ref_idc: 0,
        marking: None,
        ..p(frame_num, lsb)
    }
}

fn adaptive(frame_num: u32, lsb: u32, operations: Vec<Mmco>) -> PictureHeader {
    PictureHeader {
        marking: Some(Marking::Adaptive(operations)),
        ..p(frame_num, lsb)
    }
}

/// Decodes pictures one after another and records the events.
struct Harness {
    dpb: Dpb,
    events: Vec<Event>,
}

impl Harness {
    fn new(sequence: SequenceParams) -> Self {
        Self {
            dpb: Dpb::new(sequence).unwrap(),
            events: Vec::new(),
        }
    }

    fn try_push(&mut self, header: PictureHeader) -> Result<FrameId, DpbError> {
        let began = self.dpb.begin_picture(header)?;
        self.events.extend(began.events);
        self.events.extend(self.dpb.finish_picture()?);
        Ok(began.id)
    }

    fn push(&mut self, header: PictureHeader) -> FrameId {
        self.try_push(header).unwrap()
    }

    /// Pushes reference pictures `frame_num` 1.. with increasing order counts.
    fn push_p_run(&mut self, count: u32) -> Vec<FrameId> {
        (1..=count).map(|n| self.push(p(n, n * 2))).collect()
    }

    fn outputs(&self) -> Vec<FrameId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Output(id) => Some(*id),
                Event::Release(_) => None,
            })
            .collect()
    }

    fn released(&self) -> Vec<FrameId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Release(id) => Some(*id),
                Event::Output(_) => None,
            })
            .collect()
    }

    fn poc(&self, id: FrameId) -> (i32, i32) {
        let frame = self.dpb.frame(id).unwrap();
        (frame.top, frame.bottom)
    }

    fn references(&self) -> Vec<u32> {
        let mut frame_nums: Vec<u32> = self
            .dpb
            .reference_frames()
            .map(|frame| frame.frame_num)
            .collect();
        frame_nums.sort_unstable();
        frame_nums
    }

    /// The reference lists of a slice of a picture started with `header`,
    /// as `frame_num`s (`None` for no reference picture).
    fn lists(
        &mut self,
        header: PictureHeader,
        kind: ListKind,
        active: [usize; 2],
        modifications: [&[Modification]; 2],
    ) -> Result<[Vec<Option<u32>>; 2], DpbError> {
        self.events.extend(self.dpb.begin_picture(header)?.events);
        let lists = self.dpb.reference_lists(kind, active, modifications);
        self.dpb.abort_picture();
        let lists = lists?;
        Ok(lists.map(|list| {
            list.into_iter()
                .map(|entry| entry.map(|id| self.dpb.frame(id).unwrap().frame_num))
                .collect()
        }))
    }
}

#[test]
fn poc_type_0_extends_the_wrapping_lsb() {
    let mut sequence = sequence();
    sequence.poc = PocType::Zero { log2_max_lsb: 4 };
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    let pocs: Vec<i32> = [(1, 6), (2, 12), (3, 2), (4, 10), (5, 1)]
        .into_iter()
        .map(|(frame_num, lsb)| {
            let id = harness.push(p(frame_num, lsb));
            harness.poc(id).0
        })
        .collect();
    // 12 - 6 is not past half of 16; 2 wraps up; 10 - 2 is exactly half;
    // 1 wraps up again.
    assert_eq!(pocs, [6, 12, 18, 26, 33]);
    // A picture after the wrap that precedes the last reference.
    let id = harness.push(b(6, 14));
    assert_eq!(harness.poc(id), (30, 30), "msb 32 minus 16 plus lsb 14");
}

#[test]
fn poc_type_0_bottom_field_follows_top() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    let mut header = p(1, 10);
    header.delta_poc_bottom = -2;
    let id = harness.push(header);
    assert_eq!(harness.poc(id), (10, 8));
}

#[test]
fn poc_type_1_follows_the_offset_cycle() {
    let mut sequence = sequence();
    sequence.poc = PocType::One {
        offset_for_non_ref_pic: -2,
        offset_for_top_to_bottom_field: 1,
        offsets_for_ref_frame: vec![4, 6],
    };
    let mut harness = Harness::new(sequence);
    let first = harness.push(idr(0));
    assert_eq!(harness.poc(first), (0, 1));
    let counts: Vec<i32> = (1..=3)
        .map(|n| {
            let id = harness.push(p(n, 0));
            harness.poc(id).0
        })
        .collect();
    assert_eq!(counts, [4, 10, 14]);
    // A non-reference picture counts the frame before it and adds the
    // non-reference offset.
    let non_ref = harness.push(b(4, 0));
    assert_eq!(harness.poc(non_ref).0, 12);
    // The next reference picture shares its frame_num with the previous
    // non-reference one.
    let next = harness.push(p(4, 0));
    assert_eq!(harness.poc(next).0, 20);
}

#[test]
fn poc_type_2_is_twice_the_decoding_order() {
    let mut sequence = sequence();
    sequence.poc = PocType::Two;
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    let first = harness.push(p(1, 0));
    assert_eq!(harness.poc(first).0, 2);
    let non_ref = harness.push(b(2, 0));
    assert_eq!(harness.poc(non_ref).0, 3);
    let second = harness.push(p(2, 0));
    assert_eq!(harness.poc(second).0, 4);
    // frame_num wraps from 15 to 0 with the offset 16.
    for frame_num in 3..=15 {
        harness.push(p(frame_num, 0));
    }
    let wrapped = harness.push(p(0, 0));
    assert_eq!(harness.poc(wrapped).0, 32);
}

#[test]
fn the_sliding_window_keeps_the_newest_frames() {
    let mut sequence = sequence();
    sequence.max_num_ref_frames = 2;
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    harness.push_p_run(3);
    assert_eq!(harness.references(), [2, 3]);
    let lists = harness
        .lists(p(4, 8), ListKind::P, [2, 0], [&[], &[]])
        .unwrap();
    assert_eq!(lists[0], [Some(3), Some(2)]);
}

#[test]
fn frame_num_wrap_orders_the_list_by_decoding_order() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    for frame_num in 1..=15 {
        harness.push(p(frame_num, frame_num * 2));
    }
    harness.push(p(0, 32));
    assert_eq!(harness.references(), [0, 13, 14, 15]);
    let lists = harness
        .lists(p(1, 34), ListKind::P, [4, 0], [&[], &[]])
        .unwrap();
    // PicNum is FrameNumWrap: 0, -1, -2, -3.
    assert_eq!(lists[0], [Some(0), Some(15), Some(14), Some(13)]);
}

#[test]
fn a_frame_num_gap_infers_non_existing_frames() {
    let mut sequence = sequence();
    sequence.gaps_allowed = true;
    let mut harness = Harness::new(sequence);
    let first = harness.push(idr(0));
    let skipping = p(3, 6);
    let began = harness.dpb.begin_picture(skipping).unwrap();
    assert!(began.events.is_empty());
    let inferred: Vec<FrameView> = harness
        .dpb
        .reference_frames()
        .filter(|frame| frame.non_existing)
        .collect();
    assert_eq!(
        inferred
            .iter()
            .map(|frame| frame.frame_num)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let lists = harness
        .dpb
        .reference_lists(ListKind::P, [3, 0], [&[], &[]])
        .unwrap();
    let frame_nums: Vec<u32> = lists[0]
        .iter()
        .map(|entry| harness.dpb.frame(entry.unwrap()).unwrap().frame_num)
        .collect();
    assert_eq!(frame_nums, [2, 1, 0]);
    harness.events.extend(harness.dpb.finish_picture().unwrap());
    harness.events.extend(harness.dpb.drain());
    // Inferred frames are never output.
    assert_eq!(harness.outputs(), [first, began.id]);
}

#[test]
fn a_frame_num_gap_slides_the_window_over_inferred_frames() {
    let mut sequence = sequence();
    sequence.gaps_allowed = true;
    sequence.max_num_ref_frames = 2;
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    harness.push(p(4, 8));
    assert_eq!(harness.references(), [3, 4]);
    // The inferred frames that slid out were released without output.
    // Inferred frames 1 and 2 slid out without output; the IDR picture
    // still waits for output.
    assert_eq!(harness.released().len(), 2);
}

#[test]
fn a_frame_num_gap_is_an_error_when_not_allowed() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    assert_eq!(
        harness.try_push(p(3, 6)),
        Err(DpbError::FrameNumGap {
            previous: 0,
            found: 3
        })
    );
}

#[test]
fn operation_1_unmarks_a_short_term_frame() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push_p_run(3);
    // picNumX = 4 - (1 + 1) = 2.
    harness.push(adaptive(
        4,
        8,
        vec![Mmco::ShortTermUnused {
            difference_of_pic_nums_minus1: 1,
        }],
    ));
    assert_eq!(harness.references(), [0, 1, 3, 4]);
}

#[test]
fn long_term_operations_move_frames_between_the_lists() {
    let mut sequence = sequence();
    sequence.max_num_ref_frames = 6;
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    harness.push_p_run(3);
    harness.push(adaptive(
        4,
        8,
        vec![
            Mmco::MaxLongTermIdx {
                max_long_term_frame_idx_plus1: 2,
            },
            Mmco::ShortToLong {
                difference_of_pic_nums_minus1: 0,
                long_term_frame_idx: 1,
            },
        ],
    ));
    // frame 3 is long-term index 1; the window then holds 0, 1, 2 and 4.
    let lists = harness
        .lists(p(5, 10), ListKind::P, [5, 0], [&[], &[]])
        .unwrap();
    assert_eq!(
        lists[0],
        [Some(4), Some(2), Some(1), Some(0), Some(3)],
        "short-term by descending PicNum, then long-term"
    );
    // Operation 2 drops it again, operation 6 makes the current frame
    // long-term in its place.
    harness.push(adaptive(
        5,
        10,
        vec![
            Mmco::LongTermUnused {
                long_term_pic_num: 1,
            },
            Mmco::CurrentToLong {
                long_term_frame_idx: 0,
            },
        ],
    ));
    let long: Vec<Option<u32>> = harness
        .dpb
        .reference_frames()
        .map(|frame| frame.long_term_idx)
        .collect();
    assert_eq!(long.iter().filter(|idx| **idx == Some(0)).count(), 1);
    assert!(!long.contains(&Some(1)));
}

#[test]
fn operation_4_drops_long_term_frames_above_the_limit() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push_p_run(2);
    harness.push(adaptive(
        3,
        6,
        vec![
            Mmco::MaxLongTermIdx {
                max_long_term_frame_idx_plus1: 2,
            },
            Mmco::ShortToLong {
                difference_of_pic_nums_minus1: 0,
                long_term_frame_idx: 1,
            },
        ],
    ));
    assert!(
        harness
            .dpb
            .reference_frames()
            .any(|frame| frame.long_term_idx == Some(1))
    );
    harness.push(adaptive(
        4,
        8,
        vec![Mmco::MaxLongTermIdx {
            max_long_term_frame_idx_plus1: 0,
        }],
    ));
    assert!(
        harness
            .dpb
            .reference_frames()
            .all(|frame| frame.long_term_idx.is_none())
    );
}

#[test]
fn operation_5_outputs_everything_and_restarts_the_counts() {
    let mut harness = Harness::new(sequence());
    let first = harness.push(idr(0));
    let second = harness.push(p(1, 20));
    let third = harness.push(adaptive(2, 40, vec![Mmco::AllUnused]));
    // The earlier pictures are output before the picture with the
    // operation, which is now the only reference and counts from 0.
    assert_eq!(harness.outputs(), [first, second]);
    assert_eq!(harness.references(), [0]);
    assert_eq!(harness.poc(third), (0, 0));
    // The next picture has frame_num 1 and an order count relative to the
    // top order count of the operation's picture, which is now 0.
    let fourth = harness.push(p(1, 6));
    assert_eq!(harness.poc(fourth), (6, 6));
}

#[test]
fn an_invalid_operation_leaves_the_buffer_untouched() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push_p_run(2);
    for operation in [
        // No frame with picNumX 0 (frame 3 is the current picture).
        Mmco::ShortTermUnused {
            difference_of_pic_nums_minus1: 9,
        },
        Mmco::LongTermUnused {
            long_term_pic_num: 0,
        },
        // No long-term indices were allowed.
        Mmco::ShortToLong {
            difference_of_pic_nums_minus1: 0,
            long_term_frame_idx: 0,
        },
        Mmco::CurrentToLong {
            long_term_frame_idx: 0,
        },
    ] {
        assert!(matches!(
            harness.try_push(adaptive(3, 6, vec![operation])),
            Err(DpbError::InvalidMmco(_))
        ));
        assert_eq!(harness.references(), [0, 1, 2]);
    }
    // The buffer is still usable.
    harness.push(p(3, 6));
    assert_eq!(harness.references(), [0, 1, 2, 3]);
}

#[test]
fn marking_more_references_than_allowed_is_an_error() {
    let mut sequence = sequence();
    sequence.max_num_ref_frames = 2;
    let mut harness = Harness::new(sequence);
    harness.push(idr(0));
    harness.push(p(1, 2));
    assert_eq!(
        harness.try_push(adaptive(2, 4, vec![])),
        Err(DpbError::TooManyReferences {
            references: 3,
            limit: 2
        })
    );
}

#[test]
fn the_sliding_window_needs_a_short_term_frame() {
    let mut sequence = sequence();
    sequence.max_num_ref_frames = 1;
    let mut harness = Harness::new(sequence);
    let mut header = idr(0);
    header.marking = Some(Marking::Idr {
        no_output_of_prior_pics: false,
        long_term_reference: true,
    });
    harness.push(header);
    assert_eq!(harness.try_push(p(1, 2)), Err(DpbError::SlidingWindowEmpty));
}

#[test]
fn modifications_move_frames_to_the_front() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push_p_run(3);
    let current = p(4, 8);
    let run = |harness: &mut Harness, modifications: &[Modification], active| {
        harness
            .lists(
                current.clone(),
                ListKind::P,
                [active, 0],
                [modifications, &[]],
            )
            .map(|lists| lists[0].clone())
    };
    // The initial list is 3, 2, 1, 0.
    assert_eq!(
        run(&mut harness, &[], 3).unwrap(),
        [Some(3), Some(2), Some(1)]
    );
    // picNum 4 - 3 = 1 goes first, the rest keeps its order.
    assert_eq!(
        run(
            &mut harness,
            &[Modification::Subtract {
                abs_diff_pic_num_minus1: 2
            }],
            3
        )
        .unwrap(),
        [Some(1), Some(3), Some(2)]
    );
    // 4 + 15 wraps over MaxPicNum 16 to picNum 3, which is first already.
    assert_eq!(
        run(
            &mut harness,
            &[Modification::Add {
                abs_diff_pic_num_minus1: 14
            }],
            3
        )
        .unwrap(),
        [Some(3), Some(2), Some(1)]
    );
    // Two operations chain their predictions: 4 - 4 = 0, then 0 + 2 = 2.
    assert_eq!(
        run(
            &mut harness,
            &[
                Modification::Subtract {
                    abs_diff_pic_num_minus1: 3
                },
                Modification::Add {
                    abs_diff_pic_num_minus1: 1
                }
            ],
            3
        )
        .unwrap(),
        [Some(0), Some(2), Some(3)]
    );
    // A list longer than the references is padded.
    assert_eq!(
        run(&mut harness, &[], 6).unwrap(),
        [Some(3), Some(2), Some(1), Some(0), None, None]
    );
    // Naming a missing frame, or more operations than entries, fails.
    assert_eq!(
        run(
            &mut harness,
            &[Modification::Subtract {
                abs_diff_pic_num_minus1: 5
            }],
            3
        ),
        Err(DpbError::MissingReference(-2))
    );
    assert_eq!(
        run(
            &mut harness,
            &[Modification::Subtract {
                abs_diff_pic_num_minus1: 0
            }; 2],
            1
        ),
        Err(DpbError::TooManyModifications)
    );
}

#[test]
fn a_modification_can_name_a_long_term_frame() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push(adaptive(
        1,
        2,
        vec![
            Mmco::MaxLongTermIdx {
                max_long_term_frame_idx_plus1: 1,
            },
            Mmco::ShortToLong {
                difference_of_pic_nums_minus1: 0,
                long_term_frame_idx: 0,
            },
        ],
    ));
    harness.push(p(2, 4));
    let lists = harness
        .lists(
            p(3, 6),
            ListKind::P,
            [3, 0],
            [
                &[Modification::LongTerm {
                    long_term_pic_num: 0,
                }],
                &[],
            ],
        )
        .unwrap();
    // The IDR picture (frame_num 0) is long-term index 0. Initially it is
    // last; the modification moves it to the front.
    assert_eq!(lists[0], [Some(0), Some(2), Some(1)]);
}

#[test]
fn b_lists_order_references_around_the_current_picture() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push(p(1, 16));
    harness.push(p(2, 8));
    let lists = harness
        .lists(b(3, 12), ListKind::B, [3, 3], [&[], &[]])
        .unwrap();
    // POC 12: list 0 goes down from 8 then up to 16, list 1 the other way.
    assert_eq!(lists[0], [Some(2), Some(0), Some(1)]);
    assert_eq!(lists[1], [Some(1), Some(2), Some(0)]);
}

#[test]
fn b_list_1_swaps_its_first_entries_when_it_equals_list_0() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push(p(1, 8));
    let lists = harness
        .lists(b(2, 12), ListKind::B, [2, 2], [&[], &[]])
        .unwrap();
    // Both references precede the picture: list 0 is 8, 0 and list 1 would
    // be the same, so its first two entries swap.
    assert_eq!(lists[0], [Some(1), Some(0)]);
    assert_eq!(lists[1], [Some(0), Some(1)]);
    // With one reference there is nothing to swap.
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    let lists = harness
        .lists(b(1, 4), ListKind::B, [1, 1], [&[], &[]])
        .unwrap();
    assert_eq!(lists[0], lists[1]);
}

#[test]
fn b_lists_end_with_the_long_term_frames() {
    let mut harness = Harness::new(sequence());
    harness.push(idr(0));
    harness.push(adaptive(
        1,
        16,
        vec![
            Mmco::MaxLongTermIdx {
                max_long_term_frame_idx_plus1: 1,
            },
            Mmco::ShortToLong {
                difference_of_pic_nums_minus1: 0,
                long_term_frame_idx: 0,
            },
        ],
    ));
    harness.push(p(2, 8));
    let lists = harness
        .lists(b(3, 12), ListKind::B, [3, 3], [&[], &[]])
        .unwrap();
    // The IDR picture (frame_num 0) became long-term index 0 and ends both
    // lists.
    assert_eq!(lists[0], [Some(2), Some(1), Some(0)]);
    assert_eq!(lists[1], [Some(1), Some(2), Some(0)]);
}

#[test]
fn reference_frames_are_output_in_order_when_the_buffer_fills() {
    let mut sequence = sequence();
    sequence.capacity = 2;
    sequence.max_num_ref_frames = 1;
    let mut harness = Harness::new(sequence);
    let ids = [
        harness.push(idr(0)),
        harness.push(p(1, 2)),
        harness.push(p(2, 4)),
        harness.push(p(3, 6)),
    ];
    // Each new picture finds the buffer full and outputs the oldest.
    assert_eq!(harness.outputs(), ids[..2]);
    harness.events.extend(harness.dpb.drain());
    assert_eq!(harness.outputs(), ids);
}

#[test]
fn bumping_reorders_pictures_by_order_count() {
    let mut sequence = sequence();
    sequence.capacity = 3;
    sequence.max_num_ref_frames = 2;
    let mut harness = Harness::new(sequence);
    let i0 = harness.push(idr(0));
    let p8 = harness.push(p(1, 8));
    let b4 = harness.push(b(2, 4));
    let p16 = harness.push(p(2, 16));
    let b12 = harness.push(b(3, 12));
    harness.events.extend(harness.dpb.drain());
    assert_eq!(harness.outputs(), [i0, b4, p8, b12, p16]);
    // The frames that are no longer references are released once output.
    let mut released = harness.released();
    released.sort();
    assert_eq!(released, [i0, b4, b12]);
}

#[test]
fn a_non_reference_picture_that_comes_first_is_output_without_storing() {
    let mut sequence = sequence();
    sequence.capacity = 1;
    sequence.max_num_ref_frames = 1;
    let mut harness = Harness::new(sequence);
    let i0 = harness.push(idr(0));
    let p8 = harness.push(p(1, 8));
    let b4 = harness.push(b(2, 4));
    assert_eq!(harness.outputs(), [i0, b4]);
    assert_eq!(
        harness.events[harness.events.len() - 2..],
        [Event::Output(b4), Event::Release(b4)]
    );
    harness.events.extend(harness.dpb.drain());
    assert_eq!(harness.outputs(), [i0, b4, p8]);
}

#[test]
fn an_idr_picture_outputs_the_pictures_before_it() {
    let mut harness = Harness::new(sequence());
    let first = harness.push(idr(0));
    let second = harness.push(p(1, 8));
    let third = harness.push(b(2, 4));
    assert!(harness.outputs().is_empty());
    let next = harness.push(idr(0));
    assert_eq!(harness.outputs(), [first, third, second]);
    assert_eq!(harness.references(), [0]);
    harness.events.extend(harness.dpb.drain());
    assert_eq!(harness.outputs().last(), Some(&next));
}

#[test]
fn no_output_of_prior_pics_discards_the_waiting_pictures() {
    let mut harness = Harness::new(sequence());
    let first = harness.push(idr(0));
    let second = harness.push(p(1, 8));
    let mut header = idr(0);
    header.marking = Some(Marking::Idr {
        no_output_of_prior_pics: true,
        long_term_reference: false,
    });
    harness.push(header);
    assert!(harness.outputs().is_empty());
    let mut released = harness.released();
    released.sort();
    assert_eq!(released, [first, second]);
}

#[test]
fn the_vui_reorder_limit_starts_output_early() {
    let mut sequence = sequence();
    sequence.num_reorder_frames = Some(1);
    sequence.max_num_ref_frames = 2;
    let mut harness = Harness::new(sequence);
    let i0 = harness.push(idr(0));
    assert!(harness.outputs().is_empty());
    let p8 = harness.push(p(1, 8));
    assert_eq!(harness.outputs(), [i0]);
    let b4 = harness.push(b(2, 4));
    assert_eq!(harness.outputs(), [i0, b4]);
    let p16 = harness.push(p(2, 16));
    assert_eq!(harness.outputs(), [i0, b4, p8]);
    harness.events.extend(harness.dpb.drain());
    assert_eq!(harness.outputs(), [i0, b4, p8, p16]);
}

#[test]
fn a_full_buffer_outputs_the_oldest_frame_as_the_window_slides() {
    let mut sequence = sequence();
    sequence.capacity = 4;
    sequence.max_num_ref_frames = 4;
    let mut harness = Harness::new(sequence);
    let first = harness.push(idr(0));
    harness.push_p_run(3);
    assert!(harness.outputs().is_empty());
    harness.push(p(4, 8));
    assert_eq!(harness.outputs(), [first]);
}

#[test]
fn misuse_and_bad_headers_are_errors() {
    let mut harness = Harness::new(sequence());
    assert_eq!(harness.dpb.finish_picture(), Err(DpbError::NoPicture));
    harness.dpb.begin_picture(idr(0)).unwrap();
    assert_eq!(
        harness.dpb.begin_picture(idr(0)),
        Err(DpbError::PictureInProgress)
    );
    assert!(harness.dpb.abort_picture().is_some());

    let mut header = idr(0);
    header.frame_num = 1;
    assert_eq!(
        harness.dpb.begin_picture(header),
        Err(DpbError::IdrFrameNum(1))
    );
    let mut header = p(1, 0);
    header.marking = None;
    assert_eq!(
        harness.dpb.begin_picture(header),
        Err(DpbError::InconsistentMarking)
    );
    assert!(matches!(
        harness.dpb.begin_picture(p(16, 0)),
        Err(DpbError::OutOfRange {
            field: "frame_num",
            ..
        })
    ));
    assert!(matches!(
        harness.dpb.begin_picture(idr(256)),
        Err(DpbError::OutOfRange {
            field: "pic_order_cnt_lsb",
            ..
        })
    ));
    let mut small = sequence();
    small.capacity = 2;
    small.max_num_ref_frames = 3;
    assert_eq!(
        Dpb::new(small).unwrap_err(),
        DpbError::BufferTooSmall {
            references: 3,
            capacity: 2
        }
    );
}
