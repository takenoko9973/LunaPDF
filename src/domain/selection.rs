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

    /// Tests a point against the visible interior of this convex PDF Quad.
    pub(crate) fn contains(self, point: PagePoint) -> bool {
        let corners = [
            self.upper_left,
            self.upper_right,
            self.lower_right,
            self.lower_left,
        ];
        let polygon_area_twice = corners
            .iter()
            .zip(corners.iter().cycle().skip(1))
            .take(corners.len())
            .map(|(start, end)| start.x * end.y - end.x * start.y)
            .sum::<f32>();
        if polygon_area_twice == 0.0 {
            // A degenerate Quad has no visible interior and must not become a
            // selection or annotation target because all edge products are zero.
            return false;
        }

        let mut has_clockwise_edge = false;
        let mut has_counterclockwise_edge = false;
        for index in 0..corners.len() {
            let start = corners[index];
            let end = corners[(index + 1) % corners.len()];
            let cross =
                (end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x);
            has_clockwise_edge |= cross < 0.0;
            has_counterclockwise_edge |= cross > 0.0;
            // Convex PDF Quad points contain a point only while all non-zero
            // edge products have the same orientation.
            if has_clockwise_edge && has_counterclockwise_edge {
                return false;
            }
        }
        true
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies why a non-text page region owns pointer interaction.
pub(crate) enum NonTextTargetKind {
    Image,
    Link,
    Form,
}

#[derive(Clone, Copy, Debug)]
/// Preserves non-text hit geometry and its cursor-relevant PDF role.
pub(crate) struct NonTextTarget {
    pub(crate) kind: NonTextTargetKind,
    pub(crate) quad: PageQuad,
}

#[derive(Clone, Debug)]
pub(crate) struct TextPageSnapshot {
    pub(crate) page_index: usize,
    pub(crate) revision: u64,
    pub(crate) glyphs: Vec<GlyphSnapshot>,
    /// Page elements that own pointer interaction independently of text selection.
    pub(crate) non_text_targets: Vec<NonTextTarget>,
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

/// Reports whether the page point is inside the current selection display geometry.
pub(crate) fn selection_contains_point(
    selection: &SelectionSnapshot,
    page_index: usize,
    point: PagePoint,
) -> bool {
    selection.page_index == page_index
        && selection
            .display_quads
            .iter()
            .any(|quad| quad.contains(point))
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

/// Returns merged display and annotation Quads for the same inclusive glyph range as copy text.
pub(crate) fn selected_quads(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Vec<PageQuad> {
    let Some(selected_glyphs) = selected_glyphs(glyphs, start, end) else {
        return Vec::new();
    };
    merge_line_bands(selected_glyphs)
}

/// Returns the geometry used only to paint the selected glyph range.
pub(crate) fn selected_display_quads(
    glyphs: &[GlyphSnapshot],
    start: PagePoint,
    end: PagePoint,
) -> Vec<PageQuad> {
    selected_quads(glyphs, start, end)
}

fn merge_line_bands(glyphs: &[GlyphSnapshot]) -> Vec<PageQuad> {
    if glyphs.is_empty() {
        return Vec::new();
    }

    let mut bands = Vec::new();
    let mut run_start = 0;
    let mut run_direction = None;
    let mut index = 1;
    while index <= glyphs.len() {
        if index == glyphs.len() {
            bands.push(merge_line_band(&glyphs[run_start..index]));
            break;
        }
        let direction = Some(glyph_direction(&glyphs[index - 1], &glyphs[index]));
        // A run ends before the current glyph when a line, continuity gap,
        // or writing direction changes; this keeps one annotation from
        // spanning unrelated columns or differently oriented text.
        let continuity_break = !glyphs_are_continuous(glyphs, index - 1, index);
        let run_ended = glyphs[index].line_index != glyphs[run_start].line_index
            || continuity_break
            || run_direction.is_some_and(|previous| direction != Some(previous));
        if run_ended {
            if let Some((space_start, space_end)) = wide_whitespace_bridge(glyphs, index - 1, index)
            {
                if space_end <= run_start {
                    // The wide whitespace was already removed from this run;
                    // skip its trailing pair without creating an empty band.
                    index += 1;
                    continue;
                }
                if run_start < space_start {
                    bands.push(merge_line_band(&glyphs[run_start..space_start]));
                }
                run_start = space_end;
                run_direction = None;
                index = space_end;
                continue;
            }
            bands.push(merge_line_band(&glyphs[run_start..index]));
            run_start = index;
            run_direction = None;
        } else if let Some(direction) = direction {
            run_direction = Some(direction);
        }
        index += 1;
    }
    bands
}

/// Returns a whitespace range that must be omitted when its non-space anchors
/// are farther apart than either neighboring glyph can account for.
fn wide_whitespace_bridge(
    glyphs: &[GlyphSnapshot],
    previous_index: usize,
    next_index: usize,
) -> Option<(usize, usize)> {
    if !glyphs[previous_index].character.is_whitespace()
        && !glyphs[next_index].character.is_whitespace()
    {
        return None;
    }
    let left_index = (0..=previous_index)
        .rev()
        .find(|&index| !glyphs[index].character.is_whitespace())?;
    let right_index =
        (next_index..glyphs.len()).find(|&index| !glyphs[index].character.is_whitespace())?;
    if glyphs[left_index].line_index != glyphs[right_index].line_index
        || glyphs_are_continuous_without_spaces(glyphs, left_index, right_index)
    {
        return None;
    }
    Some((left_index + 1, right_index))
}

fn glyph_direction(previous: &GlyphSnapshot, next: &GlyphSnapshot) -> (bool, bool) {
    let previous_center = glyph_center(previous);
    let next_center = glyph_center(next);
    let delta_x = next_center.x - previous_center.x;
    let delta_y = next_center.y - previous_center.y;
    let advances_horizontally = delta_x.abs() >= delta_y.abs();
    let forward = if advances_horizontally {
        delta_x >= 0.0
    } else {
        delta_y >= 0.0
    };
    (advances_horizontally, forward)
}

fn glyphs_are_continuous(
    glyphs: &[GlyphSnapshot],
    previous_index: usize,
    next_index: usize,
) -> bool {
    let previous = &glyphs[previous_index];
    let next = &glyphs[next_index];
    if previous.character.is_whitespace() || next.character.is_whitespace() {
        let Some(left_index) = (0..=previous_index)
            .rev()
            .find(|&index| !glyphs[index].character.is_whitespace())
        else {
            return true;
        };
        let Some(right_index) =
            (next_index..glyphs.len()).find(|&index| !glyphs[index].character.is_whitespace())
        else {
            return true;
        };
        if left_index != previous_index || right_index != next_index {
            // A wide space glyph can have zero inter-glyph gap on both sides.
            // Compare the surrounding non-space edges so a column-sized blank
            // does not silently become one continuous highlight band.
            return glyphs_are_continuous_without_spaces(glyphs, left_index, right_index);
        }
    }
    glyphs_are_continuous_without_spaces(glyphs, previous_index, next_index)
}

fn glyphs_are_continuous_without_spaces(
    glyphs: &[GlyphSnapshot],
    previous_index: usize,
    next_index: usize,
) -> bool {
    let previous = &glyphs[previous_index];
    let next = &glyphs[next_index];
    let previous_center = glyph_center(previous);
    let next_center = glyph_center(next);
    let delta_x = next_center.x - previous_center.x;
    let delta_y = next_center.y - previous_center.y;
    let (advances_horizontally, _) = glyph_direction(previous, next);
    let (gap, previous_extent, next_extent) = if advances_horizontally {
        let (previous_x0, _, previous_x1, _) = previous.quad.bounds();
        let (next_x0, _, next_x1, _) = next.quad.bounds();
        let gap = if delta_x >= 0.0 {
            next_x0 - previous_x1
        } else {
            previous_x0 - next_x1
        };
        (gap, previous_x1 - previous_x0, next_x1 - next_x0)
    } else {
        let (_, previous_y0, _, previous_y1) = previous.quad.bounds();
        let (_, next_y0, _, next_y1) = next.quad.bounds();
        let gap = if delta_y >= 0.0 {
            next_y0 - previous_y1
        } else {
            previous_y0 - next_y1
        };
        (gap, previous_y1 - previous_y0, next_y1 - next_y0)
    };

    // A run may include normal inter-glyph spacing (including a word space),
    // but a gap larger than either adjacent glyph's own extent indicates a
    // column or unrelated region.  This scales with page/text size instead
    // of imposing a fixed PDF-point threshold.
    let continuity_bound = previous_extent.max(next_extent);
    gap <= continuity_bound
}

fn merge_line_band(glyphs: &[GlyphSnapshot]) -> PageQuad {
    let mut band = glyphs[0].quad;
    for glyph in &glyphs[1..] {
        let quad = glyph.quad;
        let forward_gap = edge_distance_squared(
            band.upper_right,
            band.lower_right,
            quad.upper_left,
            quad.lower_left,
        );
        let reverse_gap = edge_distance_squared(
            band.upper_left,
            band.lower_left,
            quad.upper_right,
            quad.lower_right,
        );
        // MuPDF always stores text-progression edges as [ul,ll] and [ur,lr],
        // including rotated and bidi text. Extending the nearer edge follows
        // that contract without reinterpreting the corner names in page axes.
        if forward_gap <= reverse_gap {
            band.upper_right = quad.upper_right;
            band.lower_right = quad.lower_right;
        } else {
            band.upper_left = quad.upper_left;
            band.lower_left = quad.lower_left;
        }
    }
    band
}

fn edge_distance_squared(
    first_upper: PagePoint,
    first_lower: PagePoint,
    second_upper: PagePoint,
    second_lower: PagePoint,
) -> f32 {
    point_distance_squared(first_upper, second_upper)
        + point_distance_squared(first_lower, second_lower)
}

fn point_distance_squared(first: PagePoint, second: PagePoint) -> f32 {
    let delta_x = first.x - second.x;
    let delta_y = first.y - second.y;
    delta_x * delta_x + delta_y * delta_y
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
    Some(glyph_center(glyph))
}

/// Maps a pointer only when it is within the caller's page-space hit tolerance.
pub(crate) fn snap_to_glyph_with_max_distance(
    glyphs: &[GlyphSnapshot],
    point: PagePoint,
    maximum_distance: f32,
) -> Option<PagePoint> {
    if !maximum_distance.is_finite() || maximum_distance < 0.0 {
        return None;
    }
    let (index, squared_distance) = nearest_glyph(glyphs, point)?;
    if squared_distance > maximum_distance * maximum_distance {
        return None;
    }
    glyphs.get(index).map(glyph_center)
}

fn glyph_center(glyph: &GlyphSnapshot) -> PagePoint {
    let (x0, y0, x1, y1) = glyph.quad.bounds();
    PagePoint::new((x0 + x1) / 2.0, (y0 + y1) / 2.0)
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
    nearest_glyph(glyphs, point).map(|(index, _)| index)
}

fn nearest_glyph(glyphs: &[GlyphSnapshot], point: PagePoint) -> Option<(usize, f32)> {
    glyphs
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            let left_distance = squared_distance_to_quad_bounds(left.quad, point);
            let right_distance = squared_distance_to_quad_bounds(right.quad, point);
            left_distance.total_cmp(&right_distance)
        })
        .map(|(index, glyph)| (index, squared_distance_to_quad_bounds(glyph.quad, point)))
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
    fn bounded_pointer_hit_rejects_page_space_far_from_text() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 20.0, 0)];

        assert_eq!(
            snap_to_glyph_with_max_distance(&glyphs, PagePoint::new(31.0, 5.0), 4.0),
            Some(PagePoint::new(24.0, 5.0))
        );
        assert_eq!(
            snap_to_glyph_with_max_distance(&glyphs, PagePoint::new(80.0, 80.0), 4.0),
            None
        );
    }

    #[test]
    fn selection_quads_include_both_endpoint_glyphs() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 10.0, 0), glyph('C', 20.0, 0)];
        let start = PagePoint::new(1.0, 5.0);
        let end = PagePoint::new(27.0, 5.0);

        let forward = selected_quads(&glyphs, start, end);
        let reverse = selected_quads(&glyphs, end, start);

        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].upper_left, glyphs[0].quad.upper_left);
        assert_eq!(forward[0].lower_right, glyphs[2].quad.lower_right);
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
            1
        );
    }

    #[test]
    fn selection_quads_split_a_large_same_line_gap() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 40.0, 0), glyph('C', 50.0, 0)];

        let quads = selected_quads(&glyphs, PagePoint::new(1.0, 5.0), PagePoint::new(57.0, 5.0));

        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0], glyphs[0].quad);
        assert_eq!(quads[1].upper_left, glyphs[1].quad.upper_left);
        assert_eq!(quads[1].lower_right, glyphs[2].quad.lower_right);
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
                upper_left: PagePoint::new(20.0, 20.0),
                upper_right: PagePoint::new(20.0, 30.0),
                lower_left: PagePoint::new(10.0, 20.0),
                lower_right: PagePoint::new(10.0, 30.0),
            },
            line_index: 0,
        };
        let bottom = GlyphSnapshot {
            character: '書',
            quad: PageQuad {
                upper_left: PagePoint::new(20.0, 32.0),
                upper_right: PagePoint::new(20.0, 42.0),
                lower_left: PagePoint::new(10.0, 32.0),
                lower_right: PagePoint::new(10.0, 42.0),
            },
            line_index: 0,
        };

        let band = merge_line_band(&[top.clone(), bottom.clone()]);

        assert_eq!(band.upper_left, top.quad.upper_left);
        assert_eq!(band.lower_left, top.quad.lower_left);
        assert_eq!(band.upper_right, bottom.quad.upper_right);
        assert_eq!(band.lower_right, bottom.quad.lower_right);
    }

    #[test]
    fn vertical_selection_quads_merge_adjacent_glyphs() {
        let glyphs = vec![
            GlyphSnapshot {
                character: '縦',
                quad: PageQuad {
                    upper_left: PagePoint::new(20.0, 20.0),
                    upper_right: PagePoint::new(20.0, 30.0),
                    lower_left: PagePoint::new(10.0, 20.0),
                    lower_right: PagePoint::new(10.0, 30.0),
                },
                line_index: 0,
            },
            GlyphSnapshot {
                character: '書',
                quad: PageQuad {
                    upper_left: PagePoint::new(20.0, 32.0),
                    upper_right: PagePoint::new(20.0, 42.0),
                    lower_left: PagePoint::new(10.0, 32.0),
                    lower_right: PagePoint::new(10.0, 42.0),
                },
                line_index: 0,
            },
        ];

        let quads = selected_quads(
            &glyphs,
            PagePoint::new(15.0, 21.0),
            PagePoint::new(15.0, 41.0),
        );

        assert_eq!(quads, vec![merge_line_band(&glyphs)]);
    }

    #[test]
    fn reversed_quad_corner_orientation_does_not_collapse_horizontal_band() {
        let glyphs = vec![
            GlyphSnapshot {
                character: 'B',
                quad: PageQuad {
                    upper_left: PagePoint::new(28.0, 0.0),
                    upper_right: PagePoint::new(20.0, 0.0),
                    lower_left: PagePoint::new(28.0, 10.0),
                    lower_right: PagePoint::new(20.0, 10.0),
                },
                line_index: 0,
            },
            GlyphSnapshot {
                character: 'A',
                quad: PageQuad {
                    upper_left: PagePoint::new(18.0, 0.0),
                    upper_right: PagePoint::new(10.0, 0.0),
                    lower_left: PagePoint::new(18.0, 10.0),
                    lower_right: PagePoint::new(10.0, 10.0),
                },
                line_index: 0,
            },
        ];

        let band = merge_line_band(&glyphs);

        assert_eq!(band.bounds(), (10.0, 0.0, 28.0, 10.0));
        assert!(band.contains(PagePoint::new(19.0, 5.0)));
    }

    #[test]
    fn reversed_quad_corner_orientation_does_not_collapse_vertical_band() {
        let glyphs = vec![
            GlyphSnapshot {
                character: '書',
                quad: PageQuad {
                    upper_left: PagePoint::new(20.0, 42.0),
                    upper_right: PagePoint::new(20.0, 32.0),
                    lower_left: PagePoint::new(10.0, 42.0),
                    lower_right: PagePoint::new(10.0, 32.0),
                },
                line_index: 0,
            },
            GlyphSnapshot {
                character: '縦',
                quad: PageQuad {
                    upper_left: PagePoint::new(20.0, 30.0),
                    upper_right: PagePoint::new(20.0, 20.0),
                    lower_left: PagePoint::new(10.0, 30.0),
                    lower_right: PagePoint::new(10.0, 20.0),
                },
                line_index: 0,
            },
        ];

        let band = merge_line_band(&glyphs);

        assert_eq!(band.bounds(), (10.0, 20.0, 20.0, 42.0));
        assert!(band.contains(PagePoint::new(15.0, 31.0)));
    }

    #[test]
    fn wide_space_does_not_bridge_non_space_glyphs() {
        let glyphs = vec![
            glyph('A', 0.0, 0),
            GlyphSnapshot {
                character: ' ',
                quad: PageQuad {
                    upper_left: PagePoint::new(8.0, 0.0),
                    upper_right: PagePoint::new(108.0, 0.0),
                    lower_left: PagePoint::new(8.0, 10.0),
                    lower_right: PagePoint::new(108.0, 10.0),
                },
                line_index: 0,
            },
            glyph('B', 108.0, 0),
        ];

        let bands = merge_line_bands(&glyphs);

        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].bounds(), (0.0, 0.0, 8.0, 10.0));
        assert_eq!(bands[1].bounds(), (108.0, 0.0, 116.0, 10.0));
    }

    #[test]
    fn normal_word_space_stays_in_one_band() {
        let glyphs = vec![
            glyph('A', 0.0, 0),
            GlyphSnapshot {
                character: ' ',
                quad: PageQuad {
                    upper_left: PagePoint::new(8.0, 0.0),
                    upper_right: PagePoint::new(14.0, 0.0),
                    lower_left: PagePoint::new(8.0, 10.0),
                    lower_right: PagePoint::new(14.0, 10.0),
                },
                line_index: 0,
            },
            glyph('B', 16.0, 0),
        ];

        assert_eq!(merge_line_bands(&glyphs).len(), 1);
    }

    #[test]
    fn whitespace_only_selection_remains_a_valid_band() {
        let glyphs = vec![
            GlyphSnapshot {
                character: ' ',
                quad: PageQuad {
                    upper_left: PagePoint::new(0.0, 0.0),
                    upper_right: PagePoint::new(8.0, 0.0),
                    lower_left: PagePoint::new(0.0, 10.0),
                    lower_right: PagePoint::new(8.0, 10.0),
                },
                line_index: 0,
            },
            GlyphSnapshot {
                character: '\u{3000}',
                quad: PageQuad {
                    upper_left: PagePoint::new(8.0, 0.0),
                    upper_right: PagePoint::new(16.0, 0.0),
                    lower_left: PagePoint::new(8.0, 10.0),
                    lower_right: PagePoint::new(16.0, 10.0),
                },
                line_index: 0,
            },
        ];

        assert_eq!(merge_line_bands(&glyphs).len(), 1);
    }

    #[test]
    fn saved_and_preview_selection_geometry_are_identical() {
        let glyphs = vec![glyph('A', 0.0, 0), glyph('B', 10.0, 0)];
        let start = PagePoint::new(1.0, 5.0);
        let end = PagePoint::new(17.0, 5.0);

        assert_eq!(
            selected_quads(&glyphs, start, end),
            selected_display_quads(&glyphs, start, end)
        );
    }
}
