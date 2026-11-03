//! `mediax transcode`: the video re-encoded, the audio copied unchanged.

mod support;

use std::path::Path;

use mediax_codec::{CodecType, Decoder};
use mediax_container::EncodedSample;
use serde_json::Value;
use support::{
    AUDIO_ID, Container, FRAMES, HEIGHT, VIDEO_ID, Video, WIDTH, assert_same_samples, encode,
    fixture_path, mediax, path_arg, read_samples, same_time, scratch, write_fixture,
};

fn probe(path: &Path) -> Value {
    let output = mediax(&["probe".as_ref(), path_arg(path), "--json".as_ref()]);
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("probe prints JSON")
}

/// Runs `mediax transcode` and returns its output.
fn transcode(input: &Path, output: &Path, codec: Video, extra: &[&str]) -> std::process::Output {
    // `-v` makes the encoder log debug lines, including each packet's times.
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "-v".as_ref(),
        "transcode".as_ref(),
        path_arg(input),
        path_arg(output),
        "--codec".as_ref(),
        codec.name().as_ref(),
    ];
    args.extend(extra.iter().map(std::ffi::OsStr::new));
    let result = mediax(&args);
    // The harness shows this when the test fails.
    eprintln!(
        "mediax transcode log:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

/// Every source frame's presentation time and every output sample's
/// presentation and decode time, in ticks, for a failure message.
fn timeline(video_in: &[EncodedSample], video_out: &[EncodedSample]) -> String {
    let ticks =
        |time: mediax_container::MediaTime| format!("{}/{}", time.ticks(), time.timescale());
    let source: Vec<_> = video_in
        .iter()
        .map(|sample| ticks(sample.presentation_time()))
        .collect();
    let output: Vec<_> = video_out
        .iter()
        .map(|sample| {
            format!(
                "pts {} dts {}",
                ticks(sample.presentation_time()),
                ticks(sample.decode_time())
            )
        })
        .collect();
    format!(
        "source frames: {}\noutput samples in decode order:\n{}",
        source.join(", "),
        output.join("\n")
    )
}

/// Asserts the transcoded `output` against the source's samples.
fn assert_transcoded(source: &Path, output: &Path, codec: Video, label: &str) {
    let report = probe(output);
    let video = &report["tracks"][0];
    assert_eq!(video["id"], VIDEO_ID, "{label}");
    assert_eq!(video["codec"], format!("{:?}", codec.codec()), "{label}");
    assert_eq!(video["video"]["width"], WIDTH, "{label}");
    assert_eq!(video["video"]["height"], HEIGHT, "{label}");

    let source_samples = read_samples(source);
    let output_samples = read_samples(output);
    // The audio is copied byte for byte with its timing.
    assert_same_samples(
        &output_samples[&AUDIO_ID],
        &source_samples[&AUDIO_ID],
        &format!("{label}: audio"),
    );
    // Every video sample presents at the time of a source frame the encoder
    // kept, each frame at most once; a frame the encoder dropped leaves no
    // sample. The AV1 encoder never drops.
    let (video_out, video_in) = (&output_samples[&VIDEO_ID], &source_samples[&VIDEO_ID]);
    assert_eq!(video["sample_count"], video_out.len(), "{label}");
    if codec == Video::Av1 {
        assert_eq!(video_out.len(), FRAMES, "{label}: frame count");
    }
    assert!(!video_out.is_empty() && video_out.len() <= video_in.len());
    let mut kept = vec![false; video_in.len()];
    for (index, out) in video_out.iter().enumerate() {
        let source = video_in
            .iter()
            .position(|source| same_time(out.presentation_time(), source.presentation_time()))
            .unwrap_or_else(|| {
                panic!(
                    "{label}: sample {index} presents at {:?}, which is no source frame\n{}",
                    out.presentation_time(),
                    timeline(video_in, video_out)
                )
            });
        assert!(
            !std::mem::replace(&mut kept[source], true),
            "{label}: source frame {source} is presented twice\n{}",
            timeline(video_in, video_out)
        );
        // Decode time advances from sample to sample.
        if let Some(previous) = index.checked_sub(1).map(|previous| &video_out[previous]) {
            assert!(
                previous.decode_time().ticks() < out.decode_time().ticks(),
                "{label}: decode time does not advance at sample {index}\n{}",
                timeline(video_in, video_out)
            );
        }
    }
    assert!(video_out[0].is_keyframe(), "{label}: opens on a keyframe");
    let reordered = video_out
        .iter()
        .any(|sample| sample.decode_time().ticks() != sample.presentation_time().ticks());
    tracing::info!(
        label,
        kept = video_out.len(),
        dropped = video_in.len() - video_out.len(),
        reordered,
        "video timing checked"
    );
}

/// Transcodes an AV1 MP4 into `target`, or logs why the host cannot.
fn check_from_av1(target: Video, extension: &str) {
    let Some(source_video) = encode(Video::Av1) else {
        return;
    };
    if !target.available() {
        return;
    }
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    let output = dir.path().join(format!("out.{extension}"));
    let result = transcode(&source, &output, target, &[]);
    let label = format!("av1 to {} in .{extension}", target.name());
    assert!(
        result.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_transcoded(&source, &output, target, &label);
}

#[test]
fn av1_to_av1_in_mp4() {
    check_from_av1(Video::Av1, "mp4");
}

#[test]
fn av1_to_h264() {
    check_from_av1(Video::H264, "mp4");
}

#[test]
fn av1_to_h265() {
    check_from_av1(Video::H265, "mov");
}

/// Transcodes a hardware-codec source into AV1, where the host both encodes
/// the source codec (to make the fixture) and decodes it.
fn check_into_av1(source_codec: Video, decoder: CodecType) {
    let Some(source_video) = encode(source_codec) else {
        return;
    };
    // The decoder only needs to exist; the configuration is the fixture's.
    if let Err(error) = Decoder::new(decoder, None, WIDTH, HEIGHT)
        && matches!(error, mediax_codec::CodecError::Unsupported(_))
    {
        tracing::warn!(codec = source_codec.name(), %error, "skipping: the host cannot decode this codec");
        return;
    }
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    let output = dir.path().join("out.mp4");
    let result = transcode(&source, &output, Video::Av1, &[]);
    let label = format!("{} to av1", source_codec.name());
    assert!(
        result.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_transcoded(&source, &output, Video::Av1, &label);
}

#[test]
fn h264_to_av1() {
    check_into_av1(Video::H264, CodecType::H264);
}

#[test]
fn h265_to_av1() {
    check_into_av1(Video::H265, CodecType::H265);
}

/// A bitrate far below what the pattern needs makes a hardware encoder drop
/// frames, and the offline profile lets it reorder them; either way the
/// samples keep the times of the frames that were kept.
#[test]
fn a_starved_bitrate_keeps_the_times_of_the_frames_the_encoder_kept() {
    let Some(source_video) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    for target in [Video::H264, Video::H265] {
        if !target.available() {
            continue;
        }
        let output = dir.path().join(format!("starved-{}.mp4", target.name()));
        let result = transcode(&source, &output, target, &["--bitrate", "20k"]);
        let label = format!("av1 to {} at 20 kbps", target.name());
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_transcoded(&source, &output, target, &label);
    }
}

#[test]
fn bitrate_and_keyframe_interval_are_honoured() {
    let Some(source_video) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    let output = dir.path().join("out.mp4");
    let result = transcode(
        &source,
        &output,
        Video::Av1,
        &["--bitrate", "400k", "--keyframe-interval", "0.2"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let keyframes = read_samples(&output)[&VIDEO_ID]
        .iter()
        .filter(|sample| sample.is_keyframe())
        .count();
    // 30 frames at 30 fps with a keyframe every 0.2 s (6 frames).
    assert!(keyframes >= 5, "{keyframes} keyframes");
}

#[test]
fn an_unknown_output_extension_is_an_error_and_writes_nothing() {
    let Some(source_video) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    let output = dir.path().join("out.webm");
    let result = transcode(&source, &output, Video::Av1, &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("extension"));
    assert!(!output.exists());
}

#[test]
fn a_codec_the_host_cannot_encode_is_a_clear_error() {
    let Some(source_video) = encode(Video::Av1) else {
        return;
    };
    let dir = scratch();
    let source = fixture_path(dir.path(), "source", Container::Mp4);
    write_fixture(&source_video, Container::Mp4, &source);
    let mut exercised = false;
    for target in [Video::H264, Video::H265] {
        if target.available() {
            continue;
        }
        exercised = true;
        let output = dir.path().join(format!("{}.mp4", target.name()));
        let result = transcode(&source, &output, target, &[]);
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("cannot encode"),
            "{}: unexpected error: {stderr}",
            target.name()
        );
        assert!(!output.exists(), "no output file is created");
    }
    if !exercised {
        tracing::warn!("skipping: this host encodes every codec, so none is unavailable");
    }
}
