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
    pub(crate) quads: Vec<PageQuad>,
    pub(crate) extraction_time: Duration,
}

/// Creates the logical copy string independently from the display quads.
///
/// MuPDF 0.8 exposes standard selection quads but not its range-copy string.
/// Keeping this ordered glyph selection separate ensures future Typst-only
/// display correction cannot silently alter copy order.
pub(crate) fn selected_text(glyphs: &[GlyphSnapshot], start: PagePoint, end: PagePoint) -> String {
    let Some(range) = selected_glyph_range(glyphs, start, end) else {
        return String::new();
    };

    let mut text = String::new();
    let mut previous_line = glyphs[*range.start()].line_index;
    for glyph in &glyphs[range] {
        if glyph.line_index != previous_line {
            text.push('\n');
            previous_line = glyph.line_index;
        }
        text.push(glyph.character);
    }
    text
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
}
