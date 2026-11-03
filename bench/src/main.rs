//! Performance benchmark for mediax-codec.
//!
//! Tests encoding performance using hardware accelerated (Apple `VideoToolbox`) encoders.
//! Measures throughput on synthetic NV12 frames.

mod common;

use mediax_codec::{
    Bitrate, BitstreamFormat, Encoder, EncoderCodec, EncoderConfig, EncoderProfile, EvenDimension,
    FrameOptions, FrameRate, H264Level, H264Profile, H264Settings, H265Settings,
};
use std::{num::NonZeroU32, time::Instant};

/// The benchmarked stream rate, 30 fps.
const FRAME_RATE: FrameRate = FrameRate::new(NonZeroU32::new(30).unwrap(), NonZeroU32::MIN);
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
    for _ in 0..5 {
        for result in encoder.encode_nv12(nv12_data, FrameOptions::default()) {
            let _ = result;
        }
    }

    // Timed run
    let start = Instant::now();
    let mut success_count = 0u32;
    let mut total_bytes = 0usize;

    for _ in 0..iterations {
        for result in encoder.encode_nv12(nv12_data, FrameOptions::default()) {
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
        Err(e) => println!("  Failed: {e:?}"),
    }
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
    println!("   Hardware Encoding (VideoToolbox)");
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

    print_summary(&results);
}
