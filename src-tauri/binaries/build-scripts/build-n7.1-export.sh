set -eu
export PATH=/ucrt64/bin:/usr/bin:$PATH
: "${FFMPEG_SOURCE_DIR:?Set FFMPEG_SOURCE_DIR to the configured FFmpeg n7.1 source checkout}"
cd "$FFMPEG_SOURCE_DIR"
mingw32-make -j4 STRIP=strip
mingw32-make install
