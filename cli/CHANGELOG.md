# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/water-rs/mediax/releases/tag/cli/v0.1.0) - 2026-11-13

### Added

- *(codec)* decode H.265 Main and Main 10 through VA-API
- *(codec)* VA-API decoding of progressive H.264 on Linux
- *(codec)* [**breaking**] encoded packets carry their frame's presentation and decode time
- *(cli)* mediax command-line tool with probe, transcode and remux
- *(codec)* [**breaking**] realtime encoding for WebRTC
- *(codec)* [**breaking**] remove cros-codecs and the vaapi feature
- stand up mediax from waterkit's media crates

### Fixed

- *(container)* [**breaking**] apply MP4/MOV edit lists when reading, and write pre-roll through them
- *(codec)* map Media Foundation output samples back to their submitted frames
- *(codec)* gate signed_nanos for the VA-API test build and log Windows encoder timing

### Other

- Merge remote-tracking branch 'origin/dev' into feat/9-vaapi-h264-decoder
- Merge remote-tracking branch 'origin/dev' into feat/24-packet-timestamps
