//! Parameter set and slice header parsing, on streams built by
//! [`super::testing`].

use super::{
    dpb::{LongTermRef, RpsEntry, ShortTermRps},
    error::{H265Error, Unsupported},
    nal::NalUnitType,
    params::{Pps, Sps, Vps},
    sequence::{BitDepth, ScalingFactors, Sequence, Tiling, check_pps},
    slice::{ParameterSets, SliceType},
    testing::{
        LongTermSpec, RpsCoding, ScalingData, SliceRps, SliceSpec, StreamConfig, nal_header,
    },
};
use std::sync::Arc;

fn set(negative: &[(i32, bool)], positive: &[(i32, bool)]) -> ShortTermRps {
    let entries = |list: &[(i32, bool)]| {
        list.iter()
            .map(|&(delta_poc, used_by_curr)| RpsEntry {
                delta_poc,
                used_by_curr,
            })
            .collect()
    };
    ShortTermRps {
        negative: entries(negative),
        positive: entries(positive),
    }
}

fn sets(config: &StreamConfig) -> ParameterSets {
    let mut sets = ParameterSets::default();
    sets.put_vps(&config.vps()).unwrap();
    sets.put_sps(&config.sps()).unwrap();
    sets.put_pps(&config.pps()).unwrap();
    sets
}

#[test]
fn a_sequence_parameter_set_is_read() {
    let config = StreamConfig {
        width: 192,
        height: 128,
        crop: Some([1, 2, 3, 4]),
        bit_depth: 10,
        profile_idc: 2,
        log2_max_poc_lsb: 6,
        log2_ctb: 5,
        max_dec_pic_buffering: 6,
        max_num_reorder: 3,
        max_latency_increase_plus1: 4,
        temporal_mvp: true,
        sao: true,
        ..StreamConfig::default()
    };
    let sps = Sps::parse(&config.sps()).unwrap();
    assert_eq!((sps.width, sps.height), (192, 128));
    assert_eq!(
        (
            sps.window.left,
            sps.window.right,
            sps.window.top,
            sps.window.bottom
        ),
        (2, 4, 6, 8)
    );
    assert_eq!((sps.bit_depth_luma, sps.bit_depth_chroma), (10, 10));
    assert_eq!(sps.log2_max_poc_lsb, 6);
    assert_eq!(sps.max_dec_pic_buffering_minus1, 5);
    assert_eq!(sps.max_num_reorder_pics, 3);
    assert_eq!(sps.max_latency_increase_plus1, 4);
    assert_eq!(sps.log2_ctb_size(), 5);
    assert_eq!((sps.width_in_ctbs(), sps.height_in_ctbs()), (6, 4));
    assert_eq!(sps.size_in_ctbs(), 24);
    assert!(sps.temporal_mvp_enabled && sps.sao_enabled);
    assert!(!sps.scaling_list_enabled);
    assert!(sps.profile.conforms_to(2) && !sps.profile.conforms_to(1));
    let sequence = Sequence::new(Arc::new(sps)).unwrap();
    assert_eq!(sequence.bit_depth, BitDepth::Ten);
    assert_eq!(sequence.output_size(), (186, 114));
    assert_eq!(sequence.params.max_dec_pic_buffering, 6);
    Vps::parse(&config.vps()).unwrap();
}

#[test]
fn unsupported_streams_are_reported_as_such() {
    let sequence = |config: StreamConfig| {
        Sequence::new(Arc::new(Sps::parse(&config.sps()).unwrap())).unwrap_err()
    };
    let error = sequence(StreamConfig {
        profile_idc: 4,
        ..StreamConfig::default()
    });
    assert_eq!(error, H265Error::Unsupported(Unsupported::Profile(4)));
    let error = sequence(StreamConfig {
        interlaced: true,
        ..StreamConfig::default()
    });
    assert_eq!(error, H265Error::Unsupported(Unsupported::Interlaced));
    let error = sequence(StreamConfig {
        chroma_format_idc: 2,
        ..StreamConfig::default()
    });
    assert_eq!(error, H265Error::Unsupported(Unsupported::ChromaFormat(2)));
    let error = sequence(StreamConfig {
        bit_depth: 12,
        profile_idc: 2,
        ..StreamConfig::default()
    });
    assert!(error.is_unsupported());
    let error = Pps::parse(
        &StreamConfig {
            pps_range_extension: true,
            ..StreamConfig::default()
        }
        .pps(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        H265Error::Unsupported(Unsupported::Extension("range"))
    );
}

#[test]
fn bad_sequence_parameter_sets_are_errors() {
    let good = StreamConfig::default().sps();
    for length in 0..good.len() {
        // Truncated NAL units are errors or parse a prefix; never a panic.
        let _ = Sps::parse(&good[..length]);
    }
    let too_big = StreamConfig {
        width: 20_000,
        ..StreamConfig::default()
    };
    assert!(matches!(
        Sps::parse(&too_big.sps()),
        Err(H265Error::Invalid(_))
    ));
    let unaligned = StreamConfig {
        width: 68,
        ..StreamConfig::default()
    };
    assert!(Sps::parse(&unaligned.sps()).is_err());
    let full_crop = StreamConfig {
        crop: Some([16, 16, 0, 0]),
        ..StreamConfig::default()
    };
    assert!(Sps::parse(&full_crop.sps()).is_err());
}

#[test]
fn short_term_sets_are_predicted_from_the_one_before() {
    let config = StreamConfig {
        short_term_sets: vec![
            RpsCoding::Explicit(set(&[(-2, true), (-4, true)], &[])),
            RpsCoding::Inter {
                delta_idx_minus1: 0,
                delta_rps: -1,
                flags: vec![(true, true), (true, true), (true, true)],
            },
            RpsCoding::Inter {
                delta_idx_minus1: 0,
                delta_rps: -1,
                flags: vec![(false, true), (true, true), (false, false), (false, false)],
            },
            RpsCoding::Explicit(set(&[(-2, true), (-4, true)], &[])),
            RpsCoding::Inter {
                delta_idx_minus1: 0,
                delta_rps: 1,
                flags: vec![(true, true), (true, true), (true, true)],
            },
        ],
        ..StreamConfig::default()
    };
    let sps = Sps::parse(&config.sps()).unwrap();
    assert_eq!(sps.short_term_rps[0], set(&[(-2, true), (-4, true)], &[]));
    assert_eq!(
        sps.short_term_rps[1],
        set(&[(-1, true), (-3, true), (-5, true)], &[])
    );
    // From set 1: -1 -> -2, -3 -> -4, -5 -> -6 and the picture itself, -1;
    // the first entry (used 0, use_delta 1) and the second (used 1) stay.
    assert_eq!(sps.short_term_rps[2], set(&[(-2, false), (-4, true)], &[]));
    assert_eq!(
        sps.short_term_rps[4],
        set(&[(-1, true), (-3, true)], &[(1, true)])
    );
}

#[test]
fn too_many_short_term_references_are_an_error() {
    let mut entries = Vec::new();
    for delta in 1..=17 {
        entries.push((-delta, true));
    }
    let config = StreamConfig {
        short_term_sets: vec![RpsCoding::Explicit(set(&entries, &[]))],
        ..StreamConfig::default()
    };
    assert!(Sps::parse(&config.sps()).is_err());
}

#[test]
fn scaling_lists_resolve_from_the_picture_then_the_sequence_then_the_defaults() {
    let lists = |sps: Option<Option<ScalingData>>, pps: Option<ScalingData>| {
        let config = StreamConfig {
            scaling: sps,
            pps_scaling: pps,
            ..StreamConfig::default()
        };
        let sps = Sps::parse(&config.sps()).unwrap();
        let pps = Pps::parse(&config.pps()).unwrap();
        ScalingFactors::new(&sps, &pps)
    };
    assert!(lists(None, Some(ScalingData::Flat)).is_none());
    let defaults = lists(Some(None), None).unwrap();
    assert_eq!(defaults.list_4x4[0], [16; 16]);
    // The default intra 8x8 list in raster order: the last coded coefficient
    // is the bottom right one.
    assert_eq!(defaults.list_8x8[0][63], 115);
    assert_eq!(defaults.list_8x8[3][63], 91);
    assert_eq!(defaults.list_8x8[0][0], 16);
    assert_eq!(defaults.list_16x16[1][63], 115);
    assert_eq!(defaults.list_32x32[1][63], 91);
    assert_eq!((defaults.dc_16x16, defaults.dc_32x32), ([16; 6], [16; 2]));
    // Coded explicitly in the sequence (flat 16s, DC 8, then copies).
    let flat = lists(Some(Some(ScalingData::Flat)), None).unwrap();
    assert_eq!(flat.list_8x8, [[16; 64]; 6]);
    assert_eq!(flat.list_32x32, [[16; 64]; 2]);
    assert_eq!((flat.dc_16x16, flat.dc_32x32), ([8; 6], [8; 2]));
    // The picture's lists win over the sequence's.
    let both = lists(Some(Some(ScalingData::Flat)), Some(ScalingData::Defaults)).unwrap();
    assert_eq!(both.list_8x8[0][63], 115);
}

#[test]
fn tiles_are_spaced_uniformly() {
    let config = StreamConfig {
        width: 128,
        height: 96,
        tiles: Some((3, 2)),
        ..StreamConfig::default()
    };
    let sps = Sps::parse(&config.sps()).unwrap();
    let pps = Pps::parse(&config.pps()).unwrap();
    check_pps(&sps, &pps).unwrap();
    // 8 by 6 coding tree blocks.
    let tiling = Tiling::new(&sps, &pps);
    assert_eq!(tiling.columns, [2, 3, 3]);
    assert_eq!(tiling.rows, [3, 3]);
    let single = Tiling::new(&sps, &Pps::parse(&StreamConfig::default().pps()).unwrap());
    assert_eq!((single.columns, single.rows), (vec![8], vec![6]));
    // More tile columns than coding tree blocks.
    let config = StreamConfig {
        tiles: Some((9, 1)),
        width: 128,
        ..StreamConfig::default()
    };
    let pps = Pps::parse(&config.pps()).unwrap();
    assert!(check_pps(&sps, &pps).is_err());
}

#[test]
fn an_idr_slice_header_is_read() {
    let config = StreamConfig::default();
    let sets = sets(&config);
    let mut spec = SliceSpec::idr();
    spec.qp_delta = -3;
    let nal = spec.nal(&config);
    let slice = sets.slice(&nal, None).unwrap();
    let header = &slice.header;
    assert!(header.first_slice_segment_in_pic && !header.dependent);
    assert_eq!(header.nal_unit_type, NalUnitType::IDR_W_RADL);
    assert_eq!(header.slice_type, SliceType::I);
    assert_eq!(header.qp_delta, -3);
    assert!(header.pic_output);
    assert_eq!(header.rps.short_term.negative, []);
    assert!(header.loop_filter_across_slices);
    // The two header bytes and the slice header, then the three data bytes.
    assert_eq!(
        slice.data_byte_offset + 3,
        nal.len() - slice.emulation_prevention_bytes
    );
}

#[test]
fn the_data_offset_skips_the_header_and_counts_emulation_prevention_bytes() {
    let config = StreamConfig {
        header_extension: true,
        output_flag_present: true,
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut escaped_headers = 0;
    for variant in 0..8 {
        let mut spec = SliceSpec::idr();
        spec.qp_delta = variant - 4;
        spec.pic_output = variant % 2 == 0;
        spec.data = vec![0, 0, 1, 0, 0, 3, 7];
        let nal = spec.nal(&config);
        let slice = sets.slice(&nal, None).unwrap();
        // Remove every emulation prevention byte and compare.
        let mut unescaped = Vec::new();
        let mut zeros = 0;
        let mut removed_in_header = 0;
        for &byte in &nal {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                if unescaped.len() <= slice.data_byte_offset {
                    removed_in_header += 1;
                }
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            unescaped.push(byte);
        }
        assert!(unescaped[slice.data_byte_offset..].starts_with(&spec.data));
        assert_eq!(slice.emulation_prevention_bytes, removed_in_header);
        escaped_headers += usize::from(removed_in_header > 0);
    }
    assert!(escaped_headers > 0, "no header needed an escape");
}

#[test]
fn an_inter_slice_header_is_read() {
    let config = StreamConfig {
        short_term_sets: vec![
            RpsCoding::Explicit(set(&[(-1, true)], &[])),
            RpsCoding::Explicit(set(&[(-1, true), (-2, true)], &[(1, false)])),
        ],
        long_term_sps: Some(vec![(3, true), (5, false)]),
        temporal_mvp: true,
        sao: true,
        weighted_pred: true,
        weighted_bipred: true,
        lists_modification: true,
        cabac_init_present: true,
        output_flag_present: true,
        extra_slice_header_bits: 2,
        slice_chroma_qp_offsets: true,
        deblocking_override: true,
        header_extension: true,
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 0, 17);
    spec.pic_output = false;
    spec.rps = SliceRps::Sps(1);
    spec.long_term = vec![
        LongTermSpec::Sps {
            index: 0,
            msb_cycle: Some(1),
        },
        LongTermSpec::Explicit {
            poc_lsb: 9,
            used: true,
            msb_cycle: Some(2),
        },
    ];
    spec.temporal_mvp = true;
    spec.active_override_minus1 = Some([3, 1]);
    spec.list_entries = [Some(vec![3, 2, 1, 0]), None];
    spec.collocated_from_l0 = false;
    spec.collocated_ref_idx = 1;
    spec.deblocking_override = Some(false);
    let nal = spec.nal(&config);
    let header = sets.slice(&nal, None).unwrap().header;
    assert_eq!(header.slice_type, SliceType::B);
    assert!(!header.pic_output);
    assert_eq!(header.poc_lsb, 17);
    assert_eq!(
        header.rps.short_term,
        set(&[(-1, true), (-2, true)], &[(1, false)])
    );
    assert_eq!(
        header.rps.long_term,
        [
            LongTermRef {
                poc_lsb: 3,
                msb_cycle: 1,
                msb_present: true,
                used_by_curr: true
            },
            LongTermRef {
                poc_lsb: 9,
                // The cycles accumulate within the group, and the first
                // entry of the explicit group starts over.
                msb_cycle: 2,
                msb_present: true,
                used_by_curr: true
            },
        ]
    );
    // Two short-term and two long-term pictures are used by the picture.
    assert_eq!(header.rps.num_pic_total_curr(), 4);
    assert!(header.temporal_mvp && header.sao_luma && header.sao_chroma);
    assert_eq!(header.num_ref_idx_active, [4, 2]);
    assert_eq!(header.list_entries, [Some(vec![3, 2, 1, 0]), None]);
    assert!(!header.collocated_from_l0);
    assert_eq!(header.collocated_ref_idx, 1);
    assert!(!header.deblocking_disabled);
    assert_eq!((header.beta_offset_div2, header.tc_offset_div2), (1, -1));
    let weights = header.weights.unwrap();
    assert_eq!(weights.luma_log2_denom, 2);
    // Entry 0 and 2 carry luma weights, entry 0 chroma weights.
    assert_eq!(&weights.lists[0].delta_luma_weight[..4], &[1, 0, 3, 0]);
    assert_eq!(&weights.lists[0].luma_offset[..4], &[0, 0, 4, 0]);
    assert_eq!(weights.lists[0].delta_chroma_weight[0], [1, 1]);
    // 128 + 3 - ((128 * (4 + 1)) >> 2) = -29.
    assert_eq!(weights.lists[0].chroma_offset[0], [-29, -29]);
    assert_eq!(weights.lists[0].delta_chroma_weight[1], [0, 0]);
    assert_eq!(&weights.lists[1].delta_luma_weight[..2], &[1, 0]);
    assert_eq!(header.five_minus_max_num_merge_cand, 1);
}

#[test]
fn the_long_term_msb_cycle_carries_through_an_entry_without_the_flag() {
    let config = StreamConfig {
        long_term_sps: Some(vec![(0, true)]),
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 1, 3);
    spec.rps = SliceRps::Inline(RpsCoding::Explicit(set(&[(-1, true)], &[])));
    let entry = |poc_lsb, msb_cycle| LongTermSpec::Explicit {
        poc_lsb,
        used: false,
        msb_cycle,
    };
    spec.long_term = vec![entry(1, Some(1)), entry(2, None), entry(3, Some(0))];
    let header = sets.slice(&spec.nal(&config), None).unwrap().header;
    let totals: Vec<_> = header
        .rps
        .long_term
        .iter()
        .map(|entry| (entry.msb_cycle, entry.msb_present))
        .collect();
    assert_eq!(totals, [(1, true), (1, false), (1, true)]);
}

#[test]
fn a_monochrome_slice_header_has_no_chroma_elements() {
    let config = StreamConfig {
        chroma_format_idc: 0,
        sao: true,
        weighted_pred: true,
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 1, 1);
    spec.rps = SliceRps::Inline(RpsCoding::Explicit(set(&[(-1, true)], &[])));
    let header = sets.slice(&spec.nal(&config), None).unwrap().header;
    assert!(header.sao_luma);
    assert!(!header.sao_chroma);
    let weights = header.weights.unwrap();
    assert_eq!(weights.delta_chroma_log2_denom, 0);
    assert_eq!(weights.lists[0].delta_chroma_weight[0], [0, 0]);
    assert_eq!(weights.lists[0].delta_luma_weight[0], 1);
    // The elements after the weights are where the syntax puts them.
    assert_eq!(header.five_minus_max_num_merge_cand, 1);
}

#[test]
fn a_p_slice_with_the_defaults_takes_one_reference() {
    let config = StreamConfig {
        default_ref_idx_minus1: [1, 0],
        weighted_pred: true,
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 1, 4);
    spec.rps = SliceRps::Inline(RpsCoding::Explicit(set(&[(-4, true)], &[])));
    let slice = sets.slice(&spec.nal(&config), None).unwrap();
    assert_eq!(slice.header.num_ref_idx_active, [2, 0]);
    assert!(slice.header.weights.is_some());
    assert!(slice.header.st_rps_bits > 0);
}

#[test]
fn a_slice_of_an_inter_picture_without_references_is_an_error() {
    let config = StreamConfig::default();
    let sets = sets(&config);
    let spec = SliceSpec::new(1, 1, 4);
    assert!(sets.slice(&spec.nal(&config), None).is_err());
}

#[test]
fn an_inline_set_can_be_predicted_from_one_of_the_sequence() {
    let config = StreamConfig {
        short_term_sets: vec![
            RpsCoding::Explicit(set(&[(-2, true)], &[])),
            RpsCoding::Explicit(set(&[(-1, true), (-3, true)], &[])),
        ],
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 1, 5);
    // delta_idx_minus1 0 predicts from the last set (-1, -3), one back.
    spec.rps = SliceRps::Inline(RpsCoding::Inter {
        delta_idx_minus1: 0,
        delta_rps: -1,
        flags: vec![(true, true), (true, true), (true, true)],
    });
    let header = sets.slice(&spec.nal(&config), None).unwrap().header;
    assert_eq!(
        header.rps.short_term,
        set(&[(-1, true), (-2, true), (-4, true)], &[])
    );
    // delta_idx_minus1 1 predicts from the first set.
    spec.rps = SliceRps::Inline(RpsCoding::Inter {
        delta_idx_minus1: 1,
        delta_rps: -1,
        flags: vec![(true, true), (true, true)],
    });
    let header = sets.slice(&spec.nal(&config), None).unwrap().header;
    assert_eq!(header.rps.short_term, set(&[(-1, true), (-3, true)], &[]));
}

#[test]
fn dependent_slice_segments_take_the_header_before_them() {
    let config = StreamConfig {
        dependent_slices: true,
        tiles: Some((2, 1)),
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut first = SliceSpec::idr();
    first.qp_delta = 5;
    first.entry_points = vec![10];
    let first = sets.slice(&first.nal(&config), None).unwrap();
    assert_eq!(first.header.num_entry_points, 1);
    let mut second = SliceSpec::idr();
    second.first = false;
    second.dependent = true;
    second.address = 8;
    second.entry_points = vec![4, 5, 6];
    let nal = second.nal(&config);
    assert!(sets.slice(&nal, None).is_err());
    let second = sets.slice(&nal, Some(&first.header)).unwrap().header;
    assert!(second.dependent && !second.first_slice_segment_in_pic);
    assert_eq!(second.segment_address, 8);
    assert_eq!(second.qp_delta, 5);
    assert_eq!(second.num_entry_points, 3);
    // 5 by 4 coding tree blocks: five address bits reach 31.
    let config = StreamConfig {
        width: 80,
        ..StreamConfig::default()
    };
    let sets = self::sets(&config);
    let mut outside = SliceSpec::idr();
    outside.first = false;
    outside.address = 25;
    assert!(sets.slice(&outside.nal(&config), None).is_err());
    outside.address = 19;
    assert!(sets.slice(&outside.nal(&config), None).is_ok());
}

#[test]
fn slices_of_missing_parameter_sets_are_errors() {
    let config = StreamConfig::default();
    let nal = SliceSpec::idr().nal(&config);
    let mut sets = ParameterSets::default();
    assert_eq!(
        sets.slice(&nal, None).unwrap_err(),
        H265Error::MissingParameterSet("picture parameter set")
    );
    sets.put_pps(&config.pps()).unwrap();
    assert_eq!(
        sets.slice(&nal, None).unwrap_err(),
        H265Error::MissingParameterSet("sequence parameter set")
    );
}

#[test]
fn truncated_and_garbled_units_are_errors_not_panics() {
    let config = StreamConfig {
        short_term_sets: vec![RpsCoding::Explicit(set(&[(-1, true)], &[]))],
        tiles: Some((2, 2)),
        weighted_pred: true,
        ..StreamConfig::default()
    };
    let sets = sets(&config);
    let mut spec = SliceSpec::new(1, 1, 3);
    spec.rps = SliceRps::Sps(0);
    spec.entry_points = vec![1, 2, 3];
    let nal = spec.nal(&config);
    for length in 0..nal.len() {
        let _ = sets.slice(&nal[..length], None);
    }
    for index in 2..nal.len() {
        for flip in [0x01, 0x80, 0xFF] {
            let mut garbled = nal.clone();
            garbled[index] ^= flip;
            let _ = sets.slice(&garbled, None);
        }
    }
    for unit in [config.vps(), config.sps(), config.pps()] {
        for length in 0..unit.len() {
            let _ = Vps::parse(&unit[..length]);
            let _ = Sps::parse(&unit[..length]);
            let _ = Pps::parse(&unit[..length]);
        }
    }
    assert!(sets.slice(&nal_header(40, 0), None).is_err());
}
