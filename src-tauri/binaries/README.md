# FFmpeg sidecar delivery

No FFmpeg binary is included in the scaffold. Import records file metadata only. The media-engine status command reports this absence; the UI must not present decoding, playback, or export as functional. KaiCut original code is Apache-2.0; the separately distributed FFmpeg sidecar must be an LGPL build. These are separate licenses for separate components, not a dual license for the same source.

After selecting an audited build, place the executable here using Tauri's target-triple naming convention:

- Windows x64: `ffmpeg-x86_64-pc-windows-msvc.exe`
- macOS Apple Silicon: `ffmpeg-aarch64-apple-darwin`
- macOS Intel: `ffmpeg-x86_64-apple-darwin`
- Linux x64 (deferred platform): `ffmpeg-x86_64-unknown-linux-gnu`

Use `rustc -vV` to inspect the build host triple. Cross builds still need the target platform's binary, architecture, executable permissions, and platform signing. A universal macOS application needs a deliberately prepared universal sidecar; do not assume one architecture works on both.

Build using the optional config overlay once the matching executable exists:

```text
pnpm tauri build --config src-tauri/tauri.sidecar.conf.json
```

Tauri packages the target-specific source binary under the regular `ffmpeg` / `ffmpeg.exe` executable name. The runtime searches only fixed executable-sibling or bundled-resource paths; it never searches the user's PATH or accepts an executable path from the frontend. The only implemented invocation is `-version`, with a bounded wait and no shell.

## LGPL build policy

The default target is LGPL-2.1-or-later. Use a clean, reproducible build with `--disable-gpl --disable-nonfree --disable-version3`. These three flags express a policy, not a complete build recipe or proof of compliance. Audit every external library and transitive dependency, including the exact shared libraries shipped with the executable.

- Never deliver `--enable-gpl` or `--enable-nonfree` builds under an LGPL label. Exclude libx264/libx265 and other GPL components. Do not assume a downloaded build labelled "full" or "shared" is LGPL.
- Version-3-only dependencies require a separate decision and accurate LGPL v3 notices; they are excluded from the default 2.1 baseline. Do not add libfdk_aac by default; FFmpeg's built-in `aac` is the first candidate to validate.
- Capture `ffmpeg -version`, `ffmpeg -buildconf`, `ffmpeg -L`, and `ffmpeg -encoders` for the exact binary. A valid version string does not verify licensing: the existing runtime command only checks availability.
- Record upstream release/commit, source and binary URL, SHA-256, configure command, enabled libraries/codecs, complete build scripts, local patches, and the license texts. Supply the exact corresponding source and modifications alongside the binary using a delivery method satisfying the applicable LGPL terms. A generic upstream homepage is not a substitute for matching source.
- Preserve FFmpeg attribution in the application and distribution notices. Standalone-process use does not remove FFmpeg's own source/notice obligations. If future code links FFmpeg libraries instead, reassess the linkage/relinking requirements independently.
- Codec patent obligations are separate from copyright licenses. Apache's patent grant is not an AVC/HEVC license. OpenH264 or platform encoders must be assessed for their actual build/distribution terms rather than assumed patent-free.

## Encoder availability

H.264 software decoding does not imply H.264 encoding. libx264 and libx265 are excluded. Candidates such as `h264_videotoolbox`, `h264_mf`, QSV or NVENC require both a suitable LGPL build and runtime capability checks. Do not promise universal software H.264 output. If an approved encoder is unavailable, disable that preset with an explanation, or let the user explicitly select another tested format. Proxy output must also be playable in WKWebView/WebView2; never silently change formats or fetch an unreviewed encoder.

No binary has been selected, its hash and build configuration remain pending, and no LGPL compliance claim is made for a shipped artifact yet. KaiCut's Apache license change does not relicense FFmpeg.

For macOS, sign the FFmpeg executable and its nested libraries before signing and notarizing the application. Windows builds need architecture-compatible binaries and their runtime dependencies. Linux delivery is deferred.

References: [Tauri sidecars](https://v2.tauri.app/develop/sidecar/), [FFmpeg legal guidance](https://ffmpeg.org/legal.html).
