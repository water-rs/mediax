# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/water-rs/mediax/compare/mediax-v0.0.0...mediax-v0.1.0) - 2026-11-14

### Added

- *(codec)* decode H.265 Main and Main 10 through VA-API
- *(codec)* VA-API decoding of progressive H.264 on Linux
- *(cli)* mediax command-line tool with probe, transcode and remux
- *(container)* [**breaking**] open any supported container from a path with parsed codec parameters
- *(codec)* VA-API realtime H.264 encoder on Linux
- *(container)* [**breaking**] multi-track MP4/MOV muxer with sample timestamps
- *(codec)* decode AAC through AudioToolbox on Apple
- *(codec)* [**breaking**] realtime encoding for WebRTC
- *(codec)* [**breaking**] remove cros-codecs and the vaapi feature
- stand up mediax from waterkit's media crates
- *(codec)* count imported and uploaded frames on the uploader
- *(codec)* [**breaking**] import decoded planes as engine-ready integer textures
- [**breaking**] upgrade wgpu to 30.0.1
- *(codec)* decode AVIF/HEIF through BitmapFactory on Android
- *(video)* port the filtrate effect processor to water-rs/cherenkov
- record compressed and raw camera video on desktop
- *(codec)* put GPU texture output behind a `gpu` feature
- *(video)* read the track's codec from the container, and demux AV1
- Enhance screen capture performance with ScreenCaptureKit and optimized ScreenCapturer; add profiling test for benchmarking
- Add optimized screen recording test with async capture; implement H.265 encoding and performance monitoring
- Implement video codec support with AV1 encoding/decoding; add platform-specific implementations for Apple, Android, and Windows

### Fixed

- *(codec)* gate the VA-API driver-double tests to unix hosts
- *(video-core)* decode 10-bit samples from the high bits of their element
- *(camera)* [**breaking**] record YCbCr matrix and range in WKRV raw recordings
- *(codec)* make the macOS codec harness's BT.709 description const
- *(camera)* tag desktop recordings with source color
- clear the clippy lints of every wasm32 build
- *(audio)* select the pure-Rust AAC decoder on wasm32 under he-aac
- *(codec)* compile the parameter-set record builders only where an encoder uses them
- *(audio)* keep the Media Foundation AAC decoder continuous across packets
- *(codec)* report the Media Foundation encoder's codec configuration
- *(camera)* address review of the zero-copy frame path
- *(video-streaming)* hand MoveFileExW verbatim paths on Windows
- *(tests)* clear the workspace lints in the macOS harnesses
- clear the lints and test-build errors that workspace clippy exposes
- *(codec)* expect the unused upload queue on hardware-only builds
- *(codec)* hoist the shared tail out of copy_to_buffer's match
- *(codec)* preserve feature shapes while removing lint violations
- *(video)* keep keyframe seek safe on empty tracks
- *(video)* chain fragment bases per moof and decode by PTS
- *(video)* honor fragment timing when reading fragmented MP4
- *(container)* preserve 64-bit recording sizes and offsets ([#117](https://github.com/water-rs/mediax/pull/117))
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

- release from main and retire dev
- drop the typos check
- Merge pull request #25 from water-rs/feat/3-cli
- *(bench)* realtime AV1 case and a manual Linux bench workflow
- Merge pull request #19 from water-rs/feat/8-vaapi-h264-encoder
- *(codec)* build rav1e's SIMD kernels and tile realtime AV1 frames
- Merge pull request #13 from water-rs/feat/4-audiotoolbox-aac
- note why Linux jobs stay on ubuntu-24.04
- hold Linux jobs on ubuntu-24.04 until the VA-API stack builds against libva 2.23 ([#6](https://github.com/water-rs/mediax/pull/6))
- *(release)* keep waterkit-video-processing at its published 0.1.4
- release v0.2.0
- *(deps)* update every dependency to its latest release
- *(screen)* [**breaking**] await completions and return typed errors
- *(codec)* [**breaking**] call ImageIO and Core Image through objc2 instead of Swift
- *(build)* [**breaking**] one Swift bridge builder for every crate
- Merge branch 'fix/169-recording-colour-tags' into fix/174-raw-recording-matrix
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- Merge remote-tracking branch 'origin/dev' into feat/175-frame-colour-description
- Merge branch 'fix/169-recording-colour-tags' into fix/174-raw-recording-matrix
- Merge branch 'feat/175-frame-colour-description' into fix/169-recording-colour-tags
- *(video-core)* map matrix and range to the shared YCbCr modes once
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into fix/136-camera-zero-copy
- Merge remote-tracking branch 'origin/dev' into ci/173-workspace-tests
- Merge pull request #185 from water-rs/fix/181-windows-encoder-config
- Merge pull request #186 from water-rs/fix/180-windows-aac-output-type
- Merge remote-tracking branch 'origin/dev' into fix/180-windows-aac-output-type
- Merge remote-tracking branch 'origin/dev' into fix/181-windows-encoder-config
- negotiate the Windows AAC decoder's offered float PCM output type
- *(audio)* read the HE-AAC fixture line by line
- satisfy clippy 1.99's assert_is_empty lint
- *(lints)* opt every workspace member into the workspace lints
- *(codec,video-core)* share the YCbCr range and matrix WGSL
- Merge branch 'dev' into feat/efv-frames
- Merge pull request #119 from water-rs/fix/112-fragmented-file-timing
- *(container)* name fragment run fixture parameters clearly
- *(codec)* [**breaking**] split AV1 software decode and encode; gate the Swift bridge toolchain on Apple hosts; package the YUV shader
- release v0.1.2 ([#55](https://github.com/water-rs/mediax/pull/55))
- *(deps)* zenwave 0.6.1
- *(codec)* keep the keyword list within the crates.io limit
- *(deps)* scope `image` to the codecs waterkit actually decodes
- ship the licence texts in every published crate
- Fix WaterKit Web target support
- *(processing)* adopt filtrate's shared shader cache and typed errors
- Gate codec config helpers to platforms that infer layout
- Scalarize YUV chroma coordinates
- Compile HDR tone mapping on DXC
- Unify Windows Zenwave TLS backend
- Fix WaterKit CI failures
- Fix Apple decoded frame texture upload
- Eliminate transient media and sensor allocations
- upgrade WaterKit dependencies
- Upgrade wgpu and adopt shader AOT
- Reuse Zenwave bounded response collection
- build modular cross-platform media stack
- Preserve HDR decode frames on GPU
- Fix WaterKit API drift and invariants
- reorganize the project
- Fix formatting and remove unused dependencies
- Modernize macOS and Android tests to GPU streaming
- update tests for new clipboard, codec, and dialog APIs
- use workspace dependencies throughout and remove unused deps
- Run formatter and add READMEs
- Implement manual H.265 playback pipeline (wip)
