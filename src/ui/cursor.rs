//! Platform cursor adaptation for the PDF viewport.
//!
//! The closed-hand artwork below is original LunaPDF geometry distributed under the
//! package's `AGPL-3.0-only` license. It is rendered into embedded RGBA data at
//! runtime, so Windows builds do not depend on external cursor files.

use eframe::egui::{self, CursorIcon};

#[cfg(any(windows, test))]
use std::sync::{Arc, OnceLock};

#[cfg(any(windows, test))]
const CURSOR_BASE_SIZE: f32 = 32.0;
#[cfg(any(windows, test))]
const CURSOR_SIZES: [u16; 5] = [32, 40, 48, 56, 64];
#[cfg(any(windows, test))]
const CURSOR_SUPERSAMPLING: usize = 4;
#[cfg(any(windows, test))]
const CURSOR_OUTLINE_WIDTH: f32 = 1.1;
#[cfg(any(windows, test))]
const CURSOR_DETAIL_WIDTH: f32 = 0.7;

#[cfg(any(windows, test))]
static CLOSED_HAND_IMAGES: [OnceLock<Arc<[u8]>>; CURSOR_SIZES.len()] =
    [const { OnceLock::new() }; CURSOR_SIZES.len()];

#[cfg(any(windows, test))]
const CLOSED_HAND_OUTLINE: [[f32; 2]; 24] = [
    [8.0, 29.0],
    [6.0, 26.0],
    [5.0, 21.0],
    [5.0, 16.0],
    [6.0, 13.0],
    [8.0, 12.0],
    [8.0, 10.0],
    [9.0, 8.0],
    [11.0, 7.0],
    [13.0, 8.0],
    [14.0, 10.0],
    [15.0, 8.0],
    [17.0, 7.0],
    [19.0, 8.0],
    [20.0, 10.0],
    [21.0, 9.0],
    [23.0, 9.0],
    [25.0, 11.0],
    [25.0, 13.0],
    [27.0, 14.0],
    [29.0, 17.0],
    [29.0, 22.0],
    [27.0, 26.0],
    [24.0, 29.0],
];

#[cfg(any(windows, test))]
const CLOSED_HAND_DETAILS: [[[f32; 2]; 2]; 5] = [
    [[8.0, 13.0], [25.0, 13.0]],
    [[9.0, 17.0], [23.0, 20.0]],
    [[23.0, 20.0], [19.0, 25.0]],
    [[14.0, 10.0], [14.0, 14.0]],
    [[20.0, 10.0], [20.0, 14.0]],
];

/// Applies the logical PDF cursor and substitutes Windows artwork only while
/// an established pan uses `Grabbing`.
pub(crate) fn set_pdf_cursor(context: &egui::Context, icon: CursorIcon) {
    context.set_cursor_icon(icon);
    #[cfg(windows)]
    context.set_cursor_image(match icon {
        CursorIcon::Grabbing => Some(custom_closed_hand_cursor(context.pixels_per_point())),
        _ => None,
    });
}

#[cfg(any(windows, test))]
fn custom_closed_hand_cursor(pixels_per_point: f32) -> egui::CustomCursorImage {
    let size_index = cursor_size_index(pixels_per_point);
    let size = CURSOR_SIZES[size_index];
    let rgba = CLOSED_HAND_IMAGES[size_index]
        .get_or_init(|| render_closed_hand_cursor(size))
        .clone();
    egui::CustomCursorImage {
        rgba,
        size: [size, size],
        hotspot: [size / 2, size / 2],
    }
}

#[cfg(any(windows, test))]
fn cursor_size_index(pixels_per_point: f32) -> usize {
    let requested_size = (CURSOR_BASE_SIZE * pixels_per_point.clamp(1.0, 2.0)).round() as u16;
    let mut closest_index = 0;
    let mut closest_distance = u16::MAX;
    for (index, size) in CURSOR_SIZES.iter().enumerate() {
        let distance = size.abs_diff(requested_size);
        if distance < closest_distance {
            closest_index = index;
            closest_distance = distance;
        }
    }
    closest_index
}

#[cfg(any(windows, test))]
fn render_closed_hand_cursor(size: u16) -> Arc<[u8]> {
    let mut rgba = vec![0; usize::from(size) * usize::from(size) * 4];
    let sample_count = CURSOR_SUPERSAMPLING * CURSOR_SUPERSAMPLING;

    for pixel_y in 0..usize::from(size) {
        for pixel_x in 0..usize::from(size) {
            let mut covered_samples = 0;
            let mut white_samples = 0;
            for sample_y in 0..CURSOR_SUPERSAMPLING {
                for sample_x in 0..CURSOR_SUPERSAMPLING {
                    let point = cursor_sample_point(
                        pixel_x,
                        pixel_y,
                        sample_x,
                        sample_y,
                        usize::from(size),
                    );
                    if !point_in_polygon(point, &CLOSED_HAND_OUTLINE) {
                        continue;
                    }
                    covered_samples += 1;
                    let on_outline = distance_to_polygon_edges(point, &CLOSED_HAND_OUTLINE)
                        <= CURSOR_OUTLINE_WIDTH;
                    let on_detail = CLOSED_HAND_DETAILS.iter().any(|segment| {
                        distance_to_segment(point, segment[0], segment[1]) <= CURSOR_DETAIL_WIDTH
                    });
                    if !on_outline && !on_detail {
                        white_samples += 1;
                    }
                }
            }

            if covered_samples == 0 {
                continue;
            }
            let index = (pixel_y * usize::from(size) + pixel_x) * 4;
            let channel = (white_samples * 255 / covered_samples) as u8;
            rgba[index] = channel;
            rgba[index + 1] = channel;
            rgba[index + 2] = channel;
            rgba[index + 3] = (covered_samples * 255 / sample_count) as u8;
        }
    }
    Arc::from(rgba)
}

#[cfg(any(windows, test))]
fn cursor_sample_point(
    pixel_x: usize,
    pixel_y: usize,
    sample_x: usize,
    sample_y: usize,
    size: usize,
) -> [f32; 2] {
    let sample_offset = |sample| (sample as f32 + 0.5) / CURSOR_SUPERSAMPLING as f32;
    [
        (pixel_x as f32 + sample_offset(sample_x)) * CURSOR_BASE_SIZE / size as f32,
        (pixel_y as f32 + sample_offset(sample_y)) * CURSOR_BASE_SIZE / size as f32,
    ]
}

#[cfg(any(windows, test))]
fn point_in_polygon(point: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        // A horizontal ray changes parity only when an edge straddles the
        // sample's Y coordinate and crosses to its right.
        let straddles_y = (current[1] > point[1]) != (previous[1] > point[1]);
        if straddles_y {
            let crosses_right = point[0]
                < (previous[0] - current[0]) * (point[1] - current[1]) / (previous[1] - current[1])
                    + current[0];
            if crosses_right {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

#[cfg(any(windows, test))]
fn distance_to_polygon_edges(point: [f32; 2], polygon: &[[f32; 2]]) -> f32 {
    let mut distance = f32::INFINITY;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        distance = distance.min(distance_to_segment(point, previous, current));
        previous = current;
    }
    distance
}

#[cfg(any(windows, test))]
fn distance_to_segment(point: [f32; 2], start: [f32; 2], end: [f32; 2]) -> f32 {
    let segment = [end[0] - start[0], end[1] - start[1]];
    let from_start = [point[0] - start[0], point[1] - start[1]];
    let length_squared = segment[0] * segment[0] + segment[1] * segment[1];
    let projection = ((from_start[0] * segment[0] + from_start[1] * segment[1]) / length_squared)
        .clamp(0.0, 1.0);
    let nearest = [
        start[0] + segment[0] * projection,
        start[1] + segment[1] * projection,
    ];
    let delta = [point[0] - nearest[0], point[1] - nearest[1]];
    (delta[0] * delta[0] + delta[1] * delta[1]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_sizes_cover_required_windows_dpi_scales() {
        assert_eq!(CURSOR_SIZES[cursor_size_index(1.0)], 32);
        assert_eq!(CURSOR_SIZES[cursor_size_index(1.25)], 40);
        assert_eq!(CURSOR_SIZES[cursor_size_index(1.5)], 48);
        assert_eq!(CURSOR_SIZES[cursor_size_index(2.0)], 64);
    }

    #[test]
    fn closed_hand_is_an_embedded_rgba_image_with_a_center_hotspot() {
        let closed = custom_closed_hand_cursor(1.25);

        assert_eq!(closed.size, [40, 40]);
        assert_eq!(closed.hotspot, [20, 20]);
        assert_eq!(closed.rgba.len(), 40 * 40 * 4);
        assert!(closed.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(closed.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }
}
