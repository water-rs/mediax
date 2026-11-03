//! `VideoToolbox` realtime encoding on the host's hardware.
//!
//! Headless: nothing opens a window. Per-frame encode latencies are logged
//! through `tracing` (run with `--nocapture`); they are reported, not asserted.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test patterns and latency statistics on small known ranges"
)]

use crate::{
    Bitrate, BitstreamFormat, CodecType, DecodePacket, Decoder, EncodedPacket, Encoder,
    EncoderCodec, EncoderConfig, EncoderProfile, EvenDimension, FrameOptions, FrameRate, H264Level,
    H264Profile, H264Settings, H265Settings, annexb::annex_b_nalus,
    bitstream::annex_b_to_length_prefixed, image::decode_jpeg_pixel_buffer,
};
use mediax_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorInfo,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_32BGRA,
};
use std::{
    num::NonZeroU32,
    ops::BitXor,
    ptr::{self, NonNull},
    time::{Duration, Instant},
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FPS: u32 = 30;

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn bt709() -> VideoColorInfo {
    VideoColorInfo {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferFunction::Sdr,
        matrix: MatrixCoefficients::Bt709,
        range: ColorRange::Limited,
        content_light_level: None,
        dolby_vision: false,
    }
}

fn h264(bitstream: BitstreamFormat) -> EncoderCodec {
    EncoderCodec::H264(H264Settings {
        profile: H264Profile::High,
        level: H264Level::L4_0,
        bitstream,
    })
}

fn config(codec: EncoderCodec, width: u32, height: u32, bits_per_second: u32) -> EncoderConfig {
    EncoderConfig {
        codec,
        width: EvenDimension::new(width).expect("even width"),
        height: EvenDimension::new(height).expect("even height"),
        color: bt709(),
        profile: EncoderProfile::Realtime,
        bitrate: Bitrate::new(nz(bits_per_second)),
        frame_rate: FrameRate::new(nz(FPS), nz(1)),
        keyframe_interval: None,
    }
}

/// A moving gradient with per-pixel noise, so every frame carries data and the
/// bitrate target decides how many bytes it takes.
fn nv12_frame(width: u32, height: u32, index: usize) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let mut data = vec![128_u8; width * height * 3 / 2];
    for (y, row) in data[..width * height].chunks_exact_mut(width).enumerate() {
        for (x, luma) in row.iter_mut().enumerate() {
            let noise = (x as u32)
                .wrapping_mul(73_856_093)
                .bitxor((y as u32).wrapping_mul(19_349_663))
                .bitxor((index as u32).wrapping_mul(83_492_791))
                .wrapping_mul(2_654_435_761)
                >> 27;
            *luma = ((x + y + index * 3) % 192 + 16) as u8 + noise as u8;
        }
    }
    data
}

/// An `IOSurface`-backed BGRA buffer holding a moving gradient.
fn bgra_buffer(width: u32, height: u32, index: usize) -> CFRetained<CVPixelBuffer> {
    // SAFETY: a constant key exported by `CoreVideo`.
    let key = unsafe { kCVPixelBufferIOSurfacePropertiesKey };
    let attributes = CFDictionary::<CFString, CFType>::from_slices(
        &[key],
        &[CFDictionary::<CFString, CFType>::empty().as_ref()],
    );
    let mut buffer_ptr: *mut CVPixelBuffer = ptr::null_mut();
    // SAFETY: `buffer_ptr` is a valid out pointer.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width as usize,
            height as usize,
            kCVPixelFormatType_32BGRA,
            Some(attributes.as_ref()),
            NonNull::from(&mut buffer_ptr),
        )
    };
    assert_eq!(status, 0, "CVPixelBufferCreate");
    // SAFETY: the create rule returns a +1 retained buffer.
    let buffer = unsafe { CFRetained::from_raw(NonNull::new(buffer_ptr).expect("a buffer")) };
    // SAFETY: the buffer is locked while written; rows are `stride` bytes.
    unsafe {
        let flags = CVPixelBufferLockFlags(0);
        assert_eq!(CVPixelBufferLockBaseAddress(&buffer, flags), 0);
        let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        for y in 0..height as usize {
            let row = std::slice::from_raw_parts_mut(base.add(y * stride), width as usize * 4);
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = ((x + y + index * 3) % 256) as u8;
                pixel.copy_from_slice(&[value, 255 - value, value / 2, 255]);
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, flags);
    }
    buffer
}

fn nal_types(packet: &EncodedPacket, hevc: bool) -> Vec<u8> {
    annex_b_nalus(&packet.data)
        .into_iter()
        .map(|(start, _)| {
            let header = packet.data[start];
            if hevc {
                (header >> 1) & 0x3F
            } else {
                header & 0x1F
            }
        })
        .collect()
}

fn one_packet(
    packets: impl Iterator<Item = Result<EncodedPacket, crate::CodecError>>,
) -> EncodedPacket {
    let mut packets = packets
        .collect::<Result<Vec<_>, _>>()
        .expect("encode succeeds");
    assert_eq!(
        packets.len(),
        1,
        "a realtime session emits one packet per frame"
    );
    packets.remove(0)
}

fn report_latency(label: &str, latencies: &mut [Duration]) {
    latencies.sort();
    let total: Duration = latencies.iter().sum();
    let at = |q: f64| latencies[((latencies.len() - 1) as f64 * q) as usize];
    crate::init_test_tracing();
    tracing::info!(
        label,
        frames = latencies.len(),
        min = ?latencies[0],
        median = ?at(0.5),
        mean = ?(total / u32::try_from(latencies.len()).expect("few frames")),
        p95 = ?at(0.95),
        max = ?latencies[latencies.len() - 1],
        frame_interval = ?(Duration::from_secs(1) / FPS),
        "encode latency"
    );
}

#[test]
fn realtime_nv12_keyframe_request_yields_an_idr_on_that_frame() {
    let mut encoder = Encoder::new(config(
        h264(BitstreamFormat::AnnexB),
        WIDTH,
        HEIGHT,
        2_000_000,
    ))
    .expect("encoder");
    let mut latencies = Vec::new();
    for index in 0..60 {
        let frame = nv12_frame(WIDTH, HEIGHT, index);
        let keyframe = index == 30;
        let started = Instant::now();
        let packet = one_packet(encoder.encode_nv12(&frame, FrameOptions { keyframe }));
        latencies.push(started.elapsed());

        let types = nal_types(&packet, false);
        let is_idr = types.contains(&5);
        assert_eq!(
            is_idr,
            index == 0 || keyframe,
            "frame {index}: NAL types {types:?}"
        );
        assert_eq!(packet.keyframe, is_idr, "frame {index} keyframe flag");
        if is_idr {
            // Parameter sets lead the IDR so a decoder can join here.
            assert_eq!(&types[..2], &[7, 8], "frame {index}: {types:?}");
            assert_eq!(*types.last().unwrap(), 5, "frame {index}: {types:?}");
        }
    }
    report_latency("H.264 NV12 1280x720 Realtime 2 Mbps", &mut latencies);
}

#[test]
fn set_bitrate_mid_stream_is_accepted() {
    let mut encoder = Encoder::new(config(
        h264(BitstreamFormat::AnnexB),
        WIDTH,
        HEIGHT,
        500_000,
    ))
    .expect("encoder");
    let mut bytes = [0_usize; 2];
    let mut dropped = [0_usize; 2];
    for (phase, bits_per_second) in [(0, 500_000), (1, 6_000_000)] {
        if phase == 1 {
            encoder
                .set_bitrate(Bitrate::new(nz(bits_per_second)))
                .expect("set_bitrate mid-stream");
        }
        for index in 0..45 {
            let frame = nv12_frame(WIDTH, HEIGHT, phase * 45 + index);
            // At a starved rate the low-latency controller may skip a frame.
            let packets = encoder
                .encode_nv12(&frame, FrameOptions::default())
                .collect::<Result<Vec<_>, _>>()
                .expect("encode succeeds");
            assert!(packets.len() <= 1, "at most one packet per frame");
            dropped[phase] += 1 - packets.len();
            bytes[phase] += packets
                .iter()
                .map(|packet| packet.data.len())
                .sum::<usize>();
        }
    }
    crate::init_test_tracing();
    tracing::info!(
        low_bytes = bytes[0],
        low_skipped = dropped[0],
        high_bytes = bytes[1],
        high_skipped = dropped[1],
        "set_bitrate: 45 frames at 0.5 Mbps, then 45 frames at 6 Mbps"
    );
    assert!(
        bytes[1] > bytes[0],
        "a 12x higher target produces more bytes"
    );
}

#[test]
fn bgra_pixel_buffer_sequence_encodes_with_keyframe_on_request() {
    let mut encoder =
        Encoder::new(config(h264(BitstreamFormat::AnnexB), 960, 540, 3_000_000)).expect("encoder");
    let mut latencies = Vec::new();
    for index in 0..40 {
        let buffer = bgra_buffer(960, 540, index);
        let keyframe = index == 20;
        let started = Instant::now();
        let packet = one_packet(encoder.encode_pixel_buffer(&buffer, FrameOptions { keyframe }));
        latencies.push(started.elapsed());
        let is_idr = nal_types(&packet, false).contains(&5);
        assert_eq!(is_idr, index == 0 || keyframe, "frame {index}");
        assert_eq!(packet.keyframe, is_idr, "frame {index} keyframe flag");
    }
    report_latency(
        "H.264 BGRA IOSurface 960x540 Realtime 3 Mbps",
        &mut latencies,
    );
}

#[test]
fn pixel_buffer_of_another_size_is_rejected() {
    let mut encoder =
        Encoder::new(config(h264(BitstreamFormat::AnnexB), 640, 360, 1_000_000)).expect("encoder");
    let buffer = bgra_buffer(320, 180, 0);
    let result = encoder
        .encode_pixel_buffer(&buffer, FrameOptions::default())
        .next()
        .expect("an error is yielded");
    assert!(
        matches!(result, Err(crate::CodecError::Unsupported(_))),
        "{result:?}"
    );
}

fn decode_round_trip(codec: EncoderCodec, codec_type: CodecType, hevc: bool) {
    let (width, height, frames) = (640, 360, 24);
    let mut encoder = Encoder::new(config(codec, width, height, 1_500_000)).expect("encoder");
    let mut packets = Vec::new();
    for index in 0..frames {
        let frame = nv12_frame(width, height, index);
        packets.push(one_packet(
            encoder.encode_nv12(&frame, FrameOptions::default()),
        ));
    }
    let record = encoder
        .codec_config()
        .expect("the first frame publishes the codec record");
    let mut decoder =
        Decoder::new(codec_type, Some(&record), width, height).expect("decoder for the record");
    let mut frame_count = 0;
    for (index, packet) in packets.iter().enumerate() {
        // The Apple decoder takes length-prefixed samples; reframe the Annex B access unit.
        let (sample, keyframe) = annex_b_to_length_prefixed(&packet.data, hevc);
        assert_eq!(keyframe, packet.keyframe, "frame {index}");
        let sample = sample.expect("Annex B access unit has start codes");
        let presentation_time = Duration::from_secs(1) * u32::try_from(index).unwrap() / FPS;
        for frame in decoder.decode(DecodePacket::new(&sample, presentation_time)) {
            frame.expect("decode succeeds");
            frame_count += 1;
        }
    }
    for frame in decoder.drain() {
        frame.expect("drain succeeds");
        frame_count += 1;
    }
    assert_eq!(
        frame_count, frames,
        "every Annex B access unit decodes to a frame"
    );
}

#[test]
fn annex_b_h264_decodes_through_the_decoder() {
    decode_round_trip(h264(BitstreamFormat::AnnexB), CodecType::H264, false);
}

#[test]
fn annex_b_h265_decodes_through_the_decoder() {
    let codec = EncoderCodec::H265(H265Settings {
        bitstream: BitstreamFormat::AnnexB,
    });
    decode_round_trip(codec, CodecType::H265, true);
}

#[test]
fn hevc_annex_b_keyframe_leads_with_vps_sps_pps() {
    let codec = EncoderCodec::H265(H265Settings {
        bitstream: BitstreamFormat::AnnexB,
    });
    let mut encoder =
        Encoder::new(config(codec, 640, 360, 1_500_000)).expect("H.265 hardware encoder");
    for index in 0..10 {
        let frame = nv12_frame(640, 360, index);
        let packet = one_packet(encoder.encode_nv12(
            &frame,
            FrameOptions {
                keyframe: index == 5,
            },
        ));
        let types = nal_types(&packet, true);
        let is_idr = types.iter().any(|kind| matches!(kind, 19 | 20));
        assert_eq!(is_idr, index == 0 || index == 5, "frame {index}: {types:?}");
        assert_eq!(packet.keyframe, is_idr);
        if is_idr {
            assert_eq!(&types[..3], &[32, 33, 34], "frame {index}: {types:?}");
        }
    }
}

#[test]
fn length_prefixed_output_has_no_start_codes_and_flags_keyframes() {
    let mut encoder = Encoder::new(config(
        h264(BitstreamFormat::LengthPrefixed),
        640,
        360,
        1_000_000,
    ))
    .expect("encoder");
    let first = one_packet(encoder.encode_nv12(&nv12_frame(640, 360, 0), FrameOptions::default()));
    assert!(first.keyframe);
    let length = u32::from_be_bytes(first.data[..4].try_into().unwrap()) as usize;
    assert!(
        length + 4 <= first.data.len(),
        "first NAL length fits the sample"
    );
    let second = one_packet(encoder.encode_nv12(&nv12_frame(640, 360, 1), FrameOptions::default()));
    assert!(!second.keyframe);
}

#[test]
fn requested_h264_profile_and_level_reach_the_sps() {
    let cases = [
        (H264Profile::ConstrainedBaseline, H264Level::L3_1),
        (H264Profile::Main, H264Level::L4_0),
        (H264Profile::High, H264Level::L4_2),
    ];
    for (profile, level) in cases {
        let settings = H264Settings {
            profile,
            level,
            bitstream: BitstreamFormat::AnnexB,
        };
        let mut encoder = Encoder::new(config(EncoderCodec::H264(settings), 640, 360, 1_000_000))
            .expect("encoder");
        let packet =
            one_packet(encoder.encode_nv12(&nv12_frame(640, 360, 0), FrameOptions::default()));
        let (start, _) = annex_b_nalus(&packet.data)[0];
        // SPS NAL: header, profile_idc, constraint flags, level_idc.
        let sps = &packet.data[start..start + 4];
        crate::init_test_tracing();
        tracing::info!(
            ?profile,
            ?level,
            profile_idc = sps[1],
            constraints = format!("{:#04x}", sps[2]),
            level_idc = sps[3],
            profile_level_id = ?settings.profile_level_id(),
            "SPS of the requested stream"
        );
        let [profile_idc, constraints, _] = settings.profile_level_id();
        assert_eq!(sps[1], profile_idc, "{profile:?} {level:?}");
        if profile == H264Profile::ConstrainedBaseline {
            // constraint_set0 and constraint_set1 are the top two bits.
            assert_eq!(sps[2] & 0xC0, 0xC0, "constraint_set0/1 for {level:?}");
            // VideoToolbox does not set constraint_set2, which RFC 6184 ignores
            // when matching Constrained Baseline, so compare the flags it sets.
            assert_eq!(
                sps[2] & 0xC0,
                constraints & 0xC0,
                "constraint byte for {level:?}"
            );
            // The level is automatic; the encoder enforces the ceiling.
            assert!(sps[3] <= level.idc(), "{profile:?} {level:?}");
        } else {
            assert_eq!(sps[3], level.idc(), "{profile:?} {level:?}");
            assert_eq!(sps[2], constraints, "{profile:?} {level:?}");
        }
    }
}

/// A smooth RGB JPEG built with the `image` crate's encoder.
fn test_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, 128])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
        .encode_image(&image)
        .expect("JPEG encode");
    bytes
}

#[test]
fn jpeg_decodes_to_a_pixel_buffer_the_encoder_takes() {
    let jpeg = test_jpeg(640, 480);
    let started = Instant::now();
    // Hosted macOS runners are virtual machines that do not always expose the
    // hardware JPEG decoder.
    let buffer = match decode_jpeg_pixel_buffer(&jpeg) {
        Ok(buffer) => buffer,
        Err(crate::CodecError::Unsupported(reason)) => {
            crate::init_test_tracing();
            tracing::warn!(%reason, "skipping: no hardware JPEG decoder");
            return;
        }
        Err(error) => panic!("hardware JPEG decode: {error}"),
    };
    let decode = started.elapsed();
    let mut encoder =
        Encoder::new(config(h264(BitstreamFormat::AnnexB), 640, 480, 2_000_000)).expect("encoder");
    let started = Instant::now();
    let packet = one_packet(encoder.encode_pixel_buffer(&buffer, FrameOptions::default()));
    crate::init_test_tracing();
    tracing::info!(
        ?decode,
        encode = ?started.elapsed(),
        "JPEG 640x480: hardware decode to BGRA IOSurface, then encode of that buffer"
    );
    assert!(packet.keyframe);
    assert!(nal_types(&packet, false).contains(&5));
}

#[test]
fn undecodable_jpeg_is_an_error() {
    assert!(decode_jpeg_pixel_buffer(&[0xFF, 0xD8, 0x00]).is_err());
}
