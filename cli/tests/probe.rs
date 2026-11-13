//! `mediax probe` on every container, against media generated at test time.

mod support;

use std::path::Path;

use serde_json::Value;
use support::{
    AUDIO_ID, Container, FRAMES, HEIGHT, VIDEO_ID, Video, WIDTH, encode, fixture_path, mediax,
    path_arg, scratch, write_fixture,
};

fn track(report: &Value, id: u32) -> &Value {
    report["tracks"]
        .as_array()
        .expect("tracks array")
        .iter()
        .find(|track| track["id"] == id)
        .unwrap_or_else(|| panic!("track {id} in {report}"))
}

fn probe_json(path: &Path) -> Value {
    let output = mediax(&["probe".as_ref(), path_arg(path), "--json".as_ref()]);
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("probe prints JSON")
}

fn check(video: Video, container: Container) {
    let Some(encoded) = encode(video) else {
        return;
    };
    let dir = scratch();
    let path = fixture_path(dir.path(), "probe", container);
    write_fixture(&encoded, container, &path);
    let label = format!("{} in {}", video.name(), container.label());

    let report = probe_json(&path);
    assert_eq!(report["format"], container.probe_name(), "{label}");
    assert_eq!(
        report["tracks"].as_array().expect("tracks").len(),
        2,
        "{label}"
    );

    let video_track = track(&report, VIDEO_ID);
    assert_eq!(video_track["kind"], "video", "{label}");
    assert_eq!(
        video_track["codec"],
        format!("{:?}", video.codec()),
        "{label}"
    );
    let parameters = &video_track["video"];
    assert_eq!(parameters["width"], WIDTH, "{label}");
    assert_eq!(parameters["height"], HEIGHT, "{label}");
    assert_eq!(parameters["bit_depth"], 8, "{label}");
    assert_eq!(parameters["chroma"], "4:2:0", "{label}");
    assert!(parameters["profile"].is_string(), "{label}: profile");
    // Only the progressive writer signals colour; `transmux` repackages
    // without it, leaving the default for the frame height.
    let primaries = if matches!(container, Container::Mp4 | Container::Mov) {
        "Bt709"
    } else {
        "Bt601"
    };
    assert_eq!(parameters["color"]["primaries"], primaries, "{label}");
    // A transport stream declares a frame rate only through in-band timing.
    if container != Container::TransportStream {
        assert_eq!(parameters["frame_rate"]["numerator"], 30, "{label}");
        assert_eq!(parameters["frame_rate"]["denominator"], 1, "{label}");
        assert_eq!(parameters["frame_rate"]["fps"], "30", "{label}");
    }
    assert_eq!(video_track["sample_count"], FRAMES, "{label}");
    let seconds = video_track["duration_seconds"].as_f64().expect("duration");
    let expected = f64::from(u32::try_from(FRAMES).expect("frame count")) / 30.0;
    assert!(
        (seconds - expected).abs() < 0.05,
        "{label}: video lasts {seconds} s, expected about {expected}"
    );

    let audio_track = track(&report, AUDIO_ID);
    assert_eq!(audio_track["kind"], "audio", "{label}");
    assert_eq!(audio_track["codec"], "Aac", "{label}");
    assert_eq!(audio_track["audio"]["sample_rate"], 44_100, "{label}");
    assert_eq!(audio_track["audio"]["channels"], 2, "{label}");

    // The text form names the same facts.
    let output = mediax(&["probe".as_ref(), path_arg(&path)]);
    assert!(output.status.success(), "{label}: text probe failed");
    let text = String::from_utf8(output.stdout).expect("text output");
    for needle in [
        container.probe_name(),
        &format!("{WIDTH}x{HEIGHT}"),
        "44100 Hz",
        primaries,
    ] {
        assert!(
            text.contains(needle),
            "{label}: {needle:?} missing from\n{text}"
        );
    }
}

macro_rules! probe_tests {
    ($($name:ident: $video:expr, $container:expr;)*) => {$(
        #[test]
        fn $name() {
            check($video, $container);
        }
    )*};
}

// MPEG-TS carries no AV1 here: `transmux` does not package it and the stream
// reader lists only the audio track, so there is no AV1 transport stream to probe.
probe_tests! {
    av1_in_mp4: Video::Av1, Container::Mp4;
    av1_in_mov: Video::Av1, Container::Mov;
    av1_in_fragmented_mp4: Video::Av1, Container::FragmentedMp4;
    h264_in_mp4: Video::H264, Container::Mp4;
    h264_in_mov: Video::H264, Container::Mov;
    h264_in_fragmented_mp4: Video::H264, Container::FragmentedMp4;
    h264_in_transport_stream: Video::H264, Container::TransportStream;
    h265_in_mp4: Video::H265, Container::Mp4;
    h265_in_mov: Video::H265, Container::Mov;
    h265_in_fragmented_mp4: Video::H265, Container::FragmentedMp4;
    h265_in_transport_stream: Video::H265, Container::TransportStream;
}

#[test]
fn a_missing_file_fails() {
    let dir = scratch();
    let output = mediax(&["probe".as_ref(), path_arg(&dir.path().join("absent.mp4"))]);
    assert!(!output.status.success());
}
