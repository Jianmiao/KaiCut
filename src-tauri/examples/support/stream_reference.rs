//! Offline reference hashes for completed native-stream probe submissions.
//! Call after playback; these helpers do not decode, play or load whole media.

use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const VIDEO_FRAME_BYTES: usize = 160 * 90 * 3;
const PCM_READ_BYTES: usize = 32 * 1024;
const MAX_FMT_BYTES: u32 = 4096;

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = bytes
        .get(offset..offset + 2)
        .ok_or("Truncated WAV format integer")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = bytes
        .get(offset..offset + 4)
        .ok_or("Truncated WAV format integer")?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn validate_format(format: &[u8], sample_rate: u32, channels: u16) -> Result<(), String> {
    if format.len() < 16 {
        return Err("Truncated WAV fmt chunk".into());
    }
    if format.len() != 16
        && (format.len() < 18 || usize::from(read_u16(format, 16)?) + 18 != format.len())
    {
        return Err("WAV fmt extension length is inconsistent".into());
    }
    match read_u16(format, 0)? {
        1 => {}
        0xfffe => {
            const PCM_GUID: [u8; 16] = [
                1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
            ];
            if format.len() < 40
                || read_u16(format, 16)? < 22
                || read_u16(format, 18)? != 16
                || format.get(24..40) != Some(PCM_GUID.as_slice())
            {
                return Err("WAVEFORMATEXTENSIBLE must contain 16-bit PCM".into());
            }
        }
        _ => return Err("Reference WAV must contain integer PCM16".into()),
    }
    let stride = channels
        .checked_mul(2)
        .ok_or("PCM channel stride overflow")?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(stride))
        .ok_or("PCM byte rate overflow")?;
    if read_u16(format, 2)? != channels
        || read_u32(format, 4)? != sample_rate
        || read_u32(format, 8)? != byte_rate
        || read_u16(format, 12)? != stride
        || read_u16(format, 14)? != 16
    {
        return Err(
            "Reference WAV rate, channels or sample stride do not match requested PCM16 format"
                .into(),
        );
    }
    Ok(())
}

/// Scan every RIFF chunk using fixed-size headers and seeks. Data size is known
/// and exact; 0xffffffff streaming sentinels and truncated trailing chunks are
/// invalid in this completed offline reference file.
fn pcm_data(file: &mut File, sample_rate: u32, channels: u16) -> Result<(u64, u64), String> {
    let info = file.metadata().map_err(|error| error.to_string())?;
    if !info.is_file() || info.len() < 12 {
        return Err("Reference WAV is not a complete regular file".into());
    }
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|error| error.to_string())?;
    let riff_size = read_u32(&header, 4)?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WAVE"
        || riff_size == u32::MAX
        || u64::from(riff_size) + 8 != info.len()
    {
        return Err("Reference WAV requires an exact known RIFF length".into());
    }
    let mut cursor = 12_u64;
    let mut format = None;
    let mut data = None;
    while cursor < info.len() {
        if info.len() - cursor < 8 {
            return Err("Truncated WAV chunk header".into());
        }
        file.seek(SeekFrom::Start(cursor))
            .map_err(|error| error.to_string())?;
        let mut chunk = [0_u8; 8];
        file.read_exact(&mut chunk)
            .map_err(|error| error.to_string())?;
        let length = read_u32(&chunk, 4)?;
        if length == u32::MAX {
            return Err("Unknown WAV chunk length is not permitted for offline references".into());
        }
        let start = cursor.checked_add(8).ok_or("WAV offset overflow")?;
        let end = start
            .checked_add(u64::from(length))
            .ok_or("WAV chunk length overflow")?;
        let next = end
            .checked_add(u64::from(length & 1))
            .ok_or("WAV chunk padding overflow")?;
        if next > info.len() {
            return Err("Truncated WAV chunk or missing alignment padding".into());
        }
        match &chunk[..4] {
            b"fmt " => {
                if format.is_some() || !(16..=MAX_FMT_BYTES).contains(&length) {
                    return Err("Duplicate or oversized WAV fmt chunk".into());
                }
                let mut bytes = vec![0_u8; length as usize];
                file.read_exact(&mut bytes)
                    .map_err(|error| error.to_string())?;
                validate_format(&bytes, sample_rate, channels)?;
                format = Some(());
            }
            b"data" => {
                if data.replace((start, u64::from(length))).is_some() {
                    return Err("Duplicate WAV data chunk".into());
                }
            }
            _ => {}
        }
        cursor = next;
    }
    format.ok_or("Missing WAV fmt chunk")?;
    let (offset, bytes) = data.ok_or("Missing WAV data chunk")?;
    if bytes % (u64::from(channels) * 2) != 0 {
        return Err("PCM data ends in a partial interleaved sample frame".into());
    }
    Ok((offset, bytes))
}

/// Hash exactly one interleaved source-frame interval as little-endian f32
/// samples, matching the probe's i16 / 32768.0 submission representation.
/// Memory is bounded to one 32 KiB block plus a bounded format chunk regardless
/// of file or requested range length. A valid empty interval hashes empty bytes.
pub fn reference_pcm_hash(
    path: &Path,
    start_frame: u64,
    frame_count: u64,
    sample_rate: u32,
    channels: u16,
) -> Result<String, String> {
    if !(8000..=192000).contains(&sample_rate) || !(1..=8).contains(&channels) {
        return Err("Reference PCM format must be 8–192 kHz with 1–8 channels".into());
    }
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let (data_offset, data_bytes) = pcm_data(&mut file, sample_rate, channels)?;
    let stride = u64::from(channels) * 2;
    let total_frames = data_bytes / stride;
    let end = start_frame
        .checked_add(frame_count)
        .ok_or("PCM reference frame range overflow")?;
    if start_frame > total_frames || end > total_frames {
        return Err("Requested PCM source range exceeds the WAV data chunk".into());
    }
    let offset = start_frame
        .checked_mul(stride)
        .and_then(|bytes| data_offset.checked_add(bytes))
        .ok_or("PCM reference byte offset overflow")?;
    let mut remaining = frame_count
        .checked_mul(stride)
        .ok_or("PCM reference byte length overflow")?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut buffer = [0_u8; PCM_READ_BYTES];
    let mut hash = Sha256::new();
    while remaining > 0 {
        let count = remaining.min(PCM_READ_BYTES as u64) as usize;
        file.read_exact(&mut buffer[..count])
            .map_err(|error| format!("Reference PCM truncated while hashing: {error}"))?;
        for sample in buffer[..count].chunks_exact(2) {
            let value = f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0;
            hash.update(value.to_le_bytes());
        }
        remaining -= count as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Read a small interleaved PCM reference window after validating the complete
/// known-length WAV structure. This offline helper is bounded to 4800 frames;
/// it is not a playback decoder or an unbounded whole-source allocation.
pub fn reference_pcm_samples(
    path: &Path,
    start_frame: u64,
    frame_count: u32,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<f32>, String> {
    if !(8000..=192000).contains(&sample_rate) || !(1..=8).contains(&channels) || frame_count > 4800
    {
        return Err(
            "PCM reference window requires 8–192 kHz, 1–8 channels and at most 4800 frames".into(),
        );
    }
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let (data_offset, data_bytes) = pcm_data(&mut file, sample_rate, channels)?;
    let stride = u64::from(channels) * 2;
    let total_frames = data_bytes / stride;
    let end = start_frame
        .checked_add(u64::from(frame_count))
        .ok_or("PCM reference window range overflow")?;
    if start_frame > total_frames || end > total_frames {
        return Err("PCM reference window exceeds source data".into());
    }
    let offset = start_frame
        .checked_mul(stride)
        .and_then(|value| data_offset.checked_add(value))
        .ok_or("PCM reference window byte offset overflow")?;
    let bytes = usize::try_from(u64::from(frame_count) * stride)
        .map_err(|_| "PCM reference window allocation overflow")?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut data = vec![0_u8; bytes];
    file.read_exact(&mut data)
        .map_err(|error| format!("Reference PCM window truncated: {error}"))?;
    Ok(data
        .chunks_exact(2)
        .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0)
        .collect())
}

/// Hash one dense 160x90 RGB24 source frame by seeking directly to its index.
/// Full-file length must be an integral number of frames, including when index
/// zero was requested; a partially written reference file is never accepted.
pub fn reference_video_hash(path: &Path, index: usize) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let info = file.metadata().map_err(|error| error.to_string())?;
    let stride = VIDEO_FRAME_BYTES as u64;
    if !info.is_file() || info.len() == 0 || info.len() % stride != 0 {
        return Err("Reference RGB file must contain complete dense 160x90 RGB24 frames".into());
    }
    let index = u64::try_from(index).map_err(|_| "Video reference index overflow")?;
    if index >= info.len() / stride {
        return Err("Video reference frame index is outside the file".into());
    }
    let offset = index
        .checked_mul(stride)
        .ok_or("Video reference offset overflow")?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut frame = vec![0_u8; VIDEO_FRAME_BYTES];
    file.read_exact(&mut frame)
        .map_err(|error| format!("Video reference frame truncated: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(frame)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn wave(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + samples.len() as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn hashes_only_the_requested_interleaved_pcm_frame_range() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("PCM 'quoted' [区间].wav");
        fs::write(
            &path,
            wave(&[1, 2, i16::MIN, 0, i16::MAX, 16384, 4, 5], 48000, 2),
        )
        .unwrap();
        let expected: Vec<u8> = [-1.0_f32, 0.0, 32767.0 / 32768.0, 0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            reference_pcm_hash(&path, 1, 2, 48000, 2).unwrap(),
            format!("{:x}", Sha256::digest(expected))
        );
        assert_eq!(
            reference_pcm_hash(&path, 4, 0, 48000, 2).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(
            reference_pcm_hash(&path, 0, 2, 48000, 2).unwrap(),
            reference_pcm_hash(&path, 1, 2, 48000, 2).unwrap()
        );
    }

    #[test]
    fn sample_window_returns_exact_stereo_tail_and_empty_eof() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("window.wav");
        fs::write(
            &path,
            wave(&[111, 222, i16::MIN, i16::MAX, 16384, -16384], 48000, 2),
        )
        .unwrap();
        assert_eq!(
            reference_pcm_samples(&path, 1, 2, 48000, 2).unwrap(),
            vec![-1.0, 32767.0 / 32768.0, 0.5, -0.5]
        );
        assert!(
            reference_pcm_samples(&path, 3, 0, 48000, 2)
                .unwrap()
                .is_empty()
        );
        assert!(reference_pcm_samples(&path, 2, 2, 48000, 2).is_err());
        assert!(reference_pcm_samples(&path, 4, 0, 48000, 2).is_err());
    }

    #[test]
    fn sample_window_rejects_unbounded_requests_and_invalid_pcm_before_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bounded.wav");
        fs::write(&path, wave(&[0; 9600], 48000, 2)).unwrap();
        assert_eq!(
            reference_pcm_samples(&path, 0, 4800, 48000, 2)
                .unwrap()
                .len(),
            9600
        );
        for count in [4801, u32::MAX] {
            assert!(reference_pcm_samples(&path, 0, count, 48000, 2).is_err());
        }
        assert!(reference_pcm_samples(&path, u64::MAX, 1, 48000, 2).is_err());
        assert!(reference_pcm_samples(&path, 0, 1, 44100, 2).is_err());
        let mut truncated = wave(&[1, 2, 3, 4], 48000, 2);
        truncated.pop();
        fs::write(&path, truncated).unwrap();
        assert!(reference_pcm_samples(&path, 0, 1, 48000, 2).is_err());
    }

    #[test]
    fn rejects_pcm_ranges_outside_data_and_format_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.wav");
        fs::write(&path, wave(&[1, 2, 3, 4], 48000, 2)).unwrap();
        for (start, count) in [(2, 1), (3, 0), (u64::MAX, 1), (1, u64::MAX)] {
            assert!(reference_pcm_hash(&path, start, count, 48000, 2).is_err());
        }
        assert!(reference_pcm_hash(&path, 0, 1, 44100, 2).is_err());
        assert!(reference_pcm_hash(&path, 0, 1, 48000, 1).is_err());
        assert!(reference_pcm_hash(&path, 0, 1, 0, 2).is_err());
    }

    #[test]
    fn validates_the_entire_riff_even_if_requested_audio_precedes_truncated_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.wav");
        let good = wave(&[1, 2, 3, 4], 48000, 2);
        let mut tail = good.clone();
        tail.extend_from_slice(b"JUNK");
        tail.extend_from_slice(&10_u32.to_le_bytes());
        tail.push(0);
        let size = tail.len() as u32 - 8;
        tail[4..8].copy_from_slice(&size.to_le_bytes());
        fs::write(&path, tail).unwrap();
        assert!(reference_pcm_hash(&path, 0, 1, 48000, 2).is_err());
        fs::write(&path, &good[..good.len() - 1]).unwrap();
        assert!(reference_pcm_hash(&path, 0, 0, 48000, 2).is_err());
        fs::write(&path, wave(&[1, 2, 3], 48000, 2)).unwrap();
        assert!(reference_pcm_hash(&path, 0, 1, 48000, 2).is_err());
    }

    #[test]
    fn accepts_padded_unknown_chunks_without_hashing_them_as_audio() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extra.wav");
        let good = wave(&[16384, -16384], 8000, 1);
        let mut extra = good[..12].to_vec();
        extra.extend_from_slice(b"JUNK");
        extra.extend_from_slice(&3_u32.to_le_bytes());
        extra.extend_from_slice(&[8, 9, 10, 0]);
        extra.extend_from_slice(&good[12..]);
        let size = extra.len() as u32 - 8;
        extra[4..8].copy_from_slice(&size.to_le_bytes());
        fs::write(&path, extra).unwrap();
        let expected: Vec<u8> = [0.5_f32, -0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            reference_pcm_hash(&path, 0, 2, 8000, 1).unwrap(),
            format!("{:x}", Sha256::digest(expected))
        );
    }

    #[test]
    fn accepts_pcm_extensible_but_rejects_float_guid_and_streaming_length_sentinels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extended.wav");
        let original = wave(&[16384, -16384], 48000, 2);
        let mut extended = original[..36].to_vec();
        extended[16..20].copy_from_slice(&40_u32.to_le_bytes());
        extended[20..22].copy_from_slice(&0xfffe_u16.to_le_bytes());
        extended.extend_from_slice(&22_u16.to_le_bytes());
        extended.extend_from_slice(&16_u16.to_le_bytes());
        extended.extend_from_slice(&3_u32.to_le_bytes());
        extended.extend_from_slice(&[
            1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ]);
        extended.extend_from_slice(&original[36..]);
        let length = extended.len() as u32 - 8;
        extended[4..8].copy_from_slice(&length.to_le_bytes());
        fs::write(&path, &extended).unwrap();
        let expected: Vec<u8> = [0.5_f32, -0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            reference_pcm_hash(&path, 0, 1, 48000, 2).unwrap(),
            format!("{:x}", Sha256::digest(expected))
        );
        extended[44] = 3;
        fs::write(&path, &extended).unwrap();
        assert!(reference_pcm_hash(&path, 0, 1, 48000, 2).is_err());
        let mut sentinel = original.clone();
        sentinel[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, &sentinel).unwrap();
        assert!(reference_pcm_hash(&path, 0, 0, 48000, 2).is_err());
        let mut sentinel = original;
        sentinel[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, &sentinel).unwrap();
        assert!(reference_pcm_hash(&path, 0, 0, 48000, 2).is_err());
    }

    #[test]
    fn video_reference_reads_one_dense_frame_at_the_exact_index() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frames.rgb");
        let first = vec![7_u8; VIDEO_FRAME_BYTES];
        let second = vec![91_u8; VIDEO_FRAME_BYTES];
        fs::write(&path, [first.clone(), second.clone()].concat()).unwrap();
        assert_eq!(
            reference_video_hash(&path, 0).unwrap(),
            format!("{:x}", Sha256::digest(first))
        );
        assert_eq!(
            reference_video_hash(&path, 1).unwrap(),
            format!("{:x}", Sha256::digest(second))
        );
        assert!(reference_video_hash(&path, 2).is_err());
        assert!(reference_video_hash(&path, usize::MAX).is_err());
        fs::write(&path, vec![0; VIDEO_FRAME_BYTES + 1]).unwrap();
        assert!(reference_video_hash(&path, 0).is_err());
    }
}
