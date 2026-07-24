use std::ops::RangeInclusive;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PagePoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl PagePoint {
    pub(crate) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageQuad {
    pub(crate) upper_left: PagePoint,
    pub(crate) upper_right: PagePoint,
    pub(crate) lower_left: PagePoint,
    pub(crate) lower_right: PagePoint,
}

impl PageQuad {
    pub(crate) fn bounds(self) -> (f32, f32, f32, f32) {
        let xs = [
            self.upper_left.x,
            self.upper_right.x,
            self.lower_left.x,
            self.lower_right.x,
        ];
        let ys = [
            self.upper_left.y,
            self.upper_right.y,
            self.lower_left.y,
            self.lower_right.y,
        ];
        let x0 = xs.into_iter().fold(f32::INFINITY, f32::min);
        let x1 = xs.into_iter().fold(f32::NEG_INFINITY, f32::max);
        let y0 = ys.into_iter().fold(f32::INFINITY, f32::min);
        let y1 = ys.into_iter().fold(f32::NEG_INFINITY, f32::max);
        (x0, y0, x1, y1)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlyphSnapshot {
    pub(crate) character: char,
    pub(crate) quad: PageQuad,
    pub(crate) line_index: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TextSnapshotRequest {
    pub(crate) page_index: usize,
    pub(crate) expected_revision: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct TextPageSnapshot {
    pub(crate) page_index: usize,
    pub(crate) revision: u64,
    pub(crate) glyphs: Vec<GlyphSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectionSnapshot {
    pub(crate) page_index: usize,
    pub(crate) generation: u64,
    pub(crate) text: String,
    pub(crate) display_quads: Vec<PageQuad>,
    pub(crate) quads: Vec<PageQuad>,
    pub(crate) extraction_time: Duration,
}

/// Creates the logical copy string independently from the display quads.
///
/// MuPDF 0.8 exposes standard selection quads but not its range-copy string.
/// Keeping this ordered glyph selection separate ensures future Typst-only
/// display correction cannot silently alter copy order.
pub(crate) fn selected_text(glyphs: &[GlyphSnapshot], start: PagePoint, end: PagePoint) -> String {
    let Some(selected_glyphs) = selected_glyphs(glyphs, start, end) else {
        return String::new();
    };

    let mut text = String::new();
    let mut previous_line = selected_glyphs[0].line_index;
    for glyph in selected_glyphs {
        if glyph.line_index != previous_line {
            text.push('\n');
            previous_line = glyph.line_index;
        }
        text.push(glyph.character);
    }
    text
}

/// Returns display and annotation Quads for the same inclusive glyph range as copy text.
pub(crate) fn selected_quads(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Vec<PageQuad> {
    let Some(selected_glyphs) = selected_glyphs(glyphs, start, end) else {
        return Vec::new();
    };
    selected_glyphs.iter().map(|glyph| glyph.quad).collect()
}

/// Returns the geometry used only to paint the selected glyph range.
pub(crate) fn selected_display_quads(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Vec<PageQuad> {
    let Some(selected_glyphs) = selected_glyphs(glyphs, start, end) else {
        return Vec::new();
    };
    let mut bands = Vec::new();
    let mut line_start = 0;
    for index in 1..=selected_glyphs.len() {
        let line_ended = index == selected_glyphs.len()
            || selected_glyphs[index].line_index != selected_glyphs[line_start].line_index;
        if line_ended {
            bands.push(merge_line_band(&selected_glyphs[line_start..index]));
            line_start = index;
        }
    }
    bands
}

fn merge_line_band(glyphs: &[GlyphSnapshot]) -> PageQuad {
    let first = &glyphs[0];
    if glyphs.len() == 1 {
        return first.quad;
    }

    let center = |glyph: &GlyphSnapshot| {
        let (x0, y0, x1, y1) = glyph.quad.bounds();
        PagePoint::new((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    };
    let horizontal_span = glyphs.iter().map(|glyph| center(glyph).x).fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), x| (minimum.min(x), maximum.max(x)),
    );
    let vertical_span = glyphs.iter().map(|glyph| center(glyph).y).fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), y| (minimum.min(y), maximum.max(y)),
    );
    let advances_horizontally =
        horizontal_span.1 - horizontal_span.0 >= vertical_span.1 - vertical_span.0;

    if advances_horizontally {
        let left = glyphs
            .iter()
            .min_by(|left, right| center(left).x.total_cmp(&center(right).x))
            .expect("a line band always has at least one glyph");
        let right = glyphs
            .iter()
            .max_by(|left, right| center(left).x.total_cmp(&center(right).x))
            .expect("a line band always has at least one glyph");
        // Reusing the outer glyph edges keeps tilted and rotated text geometry;
        // an axis-aligned bounds union would overpaint Typst text lines.
        PageQuad {
            upper_left: left.quad.upper_left,
            upper_right: right.quad.upper_right,
            lower_left: left.quad.lower_left,
            lower_right: right.quad.lower_right,
        }
    } else {
        let top = glyphs
            .iter()
            .min_by(|top, bottom| center(top).y.total_cmp(&center(bottom).y))
            .expect("a line band always has at least one glyph");
        let bottom = glyphs
            .iter()
            .max_by(|top, bottom| center(top).y.total_cmp(&center(bottom).y))
            .expect("a line band always has at least one glyph");
        PageQuad {
            upper_left: top.quad.upper_left,
            upper_right: top.quad.upper_right,
            lower_left: bottom.quad.lower_left,
            lower_right: bottom.quad.lower_right,
        }
    }
}

/// Borrows the canonical inclusive glyph range without allocating during drag preview.
pub(crate) fn selected_glyphs(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Option<&[GlyphSnapshot]> {
    let range = selected_glyph_range(glyphs, start, end)?;
    Some(&glyphs[range])
}

/// Maps a pointer to a stable character center in the Rust-owned text snapshot.
pub(crate) fn snap_to_glyph(glyphs: &[GlyphSnapshot], point: PagePoint) -> Option<PagePoint> {
    let glyph = glyphs.get(nearest_glyph_index(glyphs, point)?)?;
    let (x0, y0, x1, y1) = glyph.quad.bounds();
    Some(PagePoint::new((x0 + x1) / 2.0, (y0 + y1) / 2.0))
}

/// Returns the logical glyph range selected by two already page-local points.
pub(crate) fn selected_glyph_range(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Option<RangeInclusive<usize>> {
    let start_index = nearest_glyph_index(glyphs, start)?;
    let end_index = nearest_glyph_index(glyphs, end)?;
    Some(start_index.min(end_index)..=start_index.max(end_index))
}

fn nearest_glyph_index(glyphs: &[GlyphSnapshot], point: PagePoint) -> Option<usize> {
    glyphs
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            let left_distance = squared_distance_to_quad_bounds(left.quad, point);
            let right_distance = squared_distance_to_quad_bounds(right.quad, point);
            left_distance.total_cmp(&right_distance)
        })
        .map(|(index, _)| index)
}

fn squared_distance_to_quad_bounds(quad: PageQuad, point: PagePoint) -> f32 {
    let (x0, y0, x1, y1) = quad.bounds();
    let closest_x = point.x.clamp(x0, x1);
    let closest_y = point.y.clamp(y0, y1);
    let delta_x = point.x - closest_x;
    let delta_y = point.y - closest_y;
    delta_x * delta_x + delta_y * delta_y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(character: char, x: f32, line_index: usize) -> GlyphSnapshot {
        GlyphSnapshot {
            character,
            quad: PageQuad {
                upper_left: PagePoint::new(x, line_index as f32 * 20.0),
                upper_right: PagePoint::new(x + 8.0, line_index as f32 * 20.0),
                lower_left: PagePoint::new(x, line_index as f32 * 20.0 + 10.0),
                lower_right: PagePoint::new(x + 8.0, line_index as f32 * 20.0 + 10.0),
            },
            line_index,
        }
    }

    #[test]
    fn logical_selection_uses_document_order_for_reverse_drag() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 10.0, 0), glyph('C', 20.0, 0)];

        let text = selected_text(&glyphs, PagePoint::new(25.0, 5.0), PagePoint::new(1.0, 5.0));

        assert_eq!(text, "ABC");
    }

    #[test]
    fn logical_selection_preserves_line_boundaries() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 0.0, 1)];

        let text = selected_text(&glyphs, PagePoint::new(1.0, 5.0), PagePoint::new(1.0, 25.0));

        assert_eq!(text, "A\nB");
    }

    #[test]
    fn pointer_hit_mapping_snaps_to_the_nearest_glyph_center() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 20.0, 0)];

        let snapped = snap_to_glyph(&glyphs, PagePoint::new(27.0, 4.0)).unwrap();

        assert_eq!(snapped, PagePoint::new(24.0, 5.0));
        assert_eq!(
            selected_glyph_range(&glyphs, snapped, PagePoint::new(1.0, 5.0)),
            Some(0..=1)
        );
    }

    #[test]
    fn selection_quads_include_both_endpoint_glyphs() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 10.0, 0), glyph('C', 20.0, 0)];
        let start = PagePoint::new(1.0, 5.0);
        let end = PagePoint::new(27.0, 5.0);

        let forward = selected_quads(&glyphs, start, end);
        let reverse = selected_quads(&glyphs, end, start);

        assert_eq!(
            forward,
            glyphs.iter().map(|glyph| glyph.quad).collect::<Vec<_>>()
        );
        assert_eq!(reverse, forward);
    }

    #[test]
    fn single_glyph_selection_preserves_its_original_quad() {
        let mut tilted = glyph('A', 10.0, 0);
        tilted.quad.upper_right.y = 2.0;
        tilted.quad.lower_right.y = 12.0;
        let point = PagePoint::new(14.0, 6.0);

        let quads = selected_quads(std::slice::from_ref(&tilted), point, point);

        assert_eq!(quads, vec![tilted.quad]);
    }

    #[test]
    fn selection_outside_the_last_glyph_uses_the_same_endpoint_for_text_and_quads() {
        let glyphs = vec![glyph('A', 10.0, 0), glyph('B', 20.0, 1)];
        let start = PagePoint::new(11.0, 5.0);
        let outside = PagePoint::new(200.0, 200.0);

        let text = selected_text(&glyphs, start, outside);
        let quads = selected_quads(&glyphs, start, outside);

        assert_eq!(text, "A\nB");
        assert_eq!(
            quads,
            glyphs.iter().map(|glyph| glyph.quad).collect::<Vec<_>>()
        );
    }

    #[test]
    fn display_quads_merge_adjacent_glyphs_on_the_same_line() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 10.0, 0), glyph('C', 20.0, 0)];

        let bands =
            selected_display_quads(&glyphs, PagePoint::new(1.0, 5.0), PagePoint::new(27.0, 5.0));

        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].upper_left, glyphs[0].quad.upper_left);
        assert_eq!(bands[0].lower_right, glyphs[2].quad.lower_right);
        assert_eq!(
            selected_quads(&glyphs, PagePoint::new(1.0, 5.0), PagePoint::new(27.0, 5.0),).len(),
            3
        );
    }

    #[test]
    fn display_quads_keep_separate_line_bands() {
        let glyphs = vec![
            glyph('A', 0.0, 0),
            glyph('B', 10.0, 0),
            glyph('C', 0.0, 1),
            glyph('D', 10.0, 1),
        ];

        let bands = selected_display_quads(
            &glyphs,
            PagePoint::new(1.0, 5.0),
            PagePoint::new(17.0, 25.0),
        );

        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].upper_left, glyphs[0].quad.upper_left);
        assert_eq!(bands[0].lower_right, glyphs[1].quad.lower_right);
        assert_eq!(bands[1].upper_left, glyphs[2].quad.upper_left);
        assert_eq!(bands[1].lower_right, glyphs[3].quad.lower_right);
    }

    #[test]
    fn display_band_preserves_tilted_outer_edges() {
        let mut left = glyph('A', 10.0, 0);
        left.quad.upper_left.y = 3.0;
        left.quad.upper_right.y = 5.0;
        left.quad.lower_left.y = 13.0;
        left.quad.lower_right.y = 15.0;
        let mut right = glyph('B', 20.0, 0);
        right.quad.upper_left.y = 5.0;
        right.quad.upper_right.y = 7.0;
        right.quad.lower_left.y = 15.0;
        right.quad.lower_right.y = 17.0;

        let band = merge_line_band(&[left.clone(), right.clone()]);

        assert_eq!(band.upper_left, left.quad.upper_left);
        assert_eq!(band.lower_left, left.quad.lower_left);
        assert_eq!(band.upper_right, right.quad.upper_right);
        assert_eq!(band.lower_right, right.quad.lower_right);
    }

    #[test]
    fn display_band_preserves_vertical_outer_edges() {
        let top = GlyphSnapshot {
            character: '縦',
            quad: PageQuad {
                upper_left: PagePoint::new(10.0, 20.0),
                upper_right: PagePoint::new(20.0, 20.0),
                lower_left: PagePoint::new(10.0, 30.0),
                lower_right: PagePoint::new(20.0, 30.0),
            },
            line_index: 0,
        };
        let bottom = GlyphSnapshot {
            character: '書',
            quad: PageQuad {
                upper_left: PagePoint::new(10.0, 32.0),
                upper_right: PagePoint::new(20.0, 32.0),
                lower_left: PagePoint::new(10.0, 42.0),
                lower_right: PagePoint::new(20.0, 42.0),
            },
            line_index: 0,
        };

        let band = merge_line_band(&[top.clone(), bottom.clone()]);

        assert_eq!(band.upper_left, top.quad.upper_left);
        assert_eq!(band.upper_right, top.quad.upper_right);
        assert_eq!(band.lower_left, bottom.quad.lower_left);
        assert_eq!(band.lower_right, bottom.quad.lower_right);
    }
}
