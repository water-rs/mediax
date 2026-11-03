//! Apple AAC packet decoding through `AudioToolbox`'s `AudioConverter`, which
//! decodes AAC-LC, HE-AAC (SBR) and HE-AAC v2 (SBR + parametric stereo).

mod asc;

use std::ffi::c_void;
use std::num::{NonZeroU16, NonZeroU32};
use std::ptr::{self, NonNull};
use std::time::Duration;

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterNew, AudioConverterRef,
    AudioConverterReset, AudioConverterSetProperty, kAudioConverterDecompressionMagicCookie,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
    kAudioFormatMPEG4AAC_HE, kAudioFormatMPEG4AAC_HE_V2,
};

use super::{
    AacDecoderConfig, DecodedAudioFrame, EncodedAudioPacket, PacketAudioError, PcmFrameError,
};
use asc::StreamFormat;

/// Returned by the input callback once its single access unit is consumed; the
/// converter keeps its state and hands back the PCM produced so far.
const NO_MORE_INPUT: i32 = i32::from_be_bytes(*b"nomo");
const AAC_LC_FRAMES_PER_PACKET: u32 = 1024;
const AAC_SBR_FRAMES_PER_PACKET: u32 = 2048;
const F32_BITS: u32 = 32;

/// An `AudioConverter` owned for its lifetime.
struct Converter(AudioConverterRef);

// SAFETY: the converter has no thread affinity and every call goes through
// `&mut self` of its single owner.
unsafe impl Send for Converter {}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `AudioConverterNew` and is disposed once.
        unsafe { AudioConverterDispose(self.0) };
    }
}

pub(super) struct AppleAacDecoder {
    converter: Converter,
    format: StreamFormat,
    channels: NonZeroU16,
    sample_rate: NonZeroU32,
    output: Vec<f32>,
}

impl std::fmt::Debug for AppleAacDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppleAacDecoder")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl AppleAacDecoder {
    pub(super) fn new(config: AacDecoderConfig) -> Result<Self, String> {
        let AacDecoderConfig {
            audio_specific_config,
            channels,
            sample_rate,
        } = config;
        let format = StreamFormat::parse(&audio_specific_config)?;
        let source = AudioStreamBasicDescription {
            mSampleRate: f64::from(format.sample_rate),
            mFormatID: match (format.sbr, format.parametric_stereo) {
                (_, true) => kAudioFormatMPEG4AAC_HE_V2,
                (true, false) => kAudioFormatMPEG4AAC_HE,
                (false, false) => kAudioFormatMPEG4AAC,
            },
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: frames_per_packet(format),
            mBytesPerFrame: 0,
            mChannelsPerFrame: u32::from(format.channels),
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let destination = float_pcm(format.sample_rate, format.channels);
        let mut converter = ptr::null_mut();
        // SAFETY: all three pointers reference live locals.
        check(
            unsafe {
                AudioConverterNew(
                    NonNull::from(&source),
                    NonNull::from(&destination),
                    NonNull::from(&mut converter),
                )
            },
            "AudioConverterNew",
        )?;
        let converter = Converter(converter);
        let cookie = elementary_stream_descriptor(&audio_specific_config);
        // SAFETY: the pointer and length describe `cookie`, which outlives the call.
        check(
            unsafe {
                AudioConverterSetProperty(
                    converter.0,
                    kAudioConverterDecompressionMagicCookie,
                    u32::try_from(cookie.len()).map_err(|_| "AudioSpecificConfig exceeds u32")?,
                    NonNull::from(cookie.as_slice()).cast(),
                )
            },
            "AudioConverterSetProperty(magic cookie)",
        )?;

        Ok(Self {
            converter,
            format,
            channels,
            sample_rate,
            output: Vec::new(),
        })
    }

    pub(super) fn decode(
        &mut self,
        packet: &EncodedAudioPacket,
    ) -> Result<Vec<DecodedAudioFrame>, PacketAudioError> {
        if packet.is_discontinuity() {
            self.reset();
        }
        let presentation_time = packet.presentation_time();
        if self.format.channels != self.channels.get()
            || self.format.sample_rate != self.sample_rate.get()
        {
            return Err(PacketAudioError::UnexpectedFormatChange {
                expected_channels: self.channels.get(),
                expected_sample_rate: self.sample_rate.get(),
                actual_channels: self.format.channels,
                actual_sample_rate: self.format.sample_rate,
            });
        }

        let channels = usize::from(self.format.channels);
        let capacity =
            usize::try_from(frames_per_packet(self.format)).expect("frames per packet fits usize");
        self.output.clear();
        self.output.resize(capacity * channels, 0.0);

        let data_size = u32::try_from(packet.data().len())
            .map_err(|_| decode_error(presentation_time, "access unit exceeds u32 bytes"))?;
        let mut input = Input {
            data: packet.data().as_ptr(),
            data_size,
            channels: u32::from(self.format.channels),
            description: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: data_size,
            },
            consumed: false,
        };
        let mut frames = u32::try_from(capacity).expect("frames per packet fits u32");
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.format.channels),
                mDataByteSize: u32::try_from(self.output.len() * size_of::<f32>())
                    .expect("output buffer fits u32"),
                mData: self.output.as_mut_ptr().cast(),
            }],
        };
        // SAFETY: `input` and `list` outlive the call; the callback only reads
        // `input`, whose `data` points into `packet`, and the output buffer
        // holds `frames` frames of `channels` f32 samples.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.0,
                Some(supply_input),
                ptr::from_mut(&mut input).cast::<c_void>(),
                NonNull::from(&mut frames),
                NonNull::from(&mut list),
                ptr::null_mut(),
            )
        };
        if status != 0 && status != NO_MORE_INPUT {
            return Err(decode_error(
                presentation_time,
                &describe("AudioConverterFillComplexBuffer", status),
            ));
        }
        if frames == 0 {
            return Ok(Vec::new());
        }

        let samples = usize::try_from(frames).expect("frame count fits usize") * channels;
        self.output.truncate(samples);
        let frame = DecodedAudioFrame::from_interleaved_pcm(
            presentation_time,
            self.channels,
            self.sample_rate,
            self.output.as_slice(),
        )
        .map_err(|error: PcmFrameError| decode_error(presentation_time, &error.to_string()))?;
        Ok(vec![frame])
    }

    // The signature is the platform-decoder interface `AacPacketDecoder` drives.
    #[allow(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        clippy::needless_pass_by_ref_mut,
        clippy::missing_const_for_fn
    )]
    pub(super) fn finish(&mut self) -> Result<Vec<DecodedAudioFrame>, PacketAudioError> {
        // The converter emits each access unit's PCM synchronously.
        Ok(Vec::new())
    }

    pub(super) fn reset(&mut self) {
        // SAFETY: `converter.0` is a live converter.
        let status = unsafe { AudioConverterReset(self.converter.0) };
        assert_eq!(status, 0, "{}", describe("AudioConverterReset", status));
    }
}

/// The single access unit handed to the converter by `supply_input`.
struct Input {
    data: *const u8,
    data_size: u32,
    channels: u32,
    description: AudioStreamPacketDescription,
    consumed: bool,
}

/// `AudioConverterComplexInputDataProc` supplying one packet, then
/// `NO_MORE_INPUT`.
///
/// # Safety
///
/// `user_data` must point to a live `Input`, and `packets`, `data` and
/// `descriptions` must be the valid pointers `AudioConverter` passes.
unsafe extern "C-unwind" fn supply_input(
    _converter: AudioConverterRef,
    packets: NonNull<u32>,
    data: NonNull<AudioBufferList>,
    descriptions: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> i32 {
    // SAFETY: guaranteed by the caller contract above.
    unsafe {
        let input = &mut *user_data.cast::<Input>();
        if input.consumed {
            *packets.as_ptr() = 0;
            return NO_MORE_INPUT;
        }
        input.consumed = true;
        *packets.as_ptr() = 1;
        let list = &mut *data.as_ptr();
        list.mNumberBuffers = 1;
        list.mBuffers[0] = AudioBuffer {
            mNumberChannels: input.channels,
            mDataByteSize: input.data_size,
            mData: input.data.cast_mut().cast(),
        };
        if !descriptions.is_null() {
            *descriptions = &raw mut input.description;
        }
    }
    0
}

/// Wraps an `AudioSpecificConfig` in the MPEG-4 elementary stream descriptor
/// (`esds` payload) that `AudioToolbox` takes as the AAC decoder's magic cookie.
fn elementary_stream_descriptor(audio_specific_config: &[u8]) -> Vec<u8> {
    const ES_DESCRIPTOR: u8 = 0x03;
    const DECODER_CONFIG: u8 = 0x04;
    const DECODER_SPECIFIC_INFO: u8 = 0x05;
    const SL_CONFIG: u8 = 0x06;
    /// MPEG-4 audio object type, audio stream.
    const DECODER_CONFIG_HEADER: [u8; 13] = [0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// A descriptor whose length uses the four-byte form.
    fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
        let length = u32::try_from(body.len()).expect("descriptor fits u32");
        let mut bytes = vec![tag];
        bytes.extend((0..4).rev().map(|group| {
            let bits = u8::try_from((length >> (7 * group)) & 0x7f).expect("7 bits fit u8");
            if group == 0 { bits } else { bits | 0x80 }
        }));
        bytes.extend_from_slice(body);
        bytes
    }

    let decoder_config = [
        DECODER_CONFIG_HEADER.as_slice(),
        &descriptor(DECODER_SPECIFIC_INFO, audio_specific_config),
    ]
    .concat();
    let stream = [
        [0_u8; 3].as_slice(),
        &descriptor(DECODER_CONFIG, &decoder_config),
        &descriptor(SL_CONFIG, &[0x02]),
    ]
    .concat();
    descriptor(ES_DESCRIPTOR, &stream)
}

const fn frames_per_packet(format: StreamFormat) -> u32 {
    if format.sbr {
        AAC_SBR_FRAMES_PER_PACKET
    } else {
        AAC_LC_FRAMES_PER_PACKET
    }
}

fn float_pcm(sample_rate: u32, channels: u16) -> AudioStreamBasicDescription {
    let bytes_per_frame = u32::from(channels) * F32_BITS / 8;
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: u32::from(channels),
        mBitsPerChannel: F32_BITS,
        mReserved: 0,
    }
}

fn check(status: i32, call: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(describe(call, status))
    }
}

/// Names a failed call by its `OSStatus`, shown as a four-character code when
/// it is one.
fn describe(call: &str, status: i32) -> String {
    let code = status.to_be_bytes();
    if code.iter().all(u8::is_ascii_graphic) {
        format!(
            "{call} failed with '{}' ({status})",
            String::from_utf8_lossy(&code)
        )
    } else {
        format!("{call} failed with OSStatus {status}")
    }
}

fn decode_error(presentation_time: Duration, message: &str) -> PacketAudioError {
    PacketAudioError::Decode {
        codec: "AAC",
        presentation_time,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
