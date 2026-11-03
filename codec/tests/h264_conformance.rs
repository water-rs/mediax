//! Conformance of the VA-API H.264 decoder against the JVT-AVC_V1 and `FRExt`
//! bitstreams, on hardware.
//!
//! The vectors are fetched at test time from the ITU-T JVT site, checked
//! against a pinned SHA-256, and cached under the target directory; none is
//! committed. The test needs `curl`, `unzip` and `sha256sum`, a VA-API render
//! node, and is `#[ignore]`d: run it with `cargo test -p mediax-codec --test
//! h264_conformance -- --ignored`.
//!
//! Every frame the decoder outputs is compared byte for byte with the
//! reference YUV shipped in the vector's archive.

#![cfg(target_os = "linux")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use mediax_codec::{CodecType, DecodePacket, DecodeStream, Decoder};

const SITE: &str = "https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance";

/// A progressive Baseline, Main or High vector.
struct Vector {
    /// Directory under [`SITE`].
    set: &'static str,
    /// Archive name without `.zip`.
    name: &'static str,
    sha256: &'static str,
    /// Annex B stream inside the archive.
    stream: &'static str,
    /// Reference decoder output (planar 4:2:0) inside the archive.
    reference: &'static str,
    width: usize,
    height: usize,
}

const fn vector(
    set: &'static str,
    name: &'static str,
    sha256: &'static str,
    stream: &'static str,
    reference: &'static str,
    (width, height): (usize, usize),
) -> Vector {
    Vector {
        set,
        name,
        sha256,
        stream,
        reference,
        width,
        height,
    }
}

const QCIF: (usize, usize) = (176, 144);
const CIF: (usize, usize) = (352, 288);

const VECTORS: &[Vector] = &[
    vector(
        "AVCv1",
        "BA1_Sony_D",
        "399eae2eb03f82fc291bdc4fe9cedd110b40a96368ffe1e1bcf9ff6436fa9df7",
        "BA1_Sony_D.jsv",
        "BA1_Sony_D.yuv",
        QCIF,
    ),
    vector(
        "AVCv1",
        "BA_MW_D",
        "f85d0c1e6a8317ecff19bf11d1496b327ee11e391de27453031062ea62241843",
        "BA_MW_D.264",
        "BA_MW_D_rec.qcif",
        QCIF,
    ),
    vector(
        "AVCv1",
        "SVA_BA2_D",
        "e0cefee7e9206d06cad7bbdbe1441b6566208d693bcebc4396fa896be3a2c8d4",
        "SVA_BA2_D.264",
        "SVA_BA2_D_rec.yuv",
        QCIF,
    ),
    vector(
        "AVCv1",
        "MIDR_MW_D",
        "69e7d04bd4b782f93401b12b79d58a96b39319d932743af8bc7134c752d6b577",
        "MIDR_MW_D.264",
        "MIDR_MW_D_rec.qcif",
        QCIF,
    ),
    vector(
        "AVCv1",
        "NRF_MW_E",
        "c48f067fe8ac1309b99292d024324a0a7561bf28312a133995a42d7a98ae9835",
        "NRF_MW_E.264",
        "NRF_MW_E_rec.qcif",
        QCIF,
    ),
    vector(
        "AVCv1",
        "MPS_MW_A",
        "dd96a4e890224097f4f7d8e4c3b47aba3bb986e86b6374d6a50af44453841575",
        "MPS_MW_A.264",
        "MPS_MW_A_rec.qcif",
        QCIF,
    ),
    vector(
        "AVCv1",
        "HCBP2_HHI_A",
        "9847f5d4941ac05c0736f9777398005d7fa72faea6f283a18e2fa72c8ea42659",
        "HCBP2_HHI_A.264",
        "HCBP2_HHI_A.yuv",
        CIF,
    ),
    vector(
        "AVCv1",
        "CABA1_Sony_D",
        "9726f1c1012daef198c98810a88a7776a33159c1bc8daa6e557b55959db50ead",
        "CABA1_Sony_D.jsv",
        "CABA1_Sony_D.yuv",
        QCIF,
    ),
    vector(
        "AVCv1",
        "CABA3_Sony_C",
        "c3b61cff228f09847f17bb0d037f4275f82759565b3f19ab39f6cde482d90755",
        "CABA3_Sony_C.jsv",
        "CABA3_Sony_C.yuv",
        QCIF,
    ),
    vector(
        "AVCv1",
        "HCMP1_HHI_A",
        "4dd7649e42c4e26e22e3877f3d0b30cd42dd3c456d97ee1c19a465322971221e",
        "HCMP1_HHI_A.264",
        "HCMP1_HHI_A.yuv",
        CIF,
    ),
    vector(
        "AVCv1",
        "CVWP1_TOSHIBA_E",
        "4402349be955730a67401e218c6fd20874e1bba4f33ea5c5123e25a80682c6ba",
        "CVWP1_TOSHIBA_E/CVWP1_TOSHIBA_E.264",
        "CVWP1_TOSHIBA_E/CVWP1_TOSHIBA_E_dec.yuv",
        QCIF,
    ),
    vector(
        "FRExt",
        "FRExt1_Panasonic_D",
        "832ea7fae6fe6296c3f0377585a24cb3bf73fbfb51a3adbd852cf571a95ae16d",
        "FRExt1_Panasonic.avc",
        "FRExt1_Panasonic_rec.yuv",
        CIF,
    ),
];

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
fn archive(vector: &Vector) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jvt-conformance");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{}.zip", vector.name));
    if !path.exists() || sha256(&path) != vector.sha256 {
        let url = format!("{SITE}/{}/{}.zip", vector.set, vector.name);
        run(Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--output",
            ])
            .arg(&path)
            .arg(&url));
    }
    assert_eq!(
        sha256(&path),
        vector.sha256,
        "{} does not match its pinned hash",
        vector.name
    );
    path
}

fn extract(archive: &Path, entry: &str) -> Vec<u8> {
    run(Command::new("unzip").arg("-p").arg(archive).arg(entry))
}

/// Splits an Annex B stream into access units. A unit ends where the next
/// picture's first slice begins, or where an SEI, parameter set or delimiter
/// follows a slice.
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
    // A NAL unit never ends in a zero byte, so zeros before a start code are
    // padding of the start code, not part of the previous unit.
    let trimmed = |end: usize| {
        stream[..end]
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |p| p + 1)
    };
    let mut units = Vec::new();
    let mut begin = None;
    let mut has_slice = false;
    for &code in &start_codes {
        let nal = code + 3;
        let Some(&header) = stream.get(nal) else {
            continue;
        };
        let kind = header & 0x1F;
        // `first_mb_in_slice` is 0 when the Exp-Golomb code starts with a one.
        let first_slice =
            matches!(kind, 1 | 5) && stream.get(nal + 1).is_some_and(|b| b & 0x80 != 0);
        if has_slice && (first_slice || matches!(kind, 6..=9)) {
            units.push(&stream[begin.take().unwrap()..trimmed(code)]);
            has_slice = false;
        }
        begin.get_or_insert(code);
        has_slice |= matches!(kind, 1 | 5);
    }
    if let Some(begin) = begin {
        units.push(&stream[begin..]);
    }
    units
}

/// Converts a decoded NV12 frame to planar 4:2:0.
fn to_planar(nv12: &[u8], luma_len: usize) -> Vec<u8> {
    let (luma, chroma) = nv12.split_at(luma_len);
    let mut planar = Vec::with_capacity(nv12.len());
    planar.extend_from_slice(luma);
    planar.extend(chroma.chunks_exact(2).map(|pair| pair[0]));
    planar.extend(chroma.chunks_exact(2).map(|pair| pair[1]));
    planar
}

fn conformance(vector: &Vector) -> Result<(), String> {
    let archive = archive(vector);
    let stream = extract(&archive, vector.stream);
    let reference = extract(&archive, vector.reference);
    let luma_len = vector.width * vector.height;
    let frame_len = luma_len * 3 / 2;
    assert_eq!(
        reference.len() % frame_len,
        0,
        "{}: the reference is not whole {}x{} frames",
        vector.name,
        vector.width,
        vector.height
    );

    let mut decoder = Decoder::new(
        CodecType::H264,
        None,
        u32::try_from(vector.width).unwrap(),
        u32::try_from(vector.height).unwrap(),
    )
    .map_err(|error| format!("cannot open the decoder: {error}"))?;
    let mut decoded: Vec<Vec<u8>> = Vec::new();
    let mut collect = |frames: DecodeStream| -> Result<(), String> {
        for frame in frames {
            let frame = frame.map_err(|error| format!("decode failed: {error}"))?;
            let size = (frame.width() as usize, frame.height() as usize);
            if size != (vector.width, vector.height) {
                return Err(format!(
                    "a frame is {}x{}, expected {}x{}",
                    size.0, size.1, vector.width, vector.height
                ));
            }
            let mut nv12 = vec![0; frame_len];
            let written = frame.copy_to_buffer(&mut nv12);
            if written != frame_len {
                return Err(format!(
                    "a frame holds {written} bytes, expected {frame_len}"
                ));
            }
            decoded.push(to_planar(&nv12, luma_len));
        }
        Ok(())
    };
    for (index, unit) in access_units(&stream).into_iter().enumerate() {
        let time = Duration::from_millis(index as u64 * 40);
        collect(decoder.decode(DecodePacket::new(unit, time)))?;
    }
    collect(decoder.drain())?;

    let expected = reference.len() / frame_len;
    if decoded.len() != expected {
        return Err(format!(
            "decoded {} frames, expected {expected}",
            decoded.len()
        ));
    }
    for (index, (got, want)) in decoded
        .iter()
        .zip(reference.chunks_exact(frame_len))
        .enumerate()
    {
        if let Some(offset) = got.iter().zip(want).position(|(a, b)| a != b) {
            return Err(format!("frame {index} differs, first at byte {offset}"));
        }
    }
    Ok(())
}

#[test]
#[ignore = "needs a VA-API render node, network access, curl, unzip and sha256sum"]
fn progressive_vectors_decode_bit_exactly() {
    let failures: Vec<String> = VECTORS
        .iter()
        .filter_map(|vector| {
            conformance(vector)
                .err()
                .map(|reason| format!("{}: {reason}", vector.name))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
