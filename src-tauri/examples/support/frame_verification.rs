//! Pure verification of dense GPU readback bytes; this module never creates a
//! device, renders a frame, opens a window or observes physical presentation.

use sha2::{Digest, Sha256};

const MAX_DIMENSION: u32 = 4096;
const MAX_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameVerification {
    pub input_rgb_sha256: String,
    pub expected_rgba_sha256: String,
    pub actual_rgba_sha256: String,
    /// Number of pixels with at least one incorrect RGB or alpha channel.
    pub mismatched_pixels: u64,
    /// Largest absolute difference in any RGB or alpha channel (0..255).
    pub max_channel_error: u8,
    /// Number of pixels whose output alpha is not exactly 255.
    pub alpha_errors: u64,
    pub exact: bool,
}

fn dimensions(width: u32, height: u32) -> Result<(usize, usize, usize), String> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err("Frame dimensions must each be within 1..=4096".into());
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or("Frame pixel count overflow")?;
    if pixels > MAX_PIXELS {
        return Err("Frame exceeds the 16 Mi-pixel validation limit".into());
    }
    let rgb = pixels.checked_mul(3).ok_or("RGB frame length overflow")?;
    let rgba = pixels.checked_mul(4).ok_or("RGBA frame length overflow")?;
    Ok((pixels, rgb, rgba))
}

/// A deterministic, directional calibration image with separate channel
/// gradients, offset grid markers and asymmetric corners. Use an odd size such
/// as 161x91 when testing staging row pitch. Values are raw UNORM channel bytes,
/// not a color-managed image or a claim about display appearance.
pub fn calibration_rgb(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let (_, length, _) = dimensions(width, height)?;
    let mut rgb = Vec::with_capacity(length);
    for y in 0..height {
        for x in 0..width {
            let mut pixel = [
                ((x * 17 + y * 3 + 11) % 256) as u8,
                ((x * 5 + y * 29 + 67) % 256) as u8,
                ((x * 37 + y * 7 + 151) % 256) as u8,
            ];
            if x % 17 == 3 {
                pixel[0] ^= 0x81;
            }
            if y % 13 == 5 {
                pixel[1] ^= 0x42;
            }
            if (x / 17 + y / 13) % 2 == 1 {
                pixel[2] ^= 0x24;
            }
            rgb.extend_from_slice(&pixel);
        }
    }
    Ok(rgb)
}

/// Compare tightly packed top-to-bottom RGB24 input with tightly packed RGBA8
/// readback. The GPU owner must remove staging row padding first. Incorrect
/// pixels produce a complete `exact=false` evidence record, while invalid
/// dimensions or byte lengths return an error before allocation/comparison.
pub fn verify_readback(
    rgb: &[u8],
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<FrameVerification, String> {
    let (_, rgb_length, rgba_length) = dimensions(width, height)?;
    if rgb.len() != rgb_length || rgba.len() != rgba_length {
        return Err(format!(
            "Dense frame length mismatch: expected RGB {rgb_length} and RGBA {rgba_length} bytes, received {} and {}",
            rgb.len(),
            rgba.len()
        ));
    }
    let mut expected_hash = Sha256::new();
    let mut mismatched_pixels = 0;
    let mut max_channel_error = 0;
    let mut alpha_errors = 0;
    for (source, actual) in rgb.chunks_exact(3).zip(rgba.chunks_exact(4)) {
        let expected = [source[0], source[1], source[2], 255];
        expected_hash.update(expected);
        let mut mismatch = false;
        for (wanted, received) in expected.into_iter().zip(actual.iter().copied()) {
            let difference = wanted.abs_diff(received);
            mismatch |= difference != 0;
            max_channel_error = max_channel_error.max(difference);
        }
        mismatched_pixels += u64::from(mismatch);
        alpha_errors += u64::from(actual[3] != 255);
    }
    Ok(FrameVerification {
        input_rgb_sha256: format!("{:x}", Sha256::digest(rgb)),
        expected_rgba_sha256: format!("{:x}", expected_hash.finalize()),
        actual_rgba_sha256: format!("{:x}", Sha256::digest(rgba)),
        mismatched_pixels,
        max_channel_error,
        alpha_errors,
        exact: mismatched_pixels == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(rgb: &[u8]) -> Vec<u8> {
        rgb.chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect()
    }

    #[test]
    fn verifies_literal_rgba_pixels_and_reports_independent_hashes() {
        let rgb = [1, 2, 3, 10, 20, 30];
        let readback = [1, 2, 3, 255, 10, 20, 30, 255];
        let result = verify_readback(&rgb, &readback, 2, 1).unwrap();
        assert!(result.exact);
        assert_eq!(result.mismatched_pixels, 0);
        assert_eq!(result.max_channel_error, 0);
        assert_eq!(result.alpha_errors, 0);
        assert_eq!(result.expected_rgba_sha256, result.actual_rgba_sha256);
        assert_ne!(result.input_rgb_sha256, result.actual_rgba_sha256);
    }

    #[test]
    fn swapped_red_and_blue_channels_are_not_accepted_as_equal() {
        let result = verify_readback(
            &[1, 20, 90, 100, 80, 5],
            &[90, 20, 1, 255, 5, 80, 100, 255],
            2,
            1,
        )
        .unwrap();
        assert!(!result.exact);
        assert_eq!(result.mismatched_pixels, 2);
        assert_eq!(result.max_channel_error, 95);
        assert_eq!(result.alpha_errors, 0);
        assert_ne!(result.expected_rgba_sha256, result.actual_rgba_sha256);
    }

    #[test]
    fn pixel_mismatch_counts_each_pixel_once_and_includes_alpha() {
        let result = verify_readback(
            &[10, 20, 30, 40, 50, 60],
            &[0, 0, 0, 0, 40, 50, 60, 254],
            2,
            1,
        )
        .unwrap();
        assert!(!result.exact);
        assert_eq!(result.mismatched_pixels, 2);
        assert_eq!(result.alpha_errors, 2);
        assert_eq!(result.max_channel_error, 255);
    }

    #[test]
    fn odd_sized_calibration_exposes_vertical_flip_and_shifted_rows() {
        let width = 161;
        let height = 91;
        let rgb = calibration_rgb(width, height).unwrap();
        assert_eq!(rgb.len(), 161 * 91 * 3);
        let dense = rgba(&rgb);
        assert!(verify_readback(&rgb, &dense, width, height).unwrap().exact);
        let flipped: Vec<u8> = dense
            .chunks_exact(161 * 4)
            .rev()
            .flatten()
            .copied()
            .collect();
        let flipped_result = verify_readback(&rgb, &flipped, width, height).unwrap();
        assert!(!flipped_result.exact);
        assert!(flipped_result.mismatched_pixels >= 161 * 2);
        let mut shifted = dense.clone();
        shifted.rotate_left(4);
        assert!(
            !verify_readback(&rgb, &shifted, width, height)
                .unwrap()
                .exact
        );
        // D3D staging padding must be stripped by the renderer, never silently
        // interpreted as an extra pixel or row by this dense-format verifier.
        let padded: Vec<u8> = dense
            .chunks_exact(161 * 4)
            .flat_map(|row| row.iter().copied().chain([0; 12]))
            .collect();
        assert!(verify_readback(&rgb, &padded, width, height).is_err());
    }

    #[test]
    fn calibration_has_distinct_channels_horizontal_vertical_and_corner_direction() {
        let rgb = calibration_rgb(161, 91).unwrap();
        let pixel = |x: usize, y: usize| &rgb[(y * 161 + x) * 3..(y * 161 + x) * 3 + 3];
        assert_ne!(pixel(0, 0), pixel(1, 0));
        assert_ne!(pixel(0, 0), pixel(0, 1));
        assert_ne!(pixel(0, 0), pixel(160, 90));
        assert_ne!(pixel(160, 0), pixel(0, 90));
        assert!(
            rgb.chunks_exact(3)
                .any(|value| value[0] != value[1] && value[1] != value[2] && value[0] != value[2])
        );
    }

    #[test]
    fn invalid_dimensions_and_buffer_lengths_fail_before_allocating() {
        for (width, height) in [(0, 1), (1, 0), (u32::MAX, u32::MAX), (4097, 1), (1, 4097)] {
            assert!(calibration_rgb(width, height).is_err());
            assert!(verify_readback(&[], &[], width, height).is_err());
        }
        assert!(verify_readback(&[1, 2], &[1, 2, 3, 255], 1, 1).is_err());
        assert!(verify_readback(&[1, 2, 3], &[1, 2, 3], 1, 1).is_err());
        assert!(verify_readback(&[1, 2, 3, 4], &[1, 2, 3, 255], 1, 1).is_err());
        assert!(verify_readback(&[], &[], 4096, 4096).is_err());
    }
}
