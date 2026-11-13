//! `mediax remux`: every track copied sample for sample, timestamps kept,
//! checked by reading the output back.

mod support;

use std::path::Path;

use support::{
    Container, Video, assert_same_samples, encode, fixture_path, mediax, path_arg, read_samples,
    scratch, write_fixture,
};

/// Remuxes `from` into `to` and asserts the copy carries the same samples.
fn remux_and_compare(from: &Path, to: &Path, label: &str) {
    let output = mediax(&["remux".as_ref(), path_arg(from), path_arg(to)]);
    assert!(
        output.status.success(),
        "{label}: remux failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = read_samples(from);
    let actual = read_samples(to);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "{label}: track ids"
    );
    for (id, expected) in &expected {
        assert_same_samples(&actual[id], expected, &format!("{label}, track {id}"));
    }
}

fn check(video: Video) {
    let Some(encoded) = encode(video) else {
        return;
    };
    let dir = scratch();
    let name = video.name();

    // MP4 to MOV and back.
    let mp4 = fixture_path(dir.path(), name, Container::Mp4);
    write_fixture(&encoded, Container::Mp4, &mp4);
    let mov = dir.path().join("copy.mov");
    remux_and_compare(&mp4, &mov, &format!("{name}: MP4 to MOV"));
    let back = dir.path().join("copy.mp4");
    remux_and_compare(&mov, &back, &format!("{name}: MOV to MP4"));
    // The round trip also matches the original.
    assert_eq!(read_samples(&back).len(), 2, "{name}: both tracks survive");

    // Fragmented MP4 to MOV.
    let fragmented = dir.path().join("fragmented.mp4");
    write_fixture(&encoded, Container::FragmentedMp4, &fragmented);
    remux_and_compare(
        &fragmented,
        &dir.path().join("from-fragmented.mov"),
        &format!("{name}: fragmented MP4 to MOV"),
    );

    // MPEG-TS to MP4. AV1 has no transport stream fixture.
    if video != Video::Av1 {
        let ts = fixture_path(dir.path(), name, Container::TransportStream);
        write_fixture(&encoded, Container::TransportStream, &ts);
        remux_and_compare(
            &ts,
            &dir.path().join("from-ts.mp4"),
            &format!("{name}: MPEG-TS to MP4"),
        );
    }
}

#[test]
fn av1_remuxes_between_containers() {
    check(Video::Av1);
}

#[test]
fn h264_remuxes_between_containers() {
    check(Video::H264);
}

#[test]
fn h265_remuxes_between_containers() {
    check(Video::H265);
}

#[test]
fn colour_signalling_survives_a_remux() {
    let Some(encoded) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let mp4 = fixture_path(dir.path(), "colour", Container::Mp4);
    write_fixture(&encoded, Container::Mp4, &mp4);
    let mov = dir.path().join("colour.mov");
    assert!(
        mediax(&["remux".as_ref(), path_arg(&mp4), path_arg(&mov)])
            .status
            .success()
    );
    let output = mediax(&["probe".as_ref(), path_arg(&mov), "--json".as_ref()]);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    // The fixture signals BT.709; a 240-line default would be BT.601.
    assert_eq!(report["tracks"][0]["video"]["color"]["primaries"], "Bt709");
}

#[test]
fn an_unknown_output_extension_is_an_error_and_writes_nothing() {
    let Some(encoded) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let mp4 = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&encoded, Container::Mp4, &mp4);
    let output_path = dir.path().join("copy.mkv");
    let output = mediax(&["remux".as_ref(), path_arg(&mp4), path_arg(&output_path)]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("extension"), "unexpected error: {stderr}");
    assert!(!output_path.exists(), "no output file is created");
}
