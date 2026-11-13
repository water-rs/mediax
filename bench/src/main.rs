//! Performance benchmark for mediax-codec.
//!
//! Tests encoding performance using the platform's hardware encoders, which are skipped
//! on a host without one, and the software AV1 encoder in its realtime profile.
//! Measures throughput on synthetic NV12 frames.

mod common;

use mediax_codec::{
    Bitrate, BitstreamFormat, Encoder, EncoderCodec, EncoderConfig, EncoderProfile, EvenDimension,
    FrameOptions, FrameRate, H264Level, H264Profile, H264Settings, H265Settings,
};
use rand::{RngExt, SeedableRng, rngs::SmallRng};
use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

/// The benchmarked stream rate, 30 fps.
const FRAME_RATE: FrameRate = FrameRate::new(NonZeroU32::new(30).unwrap(), NonZeroU32::MIN);

/// The presentation time of frame `index` at [`FRAME_RATE`].
fn frame_time(index: usize) -> Duration {
    Duration::from_secs(index as u64) * FRAME_RATE.denominator().get()
        / FRAME_RATE.numerator().get()
}

/// The benchmarked target bitrate, 20 Mbps.
const BITRATE: Bitrate = Bitrate::new(NonZeroU32::new(20_000_000).unwrap());

const H264: EncoderCodec = EncoderCodec::H264(H264Settings {
    profile: H264Profile::High,
    level: H264Level::L5_2,
    bitstream: BitstreamFormat::LengthPrefixed,
});
const H265: EncoderCodec = EncoderCodec::H265(H265Settings {
    bitstream: BitstreamFormat::LengthPrefixed,
});

fn create_test_nv12(width: u32, height: u32) -> Vec<u8> {
    // Create a dummy NV12 frame for testing
    // Y plane: width * height bytes
    // UV plane: width * height / 2 bytes (interleaved)
    let width = width as usize;
    let y_size = width * height as usize;
    let uv_size = y_size / 2;
    let mut data = vec![128u8; y_size + uv_size]; // Flat grey

    // Fill Y plane with a diagonal gradient that wraps every 256 steps.
    for (y, row) in data[..y_size].chunks_exact_mut(width).enumerate() {
        for (x, luma) in row.iter_mut().enumerate() {
            *luma = u8::try_from((x + y) % 256).expect("a value reduced modulo 256 fits in u8");
        }
    }

    data
}

/// Converts a byte count to megabits for a printed bitrate.
#[expect(
    clippy::cast_precision_loss,
    reason = "a benchmark's encoded byte total stays far below 2^52, where the conversion is exact"
)]
fn megabits(bytes: usize) -> f64 {
    bytes as f64 * 8.0 / 1_000_000.0
}

fn benchmark_encoder(
    name: &str,
    encoder: &mut Encoder,
    nv12_data: &[u8],
    iterations: u32,
) -> BenchResult {
    println!("\n=== Benchmarking {name} ===");

    // Warmup
    for index in 0..5 {
        for result in encoder.encode_nv12(nv12_data, frame_time(index), FrameOptions::default()) {
            let _ = result;
        }
    }

    // Timed run
    let start = Instant::now();
    let mut success_count = 0u32;
    let mut total_bytes = 0usize;

    for index in 5..5 + iterations as usize {
        for result in encoder.encode_nv12(nv12_data, frame_time(index), FrameOptions::default()) {
            match result {
                Ok(data) => {
                    success_count += 1;
                    total_bytes += data.data.len();
                }
                Err(e) => {
                    eprintln!("Encode error: {e:?}");
                }
            }
        }
    }
    // A reordering or lookahead encoder holds frames back until flushed.
    for packet in encoder.flush().expect("flush after the timed run") {
        success_count += 1;
        total_bytes += packet.data.len();
    }

    let elapsed = start.elapsed();
    let fps = f64::from(iterations) / elapsed.as_secs_f64();
    let frame_time_ms = elapsed.as_secs_f64() * 1000.0 / f64::from(iterations);

    println!("  Iterations: {iterations}");
    println!("  Successful: {success_count}");
    println!("  Total time: {elapsed:?}");
    println!("  FPS: {fps:.1}");
    println!("  Frame time: {frame_time_ms:.2} ms");
    if total_bytes > 0 {
        let mbps = megabits(total_bytes) / elapsed.as_secs_f64();
        println!("  Output bitrate: {mbps:.2} Mbps");
    }

    BenchResult {
        name: name.to_string(),
        fps,
        frame_time_ms,
        success_count,
        iterations,
    }
}

struct BenchResult {
    name: String,
    fps: f64,
    frame_time_ms: f64,
    success_count: u32,
    iterations: u32,
}

/// One encoder configuration to benchmark against one input frame.
struct BenchCase<'a> {
    name: &'a str,
    codec: EncoderCodec,
    width: EvenDimension,
    height: EvenDimension,
    profile: EncoderProfile,
    iterations: u32,
}

fn run_case(results: &mut Vec<BenchResult>, case: &BenchCase<'_>, nv12_data: &[u8]) {
    match Encoder::new(EncoderConfig {
        codec: case.codec,
        width: case.width,
        height: case.height,
        color: common::bt709_sdr_limited(),
        profile: case.profile,
        bitrate: BITRATE,
        frame_rate: FRAME_RATE,
        keyframe_interval: None,
    }) {
        Ok(mut encoder) => {
            results.push(benchmark_encoder(
                case.name,
                &mut encoder,
                nv12_data,
                case.iterations,
            ));
        }
        Err(e) => println!("  Skipped {}: no encoder on this host ({e})", case.name),
    }
}

/// A looping NV12 texture that scrolls diagonally, one step per frame.
///
/// Software encoders skip most of their work on flat or trivially predictable
/// input, so the texture is a gradient plus seeded noise that moves between
/// frames the way camera content does.
struct ScrollingNv12 {
    width: usize,
    height: usize,
    texture: Vec<u8>,
}

impl ScrollingNv12 {
    /// Seed of the texture noise, fixed so every run encodes the same frames.
    /// Noise amplitude as a right shift of a random byte: 7 leaves 0 or 1 level.
    const NOISE_SHIFT: u32 = 7;
    const SEED: u64 = 0x6d65_6469_6178;

    fn new(width: usize, height: usize) -> Self {
        let mut rng = SmallRng::seed_from_u64(Self::SEED);
        let y_size = width * height;
        let mut texture = vec![0u8; y_size + y_size / 2];
        let (luma, chroma) = texture.split_at_mut(y_size);
        for (y, row) in luma.chunks_exact_mut(width).enumerate() {
            rng.fill(row);
            for (x, sample) in row.iter_mut().enumerate() {
                let gradient = u8::try_from((x / 4 + y / 3) % 192)
                    .expect("a value reduced modulo 192 fits in u8");
                *sample = gradient + (*sample >> Self::NOISE_SHIFT);
            }
        }
        rng.fill(chroma);
        for sample in chroma {
            *sample = 112 + (*sample >> Self::NOISE_SHIFT);
        }
        Self {
            width,
            height,
            texture,
        }
    }

    /// Size in bytes of one frame.
    const fn frame_len(&self) -> usize {
        self.texture.len()
    }

    /// Writes frame `index` into `frame`: the texture shifted 2 px right and
    /// 1 px down per frame, wrapping at its edges.
    fn write_frame(&self, index: usize, frame: &mut [u8]) {
        let y_size = self.width * self.height;
        let (src_luma, src_chroma) = self.texture.split_at(y_size);
        let (dst_luma, dst_chroma) = frame.split_at_mut(y_size);
        // Luma shifts `dx` columns; an interleaved UV row shifts `dx` bytes,
        // which stays pair-aligned because `dx` is even.
        let dx = 2 * index % self.width;
        shift_rows(src_luma, dst_luma, self.width, index, dx);
        shift_rows(src_chroma, dst_chroma, self.width, index / 2, dx);
    }
}

/// Copies `src` rows to `dst`, each rotated left by `dx` bytes, starting at
/// row `dy` and wrapping vertically.
fn shift_rows(src: &[u8], dst: &mut [u8], row_len: usize, dy: usize, dx: usize) {
    let rows = src.len() / row_len;
    for (y, dst_row) in dst.chunks_exact_mut(row_len).enumerate() {
        let start = (y + dy) % rows * row_len;
        let src_row = &src[start..start + row_len];
        let (head, tail) = dst_row.split_at_mut(row_len - dx);
        head.copy_from_slice(&src_row[dx..]);
        tail.copy_from_slice(&src_row[..dx]);
    }
}

/// Nearest-rank percentile of an ascending-sorted, non-empty slice.
const fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    sorted[(sorted.len() * percent).div_ceil(100) - 1]
}

/// Realtime software AV1 at one resolution: per-frame encode time over a fixed
/// frame count, on moving textured input.
fn benchmark_av1(width: u32, height: u32, bitrate: Bitrate, measured_frames: usize) {
    const WARMUP_FRAMES: usize = 10;

    let name = format!("AV1 realtime ({width}x{height})");
    println!("\n--- Software AV1, realtime profile at {width}x{height} ---");
    let mut encoder = Encoder::new(EncoderConfig {
        codec: EncoderCodec::Av1,
        width: EvenDimension::new(width).unwrap(),
        height: EvenDimension::new(height).unwrap(),
        color: common::bt709_sdr_limited(),
        profile: EncoderProfile::Realtime,
        bitrate,
        frame_rate: FRAME_RATE,
        keyframe_interval: None,
    })
    .unwrap_or_else(|e| panic!("{name}: the AV1 software encoder must be available: {e}"));

    let source = ScrollingNv12::new(width as usize, height as usize);
    let mut frame = vec![0u8; source.frame_len()];
    let mut times = Vec::with_capacity(measured_frames);
    let mut total_bytes = 0usize;
    for index in 0..WARMUP_FRAMES + measured_frames {
        source.write_frame(index, &mut frame);
        let start = Instant::now();
        let packets = encoder.encode_nv12(&frame, frame_time(index), FrameOptions::default());
        let mut frame_bytes = 0usize;
        for packet in packets {
            frame_bytes += packet.expect("AV1 encode must succeed").data.len();
        }
        let elapsed = start.elapsed();
        if index >= WARMUP_FRAMES {
            times.push(elapsed);
            total_bytes += frame_bytes;
        }
    }
    for packet in encoder.flush().expect("AV1 flush must succeed") {
        total_bytes += packet.data.len();
    }

    times.sort_unstable();
    let median = percentile(&times, 50);
    let p90 = percentile(&times, 90);
    let max = times[times.len() - 1];
    println!("  Frames: {measured_frames} (after {WARMUP_FRAMES} warmup)");
    println!(
        "  Median: {:.2} ms  p90: {:.2} ms  max: {:.2} ms  (30 fps interval 33.33 ms)",
        median.as_secs_f64() * 1000.0,
        p90.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0,
    );
    println!(
        "  Output bitrate: {:.2} Mbps",
        megabits(total_bytes) / (f64::from(u32::try_from(measured_frames).unwrap()) / 30.0)
    );
}

fn print_summary(results: &[BenchResult]) {
    println!("\n=================================================");
    println!("                  SUMMARY");
    println!("=================================================");
    println!(
        "{:<20} {:>10} {:>12} {:>10}",
        "Encoder", "FPS", "Frame(ms)", "Success"
    );
    println!("-------------------------------------------------");
    for r in results {
        println!(
            "{:<20} {:>10.1} {:>12.2} {:>7}/{}",
            r.name, r.fps, r.frame_time_ms, r.success_count, r.iterations
        );
    }
    println!("=================================================");
}

fn main() {
    env_logger::init();

    println!("=================================================");
    println!("   Codec Performance Benchmark");
    println!("   Hardware encoders and software AV1");
    println!("=================================================");

    let mut results: Vec<BenchResult> = Vec::new();

    // Camera-like input (1080p, typical webcam)
    println!("\n>>> Camera Input (1080p)");
    let nv12_data = create_test_nv12(1920, 1080);
    for (label, name, codec) in [
        ("H.264", "H.264 VT (1080p)", H264),
        ("H.265", "H.265 VT (1080p)", H265),
    ] {
        println!("\n--- Hardware {label} (VideoToolbox) ---");
        let case = BenchCase {
            name,
            codec,
            width: EvenDimension::new(1920).unwrap(),
            height: EvenDimension::new(1080).unwrap(),
            profile: EncoderProfile::Offline,
            iterations: 100,
        };
        run_case(&mut results, &case, &nv12_data);
    }

    // 4K input, high pressure
    println!("\n>>> 4K Input (High Pressure)");
    let (width_4k, height_4k) = (3840, 2160);
    let nv12_data = create_test_nv12(width_4k, height_4k);
    for (label, name, codec) in [
        ("H.264", "H.264 VT (4K)", H264),
        ("H.265", "H.265 VT (4K)", H265),
    ] {
        println!("\n--- Hardware {label} (VideoToolbox) at 4K ---");
        let case = BenchCase {
            name,
            codec,
            width: EvenDimension::new(width_4k).unwrap(),
            height: EvenDimension::new(height_4k).unwrap(),
            profile: EncoderProfile::Realtime,
            iterations: 50,
        };
        run_case(&mut results, &case, &nv12_data);
    }

    // The first argument overrides the AV1 frame count, for a quick smoke run.
    let av1_frames = std::env::args().nth(1).map_or(120, |arg| {
        arg.parse().expect("the AV1 frame count is a number")
    });
    println!("\n>>> Software AV1 (realtime)");
    // Rates scale with the pixel count from 2.5 Mbps at 720p.
    for (width, height, bits_per_second) in [(640, 360, 625_000), (1280, 720, 2_500_000)] {
        benchmark_av1(
            width,
            height,
            Bitrate::new(NonZeroU32::new(bits_per_second).unwrap()),
            av1_frames,
        );
    }

    print_summary(&results);
}
