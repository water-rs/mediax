//! Entry points for the cargo-fuzz targets in `codec/fuzz`.
//!
//! They are not part of the crate's stable API: they exist so the fuzz
//! targets can reach the private H.264 parser and decoder.

use crate::{
    annexb::annex_b_nalus,
    h264::{
        sequence::{ScalingLists, Sequence},
        slice::ParameterSets,
        testing::{SliceSpec, StreamConfig, TestPoc},
    },
    h265::{
        self,
        dpb::{RpsEntry, ShortTermRps},
        nal::{NalHeader, NalUnitType},
        testing::{LongTermSpec, RpsCoding, ScalingData, SliceRps},
    },
    sys::vaapi::{H264Decoder, HevcDecoder, VaapiDecoder, double::Driver},
};
use std::path::Path;

/// The marker that splits fuzz input into packets: a start code and an access
/// unit delimiter NAL unit.
const PACKET_MARKER: [u8; 5] = [0, 0, 0, 1, 9];

/// The same marker for H.265: its access unit delimiter NAL unit header.
const HEVC_PACKET_MARKER: [u8; 6] = [0, 0, 0, 1, 0x46, 1];

/// Parses `data` as Annex B parameter sets and slices, and derives what the
/// decoder derives from them. Never panics, whatever the input.
pub fn parse_h264(data: &[u8]) {
    let mut sets = ParameterSets::default();
    for (start, end) in annex_b_nalus(data) {
        let nal = &data[start..end];
        match nal[0] & 0x1f {
            7 => drop(sets.put_sps(nal)),
            8 => drop(sets.put_pps(nal)),
            1..=5 => {
                if let Ok(slice) = sets.slice(nal)
                    && let Ok((sps, pps)) = sets.for_picture(slice.header.pps_id)
                {
                    drop(Sequence::new(sps));
                    drop(ScalingLists::new(sps, pps));
                }
            }
            _ => {}
        }
    }
}

/// Parses `data` as Annex B parameter sets and slice segments, and derives
/// what the H.265 decoder derives from them. Never panics, whatever the input.
pub fn parse_h265(data: &[u8]) {
    let mut sets = h265::slice::ParameterSets::default();
    let mut previous = None;
    for (start, end) in annex_b_nalus(data) {
        let nal = &data[start..end];
        let Ok(header) = NalHeader::parse(nal) else {
            continue;
        };
        match header.kind {
            NalUnitType::VPS => drop(sets.put_vps(nal)),
            NalUnitType::SPS => drop(sets.put_sps(nal)),
            NalUnitType::PPS => drop(sets.put_pps(nal)),
            _ if header.unit_type().is_decodable_slice() => {
                if let Ok(slice) = sets.slice(nal, previous.as_ref()) {
                    drop(h265::sequence::Sequence::new(slice.sps.clone()));
                    drop(h265::sequence::check_pps(&slice.sps, &slice.pps));
                    std::hint::black_box(h265::sequence::ScalingFactors::new(
                        &slice.sps, &slice.pps,
                    ));
                    drop(h265::sequence::Tiling::new(&slice.sps, &slice.pps));
                    previous = Some(slice.header);
                }
            }
            _ => {}
        }
    }
}

/// Decodes `data` through the H.265 decoder and the driver double.
///
/// As [`decode_h264`] does: the first byte chooses the stream, which starts
/// with an IDR picture and a few pictures that use the reference sets, and the
/// rest is cut into packets at access unit delimiters.
pub fn decode_h265(data: &[u8]) {
    use h265::testing::SliceSpec;

    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let config = h265_stream_config(selector);
    let _driver = Driver::new();
    let mut decoder = HevcDecoder::with_api(Driver::api(), Path::new("/dev/null"), None)
        .expect("the double decodes H.265");
    // The IDR picture, then pictures that take their references from the
    // sequence's sets and from the long-term list.
    let mut by_set = SliceSpec::new(1, 1, 1);
    by_set.rps = SliceRps::Sps(0);
    let mut with_long_term = SliceSpec::new(1, 1, 2);
    with_long_term.rps = SliceRps::Inline(RpsCoding::Explicit(ShortTermRps {
        negative: vec![RpsEntry {
            delta_poc: -1,
            used_by_curr: true,
        }],
        positive: Vec::new(),
    }));
    with_long_term.long_term = vec![LongTermSpec::Sps {
        index: 0,
        msb_cycle: None,
    }];
    let mut explicit_long_term = with_long_term.clone();
    explicit_long_term.poc_lsb = 3;
    explicit_long_term.long_term = vec![LongTermSpec::Explicit {
        poc_lsb: 0,
        used: true,
        msb_cycle: None,
    }];
    let start = h265::testing::annex_b(&config.parameter_sets());
    drop(decoder.decode(&start, 0));
    for (timestamp, spec) in [SliceSpec::idr(), by_set, with_long_term, explicit_long_term]
        .iter()
        .enumerate()
    {
        let sample = h265::testing::annex_b(&[spec.nal(&config)]);
        drop(decoder.decode(&sample, timestamp as u64));
    }
    for (timestamp, packet) in packets(rest, &HEVC_PACKET_MARKER).enumerate() {
        drop(decoder.decode(packet, timestamp as u64 + 4));
    }
    drop(decoder.drain());
}

/// The H.265 stream parameters a selector byte stands for.
fn h265_stream_config(selector: u8) -> h265::testing::StreamConfig {
    let reordering = selector & 0x01 != 0;
    h265::testing::StreamConfig {
        profile_idc: if selector & 0x02 != 0 { 2 } else { 1 },
        bit_depth: if selector & 0x02 != 0 { 10 } else { 8 },
        crop: (selector & 0x04 != 0).then_some([1, 1, 1, 1]),
        max_num_reorder: if reordering { 2 } else { 0 },
        max_dec_pic_buffering: if reordering { 5 } else { 3 },
        scaling: (selector & 0x08 != 0).then_some(Some(ScalingData::Flat)),
        pps_scaling: (selector & 0x08 != 0).then_some(ScalingData::Defaults),
        short_term_sets: vec![
            RpsCoding::Explicit(ShortTermRps {
                negative: vec![RpsEntry {
                    delta_poc: -1,
                    used_by_curr: true,
                }],
                positive: Vec::new(),
            }),
            RpsCoding::Inter {
                delta_idx_minus1: 0,
                delta_rps: -1,
                flags: vec![(true, true), (true, true)],
            },
        ],
        long_term_sps: Some(vec![(0, true)]),
        tiles: (selector & 0x10 != 0).then_some((2, 2)),
        temporal_mvp: selector & 0x20 != 0,
        weighted_pred: selector & 0x40 != 0,
        dependent_slices: selector & 0x80 != 0,
        ..h265::testing::StreamConfig::default()
    }
}

/// Decodes `data` through the decoder and the driver double.
///
/// The first byte chooses the parameters of a valid stream, which is started
/// with an IDR picture; the rest is the fuzzed continuation, cut into packets
/// at access unit delimiters. Errors are expected and ignored; a panic is a
/// bug, and so is any call the double rejects.
pub fn decode_h264(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let config = stream_config(selector);
    let _driver = Driver::new();
    let mut decoder = H264Decoder::with_api(Driver::api(), Path::new("/dev/null"), None)
        .expect("the double decodes H.264");
    let mut start = config.parameter_sets();
    start.extend_from_slice(&[0, 0, 0, 1]);
    start.extend_from_slice(&config.slice(&SliceSpec::idr()));
    drop(decoder.decode(&start, 0));
    for (timestamp, packet) in packets(rest, &PACKET_MARKER).enumerate() {
        drop(decoder.decode(packet, timestamp as u64 + 1));
    }
    drop(decoder.drain());
}

/// The stream parameters a selector byte stands for.
fn stream_config(selector: u8) -> StreamConfig {
    let reordering = selector & 0x08 != 0;
    StreamConfig {
        profile_idc: [66, 77, 100, 100][usize::from(selector >> 4 & 3)],
        poc: match selector & 3 {
            0 => TestPoc::Zero(8),
            1 => TestPoc::One {
                cycle: vec![2, 2],
                non_ref: -1,
                top_to_bottom: 0,
            },
            2 => TestPoc::Two,
            _ => TestPoc::Zero(5),
        },
        gaps_allowed: selector & 0x04 != 0,
        max_num_ref_frames: if reordering { 2 } else { 4 },
        restrictions: reordering.then_some((1, 3)),
        crop: (selector & 0x40 != 0).then_some([1, 1, 1, 1]),
        cabac: selector & 0x80 != 0,
        ..StreamConfig::default()
    }
}

/// Splits `data` at every access unit delimiter.
fn packets<'a>(data: &'a [u8], marker: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = Some(data);
    std::iter::from_fn(move || {
        let current = rest?;
        if let Some(end) = current
            .windows(marker.len())
            .position(|window| window == marker)
        {
            rest = Some(&current[end + marker.len()..]);
            Some(&current[..end])
        } else {
            rest = None;
            Some(current)
        }
    })
}

/// Mutated valid streams through the parsers and decoders: none may panic and
/// the driver double accepts every call. The generator is seeded, so a failure
/// reproduces.
#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64.
    struct Rng(u64);

    impl Rng {
        const fn next(&mut self) -> u64 {
            let mut state = self.0;
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            self.0 = state;
            state
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
        }

        fn byte(&mut self) -> u8 {
            self.next().to_le_bytes()[0]
        }
    }

    /// `seed` with a few bytes changed, dropped or added.
    fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut data = seed.to_vec();
        for _ in 0..=rng.below(8) {
            match rng.below(4) {
                0 if !data.is_empty() => {
                    let index = rng.below(data.len());
                    data[index] ^= 1 << rng.below(8);
                }
                1 => data.truncate(rng.below(data.len() + 1)),
                2 => {
                    let index = rng.below(data.len() + 1);
                    data.insert(index, rng.byte());
                }
                _ if !data.is_empty() => {
                    let index = rng.below(data.len());
                    data.remove(index);
                }
                _ => {}
            }
        }
        data
    }

    #[test]
    fn mutated_streams_never_panic() {
        let h264 = {
            let config = StreamConfig::default();
            let mut stream = config.parameter_sets();
            for spec in [SliceSpec::idr(), SliceSpec::p(1, 2)] {
                stream.extend_from_slice(&[0, 0, 0, 1]);
                stream.extend_from_slice(&config.slice(&spec));
            }
            stream
        };
        let h265 = {
            let config = h265::testing::StreamConfig::default();
            let mut nals = config.parameter_sets();
            nals.push(h265::testing::SliceSpec::idr().nal(&config));
            nals.push(h265::testing::SliceSpec::new(1, 1, 1).nal(&config));
            h265::testing::annex_b(&nals)
        };
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..150 {
            let stream = mutate(&h264, &mut rng);
            parse_h264(&stream);
            decode_h264(&[&[rng.byte()], stream.as_slice()].concat());
            let stream = mutate(&h265, &mut rng);
            parse_h265(&stream);
            decode_h265(&[&[rng.byte()], stream.as_slice()].concat());
        }
    }
}
