# mediax-codec

Low-level hardware-accelerated media encoding and decoding.

## Overview

This crate provides a unified interface for accessing system media codecs. It backs the rest of the `mediax` crates and can be used on its own for custom media processing pipelines.

## Features

- **Hardware Acceleration**: Uses specific hardware APIs where available.
- **Formats**: H.264, H.265 (HEVC), AAC.
- **Zero-Copy**: Optimized for efficient frame passing to `wgpu` textures.

## Installation

```toml
[dependencies]
mediax-codec = "0.1"
```

### Cargo features

| Feature | Default | Gates |
| :--- | :--- | :--- |
| `gpu` | yes | `wgpu` texture upload and the YUV-to-linear-RGBA compute conversion (`GpuFrame`, `DecodedFrameUploader`, `LinearRgbaConverter`, `DecodedFrame::to_gpu_frame`), plus the `wgpu`, `wgpu-hal` and `shaderloom` dependencies. |
| `software-decode` | via `software-encode` | AV1/AVIF decode in software on desktop (`rav1d`, `avif-parse`, `yuv`, `moxcms`). |
| `software-encode` | yes | AV1 encode in software on desktop (`rav1e`); implies `software-decode`. |
| `vaapi` | no | VA-API hardware decode/encode on Linux (`cros-codecs`, system libva >= 2.20). |
| `aac` | no | Packet-oriented AAC decoding (`audio`): MediaCodec on Android, Media Foundation on Windows, a pure-Rust AAC-LC decoder elsewhere. HE-AAC is not supported. |

A consumer that only wants decoded pixels can take
`default-features = false, features = ["software-decode"]` and link no `wgpu`
at all; decoded planes come out through `DecodedFrame::copy_to_buffer` and
`Decoder::decode_into`.

## Platform Support

| Platform | Technology |
| :--- | :--- |
| **macOS/iOS** | VideoToolbox |
| **Android** | MediaCodec (NDK/JNI) |
| **Windows** | Media Foundation |
| **Linux** | VA-API / rav1d |

## Usage

```rust
// Example: Concept of creating a decoder
use mediax_codec::{VideoDecoder, CodecType};

let decoder = VideoDecoder::new(CodecType::H264).unwrap();
// decoder.decode(packet)...
```
