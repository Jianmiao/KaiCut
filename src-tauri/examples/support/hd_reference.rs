//! Bounded reference ledger for the controlled 60-second native 1080p probe.
use serde::Deserialize;
use serde_json::Value;
use std::{fs::File, io::Read, path::Path};

const MAX_LEDGER_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdReference {
    pub schema_version: u32,
    pub width: u32,
    pub height: u32,
    pub frame_count: usize,
    pub duration_seconds: u64,
    pub source_video_sha256: String,
    pub frames: Vec<ReferenceFrame>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceFrame {
    pub index: usize,
    pub pts_ns: String,
    pub rgb_sha256: String,
    pub rgba_sha256: String,
}

impl HdReference {
    pub fn parse(bytes: &[u8], source_hash: &str) -> Result<Self, String> {
        if bytes.len() > MAX_LEDGER_BYTES {
            return Err("HD reference exceeds 2 MiB".into());
        }
        let ledger: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let hash_valid = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if ledger.schema_version != 1
            || ledger.width != 1920
            || ledger.height != 1080
            || ledger.frame_count != 1800
            || ledger.duration_seconds != 60
            || ledger.frames.len() != 1800
            || !hash_valid(source_hash)
            || ledger.source_video_sha256 != source_hash
        {
            return Err(
                "HD ledger source, dimensions or duration do not match the controlled fixture"
                    .into(),
            );
        }
        for (index, frame) in ledger.frames.iter().enumerate() {
            if frame.index != index
                || frame.pts_ns != (index as u64 * 1_000_000_000 / 30).to_string()
                || !hash_valid(&frame.rgb_sha256)
                || !hash_valid(&frame.rgba_sha256)
            {
                return Err(format!("Invalid HD reference at frame {index}"));
            }
        }
        Ok(ledger)
    }
    pub fn load(path: &Path, source_hash: &str) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_LEDGER_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::parse(&bytes, source_hash)
    }
    pub fn verify_record(&self, frame: &Value) -> Result<(), String> {
        let index = frame["index"]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .ok_or("Missing HD frame index")?;
        let expected = self
            .frames
            .get(index)
            .ok_or("HD frame index exceeds reference")?;
        if frame["ptsNs"] != expected.pts_ns
            || frame["inputRgbSha256"] != expected.rgb_sha256
            || frame["expectedRgbaSha256"] != expected.rgba_sha256
            || frame["actualRgbaSha256"] != expected.rgba_sha256
            || frame["exact"] != true
        {
            return Err(format!(
                "HD frame {index} does not match independent reference"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ledger() -> Value {
        json!({"schemaVersion":1,"width":1920,"height":1080,"frameCount":1800,"durationSeconds":60,
            "sourceVideoSha256":"a".repeat(64),"frames":(0..1800).map(|index|json!({"index":index,
                "ptsNs":(index as u64*1_000_000_000/30).to_string(),"rgbSha256":"b".repeat(64),"rgbaSha256":"c".repeat(64)})).collect::<Vec<_>>()})
    }
    fn parse(value: &Value) -> Result<HdReference, String> {
        HdReference::parse(&serde_json::to_vec(value).unwrap(), &"a".repeat(64))
    }
    #[test]
    fn validates_full_1080p_ledger_and_rejects_wrong_source_geometry_or_pts() {
        assert_eq!(parse(&ledger()).unwrap().frames.len(), 1800);
        let mut value = ledger();
        value["width"] = 160.into();
        assert!(parse(&value).is_err());
        let mut value = ledger();
        value["frames"][100]["ptsNs"] = "0".into();
        assert!(parse(&value).is_err());
        assert!(
            HdReference::parse(&serde_json::to_vec(&ledger()).unwrap(), &"d".repeat(64)).is_err()
        );
        assert!(HdReference::parse(&vec![b' '; MAX_LEDGER_BYTES + 1], &"a".repeat(64)).is_err());
    }
    #[test]
    fn verifies_source_and_readback_hashes_without_index_wrapping() {
        let reference = parse(&ledger()).unwrap();
        let mut frame = json!({"index":1799,"ptsNs":"59966666666","inputRgbSha256":"b".repeat(64),
            "expectedRgbaSha256":"c".repeat(64),"actualRgbaSha256":"c".repeat(64),"exact":true});
        assert!(reference.verify_record(&frame).is_ok());
        frame["index"] = 1800.into();
        assert!(reference.verify_record(&frame).is_err());
        frame["index"] = 1799.into();
        frame["actualRgbaSha256"] = "d".repeat(64).into();
        assert!(reference.verify_record(&frame).is_err());
    }
}
