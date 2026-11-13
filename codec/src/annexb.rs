//! Annex B framing shared by the encoders and the decoder-side converter.

#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
use crate::{BitstreamFormat, CodecError, EncodedPacket};
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
use std::time::Duration;

/// The four-byte Annex B start code.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Returns `(start, end)` byte ranges of each NAL unit after its start code.
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
pub fn annex_b_nalus(data: &[u8]) -> Vec<(usize, usize)> {
    let mut nalus = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let (is_start, start_code_len) = if data[i..].starts_with(&[0, 0, 0, 1]) {
            (true, 4)
        } else if data[i..].starts_with(&[0, 0, 1]) {
            (true, 3)
        } else {
            (false, 0)
        };
        if is_start {
            let start = i + start_code_len;
            let mut end = data.len();
            let mut j = start;
            while j + 3 <= data.len() {
                if data[j..].starts_with(&[0, 0, 1]) || data[j..].starts_with(&[0, 0, 0, 1]) {
                    end = j;
                    break;
                }
                j += 1;
            }
            if start < end {
                nalus.push((start, end));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    nalus
}

/// Rewrites a length-prefixed access unit with start codes.
#[cfg(any(
    test,
    mediax_hw_codec_apple,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
pub fn length_prefixed_to_annex_b(
    sample: &[u8],
    nal_length_size: usize,
) -> Result<Vec<u8>, &'static str> {
    let mut offset = 0;
    let mut output = Vec::with_capacity(sample.len() + 64);
    while offset + nal_length_size <= sample.len() {
        let nal_len = read_length_field(&sample[offset..offset + nal_length_size]);
        offset += nal_length_size;
        if nal_len == 0 {
            continue;
        }
        let end = offset
            .checked_add(nal_len)
            .ok_or("length-prefixed NAL size overflowed usize")?;
        let nal = sample
            .get(offset..end)
            .ok_or("length-prefixed sample has a truncated NAL unit")?;
        output.extend_from_slice(&START_CODE);
        output.extend_from_slice(nal);
        offset = end;
    }
    if offset != sample.len() {
        return Err("length-prefixed sample has trailing bytes");
    }
    Ok(output)
}

pub fn read_length_field(bytes: &[u8]) -> usize {
    match bytes {
        [a] => *a as usize,
        [a, b] => u16::from_be_bytes([*a, *b]) as usize,
        [a, b, c] => ((*a as usize) << 16) | ((*b as usize) << 8) | (*c as usize),
        [a, b, c, d] => u32::from_be_bytes([*a, *b, *c, *d]) as usize,
        _ => unreachable!("NAL length size is encoded in two bits and must be 1..=4"),
    }
}

/// Returns the NAL unit type of `nalu` (its first bytes, header included).
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
const fn nal_type(nalu: &[u8], is_hevc: bool) -> u8 {
    if is_hevc {
        (nalu[0] >> 1) & 0x3F
    } else {
        nalu[0] & 0x1F
    }
}

/// Whether `nal_type` is a parameter set: SPS or PPS, plus VPS for H.265.
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
const fn is_parameter_set(nal_type: u8, is_hevc: bool) -> bool {
    if is_hevc {
        matches!(nal_type, 32..=34)
    } else {
        matches!(nal_type, 7 | 8)
    }
}

/// Whether `nal_type` is an IDR slice (`IDR_W_RADL` or `IDR_N_LP` for H.265).
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
const fn is_idr(nal_type: u8, is_hevc: bool) -> bool {
    if is_hevc {
        matches!(nal_type, 19 | 20)
    } else {
        nal_type == 5
    }
}

/// Frames the raw Annex B access units a platform encoder produces as the
/// configured [`BitstreamFormat`].
///
/// Platform encoders disagree on whether an IDR carries its parameter sets, so
/// this keeps the last ones seen (seeded from the encoder's own configuration)
/// and puts them in front of every IDR that lacks them.
#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
#[derive(Debug)]
pub struct AnnexBPackets {
    is_hevc: bool,
    bitstream: BitstreamFormat,
    /// Annex B bytes of the most recent VPS/SPS/PPS.
    parameter_sets: Vec<u8>,
}

#[cfg(any(
    test,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi
))]
impl AnnexBPackets {
    pub const fn new(is_hevc: bool, bitstream: BitstreamFormat, parameter_sets: Vec<u8>) -> Self {
        Self {
            is_hevc,
            bitstream,
            parameter_sets,
        }
    }

    /// Replaces the cached parameter sets with the Annex B `parameter_sets`.
    #[cfg(any(test, mediax_hw_codec_android, mediax_hw_codec_windows))]
    pub fn set_parameter_sets(&mut self, parameter_sets: Vec<u8>) {
        self.parameter_sets = parameter_sets;
    }

    /// Frames one raw Annex B access unit.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::EncodingFailed`] when `raw` has no start code, or
    /// when an IDR arrives and no parameter sets are known for an Annex B stream.
    pub fn packet(
        &mut self,
        raw: Vec<u8>,
        presentation_time: Duration,
        decode_time_nanos: i64,
    ) -> Result<EncodedPacket, CodecError> {
        let nalus = annex_b_nalus(&raw);
        if nalus.is_empty() {
            return Err(CodecError::EncodingFailed(
                "the platform encoder emitted an access unit without a start code".into(),
            ));
        }
        let mut seen_sets = Vec::new();
        let mut keyframe = false;
        for &(start, end) in &nalus {
            let kind = nal_type(&raw[start..end], self.is_hevc);
            if is_parameter_set(kind, self.is_hevc) {
                seen_sets.extend_from_slice(&START_CODE);
                seen_sets.extend_from_slice(&raw[start..end]);
            }
            keyframe |= is_idr(kind, self.is_hevc);
        }
        let in_band = !seen_sets.is_empty();
        if in_band {
            self.parameter_sets = seen_sets;
        }
        match self.bitstream {
            BitstreamFormat::AnnexB => {
                if keyframe && !in_band {
                    if self.parameter_sets.is_empty() {
                        return Err(CodecError::EncodingFailed(
                            "the platform encoder emitted an IDR before any parameter sets".into(),
                        ));
                    }
                    let mut data = Vec::with_capacity(self.parameter_sets.len() + raw.len());
                    data.extend_from_slice(&self.parameter_sets);
                    data.extend_from_slice(&raw);
                    return Ok(EncodedPacket {
                        data,
                        keyframe: true,
                        presentation_time,
                        decode_time_nanos,
                    });
                }
                Ok(EncodedPacket {
                    data: raw,
                    keyframe,
                    presentation_time,
                    decode_time_nanos,
                })
            }
            BitstreamFormat::LengthPrefixed => {
                let (data, _) = crate::bitstream::annex_b_to_length_prefixed(&raw, self.is_hevc);
                Ok(EncodedPacket {
                    data: data.ok_or_else(|| {
                        CodecError::EncodingFailed(
                            "access unit is too large to length-prefix".into(),
                        )
                    })?,
                    keyframe,
                    presentation_time,
                    decode_time_nanos,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 4] = [0x67, 0x42, 0xE0, 0x1F];
    const PPS: [u8; 2] = [0x68, 0xCE];
    const IDR: [u8; 3] = [0x65, 0x88, 0x84];
    const P_SLICE: [u8; 3] = [0x41, 0x9A, 0x00];

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn length_prefixed_sample_gets_start_codes() {
        let mut sample = Vec::new();
        for nal in [&SPS[..], &PPS[..], &IDR[..]] {
            sample.extend_from_slice(&u32::try_from(nal.len()).unwrap().to_be_bytes());
            sample.extend_from_slice(nal);
        }
        assert_eq!(
            length_prefixed_to_annex_b(&sample, 4).unwrap(),
            annex_b(&[&SPS, &PPS, &IDR])
        );
    }

    #[test]
    fn truncated_length_prefixed_sample_is_rejected() {
        assert!(length_prefixed_to_annex_b(&[0, 0, 0, 9, 0x65], 4).is_err());
    }

    #[test]
    fn idr_without_parameter_sets_gets_them_prepended() {
        let mut packets =
            AnnexBPackets::new(false, BitstreamFormat::AnnexB, annex_b(&[&SPS, &PPS]));
        let packet = packets.packet(annex_b(&[&IDR]), Duration::ZERO, 0).unwrap();
        assert!(packet.keyframe);
        assert_eq!(packet.data, annex_b(&[&SPS, &PPS, &IDR]));
    }

    #[test]
    fn idr_with_parameter_sets_is_left_alone_and_cached() {
        let mut packets = AnnexBPackets::new(false, BitstreamFormat::AnnexB, Vec::new());
        let first = annex_b(&[&SPS, &PPS, &IDR]);
        assert_eq!(
            packets
                .packet(first.clone(), Duration::ZERO, 0)
                .unwrap()
                .data,
            first
        );
        let second = packets.packet(annex_b(&[&IDR]), Duration::ZERO, 0).unwrap();
        assert_eq!(second.data, annex_b(&[&SPS, &PPS, &IDR]));
    }

    #[test]
    fn delta_frames_are_not_keyframes_and_get_no_parameter_sets() {
        let mut packets =
            AnnexBPackets::new(false, BitstreamFormat::AnnexB, annex_b(&[&SPS, &PPS]));
        let packet = packets
            .packet(annex_b(&[&P_SLICE]), Duration::ZERO, 0)
            .unwrap();
        assert!(!packet.keyframe);
        assert_eq!(packet.data, annex_b(&[&P_SLICE]));
    }

    #[test]
    fn idr_with_no_known_parameter_sets_is_an_error() {
        let mut packets = AnnexBPackets::new(false, BitstreamFormat::AnnexB, Vec::new());
        assert!(packets.packet(annex_b(&[&IDR]), Duration::ZERO, 0).is_err());
    }

    #[test]
    fn replacing_parameter_sets_changes_what_an_idr_gets() {
        let mut packets =
            AnnexBPackets::new(false, BitstreamFormat::AnnexB, annex_b(&[&SPS, &PPS]));
        let other_pps = [0x68, 0xEE];
        packets.set_parameter_sets(annex_b(&[&SPS, &other_pps]));
        let packet = packets.packet(annex_b(&[&IDR]), Duration::ZERO, 0).unwrap();
        assert_eq!(packet.data, annex_b(&[&SPS, &other_pps, &IDR]));
    }

    #[test]
    fn hevc_idr_gets_vps_sps_pps() {
        let vps = [0x40, 0x01, 0x0C];
        let sps = [0x42, 0x01, 0x01];
        let pps = [0x44, 0x01, 0xC1];
        let idr = [0x26, 0x01, 0xAF];
        let mut packets =
            AnnexBPackets::new(true, BitstreamFormat::AnnexB, annex_b(&[&vps, &sps, &pps]));
        let packet = packets.packet(annex_b(&[&idr]), Duration::ZERO, 0).unwrap();
        assert!(packet.keyframe);
        assert_eq!(packet.data, annex_b(&[&vps, &sps, &pps, &idr]));
    }

    #[test]
    fn length_prefixed_output_converts_and_flags_keyframes() {
        let mut packets = AnnexBPackets::new(false, BitstreamFormat::LengthPrefixed, Vec::new());
        let packet = packets
            .packet(annex_b(&[&SPS, &PPS, &IDR]), Duration::ZERO, 0)
            .unwrap();
        assert!(packet.keyframe);
        assert_eq!(&packet.data[..4], &4_u32.to_be_bytes());
        let delta = packets
            .packet(annex_b(&[&P_SLICE]), Duration::ZERO, 0)
            .unwrap();
        assert!(!delta.keyframe);
    }
}
