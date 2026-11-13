//! `AudioToolbox` decoder tests. HE-AAC fixtures are encoded at test time with
//! `AudioToolbox`'s own encoder from a synthetic signal.

// Test-signal synthesis and analysis index sample positions in `f32`.
#![allow(clippy::cast_precision_loss)]

use std::f32::consts::TAU;
use std::ffi::c_void;
use std::num::{NonZeroU16, NonZeroU32};
use std::ptr::{self, NonNull};
use std::time::Duration;

use objc2_audio_toolbox::{
    AudioConverterFillComplexBuffer, AudioConverterGetProperty, AudioConverterNew,
    AudioConverterRef, AudioConverterSetProperty, kAudioConverterCompressionMagicCookie,
    kAudioConverterEncodeBitRate, kAudioConverterPropertyMaximumOutputPacketSize,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    kAudioFormatMPEG4AAC_HE, kAudioFormatMPEG4AAC_HE_V2,
};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration as SymphoniaDuration, Timestamp};

use super::super::{AacDecoderConfig, AacPacketDecoder, EncodedAudioPacket, PacketAudioDecoder};
use super::{Converter, check, float_pcm};

const AAC_LC_44K_STEREO_STREAM: &str = include_str!("../../fixtures/aac-lc-44k-stereo-stream.hex");
/// Signal-to-difference ratio required between `AudioToolbox` and the pure-Rust
/// decoder, which decode the same bitstream to float.
const LC_REFERENCE_MIN_SNR_DB: f64 = 40.0;

struct Encoded {
    audio_specific_config: Vec<u8>,
    packets: Vec<Vec<u8>>,
}

struct Pcm {
    samples: Vec<f32>,
    channels: usize,
    cursor: usize,
}

unsafe extern "C-unwind" fn supply_pcm(
    _converter: AudioConverterRef,
    packets: NonNull<u32>,
    data: NonNull<AudioBufferList>,
    _descriptions: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> i32 {
    // SAFETY: `user_data` is the `Pcm` passed to the fill call.
    unsafe {
        let pcm = &mut *user_data.cast::<Pcm>();
        let remaining = pcm.samples.len() / pcm.channels - pcm.cursor;
        let frames = remaining.min(usize::try_from(*packets.as_ptr()).unwrap());
        *packets.as_ptr() = u32::try_from(frames).unwrap();
        let list = &mut *data.as_ptr();
        list.mNumberBuffers = 1;
        list.mBuffers[0] = AudioBuffer {
            mNumberChannels: u32::try_from(pcm.channels).unwrap(),
            mDataByteSize: u32::try_from(frames * pcm.channels * 4).unwrap(),
            mData: pcm
                .samples
                .as_mut_ptr()
                .add(pcm.cursor * pcm.channels)
                .cast(),
        };
        pcm.cursor += frames;
    }
    0
}

fn property<T>(converter: &Converter, id: u32) -> Vec<u8> {
    let mut size = u32::try_from(size_of::<T>()).unwrap().max(256);
    let mut bytes = vec![0_u8; size as usize];
    // SAFETY: the buffer holds `size` bytes.
    check(
        unsafe {
            AudioConverterGetProperty(
                converter.0,
                id,
                NonNull::from(&mut size),
                NonNull::new(bytes.as_mut_ptr().cast()).unwrap(),
            )
        },
        "AudioConverterGetProperty",
    )
    .unwrap();
    bytes.truncate(size as usize);
    bytes
}

/// Walks the MPEG-4 `esds` descriptors of an encoder magic cookie down to the
/// `AudioSpecificConfig`.
fn audio_specific_config_of(cookie: &[u8]) -> Vec<u8> {
    fn descriptor(data: &[u8]) -> (u8, &[u8], &[u8]) {
        let tag = data[0];
        let (mut length, mut index) = (0_usize, 1);
        loop {
            let byte = data[index];
            index += 1;
            length = (length << 7) | usize::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                break;
            }
        }
        (tag, &data[index..index + length], &data[index + length..])
    }
    let (tag, es, _) = descriptor(cookie);
    assert_eq!(tag, 0x03);
    let (tag, config, _) = descriptor(&es[3..]);
    assert_eq!(tag, 0x04);
    let (tag, asc, _) = descriptor(&config[13..]);
    assert_eq!(tag, 0x05);
    asc.to_vec()
}

/// Encodes `pcm` one access unit at a time until the encoder is drained.
fn pull_packets(converter: &Converter, pcm: &mut Pcm, max_packet: u32) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    loop {
        let mut buffer = vec![0_u8; max_packet as usize];
        let mut out = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::try_from(pcm.channels).unwrap(),
                mDataByteSize: max_packet,
                mData: buffer.as_mut_ptr().cast(),
            }],
        };
        let mut description = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        };
        let mut count = 1_u32;
        // SAFETY: all pointers reference live locals; `pcm` outlives the call.
        check(
            unsafe {
                AudioConverterFillComplexBuffer(
                    converter.0,
                    Some(supply_pcm),
                    ptr::from_mut(pcm).cast(),
                    NonNull::from(&mut count),
                    NonNull::from(&mut out),
                    &raw mut description,
                )
            },
            "AudioConverterFillComplexBuffer(encoder)",
        )
        .unwrap();
        if count == 0 {
            break;
        }
        buffer.truncate(out.mBuffers[0].mDataByteSize as usize);
        packets.push(buffer);
    }
    packets
}

fn encode(format_id: u32, sample_rate: u32, bit_rate: u32, channel_tones: &[f32]) -> Encoded {
    let channels = channel_tones.len();
    let frames = usize::try_from(sample_rate).unwrap() * 2;
    let mut pcm = Pcm {
        samples: (0..frames)
            .flat_map(|frame| {
                channel_tones
                    .iter()
                    .map(move |tone| 0.5 * (TAU * tone * frame as f32 / sample_rate as f32).sin())
            })
            .collect(),
        channels,
        cursor: 0,
    };
    let source = float_pcm(sample_rate, u16::try_from(channels).unwrap());
    let destination = AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: format_id,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 2048,
        mBytesPerFrame: 0,
        mChannelsPerFrame: u32::try_from(channels).unwrap(),
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut raw = ptr::null_mut();
    // SAFETY: pointers reference live locals.
    check(
        unsafe {
            AudioConverterNew(
                NonNull::from(&source),
                NonNull::from(&destination),
                NonNull::from(&mut raw),
            )
        },
        "AudioConverterNew(encoder)",
    )
    .unwrap();
    let converter = Converter(raw);
    // SAFETY: `bit_rate` is a live u32.
    check(
        unsafe {
            AudioConverterSetProperty(
                converter.0,
                kAudioConverterEncodeBitRate,
                4,
                NonNull::from(&bit_rate).cast(),
            )
        },
        "set bit rate",
    )
    .unwrap();
    let audio_specific_config = audio_specific_config_of(&property::<u8>(
        &converter,
        kAudioConverterCompressionMagicCookie,
    ));
    let max_packet = u32::from_ne_bytes(
        property::<u32>(&converter, kAudioConverterPropertyMaximumOutputPacketSize)[..4]
            .try_into()
            .unwrap(),
    );

    let packets = pull_packets(&converter, &mut pcm, max_packet);
    assert!(
        packets.len() > 20,
        "encoder produced {} packets",
        packets.len()
    );
    Encoded {
        audio_specific_config,
        packets,
    }
}

fn decode_all(
    encoded: &Encoded,
    channels: u16,
    sample_rate: u32,
) -> Vec<crate::audio::DecodedAudioFrame> {
    let mut decoder = AacPacketDecoder::new(AacDecoderConfig::new(
        encoded.audio_specific_config.clone(),
        NonZeroU16::new(channels).unwrap(),
        NonZeroU32::new(sample_rate).unwrap(),
    ))
    .expect("AudioToolbox must accept the encoder's AudioSpecificConfig");
    let mut frames = Vec::new();
    for (index, packet) in encoded.packets.iter().enumerate() {
        frames.extend(
            decoder
                .decode(EncodedAudioPacket::new(
                    Duration::from_millis(index as u64),
                    Duration::from_millis(1),
                    packet.clone(),
                ))
                .expect("every access unit must decode"),
        );
    }
    frames.extend(decoder.finish().unwrap());
    frames
}

/// Magnitude of `channel` at `frequency`, skipping the codec's priming frames.
fn tone_magnitude(
    samples: &[f32],
    channels: usize,
    channel: usize,
    rate: u32,
    frequency: f32,
) -> f32 {
    let (mut re, mut im) = (0.0_f32, 0.0_f32);
    let mono: Vec<f32> = samples
        .iter()
        .skip(channel)
        .step_by(channels)
        .copied()
        .collect();
    let mono = &mono[8_192..];
    for (index, sample) in mono.iter().enumerate() {
        let phase = TAU * frequency * index as f32 / rate as f32;
        re += sample * phase.cos();
        im += sample * phase.sin();
    }
    2.0 * re.hypot(im) / mono.len() as f32
}

fn dominant_frequency(samples: &[f32], channels: usize, channel: usize, rate: u32) -> f32 {
    (1..=200)
        .map(|step| step as f32 * 50.0)
        .max_by(|a, b| {
            tone_magnitude(samples, channels, channel, rate, *a)
                .total_cmp(&tone_magnitude(samples, channels, channel, rate, *b))
        })
        .unwrap()
}

/// `AudioToolbox` drops the SBR decoder delay from the first access unit's PCM;
/// every later unit decodes to a full 2048 frames.
fn assert_full_sbr_frames(frames: &[crate::audio::DecodedAudioFrame]) {
    let (first, rest) = frames.split_first().unwrap();
    assert!(first.sample_frames() <= 2048);
    assert!(rest.iter().all(|f| f.sample_frames() == 2048));
}

fn assert_signal_survives(
    frames: &[crate::audio::DecodedAudioFrame],
    channels: usize,
    rate: u32,
    tones: &[f32],
) {
    let samples: Vec<f32> = frames
        .iter()
        .flat_map(|f| f.samples().iter().copied())
        .collect();
    for (channel, tone) in tones.iter().enumerate() {
        let dominant = dominant_frequency(&samples, channels, channel, rate);
        assert!(
            (dominant - tone).abs() <= 50.0,
            "channel {channel} dominant frequency {dominant} Hz, expected {tone} Hz"
        );
        let magnitude = tone_magnitude(&samples, channels, channel, rate, *tone);
        assert!(
            (0.3..=0.7).contains(&magnitude),
            "channel {channel} tone amplitude {magnitude}, expected about 0.5"
        );
    }
}

#[test]
fn aac_lc_matches_the_pure_rust_decoder() {
    let packets: Vec<Vec<u8>> = AAC_LC_44K_STEREO_STREAM
        .trim()
        .lines()
        .map(|line| hex::decode(line).unwrap())
        .collect();
    let asc = [0x12, 0x10];
    let mut toolbox = AacPacketDecoder::new(AacDecoderConfig::new(
        asc,
        NonZeroU16::new(2).unwrap(),
        NonZeroU32::new(44_100).unwrap(),
    ))
    .unwrap();
    let mut parameters = AudioCodecParameters::new();
    parameters
        .for_codec(CODEC_ID_AAC)
        .with_sample_rate(44_100)
        .with_extra_data(Box::new(asc));
    let mut reference = AacDecoder::try_new(&parameters, &AudioDecoderOptions::default()).unwrap();

    let (mut error_energy, mut signal_energy, mut peak) = (0.0_f64, 0.0_f64, 0.0_f32);
    for data in packets {
        let ours = toolbox
            .decode(EncodedAudioPacket::new(
                Duration::ZERO,
                Duration::ZERO,
                data.clone(),
            ))
            .unwrap();
        let theirs = reference
            .decode(&Packet::new(
                0,
                Timestamp::ZERO,
                SymphoniaDuration::ZERO,
                data,
            ))
            .unwrap();
        let mut expected = Vec::new();
        theirs.copy_to_vec_interleaved::<f32>(&mut expected);
        let [frame] = ours.as_slice() else {
            panic!("one access unit must give one frame")
        };
        assert_eq!(frame.samples().len(), expected.len());
        assert_eq!(
            (frame.channels().get(), frame.sample_rate().get()),
            (2, 44_100)
        );
        for (a, b) in frame.samples().iter().zip(&expected) {
            error_energy += f64::from(a - b).powi(2);
            signal_energy += f64::from(*b).powi(2);
            peak = peak.max((a - b).abs());
        }
    }
    let snr_db = 10.0 * (signal_energy / error_energy).log10();
    eprintln!("SNR against the pure-Rust decoder: {snr_db} dB, peak difference {peak}");
    assert!(snr_db >= LC_REFERENCE_MIN_SNR_DB, "SNR {snr_db} dB");
}

#[test]
fn he_aac_v1_doubles_the_core_sample_rate() {
    let tones = [1_000.0, 2_500.0];
    let encoded = encode(kAudioFormatMPEG4AAC_HE, 48_000, 48_000, &tones);
    let format = super::StreamFormat::parse(&encoded.audio_specific_config).unwrap();
    assert!(format.sbr && !format.parametric_stereo);
    assert_eq!((format.sample_rate, format.channels), (48_000, 2));
    let frames = decode_all(&encoded, 2, 48_000);
    assert_full_sbr_frames(&frames);
    assert!(
        frames
            .iter()
            .all(|f| f.sample_rate().get() == 48_000 && f.channels().get() == 2)
    );
    assert_signal_survives(&frames, 2, 48_000, &tones);
}

#[test]
fn he_aac_v2_upmixes_to_stereo() {
    let tones = [1_000.0, 1_000.0];
    let encoded = encode(kAudioFormatMPEG4AAC_HE_V2, 48_000, 32_000, &tones);
    let format = super::StreamFormat::parse(&encoded.audio_specific_config).unwrap();
    assert!(format.sbr && format.parametric_stereo);
    assert_eq!((format.sample_rate, format.channels), (48_000, 2));
    let frames = decode_all(&encoded, 2, 48_000);
    assert!(frames.iter().all(|f| f.channels().get() == 2));
    assert_full_sbr_frames(&frames);
    assert_signal_survives(&frames, 2, 48_000, &tones);
}
