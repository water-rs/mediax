//! The H.265 decoder against the driver double: what it submits and what it
//! outputs, on streams built by `h265::testing`.

use super::{
    VaapiDecoder, VaapiError,
    double::{Driver, chroma, fill_of, luma},
    hevc::HevcDecoder,
    target::DecodedPicture,
    types::{
        VA_HEVC_REF_INVALID, VA_PICTURE_HEVC_RPS_ST_CURR_AFTER, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
        VAProfileHEVCMain, VAProfileHEVCMain10,
    },
};
use crate::{
    CodecError, DecodedPixelLayout,
    bitstream::{annex_b_to_length_prefixed, build_h265_hvcc_from_annex_b},
    h265::{
        H265Error,
        dpb::{RpsEntry, ShortTermRps},
        error::Unsupported,
        testing::{RpsCoding, ScalingData, SliceRps, SliceSpec, StreamConfig, annex_b, nal_header},
    },
};
use std::path::Path;

fn open(config: Option<&[u8]>) -> Result<HevcDecoder, VaapiError> {
    HevcDecoder::with_api(Driver::api(), Path::new("/dev/null"), config)
}

fn decoder() -> (Driver, HevcDecoder) {
    let driver = Driver::new();
    let decoder = open(None).unwrap();
    (driver, decoder)
}

/// A stream without reordering: every picture is output at once.
fn low_delay() -> StreamConfig {
    StreamConfig {
        max_dec_pic_buffering: 3,
        max_num_reorder: 0,
        ..StreamConfig::default()
    }
}

fn set(negative: &[i32], positive: &[i32]) -> RpsCoding {
    let entries = |list: &[i32]| {
        list.iter()
            .map(|&delta_poc| RpsEntry {
                delta_poc,
                used_by_curr: true,
            })
            .collect()
    };
    RpsCoding::Explicit(ShortTermRps {
        negative: entries(negative),
        positive: entries(positive),
    })
}

/// A P picture of POC `poc` referring to the picture `distance` before it.
fn p(poc: u32, distance: i32) -> SliceSpec {
    let mut spec = SliceSpec::new(1, 1, poc);
    spec.rps = SliceRps::Inline(set(&[-distance], &[]));
    spec
}

/// A B picture between the pictures `before` and `after` away from it.
fn b(poc: u32, before: i32, after: i32) -> SliceSpec {
    let mut spec = SliceSpec::new(0, 0, poc);
    spec.rps = SliceRps::Inline(set(&[-before], &[after]));
    spec
}

/// An access unit of the NAL units `nals`, led by the parameter sets when
/// `with_parameters`.
fn access_unit(config: &StreamConfig, with_parameters: bool, nals: Vec<Vec<u8>>) -> Vec<u8> {
    let mut all = Vec::new();
    if with_parameters {
        all.extend(config.parameter_sets());
    }
    all.extend(nals);
    annex_b(&all)
}

fn decode(
    decoder: &mut HevcDecoder,
    config: &StreamConfig,
    first: bool,
    spec: &SliceSpec,
    pts: u64,
) -> Vec<DecodedPicture> {
    decoder
        .decode(&access_unit(config, first, vec![spec.nal(config)]), pts)
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
fn a_low_delay_stream_is_output_as_it_is_decoded() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    let first = decode(&mut decoder, &config, true, &SliceSpec::idr(), 100);
    assert_eq!(first.len(), 1);
    let frame = &first[0];
    assert_eq!(
        (frame.width, frame.height, frame.timestamp_ns),
        (64, 64, 100)
    );
    assert_eq!(frame.layout, DecodedPixelLayout::Nv12);
    assert_eq!(frame.data.len(), 64 * 64 * 3 / 2);
    assert_eq!(decode_index(frame), 1);
    // Luma and chroma come from the right rows of the padded image.
    assert_eq!(frame.data[5 + 7 * 64], luma(fill_of(1), 5, 7));
    assert_eq!(frame.data[64 * 64 + 6 + 3 * 64], chroma(fill_of(1), 6, 3));

    for (poc, pts) in [(1, 200), (2, 300)] {
        let frames = decode(&mut decoder, &config, false, &p(poc, 1), pts);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].timestamp_ns, pts);
        assert_eq!(decode_index(&frames[0]), poc as usize + 1);
    }
    assert_eq!(driver.hevc_pictures().len(), 3);
    assert!(decoder.drain().unwrap().is_empty());
    assert_eq!(driver.config_profiles(), [VAProfileHEVCMain]);
}

#[test]
fn a_p_picture_names_its_reference_in_both_lists_of_the_driver() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    decode(&mut decoder, &config, false, &p(1, 1), 1);
    let pictures = driver.hevc_pictures();
    let parameters = pictures[1].parameters;
    let reference = parameters.ReferenceFrames[0];
    assert_eq!(reference.picture_id, pictures[0].target);
    assert_eq!(reference.pic_order_cnt, 0);
    assert_eq!(reference.flags, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE);
    assert_eq!(parameters.CurrPic.pic_order_cnt, 1);
    let (slice, data) = &pictures[1].slices[0];
    assert_eq!(slice.RefPicList[0][0], 0);
    assert_eq!(slice.RefPicList[0][1], VA_HEVC_REF_INVALID);
    assert_eq!(slice.RefPicList[1], [VA_HEVC_REF_INVALID; 15]);
    assert_eq!(slice.slice_data_size as usize, data.len());
    // An I slice names nothing.
    let (intra, _) = &pictures[0].slices[0];
    assert_eq!(intra.RefPicList, [[VA_HEVC_REF_INVALID; 15]; 2]);
    assert_eq!(
        pictures[0].parameters.ReferenceFrames[0].picture_id,
        u32::MAX
    );
}

#[test]
fn reordered_pictures_are_output_by_picture_order_count() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig::default();
    let mut frames = Vec::new();
    // Decode order I0 P4 B2 P8 B6, display order by POC.
    frames.extend(decode(&mut decoder, &config, true, &SliceSpec::idr(), 0));
    frames.extend(decode(&mut decoder, &config, false, &p(4, 4), 40));
    frames.extend(decode(&mut decoder, &config, false, &b(2, 2, 2), 20));
    frames.extend(decode(&mut decoder, &config, false, &p(8, 4), 80));
    frames.extend(decode(&mut decoder, &config, false, &b(6, 2, 2), 60));
    frames.extend(decoder.drain().unwrap());
    let pts: Vec<_> = frames.iter().map(|frame| frame.timestamp_ns).collect();
    assert_eq!(pts, [0, 20, 40, 60, 80]);
    let order: Vec<_> = frames.iter().map(decode_index).collect();
    assert_eq!(order, [1, 3, 2, 5, 4]);

    // The B picture refers to both neighbours.
    let pictures = driver.hevc_pictures();
    let parameters = pictures[2].parameters;
    let flags: Vec<_> = parameters
        .ReferenceFrames
        .iter()
        .take(2)
        .map(|reference| (reference.picture_id, reference.flags))
        .collect();
    assert!(flags.contains(&(pictures[0].target, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE)));
    assert!(flags.contains(&(pictures[1].target, VA_PICTURE_HEVC_RPS_ST_CURR_AFTER)));
    let (slice, _) = &pictures[2].slices[0];
    assert_eq!(slice.LongSliceFlags >> 2 & 3, 0);
    let list0 = parameters.ReferenceFrames[usize::from(slice.RefPicList[0][0])];
    let list1 = parameters.ReferenceFrames[usize::from(slice.RefPicList[1][0])];
    assert_eq!(list0.picture_id, pictures[0].target);
    assert_eq!(list1.picture_id, pictures[1].target);
}

#[test]
fn a_ten_bit_stream_decodes_to_p010() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        profile_idc: 2,
        bit_depth: 10,
        ..low_delay()
    };
    let frames = decode(&mut decoder, &config, true, &SliceSpec::idr(), 7);
    assert_eq!(driver.config_profiles(), [VAProfileHEVCMain10]);
    let frame = &frames[0];
    assert_eq!(frame.layout, DecodedPixelLayout::P010);
    assert_eq!(frame.data.len(), 64 * 64 * 3);
    // 16-bit little-endian samples with the value in the high bits.
    assert_eq!(frame.data[..2], [0, luma(fill_of(1), 0, 0)]);
    let row = 3 * 128 + 2 * 5;
    assert_eq!(frame.data[row + 1], luma(fill_of(1), 5, 3));
    let chroma_row = 64 * 64 * 2 + 128 + 2 * 4;
    assert_eq!(frame.data[chroma_row + 1], chroma(fill_of(1), 4, 1));
    assert_eq!(
        driver.hevc_pictures()[0].parameters.bit_depth_luma_minus8,
        2
    );
}

#[test]
fn the_conformance_window_is_cropped() {
    let (_driver, mut decoder) = decoder();
    // Offsets in chroma units: 2 luma columns on the left, 6 rows at the top.
    let config = StreamConfig {
        crop: Some([1, 2, 3, 4]),
        ..low_delay()
    };
    let frames = decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let frame = &frames[0];
    assert_eq!((frame.width, frame.height), (58, 50));
    assert_eq!(frame.data.len(), 58 * 50 * 3 / 2);
    assert_eq!(frame.data[0], luma(fill_of(1), 2, 6));
    assert_eq!(frame.data[58 + 4], luma(fill_of(1), 6, 7));
    assert_eq!(frame.data[58 * 50], chroma(fill_of(1), 2, 3));
}

#[test]
fn scaling_lists_are_sent_only_when_enabled() {
    let (driver, mut decoder) = decoder();
    decode(&mut decoder, &low_delay(), true, &SliceSpec::idr(), 0);
    assert!(driver.hevc_pictures()[0].scaling.is_none());
    drop(decoder);

    let driver = Driver::new();
    let mut decoder = open(None).unwrap();
    let config = StreamConfig {
        scaling: Some(Some(ScalingData::Flat)),
        ..low_delay()
    };
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let matrix = driver.hevc_pictures()[0].scaling.unwrap();
    assert_eq!(matrix.ScalingList4x4, [[16; 16]; 6]);
    assert_eq!(matrix.ScalingList8x8, [[16; 64]; 6]);
    assert_eq!(matrix.ScalingList32x32, [[16; 64]; 2]);
    assert_eq!(matrix.ScalingListDC16x16, [8; 6]);
}

#[test]
fn tiles_reach_the_driver() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        tiles: Some((2, 2)),
        ..low_delay()
    };
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let parameters = driver.hevc_pictures()[0].parameters;
    assert_eq!(parameters.num_tile_columns_minus1, 1);
    assert_eq!(parameters.num_tile_rows_minus1, 1);
    assert_eq!(parameters.column_width_minus1[..2], [1, 1]);
    assert_eq!(parameters.row_height_minus1[..2], [1, 1]);
}

#[test]
fn prediction_weights_reach_the_driver() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        weighted_pred: true,
        ..low_delay()
    };
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    decode(&mut decoder, &config, false, &p(1, 1), 1);
    let (slice, _) = &driver.hevc_pictures()[1].slices[0];
    assert_eq!(slice.luma_log2_weight_denom, 2);
    assert_eq!(slice.delta_luma_weight_l0[0], 1);
    assert_eq!(slice.luma_offset_l0[0], 0);
    assert_eq!(slice.delta_chroma_weight_l0[0], [1, 1]);
    // 128 + 3 - ((128 * (4 + 1)) >> 2)
    assert_eq!(slice.ChromaOffsetL0[0], [-29, -29]);
}

#[test]
fn slice_segments_are_submitted_with_the_last_one_marked() {
    let (driver, mut decoder) = decoder();
    let config = StreamConfig {
        dependent_slices: true,
        ..low_delay()
    };
    let first = SliceSpec::idr();
    let mut second = SliceSpec::idr();
    second.first = false;
    second.address = 4;
    let mut third = second.clone();
    third.dependent = true;
    third.address = 8;
    let nals = [&first, &second, &third]
        .map(|spec| spec.nal(&config))
        .to_vec();
    let frames = decoder
        .decode(&access_unit(&config, true, nals.clone()), 5)
        .unwrap();
    assert_eq!(frames.len(), 1);
    let picture = &driver.hevc_pictures()[0];
    assert_eq!(picture.slices.len(), 3);
    let flags: Vec<_> = picture
        .slices
        .iter()
        .map(|(slice, _)| slice.LongSliceFlags & 0b11)
        .collect();
    assert_eq!(flags, [0b00, 0b00, 0b11]);
    let addresses: Vec<_> = picture
        .slices
        .iter()
        .map(|(slice, _)| slice.slice_segment_address)
        .collect();
    assert_eq!(addresses, [0, 4, 8]);
    for ((_, data), nal) in picture.slices.iter().zip(&nals) {
        assert_eq!(data, nal);
    }
}

#[test]
fn a_cra_skips_its_undecodable_leading_pictures() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    let cra = SliceSpec::new(21, 2, 8);
    let frames = decode(&mut decoder, &config, true, &cra, 0);
    assert_eq!(frames.len(), 1);
    // A RASL picture refers to a picture before the CRA, which never arrived.
    let mut rasl = SliceSpec::new(9, 1, 5);
    rasl.rps = SliceRps::Inline(set(&[-3], &[3]));
    assert!(decode(&mut decoder, &config, false, &rasl, 1).is_empty());
    let frames = decode(&mut decoder, &config, false, &p(9, 1), 2);
    assert_eq!(frames.len(), 1);
    assert_eq!(driver.hevc_pictures().len(), 2);
}

#[test]
fn pictures_before_the_first_irap_are_skipped() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    assert!(decode(&mut decoder, &config, true, &p(1, 1), 0).is_empty());
    assert!(driver.hevc_pictures().is_empty());
    assert_eq!(
        decode(&mut decoder, &config, false, &SliceSpec::idr(), 1).len(),
        1
    );
}

#[test]
fn an_end_of_sequence_outputs_every_waiting_picture() {
    let (_driver, mut decoder) = decoder();
    let config = StreamConfig::default();
    assert!(decode(&mut decoder, &config, true, &SliceSpec::idr(), 0).is_empty());
    assert!(decode(&mut decoder, &config, false, &p(4, 4), 40).is_empty());
    let end = access_unit(&config, false, vec![nal_header(36, 0).to_vec()]);
    let frames = decoder.decode(&end, 50).unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.timestamp_ns)
            .collect::<Vec<_>>(),
        [0, 40]
    );
    // Anything but an IRAP picture is an error after it, and an IDR starts
    // the next sequence.
    let error = decoder
        .decode(&access_unit(&config, false, vec![p(5, 1).nal(&config)]), 60)
        .unwrap_err();
    assert!(matches!(error, VaapiError::Hevc(_)), "{error}");
    assert!(decode(&mut decoder, &config, false, &SliceSpec::idr(), 70).is_empty());
    assert_eq!(decoder.drain().unwrap().len(), 1);
}

#[test]
fn a_missing_reference_is_an_error_and_decoding_goes_on() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let error = decoder
        .decode(&access_unit(&config, false, vec![p(5, 3).nal(&config)]), 1)
        .unwrap_err();
    assert!(matches!(error, VaapiError::Hevc(_)), "{error}");
    assert!(matches!(error.at_decode(), CodecError::DecodingFailed(_)));
    assert_eq!(driver.hevc_pictures().len(), 1);
    // The next IRAP picture recovers.
    assert_eq!(
        decode(&mut decoder, &config, false, &SliceSpec::idr(), 2).len(),
        1
    );
}

#[test]
fn a_new_sequence_starts_at_an_idr_picture() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let smaller = StreamConfig {
        width: 32,
        height: 32,
        ..low_delay()
    };
    let frames = decode(&mut decoder, &smaller, true, &SliceSpec::idr(), 1);
    assert_eq!((frames[0].width, frames[0].height), (32, 32));
    assert_eq!(driver.surface_size(), (32, 32));
}

#[test]
fn a_parameter_set_change_outside_an_irap_picture_is_an_error() {
    let (_driver, mut decoder) = decoder();
    let config = low_delay();
    decode(&mut decoder, &config, true, &SliceSpec::idr(), 0);
    let smaller = StreamConfig {
        width: 32,
        height: 32,
        ..low_delay()
    };
    let error = decoder
        .decode(&access_unit(&smaller, true, vec![p(1, 1).nal(&smaller)]), 1)
        .unwrap_err();
    assert!(
        matches!(error, VaapiError::Hevc(H265Error::SequenceChange)),
        "{error}"
    );
}

#[test]
fn unsupported_streams_are_reported_as_such() {
    let (_driver, mut decoder) = decoder();
    let config = StreamConfig {
        interlaced: true,
        ..low_delay()
    };
    let error = decoder
        .decode(
            &access_unit(&config, true, vec![SliceSpec::idr().nal(&config)]),
            0,
        )
        .unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(error.at_decode(), CodecError::Unsupported(_)),
        "{message}"
    );
}

#[test]
fn other_chroma_formats_are_unsupported_before_the_slice_header_is_read() {
    for chroma_format_idc in [0, 2] {
        let (_driver, mut decoder) = decoder();
        let config = StreamConfig {
            chroma_format_idc,
            ..low_delay()
        };
        // A slice header cut short after the parameter set id: reading it
        // would fail, so the format check must come first.
        let mut nal = SliceSpec::idr().nal(&config);
        nal.truncate(3);
        let error = decoder
            .decode(&access_unit(&config, true, vec![nal]), 0)
            .unwrap_err();
        assert!(
            matches!(
                error,
                VaapiError::Hevc(H265Error::Unsupported(Unsupported::ChromaFormat(format)))
                    if format == chroma_format_idc
            ),
            "{error}"
        );
        assert!(matches!(error.at_decode(), CodecError::Unsupported(_)));
    }
}

#[test]
fn malformed_input_is_an_error() {
    let (_driver, mut decoder) = decoder();
    let config = low_delay();
    let mut nal = SliceSpec::idr().nal(&config);
    nal.truncate(3);
    let error = decoder
        .decode(&access_unit(&config, true, vec![nal]), 0)
        .unwrap_err();
    assert!(matches!(error, VaapiError::Hevc(_)), "{error}");
}

#[test]
fn length_prefixed_samples_follow_the_hvcc_record() {
    let config = low_delay();
    let parameter_sets = annex_b(&config.parameter_sets());
    let hvcc =
        build_h265_hvcc_from_annex_b(&parameter_sets).expect("the stream has parameter sets");
    let driver = Driver::new();
    let mut decoder = open(Some(&hvcc)).unwrap();
    let annex = access_unit(&config, false, vec![SliceSpec::idr().nal(&config)]);
    let (sample, keyframe) = annex_b_to_length_prefixed(&annex, true);
    assert!(keyframe);
    let frames = decoder.decode(&sample.unwrap(), 7).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].timestamp_ns, 7);
    assert_eq!(driver.hevc_pictures().len(), 1);
    // A truncated sample is a framing error, not a panic.
    let error = decoder.decode(&[0, 0, 0, 9, 1], 8).unwrap_err();
    assert!(matches!(error, VaapiError::Framing(_)), "{error}");
}

#[test]
fn a_slice_without_parameter_sets_is_an_error() {
    let (_driver, mut decoder) = decoder();
    let config = low_delay();
    let error = decoder
        .decode(
            &access_unit(&config, false, vec![SliceSpec::idr().nal(&config)]),
            0,
        )
        .unwrap_err();
    assert!(
        matches!(error, VaapiError::Hevc(H265Error::MissingParameterSet(_))),
        "{error}"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_host_without_libva_gets_a_clear_error_from_the_constructor() {
    let error = HevcDecoder::new(None).unwrap_err();
    assert!(matches!(
        error.clone().at_open(),
        CodecError::Unsupported(_)
    ));
    assert!(
        matches!(&error, VaapiError::LibraryUnavailable { .. }),
        "{error}"
    );
}

#[test]
fn a_driver_without_h265_decoding_is_unsupported() {
    let mut driver = Driver::new();
    driver.set_profiles(&[]);
    let error = open(None).unwrap_err();
    assert!(matches!(error, VaapiError::NoDecodeEntrypoint), "{error}");
    assert!(matches!(error.at_open(), CodecError::Unsupported(_)));

    // A driver with Main 10 only cannot decode a Main stream.
    driver.set_profiles(&[VAProfileHEVCMain10]);
    let mut decoder = open(None).unwrap();
    let config = low_delay();
    let error = decoder
        .decode(
            &access_unit(&config, true, vec![SliceSpec::idr().nal(&config)]),
            0,
        )
        .unwrap_err();
    assert!(matches!(error, VaapiError::NoDecodeEntrypoint), "{error}");
}

fn through_the_trait(decoder: &mut impl VaapiDecoder, sample: &[u8]) -> usize {
    let frames = VaapiDecoder::decode(decoder, sample, 3).unwrap();
    assert!(VaapiDecoder::drain(decoder).unwrap().is_empty());
    frames.len()
}

#[test]
fn the_decoder_trait_reaches_the_h265_decoder() {
    let (driver, mut decoder) = decoder();
    let config = low_delay();
    let sample = access_unit(&config, true, vec![SliceSpec::idr().nal(&config)]);
    assert_eq!(through_the_trait(&mut decoder, &sample), 1);
    assert_eq!(driver.hevc_pictures().len(), 1);
}
