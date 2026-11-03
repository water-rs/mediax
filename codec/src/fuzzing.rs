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
    sys::vaapi::{VaapiDecoder, double::Driver},
};
use std::path::Path;

/// The marker that splits fuzz input into packets: a start code and an access
/// unit delimiter NAL unit.
const PACKET_MARKER: [u8; 5] = [0, 0, 0, 1, 9];

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
    let mut decoder = VaapiDecoder::with_api(Driver::api(), Path::new("/dev/null"), None)
        .expect("the double decodes H.264");
    let mut start = config.parameter_sets();
    start.extend_from_slice(&[0, 0, 0, 1]);
    start.extend_from_slice(&config.slice(&SliceSpec::idr()));
    drop(decoder.decode(&start, 0));
    for (timestamp, packet) in packets(rest).enumerate() {
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
fn packets(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(data);
    std::iter::from_fn(move || {
        let current = rest?;
        if let Some(end) = current
            .windows(PACKET_MARKER.len())
            .position(|window| window == PACKET_MARKER)
        {
            rest = Some(&current[end + PACKET_MARKER.len()..]);
            Some(&current[..end])
        } else {
            rest = None;
            Some(current)
        }
    })
}
