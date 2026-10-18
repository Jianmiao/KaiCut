//! Bounds policy only: these tests never create a window or graphics device.

pub(super) fn clipped_rect(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    parent_width: u32,
    parent_height: u32,
) -> Result<(i32, i32, u32, u32), String> {
    if width > 1920 || height > 1080 {
        return Err("Native preview extent must not exceed 1920x1080".into());
    }
    if parent_width > i32::MAX as u32 || parent_height > i32::MAX as u32 {
        return Err("Parent extent exceeds Win32 coordinates".into());
    }
    let left = i64::from(x).clamp(0, i64::from(parent_width));
    let top = i64::from(y).clamp(0, i64::from(parent_height));
    let right = (i64::from(x) + i64::from(width)).clamp(left, i64::from(parent_width));
    let bottom = (i64::from(y) + i64::from(height)).clamp(top, i64::from(parent_height));
    Ok((
        left as i32,
        top as i32,
        (right - left) as u32,
        (bottom - top) as u32,
    ))
}

pub(super) fn may_show(
    requested: bool,
    parent_visible: bool,
    parent_iconic: bool,
    nonempty: bool,
) -> bool {
    requested && parent_visible && !parent_iconic && nonempty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_rectangle_is_clipped_on_every_parent_edge() {
        assert_eq!(
            clipped_rect(-10, -20, 100, 80, 50, 40).unwrap(),
            (0, 0, 50, 40)
        );
        assert_eq!(
            clipped_rect(90, 60, 50, 40, 100, 80).unwrap(),
            (90, 60, 10, 20)
        );
        assert_eq!(clipped_rect(4, 5, 20, 30, 100, 80).unwrap(), (4, 5, 20, 30));
    }

    #[test]
    fn outside_or_zero_extent_is_empty_and_cannot_wrap() {
        assert_eq!(
            clipped_rect(i32::MAX, i32::MAX, 10, 10, 100, 80).unwrap(),
            (100, 80, 0, 0)
        );
        assert_eq!(
            clipped_rect(i32::MIN, i32::MIN, 10, 10, 100, 80).unwrap(),
            (0, 0, 0, 0)
        );
        assert_eq!(clipped_rect(4, 5, 0, 30, 100, 80).unwrap(), (4, 5, 0, 30));
        assert_eq!(clipped_rect(4, 5, 20, 30, 0, 0).unwrap(), (0, 0, 0, 0));
    }

    #[test]
    fn oversized_input_is_rejected_before_gpu_allocation() {
        assert!(clipped_rect(0, 0, 1921, 1080, 4000, 4000).is_err());
        assert!(clipped_rect(0, 0, 1920, 1081, 4000, 4000).is_err());
        assert!(clipped_rect(0, 0, u32::MAX, 1, 4000, 4000).is_err());
        assert!(clipped_rect(0, 0, 1, 1, u32::MAX, 80).is_err());
        assert_eq!(
            clipped_rect(0, 0, 1920, 1080, 1920, 1080).unwrap(),
            (0, 0, 1920, 1080)
        );
    }

    #[test]
    fn showing_requires_explicit_request_and_visible_restored_parent() {
        assert!(may_show(true, true, false, true));
        assert!(!may_show(false, true, false, true));
        assert!(!may_show(true, false, false, true));
        assert!(!may_show(true, true, true, true));
        assert!(!may_show(true, true, false, false));
    }
}
