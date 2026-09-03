use serde::{Deserialize, Serialize};

/// Display resolution and scaling information
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

impl Default for DisplayInfo {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        }
    }
}

/// Screen edges for auto-forwarding transitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Scale coordinates from source display dimensions & DPI to target display dimensions & DPI.
/// Handles zero dimensions, negative inputs, NaN/Inf scale factors, and boundary clamping safely.
pub fn scale_coordinates(x: i32, y: i32, source: &DisplayInfo, target: &DisplayInfo) -> (i32, i32) {
    if source.width == 0 || source.height == 0 || target.width == 0 || target.height == 0 {
        return (x, y);
    }

    // Convert to normalized ratio (0.0 to 1.0) on source display
    let norm_x = (x as f64) / (source.width as f64);
    let norm_y = (y as f64) / (source.height as f64);

    // Sanitize scale factors against 0.0, NaN, or Infinity
    let src_scale = if source.scale_factor.is_finite() && source.scale_factor > 0.0 {
        source.scale_factor
    } else {
        1.0
    };
    let tgt_scale = if target.scale_factor.is_finite() && target.scale_factor > 0.0 {
        target.scale_factor
    } else {
        1.0
    };

    let dpi_ratio = tgt_scale / src_scale;

    let target_x = (norm_x * (target.width as f64) * dpi_ratio).round() as i32;
    let target_y = (norm_y * (target.height as f64) * dpi_ratio).round() as i32;

    // Clamp within target screen bounds
    let max_x = (target.width as i32 - 1).max(0);
    let max_y = (target.height as i32 - 1).max(0);

    let clamped_x = target_x.clamp(0, max_x);
    let clamped_y = target_y.clamp(0, max_y);

    (clamped_x, clamped_y)
}

/// Detect if cursor position touches a screen border within `threshold` pixels
pub fn detect_edge_transition(
    x: i32,
    y: i32,
    display: &DisplayInfo,
    threshold: i32,
) -> Option<ScreenEdge> {
    if display.width == 0 || display.height == 0 {
        return None;
    }

    let threshold = threshold.max(0);

    if x <= threshold {
        Some(ScreenEdge::Left)
    } else if x >= (display.width as i32 - 1 - threshold) {
        Some(ScreenEdge::Right)
    } else if y <= threshold {
        Some(ScreenEdge::Top)
    } else if y >= (display.height as i32 - 1 - threshold) {
        Some(ScreenEdge::Bottom)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_coordinates_identical() {
        let display = DisplayInfo {
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        };
        let (scaled_x, scaled_y) = scale_coordinates(960, 540, &display, &display);
        assert_eq!((scaled_x, scaled_y), (960, 540));
    }

    #[test]
    fn test_scale_coordinates_4k_to_1080p() {
        let source = DisplayInfo {
            width: 3840,
            height: 2160,
            scale_factor: 1.0,
        };
        let target = DisplayInfo {
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        };
        let (scaled_x, scaled_y) = scale_coordinates(1920, 1080, &source, &target);
        assert_eq!((scaled_x, scaled_y), (960, 540));
    }

    #[test]
    fn test_scale_coordinates_zero_dimensions_graceful_fallback() {
        let source = DisplayInfo {
            width: 0,
            height: 0,
            scale_factor: 1.0,
        };
        let target = DisplayInfo::default();
        let (scaled_x, scaled_y) = scale_coordinates(100, 200, &source, &target);
        assert_eq!((scaled_x, scaled_y), (100, 200));

        let target_zero = DisplayInfo {
            width: 0,
            height: 1080,
            scale_factor: 1.0,
        };
        let (scaled_x2, scaled_y2) =
            scale_coordinates(100, 200, &DisplayInfo::default(), &target_zero);
        assert_eq!((scaled_x2, scaled_y2), (100, 200));
    }

    #[test]
    fn test_scale_coordinates_negative_and_overflow_inputs() {
        let source = DisplayInfo::default();
        let target = DisplayInfo::default();

        // Negative coordinate input clamped to 0
        let (clamped_x, clamped_y) = scale_coordinates(-50, -100, &source, &target);
        assert_eq!((clamped_x, clamped_y), (0, 0));

        // Excessively large coordinate input clamped to screen max
        let (clamped_x2, clamped_y2) = scale_coordinates(99999, 99999, &source, &target);
        assert_eq!((clamped_x2, clamped_y2), (1919, 1079));
    }

    #[test]
    fn test_scale_coordinates_nan_and_inf_scale_factors() {
        let source = DisplayInfo {
            width: 1920,
            height: 1080,
            scale_factor: f64::NAN,
        };
        let target = DisplayInfo {
            width: 1920,
            height: 1080,
            scale_factor: f64::INFINITY,
        };
        // Should fallback to 1.0 ratio and not crash or produce NaN
        let (scaled_x, scaled_y) = scale_coordinates(500, 500, &source, &target);
        assert_eq!((scaled_x, scaled_y), (500, 500));
    }

    #[test]
    fn test_detect_edge_transition_left() {
        let display = DisplayInfo::default();
        assert_eq!(
            detect_edge_transition(2, 500, &display, 5),
            Some(ScreenEdge::Left)
        );
    }

    #[test]
    fn test_detect_edge_transition_right() {
        let display = DisplayInfo::default();
        assert_eq!(
            detect_edge_transition(1918, 500, &display, 5),
            Some(ScreenEdge::Right)
        );
    }

    #[test]
    fn test_detect_edge_transition_center() {
        let display = DisplayInfo::default();
        assert_eq!(detect_edge_transition(500, 500, &display, 5), None);
    }

    #[test]
    fn test_detect_edge_transition_zero_display() {
        let display = DisplayInfo {
            width: 0,
            height: 0,
            scale_factor: 1.0,
        };
        assert_eq!(detect_edge_transition(0, 0, &display, 5), None);
    }
}
