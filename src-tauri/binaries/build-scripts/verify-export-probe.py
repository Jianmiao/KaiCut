"""Verify the bounded media emitted by probe-export.sh without desktop control."""
import array
import json
import math
import os
from pathlib import Path
import wave

records = Path(os.environ.get("FFMPEG_RECORD_DIR", "records/n7.1-export"))
probe = json.loads((records / "export-probe.json").read_text(encoding="utf-8"))
video, audio = probe["streams"]
assert video["codec_name"] == "h264" and int(video["nb_frames"]) == 90
assert audio["codec_name"] == "aac" and audio["sample_rate"] == "48000"
assert audio["channels"] == 2
assert float(video["duration"]) == float(audio["duration"]) == 3.0
assert "progress=end" in (records / "export-progress.txt").read_text()

frame = (records / "export-first-frame.rgb").read_bytes()
assert len(frame) == 320 * 240 * 3
# H.264 is lossy: dark values within 4/255 are a black initial timeline gap.
assert max(frame) <= 4 and sum(frame) / len(frame) < 0.1

with wave.open(str(records / "export-audio.wav"), "rb") as stream:
    assert stream.getsampwidth() == 2
    rate, channels = stream.getframerate(), stream.getnchannels()
    samples = array.array("h", stream.readframes(stream.getnframes()))


def rms(begin, end):
    values = samples[int(begin * rate * channels):int(end * rate * channels)]
    return math.sqrt(sum(value * value for value in values) / len(values))


measurements = {
    "decodedFirstFrameBytes": len(frame),
    "decodedFirstFrameMaxChannel": max(frame),
    "decodedFirstFrameMeanChannel": sum(frame) / len(frame),
    "leadingSilenceRmsPcm16": rms(0, 0.3),
    "activeAudioRmsPcm16": rms(1, 1.5),
    "trailingSilenceRmsPcm16": rms(2.7, 2.9),
}
assert measurements["leadingSilenceRmsPcm16"] < 2
assert measurements["activeAudioRmsPcm16"] > 100
assert measurements["trailingSilenceRmsPcm16"] < 2
measurements["passed"] = True
text = json.dumps(measurements, indent=2) + "\n"
(records / "export-verify.json").write_text(text, encoding="utf-8")
print(text, end="")
