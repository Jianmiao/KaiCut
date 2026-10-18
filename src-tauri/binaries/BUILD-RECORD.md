# Windows FFmpeg sidecar build record

This record describes a Windows x64 candidate built and tested locally. The
source repository intentionally does not include the prebuilt executables; it
records build provenance and runtime checks, not patent clearance or a legal
opinion.

## Source

- Remote: `https://git.ffmpeg.org/ffmpeg.git`
- Release tag: `n7.1`
- Commit: `b08d7969c550a804a59511c7b83f2dd8cc0499b8`
- The original local clean checkout used for the build was outside this
  repository. To rebuild, obtain the exact source tag/commit above and set
  `FFMPEG_SOURCE_DIR` to that clean checkout.
- Local patches: none

## Build

The 2026-10-11 export build used MSYS2 UCRT64 on Windows with GCC 16.2.0,
NASM 3.02, Yasm 1.3.0, and static linking. The executable build scripts are
retained in `build-scripts/`; they use a clean, unmodified checkout selected by
`FFMPEG_SOURCE_DIR` and an install path selected by `FFMPEG_INSTALL_PREFIX`:

```text
--prefix=/e/ffmpeg-build/install-n7.1-export
--target-os=mingw32 --arch=x86_64
--cross-prefix=x86_64-w64-mingw32-
--cc=x86_64-w64-mingw32-gcc --cxx=x86_64-w64-mingw32-g++
--ar=ar --ranlib=ranlib --nm=nm --pkg-config=pkg-config
--pkg-config-flags=--static
--extra-ldflags=-static --extra-libs="-static-libgcc -static-libstdc++"
--disable-gpl --disable-nonfree --disable-version3
--disable-debug --disable-doc --disable-network --disable-autodetect
--disable-everything --enable-static --disable-shared
--enable-mediafoundation --enable-protocol=file,pipe --enable-zlib
--enable-demuxer=mov,mp3,wav,image2,image2pipe,lavfi
--enable-muxer=image2,image2pipe,mp4,wav,mp3,rawvideo
--enable-decoder=h264,aac,mp3,pcm_s16le,mjpeg,png,wrapped_avframe
--enable-encoder=h264_mf,aac,mjpeg,png,wrapped_avframe,pcm_s16le,rawvideo
--enable-parser=h264,aac,mpegaudio,png
--enable-bsf=h264_mp4toannexb,aac_adtstoasc
--enable-filter=scale,format,thumbnail,showwaves,aresample,anull,atrim,testsrc2,color,anullsrc,trim,setpts,asetpts,fps,setsar,pad,overlay,concat,amix,volume,afade,adelay,apad,aformat,split,asplit
--enable-indev=lavfi
--enable-avfilter --enable-runtime-cpudetect
```

`ffmpeg -L` reports LGPL version 2.1 or later. The configure output reports no
 GPL, nonfree, or version3 components. `h264_mf` is an LGPL FFmpeg wrapper
 around the Windows Media Foundation H.264 MFT; the operating system performs
 the actual encoding. The built-in AAC encoder is also enabled.

## Artifacts

| File | SHA-256 | Size |
| --- | --- | ---: |
| `ffmpeg-x86_64-pc-windows-msvc.exe` | `9752FD44C9E1867C8D26702EF81DF1AAD1442C1099888D02FED7C29DE63AFF39` | 5,469,696 bytes |
| `ffprobe-x86_64-pc-windows-msvc.exe` | `3E3DFE5C77E7F3B8F9DE1C8EC2CCFEA4EC1E9F34E930B6F918894C9D2838219A` | 5,381,120 bytes |

The previous Media Foundation build and its provenance records remain in a
local backup, not in this repository. Its FFmpeg hash was
`D82040973FB8C64CCEDF8B99A3ED3209C5B6739D31EDD5F427916368CEA02D84` and its
FFprobe hash was `1F9BD790140DDC4D144B867DE84E8BC0F2B72EC70233950E9FE5417C53A734BE`.

The binaries import only Windows system/UCRT APIs. No MSYS2 DLL is imported by
either executable. Static dependency notices are in `licenses/`:

- zlib 1.3.2 (`zlib-LICENSE.txt`)
- GCC/libatomic runtime exception and GPL text (`libgcc-*`, `libatomic-*`)
- libstdc++ runtime notices are included for the static toolchain configuration

The MSYS2 package versions used for the build were recorded from the installed
UCRT64 environment: GCC/libatomic/libstdc++ 16.2.0-4, zlib 1.3.2-2, and
winpthreads 14.0.0.r426.g4564ee4b5-1. winpthreads is not imported by the final
executables.

## Runtime checks

- `ffmpeg.exe -version`, `-buildconf`, `-L`, `-encoders`, `-filters`, and
  `-protocols` are retained in `records/` for this export build. The license
  remains LGPL-2.1-or-later. The only protocols are `file` and `pipe`; network
  support remains disabled. Runtime media inputs must still use a file-only
  protocol allowlist; `pipe` is for structured progress output to the worker.
- The filters cover black/silent canvas generation, source trim and timing,
  fixed frame rate, scale/pad/aspect normalization, overlay/concat, independent
  volume and fade envelopes, delayed multi-track audio mixing, and split/asplit.
  Existing thumbnail and waveform filters are preserved. `pcm_s16le` and
  `rawvideo` encoding/muxing support deterministic decoded-output checks.
- A real Windows probe encoded 90 frames / 3 seconds at 320×240 / 30 fps with
  `h264_mf` and stereo 48 kHz AAC. The graph included delayed video on a black
  canvas, source trim, volume 0.5, fade-in/out, delayed audio and a silent mix
  base. FFprobe confirmed both streams and 3-second duration;
  `-progress pipe:1` reported `progress=end`. See `export-*.log`,
  `export-progress.txt`, and `export-probe.json` in `records/`.
- The resulting MP4 decoded successfully into PCM WAV and raw RGB video with
  this same binary. `build-scripts/verify-export-probe.py` confirmed a black
  initial frame (maximum decoded channel 2/255, allowing H.264 compression),
  silent leading/trailing audio and nonzero audio in the active range; the
  measured values are in `records/export-verify.json`. This checks the test
  decoders/encoders and graph execution; it does not substitute for
  application-level render parity or listening tests.
- `ffmpeg-dll-imports.txt` and `ffprobe-dll-imports.txt` capture PE imports:
  only Windows/UCRT system APIs; no MSYS2 or third-party DLLs.
- The older `h264-mf-aac-*` files in `records/` document the previous build;
  the current build's evidence is named `export-*`.
- This proves the encoder is available on this development machine. Runtime
  capability must still be checked per Windows machine before enabling export.

The locally retained executables are source-built and configuration-audited,
but this is not a patent clearance or a legal opinion. KaiCut's Apache license
does not relicense FFmpeg. Local binary files are ignored by Git; the hashes
above identify the candidate they describe.
