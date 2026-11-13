# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/water-rs/mediax/compare/mediax-codec-v0.0.0...mediax-codec-v0.1.0) - 2026-11-13

### Added

- *(codec)* decode H.265 Main and Main 10 through VA-API
- *(codec)* VA-API decoding of progressive H.264 on Linux
- *(codec)* [**breaking**] encoded packets carry their frame's presentation and decode time
- *(codec)* VA-API realtime H.264 encoder on Linux
- *(codec)* decode AAC through AudioToolbox on Apple
- *(codec)* [**breaking**] realtime encoding for WebRTC
- *(codec)* [**breaking**] remove cros-codecs and the vaapi feature
- stand up mediax from waterkit's media crates
- *(codec)* count imported and uploaded frames on the uploader
- *(codec)* [**breaking**] import decoded planes as engine-ready integer textures
- [**breaking**] upgrade wgpu to 30.0.1
- *(codec)* decode AVIF/HEIF through BitmapFactory on Android
- record compressed and raw camera video on desktop
- *(codec)* put GPU texture output behind a `gpu` feature

### Fixed

- satisfy Rust 1.100 clippy lints
- *(codec)* keep the H.265 slice helper out of builds without tests or fuzzing
- *(codec)* drop H.265 items only tests use from Linux builds
- *(codec)* compile VA-API test and fuzz support only where it is used
- *(codec)* map Media Foundation output samples back to their submitted frames
- *(codec)* gate signed_nanos for the VA-API test build and log Windows encoder timing
- *(codec)* set the Windows real-time hint only where the encoder supports it
- *(codec)* gate the avcC builder to the hosts that call it
- *(codec)* gate the VA-API driver-double tests to unix hosts
- *(codec)* split the Media Foundation encoder setup and gate test-only helpers per target
- *(video-core)* decode 10-bit samples from the high bits of their element
- *(camera)* tag desktop recordings with source color
- clear the clippy lints of every wasm32 build
- *(audio)* select the pure-Rust AAC decoder on wasm32 under he-aac
- *(codec)* compile the parameter-set record builders only where an encoder uses them
- *(audio)* keep the Media Foundation AAC decoder continuous across packets
- *(codec)* report the Media Foundation encoder's codec configuration
- *(camera)* address review of the zero-copy frame path
- clear the lints and test-build errors that workspace clippy exposes
- *(codec)* expect the unused upload queue on hardware-only builds
- *(codec)* hoist the shared tail out of copy_to_buffer's match
- *(codec)* preserve feature shapes while removing lint violations
- *(codec)* expect missing_const_for_fn on flush in hardware-only builds
- *(codec)* keep the Apple open_* helpers above the test module
- *(codec)* gate the Linux VA-API stack behind a vaapi feature
- *(codec)* widen half-precision pixels without Float16 ([#52](https://github.com/water-rs/mediax/pull/52))
- clear the second round of never-run lints, and let secret build on iOS
- clear the lints CI could never reach
- *(codec)* read rav1d's EAGAIN as an errno, not as a host error code
- *(codec)* give the Windows decoder its parameter sets and its exact PTS
- decode all AVIF chroma layouts
- make HDR decoding and capture profiles truthful
- *(android)* unbreak the Android workspace build and clear its lint debt

### Other

- *(codec)* satisfy clippy in the Linux conformance test
- Merge remote-tracking branch 'origin/dev' into feat/9-vaapi-h264-decoder
- Merge pull request #20 from water-rs/perf/14-av1-720p
- Merge pull request #22 from water-rs/fix/jpeg-test-no-decoder
- *(codec)* report thread-pool sizing failures like the other init errors
- *(codec)* skip the JPEG decode test where the host has no decoder
- *(codec)* four AV1 tiles per thread in realtime mode
- *(codec)* build rav1e's SIMD kernels and tile realtime AV1 frames
- Merge pull request #13 from water-rs/feat/4-audiotoolbox-aac
- release v0.2.0
- *(deps)* update every dependency to its latest release
- *(codec)* [**breaking**] call ImageIO and Core Image through objc2 instead of Swift
- *(build)* [**breaking**] one Swift bridge builder for every crate
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- Merge remote-tracking branch 'origin/dev' into feat/175-frame-colour-description
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- *(video-core)* map matrix and range to the shared YCbCr modes once
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into ci/173-workspace-tests
- Merge pull request #185 from water-rs/fix/181-windows-encoder-config
- Merge remote-tracking branch 'origin/dev' into fix/180-windows-aac-output-type
- Merge remote-tracking branch 'origin/dev' into fix/181-windows-encoder-config
- negotiate the Windows AAC decoder's offered float PCM output type
- *(audio)* read the HE-AAC fixture line by line
- *(codec,video-core)* share the YCbCr range and matrix WGSL
- Merge branch 'dev' into feat/efv-frames
- *(codec)* [**breaking**] split AV1 software decode and encode; gate the Swift bridge toolchain on Apple hosts; package the YUV shader
- release v0.1.2 ([#55](https://github.com/water-rs/mediax/pull/55))
- *(codec)* keep the keyword list within the crates.io limit
- *(deps)* scope `image` to the codecs waterkit actually decodes
- ship the licence texts in every published crate
- Fix WaterKit Web target support
- Gate codec config helpers to platforms that infer layout
- Scalarize YUV chroma coordinates
- Compile HDR tone mapping on DXC
- Fix WaterKit CI failures
- Fix Apple decoded frame texture upload
- Eliminate transient media and sensor allocations
- upgrade WaterKit dependencies
- Upgrade wgpu and adopt shader AOT
- build modular cross-platform media stack
- Preserve HDR decode frames on GPU
- reorganize the project

## [0.2.0](https://github.com/water-rs/waterkit/compare/waterkit-codec-v0.1.4...waterkit-codec-v0.2.0) - 2026-10-16

### Added

- *(codec)* count imported and uploaded frames on the uploader
- *(codec)* [**breaking**] import decoded planes as engine-ready integer textures
- [**breaking**] upgrade wgpu to 30.0.1
- *(codec)* decode AVIF/HEIF through BitmapFactory on Android
- record compressed and raw camera video on desktop

### Fixed

- *(camera)* tag desktop recordings with source color
- *(video-core)* decode 10-bit samples from the high bits of their element
- clear the clippy lints of every wasm32 build
- *(codec)* compile the parameter-set record builders only where an encoder uses them
- *(codec)* report the Media Foundation encoder's codec configuration
- clear the lints and test-build errors that workspace clippy exposes
- *(camera)* address review of the zero-copy frame path
- *(codec)* expect the unused upload queue on hardware-only builds
- *(codec)* preserve feature shapes while removing lint violations
- *(codec)* hoist the shared tail out of copy_to_buffer's match
- *(codec)* expect missing_const_for_fn on flush in hardware-only builds
- *(codec)* keep the Apple open_* helpers above the test module
- *(codec)* gate the Linux VA-API stack behind a vaapi feature

### Other

- *(deps)* update every dependency to its latest release
- *(codec)* [**breaking**] call ImageIO and Core Image through objc2 instead of Swift
- *(build)* [**breaking**] one Swift bridge builder for every crate
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- Merge remote-tracking branch 'origin/dev' into feat/175-frame-colour-description
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- *(video-core)* map matrix and range to the shared YCbCr modes once
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into fix/181-windows-encoder-config
- *(codec,video-core)* share the YCbCr range and matrix WGSL
- Merge branch 'dev' into feat/efv-frames
- *(codec)* [**breaking**] split AV1 software decode and encode; gate the Swift bridge toolchain on Apple hosts; package the YUV shader

## [0.1.2](https://github.com/water-rs/waterkit/compare/waterkit-codec-v0.1.1...waterkit-codec-v0.1.2) - 2026-09-11

### Added

- *(codec)* put GPU texture output behind a `gpu` feature

### Fixed

- *(codec)* widen half-precision pixels without Float16 ([#52](https://github.com/water-rs/waterkit/pull/52))

## [0.1.0](https://github.com/water-rs/waterkit/releases/tag/waterkit-codec-v0.1.0) - 2026-02-07

### Added

- *(codec)* GPU-first zero-copy video codec with streaming API
- Make platform modules private, adopt `thiserror`, and simplify camera API by removing frame structs.
- *(mobile)* fix audio builds on android, add ios harness, update gitignore
- Introduce top-level `waterkit` crate with modular features and refactor `codec` module's Apple platform implementation.
- Implement zero-copy video decoding and rendering on macOS using IOSurface and Metal interop
- Implement video codec support with AV1 encoding/decoding; add platform-specific implementations for Apple, Android, and Windows

### Fixed

- *(codec)* Fix remaining 2 clippy lints (raw pointer, const fn)
- *(codec)* Fix all Windows clippy lints
- *(codec)* Fix GetOutputStreamInfo API for windows-rs 0.62
- Fix Windows clippy lints and codec API for windows-rs 0.62
- *(codec)* Add backticks around FFmpeg in doc comments
- *(codec)* Add clippy allow attributes and fix doc comments for Linux
- *(codec)* Fix Linux FFmpeg context consumption errors
- add clippy allows and missing metadata for waterkit-build
- Correct HEVC codec config extraction and add NV12 to BGRA conversion for Apple video decoding.

### Other

- Fix formatting and remove unused dependencies
- Refactor Android module visibility and improve documentation
- Improve code consistency and clarity across multiple modules
- Add Linux, Android and Windows backends
- use proper README
- Standardize workspace Cargo.toml fields, refine crate descriptions, and introduce `thiserror` for improved error handling.
- use workspace dependencies throughout and remove unused deps
- Run formatter and add READMEs
- Update dependency and fix lints
- Implement manual H.265 playback pipeline (wip)
