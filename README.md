# mediax

Hardware-accelerated, permissively licensed codecs, containers and streaming for Rust.

Video encode and decode go through the platform's own hardware stack: VideoToolbox on Apple platforms, Media Foundation on Windows, and MediaCodec on Android. Linux has no hardware path yet (the VA-API backend is being rewritten, #6). The only software codec is AV1 (rav1e and rav1d). There is deliberately no software H.264 or H.265, so nothing here carries a patent-encumbered or copyleft codec library. Apple targets need macOS 12 or iOS 15.

All crates are licensed `MIT OR Apache-2.0`.

## Crates

| Crate | Description |
| :--- | :--- |
| [`mediax`](.) | Facade re-exporting the crates below behind features. |
| [`mediax-core`](core) | Engine-neutral timing, color, stream and error types. |
| [`mediax-codec`](codec) | Hardware video encode/decode with `wgpu` texture output, AV1 in software, AAC decoding. |
| [`mediax-container`](container) | MP4/MOV, CMAF and subtitle demuxing and muxing. |
| [`mediax-streaming`](streaming) | HLS and DASH delivery and cache primitives over Zenwave. |
| [`mediax-processing`](processing) | Typed frame-processing composition. |

## Features

The facade enables only what you ask for: `codec`, `container`, `streaming`, `offline`, `processing`, `filtrate`, plus the codec options `gpu`, `software-decode`, `software-encode` and `aac`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
