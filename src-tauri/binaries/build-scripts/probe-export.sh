set -eu
: "${FFMPEG_INSTALL_PREFIX:?Set FFMPEG_INSTALL_PREFIX to the configured FFmpeg install prefix}"
: "${FFMPEG_TEST_AUDIO:?Set FFMPEG_TEST_AUDIO to a local stereo PCM WAV test file}"
record_dir=${FFMPEG_RECORD_DIR:-"${FFMPEG_INSTALL_PREFIX%/}/../records/n7.1-export"}
ffmpeg="$FFMPEG_INSTALL_PREFIX/bin/ffmpeg.exe"
ffprobe="$FFMPEG_INSTALL_PREFIX/bin/ffprobe.exe"
mkdir -p "$record_dir"
"$ffmpeg" -nostdin -hide_banner -y -f lavfi -i 'testsrc2=size=320x240:rate=30:duration=2' \
  -i "$FFMPEG_TEST_AUDIO" -filter_complex_threads 1 \
  -filter_complex "color=c=black:s=320x240:r=30:d=3[base];[0:v]trim=start_frame=15:end_frame=60,setpts=PTS-STARTPTS+0.25/TB,fps=30,scale=320:240:force_original_aspect_ratio=decrease,pad=320:240:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p[v0];[base][v0]overlay=eof_action=pass:shortest=0:enable='between(t,0.25,1.75)'[vout];[1:a]atrim=start=0.1:end=2.1,asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,volume=0.5,afade=t=in:st=0:d=0.25,afade=t=out:st=1.5:d=0.5,adelay=500|500,apad=whole_dur=3[a0];anullsrc=r=48000:cl=stereo,atrim=duration=3[asilent];[asilent][a0]amix=inputs=2:normalize=0:duration=first[aout]" \
  -map '[vout]' -map '[aout]' -t 3 -c:v h264_mf -b:v 1M -pix_fmt yuv420p \
  -c:a aac -b:a 96k -movflags +faststart -progress pipe:1 -nostats \
  "$record_dir/export-probe.mp4" > "$record_dir/export-progress.txt" 2> "$record_dir/export-encode.log"
"$ffprobe" -v error -show_streams -show_format -print_format json \
  "$record_dir/export-probe.mp4" > "$record_dir/export-probe.json"
"$ffmpeg" -nostdin -hide_banner -y -i "$record_dir/export-probe.mp4" \
  -map 0:a:0 -c:a pcm_s16le "$record_dir/export-audio.wav" > "$record_dir/export-audio-decode.log" 2>&1
"$ffmpeg" -nostdin -hide_banner -y -i "$record_dir/export-probe.mp4" \
  -map 0:v:0 -frames:v 1 -pix_fmt rgb24 -c:v rawvideo -f rawvideo \
  "$record_dir/export-first-frame.rgb" > "$record_dir/export-video-decode.log" 2>&1
