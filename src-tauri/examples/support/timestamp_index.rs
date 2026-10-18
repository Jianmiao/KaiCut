//! Bounded decoded-frame timestamp index; no average frame rate is used.

pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_INDEX_FRAMES: usize = 100_000;
const MAX_DURATION_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

#[derive(Debug, Clone, Copy)]
pub struct TimestampMetadata {
    pub numerator: u32,
    pub denominator: u32,
    pub start_pts: i64,
    pub duration_ticks: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct IndexedTimestamp {
    pub index: usize,
    pub raw_pts: i64,
    pub pts_ns: u64,
}

#[derive(Debug)]
pub struct TimestampIndex {
    metadata: TimestampMetadata,
    frames: Vec<IndexedTimestamp>,
    duration_ns: u64,
    end_pts: i128,
}

impl TimestampIndex {
    pub fn parse(bytes: &[u8], metadata: TimestampMetadata) -> Result<Self, String> {
        if bytes.len() > MAX_INDEX_BYTES {
            return Err("Decoded PTS index exceeds 8 MiB JSON limit".into());
        }
        if metadata.numerator == 0 || metadata.denominator == 0 || metadata.duration_ticks == 0 {
            return Err("PTS index requires positive timebase and known duration".into());
        }
        let data: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| "Invalid PTS index JSON")?;
        let entries = data["frames"]
            .as_array()
            .ok_or("Missing decoded PTS index frames")?;
        if entries.is_empty() || entries.len() > MAX_INDEX_FRAMES {
            return Err("Decoded PTS index must contain 1..=100000 frames".into());
        }
        let first = entries[0]["best_effort_timestamp"]
            .as_i64()
            .ok_or("PTS index requires integer timestamps")?;
        let end_pts = i128::from(metadata.start_pts)
            .checked_add(i128::from(metadata.duration_ticks))
            .ok_or("PTS metadata end overflow")?;
        let duration_ns = relative_ns(end_pts, first, metadata)?;
        if duration_ns == 0 || duration_ns > MAX_DURATION_NS {
            return Err("Normalized PTS duration is empty or exceeds 24 hours".into());
        }
        let mut frames: Vec<IndexedTimestamp> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let raw_pts = entry["best_effort_timestamp"]
                .as_i64()
                .ok_or("PTS index requires integer timestamps")?;
            let pts_ns = relative_ns(i128::from(raw_pts), first, metadata)?;
            if i128::from(raw_pts) >= end_pts
                || frames
                    .last()
                    .is_some_and(|last| raw_pts <= last.raw_pts || pts_ns <= last.pts_ns)
            {
                return Err("PTS index contains duplicate, reversed, out-of-duration or indistinguishable timestamps".into());
            }
            frames.push(IndexedTimestamp {
                index,
                raw_pts,
                pts_ns,
            });
        }
        Ok(Self {
            metadata,
            frames,
            duration_ns,
            end_pts,
        })
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn first_pts(&self) -> i64 {
        self.frames[0].raw_pts
    }
    pub fn duration_ns(&self) -> u64 {
        self.duration_ns
    }
    pub fn frame(&self, index: usize) -> Option<IndexedTimestamp> {
        self.frames.get(index).copied()
    }
    pub fn anchor(&self, target_ns: u64) -> Result<IndexedTimestamp, String> {
        if target_ns >= self.duration_ns {
            return Err("PTS seek target must precede normalized video EOF".into());
        }
        let index = self
            .frames
            .partition_point(|frame| frame.pts_ns <= target_ns)
            .checked_sub(1)
            .ok_or("PTS seek precedes first frame")?;
        Ok(self.frames[index])
    }
    pub fn frame_by_raw(&self, raw_pts: i64) -> Result<IndexedTimestamp, String> {
        let index = self
            .frames
            .binary_search_by_key(&raw_pts, |frame| frame.raw_pts)
            .map_err(|_| "Streaming PTS is absent from the complete source index")?;
        Ok(self.frames[index])
    }
    pub fn absolute_microseconds(&self, pts: i64, ceil: bool) -> Result<String, String> {
        self.format_absolute_micros(i128::from(pts), ceil, 0)
    }
    pub fn probe_interval(&self, index: usize) -> Result<String, String> {
        let frame = self
            .frames
            .get(index)
            .ok_or("PTS anchor index out of bounds")?;
        Ok(format!(
            "{}%{}",
            self.absolute_microseconds(frame.raw_pts, false)?,
            self.format_absolute_micros(self.end_pts, true, 1)?
        ))
    }
    fn format_absolute_micros(&self, pts: i128, ceil: bool, extra: i128) -> Result<String, String> {
        let numerator = pts
            .checked_mul(i128::from(self.metadata.numerator))
            .and_then(|value| value.checked_mul(1_000_000))
            .ok_or("Absolute seek time overflow")?;
        let denominator = i128::from(self.metadata.denominator);
        let floor = numerator.div_euclid(denominator);
        let micros = floor
            .checked_add(i128::from(ceil && numerator.rem_euclid(denominator) != 0))
            .and_then(|value| value.checked_add(extra))
            .ok_or("Absolute seek time overflow")?;
        let absolute = micros.unsigned_abs();
        Ok(format!(
            "{}{}.{:06}",
            if micros < 0 { "-" } else { "" },
            absolute / 1_000_000,
            absolute % 1_000_000
        ))
    }
}

fn relative_ns(pts: i128, origin: i64, metadata: TimestampMetadata) -> Result<u64, String> {
    let delta = u128::try_from(pts - i128::from(origin))
        .map_err(|_| "PTS precedes decoded frame origin")?;
    let nanos = delta
        .checked_mul(u128::from(metadata.numerator))
        .and_then(|value| value.checked_mul(1_000_000_000))
        .ok_or("PTS normalization overflow")?
        / u128::from(metadata.denominator);
    u64::try_from(nanos).map_err(|_| "PTS normalization overflow".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata(start_pts: i64) -> TimestampMetadata {
        TimestampMetadata {
            numerator: 1,
            denominator: 1000,
            start_pts,
            duration_ticks: 1000,
        }
    }
    fn bytes(values: &[i64]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"frames":values.iter().map(|value| serde_json::json!({"best_effort_timestamp":value})).collect::<Vec<_>>()})).unwrap()
    }
    #[test]
    fn real_vfr_intervals_and_nonzero_origin_choose_latest_due_global_frame() {
        let index =
            TimestampIndex::parse(&bytes(&[2000, 2033, 2100, 2150, 2900]), metadata(2000)).unwrap();
        assert_eq!(index.first_pts(), 2000);
        assert_eq!(index.anchor(99_999_999).unwrap().index, 1);
        let anchor = index.anchor(100_000_000).unwrap();
        assert_eq!(
            (anchor.index, anchor.raw_pts, anchor.pts_ns),
            (2, 2100, 100_000_000)
        );
        assert_eq!(
            index.absolute_microseconds(anchor.raw_pts, false).unwrap(),
            "2.100000"
        );
        assert_eq!(
            index.probe_interval(anchor.index).unwrap(),
            "2.100000%3.000001"
        );
        assert_eq!(index.frame_by_raw(2150).unwrap().index, 3);
        assert!(index.frame_by_raw(2151).is_err());
    }
    #[test]
    fn negative_first_pts_and_late_first_frame_are_normalized_without_clamping() {
        let negative = TimestampIndex::parse(&bytes(&[-100, -50, 0, 800]), metadata(-100)).unwrap();
        assert_eq!(negative.anchor(50_000_000).unwrap().raw_pts, -50);
        assert_eq!(
            negative.absolute_microseconds(-50, false).unwrap(),
            "-0.050000"
        );
        let late = TimestampIndex::parse(&bytes(&[100, 150, 900]), metadata(0)).unwrap();
        assert_eq!(late.anchor(0).unwrap().pts_ns, 0);
        assert_eq!(late.duration_ns(), 900_000_000);
        assert_eq!(late.anchor(899_999_999).unwrap().index, 2);
        assert!(late.anchor(900_000_000).is_err());
    }
    #[test]
    fn fractional_timestamp_floor_has_exact_before_at_after_selection() {
        let index = TimestampIndex::parse(
            &bytes(&[0, 1001, 2002]),
            TimestampMetadata {
                numerator: 1,
                denominator: 30000,
                start_pts: 0,
                duration_ticks: 3003,
            },
        )
        .unwrap();
        assert_eq!(index.anchor(33_366_665).unwrap().index, 0);
        assert_eq!(index.anchor(33_366_666).unwrap().index, 1);
        assert_eq!(index.anchor(33_366_667).unwrap().index, 1);
        assert_eq!(index.probe_interval(1).unwrap(), "0.033366%0.100101");
    }
    #[test]
    fn invalid_duplicate_reversed_missing_empty_and_oversized_indexes_fail() {
        for value in [
            bytes(&[]),
            bytes(&[0, 0]),
            bytes(&[2, 1]),
            b"{\"frames\":[{}]}".to_vec(),
            bytes(&[1000]),
        ] {
            assert!(TimestampIndex::parse(&value, metadata(0)).is_err());
        }
        assert!(TimestampIndex::parse(&vec![b' '; MAX_INDEX_BYTES + 1], metadata(0)).is_err());
        assert!(
            TimestampIndex::parse(
                &bytes(&(0..100001).collect::<Vec<_>>()),
                TimestampMetadata {
                    numerator: 1,
                    denominator: 1000000,
                    start_pts: 0,
                    duration_ticks: 200000
                }
            )
            .is_err()
        );
        assert!(
            TimestampIndex::parse(
                &bytes(&[0]),
                TimestampMetadata {
                    numerator: 1,
                    denominator: 0,
                    start_pts: 0,
                    duration_ticks: 1
                }
            )
            .is_err()
        );
        assert!(
            TimestampIndex::parse(
                &bytes(&[0]),
                TimestampMetadata {
                    numerator: u32::MAX,
                    denominator: 1,
                    start_pts: 0,
                    duration_ticks: u64::MAX
                }
            )
            .is_err()
        );
    }
}
