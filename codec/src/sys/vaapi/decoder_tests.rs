//! The decoder against the driver double: what it submits and what it
//! outputs, on streams built by `h264::testing`.

use super::{
    VaapiError,
    decoder::{DecodedPicture, VaapiDecoder, apply_weights, copy_nv12_cropped},
    double::{Driver, chroma, fill_of, luma},
    plan::Nv12Planes,
    types::{
        VA_INVALID_ID, VA_PICTURE_H264_INVALID, VA_PICTURE_H264_NON_EXISTING,
        VA_PICTURE_H264_SHORT_TERM_REFERENCE, VAProfileH264ConstrainedBaseline, VAProfileH264Main,
        VASliceParameterBufferH264,
    },
};
use crate::{
    CodecError,
    annexb::START_CODE,
    bitstream::{annex_b_to_length_prefixed, build_h264_avcc_from_annex_b},
    h264::{
        H264Error,
        sequence::Crop,
        slice::{EntryWeights, PredWeights},
        testing::{SliceSpec, StreamConfig, TestPoc, annex_b},
    },
};
use std::path::Path;

fn open(config: Option<&[u8]>) -> Result<VaapiDecoder, VaapiError> {
    VaapiDecoder::with_api(Driver::api(), Path::new("/dev/null"), config)
}

fn decoder() -> (Driver, VaapiDecoder) {
    let driver = Driver::new();
    let decoder = open(None).unwrap();
    (driver, decoder)
}

/// A Baseline stream: no reordering, so every picture is output at once.
fn baseline() -> StreamConfig {
    StreamConfig {
        profile_idc: 66,
        ..StreamConfig::default()
    }
}

/// A Main stream announcing one reordered frame.
fn reordering() -> StreamConfig {
    StreamConfig {
        max_num_ref_frames: 2,
        restrictions: Some((1, 3)),
        ..StreamConfig::default()
    }
}

/// An access unit of the slices of `specs`, led by the parameter sets when
/// `with_parameters`.
fn access_unit(config: &StreamConfig, with_parameters: bool, specs: &[SliceSpec]) -> Vec<u8> {
    let mut nals = Vec::new();
    if with_parameters {
        nals.extend([config.sps(), config.pps()]);
    }
    nals.extend(specs.iter().map(|spec| config.slice(spec)));
    annex_b(&nals)
}

fn decode(
    decoder: &mut VaapiDecoder,
    config: &StreamConfig,
    first: bool,
    spec: SliceSpec,
    pts: u64,
) -> Vec<DecodedPicture> {
    decoder
        .decode(&access_unit(config, first, &[spec]), pts)
        .unwrap()
}

/// Which decode (counting from 1) produced `frame`, read from its first
/// luma sample.
fn decode_index(frame: &DecodedPicture) -> usize {
    (1..=251)
        .find(|&n| fill_of(n) == frame.data[0])
        .expect("the fill identifies the decode")
}

#[test]
fn a_baseline_stream_is_output_as_it_is_decoded() {
    let (driver, mut decoder) = decoder();
    let config = baseline();
    let first = decode(&mut decoder, &config, true, SliceSpec::idr(), 100);
    assert_eq!(first.len(), 1);
    let frame = &first[0];
    assert_eq!(
        (frame.width, frame.height, frame.timestamp_ns),
        (32, 32, 100)
    );
    assert_eq!(frame.data.len(), 32 * 32 * 3 / 2);
    assert_eq!(decode_index(frame), 1);
    // Luma and chroma come from the right rows of the padded image.
    assert_eq!(frame.data[5 + 7 * 32], luma(fill_of(1), 5, 7));
    assert_eq!(frame.data[32 * 32 + 6 + 3 * 32], chroma(fill_of(1), 6, 3));

    for (index, (frame_num, pts)) in [(1, 200), (2, 300)].into_iter().enumerate() {
        let frames = decode(
            &mut decoder,
            &config,
            false,
            SliceSpec::p(frame_num, 2 * frame_num),
            pts,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].timestamp_ns, pts);
        assert_eq!(decode_index(&frames[0]), index + 2);
    }
    assert_eq!(driver.pictures().len(), 3);
    assert!(decoder.drain().unwrap().is_empty());
}

#[test]
fn the_profile_follows_the_stream_and_falls_back_to_larger_ones() {
    let (mut driver, mut decoder) = decoder();
    decode(&mut decoder, &baseline(), true, SliceSpec::idr(), 0);
    assert_eq!(driver.config_profiles(), [VAProfileH264ConstrainedBaseline]);
    drop(decoder);

    driver.set_profiles(&[VAProfileH264Main]);
    let mut decoder = open(None).unwrap();
    decode(&mut decoder, &baseline(), true, SliceSpec::idr(), 0);
    assert_eq!(
        driver.config_profiles(),
        [VAProfileH264ConstrainedBaseline, VAProfileH264Main]
    );
}

#[test]
fn reordered_pictures_are_output_by_picture_order_count() {
    let (driver, mut decoder) = decoder();
    let config = reordering();
    let mut frames = Vec::new();
    // Decode order I0 P4 B2 P8 B6, display order by POC.
    let stream = [
        (SliceSpec::idr(), 0),
        (SliceSpec::p(1, 4), 2),
        (SliceSpec::b(2, 2), 1),
        (SliceSpec::p(2, 8), 4),
        (SliceSpec::b(3, 6), 3),
    ];
    for (index, (spec, pts)) in stream.into_iter().enumerate() {
        frames.extend(decode(&mut decoder, &config, index == 0, spec, pts));
    }
    frames.extend(decoder.drain().unwrap());
    let timestamps: Vec<u64> = frames.iter().map(|frame| frame.timestamp_ns).collect();
    assert_eq!(timestamps, [0, 1, 2, 3, 4]);
    let order: Vec<usize> = frames.iter().map(decode_index).collect();
    assert_eq!(order, [1, 3, 2, 5, 4]);
    assert_eq!(driver.pictures().len(), 5);
}

#[test]
fn main_without_reorder_information_waits_for_the_buffer_or_the_end() {
    let (_driver, mut decoder) = decoder();
    let config = StreamConfig::default();
    assert!(decode(&mut decoder, &config, true, SliceSpec::idr(), 1).is_empty());
    assert!(decode(&mut decoder, &config, false, SliceSpec::p(1, 2), 2).is_empty());
    let drained = decoder.drain().unwrap();
    let timestamps: Vec<u64> = drained.iter().map(|frame| frame.timestamp_ns).collect();
    assert_eq!(timestamps, [1, 2]);
}

#[test]
fn cropping_removes_the_signalled_edges() {
    let (_driver, mut decoder) = decoder();
    let config = StreamConfig {
        // Left, right, top and bottom, in units of two samples.
        crop: Some([1, 2, 0, 1]),
        ..baseline()
    };
    let frames = decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    let frame = &frames[0];
    assert_eq!((frame.width, frame.height), (26, 30));
    assert_eq!(frame.data.len(), 26 * 30 * 3 / 2);
    let fill = fill_of(1);
    assert_eq!(frame.data[0], luma(fill, 2, 0));
    assert_eq!(frame.data[3 + 4 * 26], luma(fill, 5, 4));
    let chroma_base = 26 * 30;
    assert_eq!(frame.data[chroma_base], chroma(fill, 2, 0));
    assert_eq!(frame.data[chroma_base + 1 + 2 * 26], chroma(fill, 3, 2));
}

#[test]
fn the_hardware_is_told_what_the_stream_says() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        cabac: true,
        ..baseline()
    };
    let idr = SliceSpec::idr();
    let (nal, header_bits) = config.slice_with_offset(&idr);
    let mut sample = config.parameter_sets();
    sample.extend_from_slice(&START_CODE);
    sample.extend_from_slice(&nal);
    decoder.decode(&sample, 0).unwrap();

    let picture = &driver.pictures()[0];
    let parameters = picture.parameters;
    assert_eq!(
        (
            parameters.picture_width_in_mbs_minus1,
            parameters.picture_height_in_mbs_minus1
        ),
        (1, 1)
    );
    assert_eq!(parameters.CurrPic.picture_id, picture.target);
    assert_eq!(parameters.CurrPic.flags, 0);
    assert!(parameters.ReferenceFrames.iter().all(|reference| {
        reference.flags == VA_PICTURE_H264_INVALID && reference.picture_id == VA_INVALID_ID
    }));
    assert_eq!(parameters.num_ref_frames, 4);
    assert_eq!(parameters.frame_num, 0);
    // chroma_format_idc 1, frame_mbs_only, direct_8x8_inference, level 4.0
    // (MinLumaBiPredSize8x8), log2_max_frame_num_minus4 0, POC type 0 and an
    // 8 bit POC lsb.
    assert_eq!(
        parameters.seq_fields,
        1 | (1 << 4) | (1 << 6) | (1 << 7) | (4 << 14)
    );
    // entropy_coding_mode, deblocking filter control and a reference picture.
    assert_eq!(parameters.pic_fields, 1 | (1 << 8) | (1 << 10));
    assert_eq!(picture.slices.len(), 1);
    let (slice, data) = &picture.slices[0];
    assert_eq!(data, &nal);
    assert_eq!(usize::from(slice.slice_data_bit_offset), 8 + header_bits);
    assert_eq!(slice.slice_type, 2);
    assert_eq!(slice.first_mb_in_slice, 0);
    // The scaling lists are flat without a matrix.
    assert!(
        picture
            .scaling
            .ScalingList4x4
            .iter()
            .flatten()
            .all(|&entry| entry == 16)
    );
    assert!(
        picture
            .scaling
            .ScalingList8x8
            .iter()
            .flatten()
            .all(|&entry| entry == 16)
    );
}

#[test]
fn p_slices_name_the_surfaces_of_their_references() {
    let (driver, mut decoder) = decoder();
    let config = baseline();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    decode(&mut decoder, &config, false, SliceSpec::p(1, 2), 1);
    decode(&mut decoder, &config, false, SliceSpec::p(2, 4), 2);

    let pictures = driver.pictures();
    let (second, _) = &pictures[2].slices[0];
    assert_eq!(second.slice_type, 0);
    assert_eq!(second.num_ref_idx_l0_active_minus1, 0);
    // List 0 is ordered by descending PicNum: the previous picture first.
    let newest = second.RefPicList0[0];
    assert_eq!(newest.picture_id, pictures[1].target);
    assert_eq!(newest.frame_idx, 1);
    assert_eq!(newest.flags, VA_PICTURE_H264_SHORT_TERM_REFERENCE);
    assert_eq!(newest.TopFieldOrderCnt, 2);
    assert!(
        second.RefPicList0[1..]
            .iter()
            .all(|entry| entry.flags == VA_PICTURE_H264_INVALID)
    );
    // Both earlier pictures are in the picture parameters.
    let references: Vec<u32> = pictures[2]
        .parameters
        .ReferenceFrames
        .iter()
        .filter(|reference| reference.flags != VA_PICTURE_H264_INVALID)
        .map(|reference| reference.picture_id)
        .collect();
    assert_eq!(references.len(), 2);
    assert!(references.contains(&pictures[0].target));
    assert!(references.contains(&pictures[1].target));
}

#[test]
fn the_surface_pool_is_reused_for_the_whole_stream() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        log2_max_frame_num: 4,
        ..baseline()
    };
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    for index in 1..100_u32 {
        decode(
            &mut decoder,
            &config,
            false,
            SliceSpec::p(index % 16, (2 * index) % 256),
            u64::from(index),
        );
    }
    let mut targets: Vec<u32> = driver
        .pictures()
        .iter()
        .map(|picture| picture.target)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    // The frames of the buffer plus the one being decoded, at most.
    assert!(targets.len() <= 17, "{} surfaces used", targets.len());
    assert_eq!(driver.surfaces(), 17);
}

#[test]
fn a_stream_joined_before_an_idr_picture_is_skipped_until_it() {
    let (driver, mut decoder) = decoder();
    let config = baseline();
    let skipped = decoder
        .decode(&access_unit(&config, true, &[SliceSpec::p(3, 6)]), 0)
        .unwrap();
    assert!(skipped.is_empty());
    assert!(driver.pictures().is_empty());
    let frames = decode(&mut decoder, &config, false, SliceSpec::idr(), 1);
    assert_eq!(frames.len(), 1);
}

#[test]
fn an_idr_picture_with_new_parameters_starts_a_new_sequence() {
    let (driver, mut decoder) = decoder();
    let first = StreamConfig {
        max_num_ref_frames: 1,
        ..StreamConfig::default()
    };
    assert!(decode(&mut decoder, &first, true, SliceSpec::idr(), 1).is_empty());
    assert_eq!(driver.surface_size(), (32, 32));
    let second = StreamConfig {
        width_mbs: 3,
        ..baseline()
    };
    // The frame waiting in the old sequence is output before the new one
    // starts, in its own size.
    let frames = decode(&mut decoder, &second, true, SliceSpec::idr(), 2);
    assert_eq!(frames.len(), 2);
    assert_eq!((frames[0].width, frames[0].timestamp_ns), (32, 1));
    assert_eq!((frames[1].width, frames[1].timestamp_ns), (48, 2));
    assert_eq!(driver.surface_size(), (48, 32));
    assert_eq!(driver.surfaces(), 17);
}

#[test]
fn changed_parameters_outside_an_idr_picture_are_an_error() {
    let (_driver, mut decoder) = decoder();
    let config = baseline();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    let other = StreamConfig {
        width_mbs: 3,
        ..config
    };
    let error = decoder
        .decode(&access_unit(&other, true, &[SliceSpec::p(1, 2)]), 1)
        .unwrap_err();
    assert!(matches!(
        error,
        VaapiError::Stream(H264Error::SequenceChange)
    ));
}

#[test]
fn unsupported_streams_are_reported_as_such() {
    let (_driver, mut decoder) = decoder();
    // High 4:4:4 Predictive.
    let config = StreamConfig {
        profile_idc: 244,
        ..StreamConfig::default()
    };
    let error = decoder
        .decode(&access_unit(&config, true, &[SliceSpec::idr()]), 0)
        .unwrap_err();
    assert!(
        matches!(&error, VaapiError::Stream(error) if error.is_unsupported()),
        "{error}"
    );
    assert!(matches!(error.at_decode(), CodecError::Unsupported(_)));
}

#[test]
fn a_malformed_slice_is_an_error_and_the_next_idr_picture_decodes() {
    let (driver, mut decoder) = decoder();
    let config = baseline();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    let mut broken = config.slice(&SliceSpec::p(1, 2));
    broken.truncate(2);
    let error = decoder.decode(&annex_b(&[broken]), 1).unwrap_err();
    assert!(
        matches!(error.clone().at_decode(), CodecError::DecodingFailed(_)),
        "{error}"
    );
    let frames = decode(&mut decoder, &config, false, SliceSpec::idr(), 2);
    assert_eq!(frames.len(), 1);
    assert_eq!(driver.pictures().len(), 2);
}

#[test]
fn a_driver_failure_drops_the_picture_and_decoding_goes_on() {
    let (mut driver, mut decoder) = decoder();
    let config = baseline();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    driver.fail("vaEndPicture", 0x02);
    let error = decoder
        .decode(&access_unit(&config, false, &[SliceSpec::p(1, 2)]), 1)
        .unwrap_err();
    assert!(
        matches!(error.clone().at_decode(), CodecError::DecodingFailed(_)),
        "{error}"
    );
    driver.clear_failure();
    // The failed picture left no frame behind and its surface came back.
    let frames = decode(&mut decoder, &config, false, SliceSpec::idr(), 2);
    assert_eq!(frames.len(), 1);
}

#[test]
fn a_sliced_picture_is_submitted_once_with_all_its_slices() {
    let (driver, mut decoder) = decoder();
    let config = baseline();
    let mut second = SliceSpec::idr();
    second.first_mb = 2;
    let frames = decoder
        .decode(&access_unit(&config, true, &[SliceSpec::idr(), second]), 0)
        .unwrap();
    assert_eq!(frames.len(), 1);
    let pictures = driver.pictures();
    assert_eq!(pictures.len(), 1);
    let firsts: Vec<u16> = pictures[0]
        .slices
        .iter()
        .map(|(slice, _)| slice.first_mb_in_slice)
        .collect();
    assert_eq!(firsts, [0, 2]);
}

#[test]
fn a_slice_outside_the_picture_is_rejected_before_the_driver_sees_it() {
    let (driver, mut decoder) = decoder();
    let mut spec = SliceSpec::idr();
    spec.first_mb = 4;
    let error = decoder
        .decode(&access_unit(&baseline(), true, &[spec]), 0)
        .unwrap_err();
    assert!(
        matches!(error, VaapiError::Stream(H264Error::Invalid(_))),
        "{error}"
    );
    assert!(driver.pictures().is_empty());
}

#[test]
fn frame_num_gaps_reference_inferred_frames() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        gaps_allowed: true,
        ..baseline()
    };
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    decode(&mut decoder, &config, false, SliceSpec::p(3, 6), 1);
    let pictures = driver.pictures();
    let references: Vec<_> = pictures[1]
        .parameters
        .ReferenceFrames
        .iter()
        .filter(|reference| reference.flags != VA_PICTURE_H264_INVALID)
        .collect();
    let mut frame_nums: Vec<u32> = references
        .iter()
        .map(|reference| reference.frame_idx)
        .collect();
    frame_nums.sort_unstable();
    assert_eq!(frame_nums, [0, 1, 2]);
    let inferred = references
        .iter()
        .filter(|reference| reference.flags & VA_PICTURE_H264_NON_EXISTING != 0)
        .count();
    assert_eq!(inferred, 2);
}

#[test]
fn length_prefixed_samples_use_the_avcc_parameter_sets() {
    let config = baseline();
    let avcc = build_h264_avcc_from_annex_b(&config.parameter_sets()).unwrap();
    let driver = Driver::new();
    let mut decoder = open(Some(&avcc)).unwrap();
    let annex = access_unit(&config, false, &[SliceSpec::idr()]);
    let (sample, keyframe) = annex_b_to_length_prefixed(&annex, false);
    assert!(keyframe);
    let frames = decoder.decode(&sample.unwrap(), 7).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].timestamp_ns, 7);
    assert_eq!(driver.pictures().len(), 1);
    // A truncated sample is a framing error, not a panic.
    let error = decoder.decode(&[0, 0, 0, 9, 1], 8).unwrap_err();
    assert!(matches!(error, VaapiError::Framing(_)), "{error}");
}

#[test]
fn a_driver_without_h264_decoding_is_unsupported() {
    let mut driver = Driver::new();
    driver.set_profiles(&[]);
    let error = open(None).unwrap_err();
    assert!(matches!(error, VaapiError::NoDecodeEntrypoint), "{error}");
    assert!(matches!(error.at_open(), CodecError::Unsupported(_)));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_host_without_libva_gets_a_clear_error_from_the_constructor() {
    let error = VaapiDecoder::new(None).unwrap_err();
    assert!(
        matches!(
            &error,
            VaapiError::LibraryUnavailable {
                library: "libva.so.2",
                ..
            }
        ),
        "{error}"
    );
    assert!(matches!(error.at_open(), CodecError::Unsupported(_)));
}

#[test]
fn a_malformed_avcc_record_fails_the_constructor() {
    let _driver = Driver::new();
    // A record that announces a five byte SPS and ends.
    let error = open(Some(&[1, 0x4d, 0, 0x28, 0xff, 0xe1, 0, 5])).unwrap_err();
    assert!(matches!(error, VaapiError::Framing(_)), "{error}");
}

#[test]
fn a_driver_without_the_render_target_format_is_unsupported() {
    let (mut driver, mut decoder) = decoder();
    driver.set_rt_format(0x2);
    let error = decoder
        .decode(&access_unit(&baseline(), true, &[SliceSpec::idr()]), 0)
        .unwrap_err();
    assert!(matches!(error, VaapiError::UnsupportedRtFormat), "{error}");
}

#[test]
fn dropping_the_decoder_destroys_everything_before_terminating() {
    let (driver, mut decoder) = decoder();
    decode(&mut decoder, &baseline(), true, SliceSpec::idr(), 0);
    drop(decoder);
    let log = driver.log();
    assert_eq!(log.last(), Some(&"vaTerminate"));
    for call in [
        "vaDestroyImage",
        "vaDestroyContext",
        "vaDestroySurfaces",
        "vaDestroyConfig",
    ] {
        assert_eq!(
            log.iter().filter(|name| **name == call).count(),
            1,
            "{call}"
        );
    }
    assert_eq!(driver.surfaces(), 0);
}

#[test]
fn drain_ends_the_stream_and_an_idr_picture_restarts_it() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig::default();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 1);
    assert_eq!(decoder.drain().unwrap().len(), 1);
    assert_eq!(driver.surfaces(), 0);
    decode(&mut decoder, &config, false, SliceSpec::idr(), 2);
    assert_eq!(decoder.drain().unwrap().len(), 1);
}

#[test]
fn picture_order_count_types_one_and_two_decode() {
    for poc in [
        TestPoc::Two,
        TestPoc::One {
            cycle: vec![2],
            non_ref: -1,
            top_to_bottom: 0,
        },
    ] {
        let (driver, mut decoder) = decoder();
        let config = StreamConfig {
            poc,
            ..StreamConfig::default()
        };
        let mut frames = decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
        frames.extend(decode(&mut decoder, &config, false, SliceSpec::p(1, 0), 1));
        frames.extend(decoder.drain().unwrap());
        assert_eq!(frames.len(), 2);
        assert_eq!(driver.pictures().len(), 2);
    }
}

#[test]
fn explicit_weights_fill_both_lists_and_default_the_other_entries() {
    let weights = PredWeights {
        luma_log2_denom: 5,
        chroma_log2_denom: 3,
        lists: [
            vec![
                EntryWeights {
                    luma: Some((40, -2)),
                    chroma: None,
                },
                EntryWeights {
                    luma: None,
                    chroma: Some([(9, 1), (7, -1)]),
                },
            ],
            Vec::new(),
        ],
    };
    // SAFETY: the struct is plain integers, for which zero is valid.
    let mut parameter: VASliceParameterBufferH264 = unsafe { std::mem::zeroed() };
    apply_weights(&weights, &mut parameter);
    assert_eq!(
        (
            parameter.luma_log2_weight_denom,
            parameter.chroma_log2_weight_denom
        ),
        (5, 3)
    );
    assert_eq!(
        (
            parameter.luma_weight_l0_flag,
            parameter.chroma_weight_l0_flag
        ),
        (1, 1)
    );
    assert_eq!(
        (parameter.luma_weight_l0[0], parameter.luma_offset_l0[0]),
        (40, -2)
    );
    assert_eq!(
        (parameter.luma_weight_l0[1], parameter.luma_offset_l0[1]),
        (32, 0)
    );
    assert_eq!(parameter.chroma_weight_l0[0], [8, 8]);
    assert_eq!(parameter.chroma_weight_l0[1], [9, 7]);
    assert_eq!(parameter.chroma_offset_l0[1], [1, -1]);
    assert_eq!(parameter.luma_weight_l0[31], 32);
    assert_eq!(
        (
            parameter.luma_weight_l1_flag,
            parameter.chroma_weight_l1_flag
        ),
        (0, 0)
    );
}

#[test]
fn cropped_copies_check_the_image_they_read() {
    let planes = Nv12Planes {
        y_offset: 0,
        y_pitch: 6,
        uv_offset: 24,
        uv_pitch: 6,
    };
    let image: Vec<u8> = (0..36).collect();
    let crop = Crop {
        left: 2,
        right: 0,
        top: 2,
        bottom: 0,
    };
    // 4x4 coded, 2x2 visible.
    let frame = copy_nv12_cropped(&image, planes, (4, 4), crop, (2, 2)).unwrap();
    assert_eq!(frame, [14, 15, 20, 21, 32, 33]);
    assert!(matches!(
        copy_nv12_cropped(&image[..30], planes, (4, 4), crop, (2, 2)),
        Err(VaapiError::BufferTooSmall(_))
    ));
    assert!(matches!(
        copy_nv12_cropped(&image, planes, (4, 4), crop, (3, 2)),
        Err(VaapiError::BufferTooSmall(_))
    ));
}

#[test]
fn b_slices_carry_both_lists() {
    let (driver, mut decoder) = decoder();
    let config = reordering();
    decode(&mut decoder, &config, true, SliceSpec::idr(), 0);
    decode(&mut decoder, &config, false, SliceSpec::p(1, 4), 2);
    decode(&mut decoder, &config, false, SliceSpec::b(2, 2), 1);
    let pictures = driver.pictures();
    let (slice, _) = &pictures[2].slices[0];
    assert_eq!(slice.slice_type, 1);
    // List 0 holds the earlier picture first, list 1 the later one.
    assert_eq!(slice.RefPicList0[0].picture_id, pictures[0].target);
    assert_eq!(slice.RefPicList1[0].picture_id, pictures[1].target);
    assert_eq!(
        pictures[2].parameters.pic_fields & (1 << 10),
        0,
        "B is not a reference"
    );
}
