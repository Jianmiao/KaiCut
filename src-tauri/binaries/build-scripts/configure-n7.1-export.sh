set -eu
export PATH=/ucrt64/bin:/usr/bin:$PATH
: "${FFMPEG_SOURCE_DIR:?Set FFMPEG_SOURCE_DIR to a clean FFmpeg n7.1 source checkout}"
: "${FFMPEG_INSTALL_PREFIX:=${FFMPEG_SOURCE_DIR%/}/../install-n7.1-export}"
cd "$FFMPEG_SOURCE_DIR"
if [ -f ffbuild/config.mak ]; then
  mingw32-make distclean >/dev/null 2>&1
fi
./configure \
  --prefix="$FFMPEG_INSTALL_PREFIX" \
  --target-os=mingw32 --arch=x86_64 \
  --cross-prefix=x86_64-w64-mingw32- \
  --cc=x86_64-w64-mingw32-gcc --cxx=x86_64-w64-mingw32-g++ \
  --ar=ar --ranlib=ranlib --nm=nm --pkg-config=pkg-config \
  --pkg-config-flags=--static \
  --extra-ldflags="-static" --extra-libs="-static-libgcc -static-libstdc++" \
  --disable-gpl --disable-nonfree --disable-version3 \
  --disable-debug --disable-doc --disable-network --disable-autodetect \
  --disable-everything --enable-static --disable-shared \
  --enable-mediafoundation --enable-protocol=file,pipe --enable-zlib \
  --enable-demuxer=mov,mp3,wav,image2,image2pipe,lavfi \
  --enable-muxer=image2,image2pipe,mp4,wav,mp3,rawvideo \
  --enable-decoder=h264,aac,mp3,pcm_s16le,mjpeg,png,wrapped_avframe \
  --enable-encoder=h264_mf,aac,mjpeg,png,wrapped_avframe,pcm_s16le,rawvideo \
  --enable-parser=h264,aac,mpegaudio,png \
  --enable-bsf=h264_mp4toannexb,aac_adtstoasc \
  --enable-filter=scale,format,thumbnail,showwaves,aresample,anull,atrim,testsrc2,color,anullsrc,trim,setpts,asetpts,fps,setsar,pad,overlay,concat,amix,volume,afade,adelay,apad,aformat,split,asplit \
  --enable-indev=lavfi --enable-avfilter --enable-runtime-cpudetect
