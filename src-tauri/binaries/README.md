# FFmpeg sidecar delivery

This source tree records a Windows x64 source-built candidate, but does not
commit the prebuilt `ffmpeg-x86_64-pc-windows-msvc.exe` and
`ffprobe-x86_64-pc-windows-msvc.exe` files. They are local/release artifacts
ignored by Git. The build scripts, notices and verification records are kept
here so a matching binary can be rebuilt and audited. It is an
LGPL-2.1-or-later FFmpeg 7.1 configuration with no GPL, nonfree, or version3
components. macOS/Linux binaries are still absent. KaiCut original code is
Apache-2.0; the separately distributed FFmpeg sidecar is a separate LGPL
component.

After selecting an audited build, place the local executable here using Tauri's target-triple naming convention:

- Windows x64: `ffmpeg-x86_64-pc-windows-msvc.exe` and `ffprobe-x86_64-pc-windows-msvc.exe`
- macOS Apple Silicon: `ffmpeg-aarch64-apple-darwin` and `ffprobe-aarch64-apple-darwin`
- macOS Intel: `ffmpeg-x86_64-apple-darwin` and `ffprobe-x86_64-apple-darwin`
- Linux x64 (deferred platform): `ffmpeg-x86_64-unknown-linux-gnu` and `ffprobe-x86_64-unknown-linux-gnu`

Use `rustc -vV` to inspect the build host triple. Cross builds still need the target platform's binary, architecture, executable permissions, and platform signing. A universal macOS application needs a deliberately prepared universal sidecar; do not assume one architecture works on both.

Build using the optional config overlay once the matching executable exists:

```text
pnpm tauri build --config src-tauri/tauri.sidecar.conf.json
```

Tauri packages each target-specific source binary under the regular `ffmpeg` / `ffmpeg.exe` and `ffprobe` / `ffprobe.exe` executable names. The runtime searches only fixed executable-sibling or bundled-resource paths; it never searches the user's PATH or accepts an executable path from the frontend. `media_engine_status` performs a bounded `ffmpeg -version` and encoder-list check. `media_probe` invokes the packaged `ffprobe` with a fixed file-only format allowlist, JSON output, a 10-second timeout, and a 16 MiB output cap; it returns runtime metadata and does not write the `.kaicut` project file. Both invocations avoid a shell.

## LGPL build policy

The default target is LGPL-2.1-or-later. Use a clean, reproducible build with `--disable-gpl --disable-nonfree --disable-version3`. These three flags express a policy, not a complete build recipe or proof of compliance. Audit every external library and transitive dependency, including the exact shared libraries shipped with the executable.

- Never deliver `--enable-gpl` or `--enable-nonfree` builds under an LGPL label. Exclude libx264/libx265 and other GPL components. Do not assume a downloaded build labelled "full" or "shared" is LGPL.
- Version-3-only dependencies require a separate decision and accurate LGPL v3 notices; they are excluded from the default 2.1 baseline. Do not add libfdk_aac by default; FFmpeg's built-in `aac` is the first candidate to validate.
- Capture `ffmpeg -version`, `ffmpeg -buildconf`, `ffmpeg -L`, `ffmpeg -encoders`, and `ffprobe -version` for the exact binaries. A valid version string or runtime probe does not verify licensing; runtime checks only establish availability and metadata parsing.
- Record upstream release/commit, source and binary URL, SHA-256, configure command, enabled libraries/codecs, complete build scripts, local patches, and the license texts. Supply the exact corresponding source and modifications alongside the binary using a delivery method satisfying the applicable LGPL terms. A generic upstream homepage is not a substitute for matching source.
- Preserve FFmpeg attribution in the application and distribution notices. Standalone-process use does not remove FFmpeg's own source/notice obligations. If future code links FFmpeg libraries instead, reassess the linkage/relinking requirements independently.
- Codec patent obligations are separate from copyright licenses. Apache's patent grant is not an AVC/HEVC license. OpenH264 or platform encoders must be assessed for their actual build/distribution terms rather than assumed patent-free.

## Encoder availability

H.264 software decoding does not imply H.264 encoding. libx264 and libx265 are excluded. Candidates such as `h264_videotoolbox`, `h264_mf`, QSV or NVENC require both a suitable LGPL build and runtime capability checks. Do not promise universal software H.264 output. If an approved encoder is unavailable, disable that preset with an explanation, or let the user explicitly select another tested format. Proxy output must also be playable in WKWebView/WebView2; never silently change formats or fetch an unreviewed encoder.

## Current Windows build record

- Source: `https://git.ffmpeg.org/ffmpeg.git`, tag `n7.1`, commit `b08d7969c550a804a59511c7b83f2dd8cc0499b8`.
- `ffmpeg-x86_64-pc-windows-msvc.exe` SHA-256: `9752FD44C9E1867C8D26702EF81DF1AAD1442C1099888D02FED7C29DE63AFF39`.
- `ffprobe-x86_64-pc-windows-msvc.exe` SHA-256: `3E3DFE5C77E7F3B8F9DE1C8EC2CCFEA4EC1E9F34E930B6F918894C9D2838219A`.
- Build scripts are in `build-scripts/`, command outputs are in `records/`, and `BUILD-RECORD.md` records the candidate configuration. The original full source checkout, compiler logs and probe media were kept outside this source repository; they are not part of this snapshot.
- The static executables import only Windows system/UCRT APIs. Static build dependencies include zlib and GCC/libatomic runtime code; their license texts are included in `licenses/`.
- The 2026-10-11 build adds the LGPL filters needed for timeline composition and mixing while preserving thumbnail/waveform support. `file` and `pipe` are enabled, with all network support disabled. Media input commands retain a file-only protocol allowlist; the export worker uses `pipe:1` only for structured progress. The binary enables H.264 Media Foundation + built-in AAC, plus PCM/rawvideo for decoded-output tests.
- A real 3-second / 90-frame H.264/AAC MP4 probe passed source trimming, video overlay, volume/fades, delayed audio mixing, progress reporting and PCM/RGB decode checks. See `BUILD-RECORD.md` and `records/export-*`; see `../../docs/MEDIA-VALIDATION.md` for application test coverage. Encoder success on this development machine does not establish availability on every Windows installation.

The binaries are source-built and configuration-audited, but this is not a patent clearance or a legal opinion. KaiCut's Apache license does not relicense FFmpeg.

For macOS, sign the FFmpeg executable and its nested libraries before signing and notarizing the application. Windows builds need architecture-compatible binaries and their runtime dependencies. Linux delivery is deferred.

References: [Tauri sidecars](https://v2.tauri.app/develop/sidecar/), [FFmpeg legal guidance](https://ffmpeg.org/legal.html).
