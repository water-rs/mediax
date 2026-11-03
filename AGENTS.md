# AGENTS.md

Guidance for coding agents working in this repository.

## Build commands

```bash
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
cargo test --workspace --all-features
cargo hack check -p mediax-codec --feature-powerset --no-dev-deps
cargo machete
```

## Architecture

**mediax** is hardware-accelerated (Linux has VA-API H.264 encoding and progressive H.264 and H.265 decoding; further decoders are still to come, #6), permissively licensed media processing for Rust: iOS, Android, macOS, Windows and Linux (plus wasm32 where a crate supports it).

- Root crate `mediax`: facade re-exporting the member crates behind features.
- `core` (`mediax-core`): engine-neutral timing, color, stream and error types.
- `codec` (`mediax-codec`): hardware video encode/decode with `wgpu` texture output, software AV1 (rav1e/rav1d), the `aac` module for AAC decoding.
- `container` (`mediax-container`): demuxing and muxing.
- `streaming` (`mediax-streaming`): HLS/DASH delivery and cache primitives over Zenwave.
- `processing` (`mediax-processing`): typed frame-processing composition.
- `cli` (`mediax-cli`): the `mediax` binary (`probe`, `transcode`, `remux`) over `mediax-container` and `mediax-codec`, shipped as prebuilt binaries by cargo-dist (`dist.yml`, triggered by the `cli/v<version>` tag).
- `bench` (`mediax-bench`): host-only encoder benchmark, not published.

Platform access goes through `objc2`, `windows` and `jni`/`ndk` directly.

## Codec policy

- No software H.264 or H.265, by design.
- No FDK AAC: its bundled Fraunhofer source grants no patent licence. AAC decodes through AudioToolbox, MediaCodec or Media Foundation (HE-AAC included); only targets without a system decoder use the pure-Rust symphonia decoder, which is AAC-LC only.
- Every dependency must be permissively licensed.

## Coding guidelines

<important>
- Follow the fail-fast principle: if an unexpected case is encountered, crash early with a clear error message rather than fall back.
- Use the type system to enforce invariants at compile time rather than runtime checks.
- Use struct, trait and generic abstractions rather than enums and type erasure when possible.
- Put shaders in separate files rather than string literals; the same for large text assets.
- Do not write duplicated code.
- Use the GPU rather than the CPU for pixel work.
- Do not revert or restore files or hide problems; fix bugs properly.
- Leave no legacy code for fallback. If a feature is deprecated, remove all related code. No backward-compatibility shims.
- No stubs, no simplifications, no workarounds.
- Import third-party crates instead of writing your own implementation. Less code is better.
</important>

Every crate has its own `thiserror` error enum. Workspace lints are strict (all clippy groups at warn); run clippy with `-D warnings` before committing.

## Branches and releases

Topic branches start from `dev`; `main` only accepts merges from `dev` or a `release-plz-*` branch. Releases go through release-plz and crates.io trusted publishing (`release.yml`); version bumps and `chore: release` commits belong to release-plz alone.
