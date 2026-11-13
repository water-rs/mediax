//! Conformance of the VA-API H.265 decoder against the JCT-VC `HEVC_v1`
//! Main and Main 10 bitstreams, on hardware.
//!
//! The vectors are fetched at test time from the ITU-T JCT-VC site, checked
//! against a pinned SHA-256, and cached under the target directory; none is
//! committed. The test needs `curl`, `unzip`, `sha256sum` and `md5sum`, a
//! VA-API render node that decodes Main and Main 10, and is `#[ignore]`d: run
//! it with `cargo test -p mediax-codec --test h265_conformance -- --ignored`.
//!
//! The archives carry the MD5 of the reference decoder's output, not the
//! output itself: all pictures in output order, cropped to the conformance
//! window, planar 4:2:0, one byte a sample for 8-bit streams and two (little
//! endian) for 10-bit ones, each picture at its own size. The pinned list,
//! `h265_conformance_vectors.txt`, holds the archive name, the SHA-256 of the
//! archive and that MD5 for every vector of the directory except the one
//! excluded below: 146 of 147. Each MD5 was confirmed against `FFmpeg`'s
//! decoding of the archive (`ffmpeg -i stream -f rawvideo -fps_mode
//! passthrough`), with two exceptions that were investigated:
//!
//! - `VPSSPSPPS_A_MainConcept_1` has six pictures of six different sizes,
//!   whose parameter sets all precede the first picture in arbitrary order.
//!   `FFmpeg` cannot decode it: it reports "SPS 2 does not exist" and the
//!   like, and outputs 1534464 bytes where the six pictures take 2756736
//!   (176x144, 320x240, 352x288, 640x480, 704x576 and 1280x720 at 4:2:0), so
//!   its MD5 differs for that reason. The archive's value is pinned
//!   unconfirmed.
//! - `SAODBLK_A_MainConcept_4` and `SAODBLK_B_MainConcept_4` decode in
//!   `FFmpeg` to exactly the 500 pictures of 1016x760 the archive describes,
//!   without a single error, single-threaded or not, with or without
//!   cropping, but to an MD5 other than the one in the archive
//!   (`b9c98937bcb6be445348fdc89c208f1e` and
//!   `031043495e9b9f4ce120afa316aa5705`); the streams carry no picture hash
//!   SEI to arbitrate, and the archive's text still names an earlier revision
//!   (`SAODBLK_A_Divx_1`), so its MD5 is probably stale. `FFmpeg`'s output is
//!   pinned for these two, which makes them a cross-check against `FFmpeg`
//!   rather than against the reference decoder: a mismatch on hardware means
//!   a difference to be settled by a third decoder.
//!
//! Excluded:
//!
//! - `TSUNEQBD_A_MAIN10_Technicolor_2`: luma and chroma have different bit
//!   depths, which is outside Main and Main 10 (both require them equal).
//!
//! Every other vector of the directory is progressive 4:2:0 at 8 or 10 bits.

#![cfg(target_os = "linux")]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use mediax_codec::{CodecType, DecodePacket, DecodeStream, DecodedPixelLayout, Decoder};

const SITE: &str =
    "https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/draft_conformance/HEVC_v1";

/// One vector per line: the archive name without `.zip`, its SHA-256, and the
/// MD5 of the reference output.
const VECTORS: &str = include_str!("h265_conformance_vectors.txt");

fn run(command: &mut Command) -> Vec<u8> {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cannot run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn sha256(path: &Path) -> String {
    let output = String::from_utf8(run(Command::new("sha256sum").arg(path))).unwrap();
    output.split_whitespace().next().unwrap().to_owned()
}

/// The archive, downloaded once and verified against its pinned hash.
fn archive(name: &str, pinned: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jctvc-conformance");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}.zip"));
    if !path.exists() || sha256(&path) != pinned {
        run(Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--output",
            ])
            .arg(&path)
            .arg(format!("{SITE}/{name}.zip")));
    }
    assert_eq!(sha256(&path), pinned, "{name} is not the pinned archive");
    path
}

/// The Annex B stream of an archive: its only `.bit` or `.bin` file outside
/// the `__MACOSX` resource forks.
fn stream(archive: &Path) -> Vec<u8> {
    let directory = archive.with_extension("");
    run(Command::new("unzip")
        .args(["-o", "-q"])
        .arg(archive)
        .arg("-d")
        .arg(&directory));
    let mut found = Vec::new();
    let mut pending = vec![directory];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == "__MACOSX") {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "bit" || extension == "bin")
            {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "{} holds {found:?}", archive.display());
    fs::read(&found[0]).unwrap()
}

/// Splits an Annex B stream into pictures: a unit ends where the next slice
/// segment starts a picture, or where a parameter set, delimiter or prefix SEI
/// follows a slice segment.
fn access_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut start_codes = Vec::new();
    let mut index = 0;
    while index + 3 <= stream.len() {
        if stream[index..index + 3] == [0, 0, 1] {
            start_codes.push(index);
            index += 3;
        } else {
            index += 1;
        }
    }
    // A NAL unit never ends in a zero byte, so zeros before a start code
    // belong to the start code.
    let trimmed = |end: usize| {
        stream[..end]
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |position| position + 1)
    };
    let mut units = Vec::new();
    let mut begin = None;
    let mut has_slice = false;
    for &code in &start_codes {
        let nal = code + 3;
        let Some(&header) = stream.get(nal) else {
            continue;
        };
        let kind = header >> 1 & 0x3f;
        let slice = matches!(kind, 0..=9 | 16..=21);
        // `first_slice_segment_in_pic_flag` is the first bit after the header.
        let first_slice = slice && stream.get(nal + 2).is_some_and(|byte| byte & 0x80 != 0);
        if has_slice && (first_slice || matches!(kind, 32..=35 | 39 | 41..=44)) {
            units.push(&stream[begin.take().unwrap()..trimmed(code)]);
            has_slice = false;
        }
        begin.get_or_insert(code);
        has_slice |= slice;
    }
    if let Some(begin) = begin {
        units.push(&stream[begin..]);
    }
    units
}

/// Converts a decoded frame to planar 4:2:0 with 8-bit samples, or 16-bit
/// little-endian ones for 10-bit streams.
fn to_planar(frame: &[u8], layout: DecodedPixelLayout, width: usize, height: usize) -> Vec<u8> {
    let mut planar = Vec::with_capacity(frame.len());
    match layout {
        DecodedPixelLayout::Nv12 => {
            let (luma, chroma) = frame.split_at(width * height);
            planar.extend_from_slice(luma);
            let (pairs, _) = chroma.as_chunks::<2>();
            planar.extend(pairs.iter().map(|[cb, _]| *cb));
            planar.extend(pairs.iter().map(|[_, cr]| *cr));
        }
        DecodedPixelLayout::P010 => {
            // The 10 significant bits sit at the top of each 16-bit word.
            let sample = |word: [u8; 2]| (u16::from_le_bytes(word) >> 6).to_le_bytes();
            let (luma, chroma) = frame.split_at(width * height * 2);
            planar.extend(
                luma.as_chunks::<2>()
                    .0
                    .iter()
                    .flat_map(|&word| sample(word)),
            );
            let (pairs, _) = chroma.as_chunks::<4>();
            planar.extend(pairs.iter().flat_map(|pair| sample([pair[0], pair[1]])));
            planar.extend(pairs.iter().flat_map(|pair| sample([pair[2], pair[3]])));
        }
    }
    planar
}

fn conformance(name: &str, pinned_archive: &str, pinned_md5: &str) -> Result<(), String> {
    let stream = stream(&archive(name, pinned_archive));
    let mut decoder = Decoder::new(CodecType::H265, None, 0, 0)
        .map_err(|error| format!("cannot open the decoder: {error}"))?;
    let mut md5 = Command::new("md5sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run md5sum: {error}"))?;
    let mut input = md5.stdin.take().unwrap();
    let mut frames_seen = 0_usize;
    let mut collect = |frames: DecodeStream| -> Result<(), String> {
        for frame in frames {
            let frame = frame.map_err(|error| format!("decode failed: {error}"))?;
            let (width, height) = (frame.width() as usize, frame.height() as usize);
            let layout = frame.pixel_layout();
            let length = layout.packed_len(frame.width(), frame.height());
            let mut data = vec![0; length];
            let written = frame.copy_to_buffer(&mut data);
            if written != length {
                return Err(format!("a frame holds {written} bytes, expected {length}"));
            }
            input
                .write_all(&to_planar(&data, layout, width, height))
                .map_err(|error| format!("cannot feed md5sum: {error}"))?;
            frames_seen += 1;
        }
        Ok(())
    };
    for (index, unit) in access_units(&stream).into_iter().enumerate() {
        let time = Duration::from_millis(index as u64 * 40);
        collect(decoder.decode(DecodePacket::new(unit, time)))?;
    }
    collect(decoder.drain())?;
    drop(input);
    let output = md5
        .wait_with_output()
        .map_err(|error| format!("md5sum failed: {error}"))?;
    let output = String::from_utf8_lossy(&output.stdout);
    let got = output.split_whitespace().next().unwrap_or_default();
    if got == pinned_md5 {
        Ok(())
    } else {
        Err(format!(
            "the output of {frames_seen} frames hashes to {got}, expected {pinned_md5}"
        ))
    }
}

#[test]
#[ignore = "needs a VA-API render node, network access, curl, unzip, sha256sum and md5sum"]
fn main_and_main_10_vectors_decode_bit_exactly() {
    let failures: Vec<String> = VECTORS
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (Some(name), Some(archive), Some(md5), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                panic!("malformed vector line {line:?}");
            };
            conformance(name, archive, md5)
                .err()
                .map(|reason| format!("{name}: {reason}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
