//! Integer aspect-fit geometry and a diagnostic-only CPU pixel oracle.
//! The production frame path performs scaling in the D3D pixel shader.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FitRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn fit_rect(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<FitRect, String> {
    validate_size(source_width, source_height)?;
    validate_size(target_width, target_height)?;
    // Truncate the non-limiting extent; odd spare pixels stay on the right or
    // bottom. Keep one pixel visible for an extremely narrow source/target.
    let (width, height) = if source_width * target_height >= source_height * target_width {
        (
            target_width,
            (source_height * target_width / source_width).max(1),
        )
    } else {
        (
            (source_width * target_height / source_height).max(1),
            target_height,
        )
    };
    Ok(FitRect {
        x: (target_width - width) / 2,
        y: (target_height - height) / 2,
        width,
        height,
    })
}

/// CPU-only diagnostic oracle. Never call this from normal frame presentation;
/// rendering uses the same integer pixel rule directly in a cached GPU shader.
pub fn nearest_letterbox_rgba(
    rgb: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, String> {
    let fit = fit_rect(source_width, source_height, target_width, target_height)?;
    if rgb.len() != source_width as usize * source_height as usize * 3 {
        return Err("RGB input length does not match the declared source dimensions".into());
    }
    let mut rgba = vec![0; target_width as usize * target_height as usize * 4];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    for y in 0..fit.height {
        let source_y = y * source_height / fit.height;
        for x in 0..fit.width {
            let source_x = x * source_width / fit.width;
            let source_index = (source_y * source_width + source_x) as usize * 3;
            let target_index = ((y + fit.y) * target_width + x + fit.x) as usize * 4;
            rgba[target_index..target_index + 3]
                .copy_from_slice(&rgb[source_index..source_index + 3]);
        }
    }
    Ok(rgba)
}

fn validate_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > 1920 || height > 1080 {
        return Err("Source and target dimensions must be within 1..1920 by 1..1080".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_and_portrait_are_centered_without_cropping() {
        assert_eq!(
            fit_rect(160, 90, 500, 300).unwrap(),
            FitRect {
                x: 0,
                y: 9,
                width: 500,
                height: 281
            }
        );
        assert_eq!(
            fit_rect(90, 160, 500, 300).unwrap(),
            FitRect {
                x: 166,
                y: 0,
                width: 168,
                height: 300
            }
        );
        assert_eq!(
            fit_rect(160, 90, 320, 180).unwrap(),
            FitRect {
                x: 0,
                y: 0,
                width: 320,
                height: 180
            }
        );
    }

    #[test]
    fn odd_and_extreme_aspects_keep_at_least_one_visible_pixel() {
        assert_eq!(
            fit_rect(1920, 1080, 161, 91).unwrap(),
            FitRect {
                x: 0,
                y: 0,
                width: 161,
                height: 90
            }
        );
        assert_eq!(
            fit_rect(1920, 1, 1, 1080).unwrap(),
            FitRect {
                x: 0,
                y: 539,
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            fit_rect(1, 1080, 1920, 1).unwrap(),
            FitRect {
                x: 959,
                y: 0,
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn geometry_rejects_zero_and_oversized_frames_before_allocation() {
        for (width, height) in [(0, 1), (1, 0), (1921, 1), (1, 1081), (u32::MAX, u32::MAX)] {
            assert!(fit_rect(width, height, 4, 4).is_err());
            assert!(fit_rect(4, 4, width, height).is_err());
        }
        assert!(nearest_letterbox_rgba(&[1, 2], 1, 1, 2, 2).is_err());
        assert!(nearest_letterbox_rgba(&[1, 2, 3, 4], 1, 1, 2, 2).is_err());
    }

    #[test]
    fn diagnostic_identity_preserves_channel_order_and_orientation() {
        assert_eq!(
            nearest_letterbox_rgba(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], 2, 2, 2, 2).unwrap(),
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn diagnostic_black_bars_and_integer_samples_match_known_pixels() {
        let source: Vec<u8> = (1..=8).flat_map(|value| [value, 0, 0]).collect();
        let rgba = nearest_letterbox_rgba(&source, 4, 2, 5, 5).unwrap();
        let red: Vec<_> = rgba.chunks_exact(4).map(|pixel| pixel[0]).collect();
        assert_eq!(
            red,
            vec![
                0, 0, 0, 0, 0, 1, 1, 2, 3, 4, 5, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert!(
            rgba.chunks_exact(4)
                .all(|pixel| pixel[1] == 0 && pixel[2] == 0 && pixel[3] == 255)
        );
    }

    #[test]
    fn diagnostic_downsample_uses_floor_not_center_or_rounding() {
        let source: Vec<u8> = (1..=9).flat_map(|value| [0, value, 0]).collect();
        let rgba = nearest_letterbox_rgba(&source, 3, 3, 2, 2).unwrap();
        assert_eq!(
            rgba,
            vec![0, 1, 0, 255, 0, 2, 0, 255, 0, 4, 0, 255, 0, 5, 0, 255]
        );
    }

    #[test]
    fn fitted_rect_and_last_samples_stay_in_bounds() {
        for (sw, sh) in [(1, 1), (1920, 1), (1, 1080), (161, 91), (1920, 1080)] {
            for (tw, th) in [(1, 1), (3, 5), (500, 300), (1919, 1079), (1920, 1080)] {
                let fit = fit_rect(sw, sh, tw, th).unwrap();
                assert!(fit.width > 0 && fit.height > 0);
                assert!(fit.x + fit.width <= tw && fit.y + fit.height <= th);
                assert!((fit.width - 1) * sw / fit.width < sw);
                assert!((fit.height - 1) * sh / fit.height < sh);
                assert!((tw - fit.width).abs_diff(2 * fit.x) <= 1);
                assert!((th - fit.height).abs_diff(2 * fit.y) <= 1);
            }
        }
    }
}
