use std::ops::Range;

use crate::domain::document::PageRect;

// Sixteen logical pixels keeps page boundaries visible at 100% without
// coupling document geometry to the GUI theme's widget spacing.
pub(crate) const PAGE_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PagePlacement {
    pub(crate) page_index: usize,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageAnchor {
    pub(crate) page_index: usize,
    pub(crate) page_x_fraction: f32,
    pub(crate) page_y_fraction: f32,
}

impl PagePlacement {
    pub(crate) fn bottom(self) -> f32 {
        self.y + self.height
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ContinuousLayout {
    placements: Vec<PagePlacement>,
    content_width: f32,
    total_height: f32,
}

impl ContinuousLayout {
    /// Computes document-space page placement for one zoom and viewport width.
    pub(crate) fn new(page_bounds: &[PageRect], zoom: f32, viewport_width: f32) -> Self {
        let mut y = PAGE_GAP;
        let mut placements = Vec::with_capacity(page_bounds.len());
        for (page_index, bounds) in page_bounds.iter().enumerate() {
            let width = bounds.width() * zoom;
            let height = bounds.height() * zoom;
            let x = ((viewport_width - width) / 2.0).max(0.0);
            placements.push(PagePlacement {
                page_index,
                x,
                y,
                width,
                height,
            });
            y += height + PAGE_GAP;
        }

        Self {
            placements,
            content_width: viewport_width,
            total_height: y,
        }
    }

    pub(crate) fn total_height(&self) -> f32 {
        self.total_height
    }

    pub(crate) fn placement(&self, page_index: usize) -> Option<PagePlacement> {
        self.placements.get(page_index).copied()
    }

    /// Returns pages intersecting the viewport plus a bounded prefetch margin.
    pub(crate) fn visible_pages(&self, viewport: Range<f32>, prefetch_margin: f32) -> Range<usize> {
        let visible_start = (viewport.start - prefetch_margin).max(0.0);
        let visible_end = viewport.end + prefetch_margin;
        let start = self
            .placements
            .partition_point(|placement| placement.bottom() < visible_start);
        let end = self
            .placements
            .partition_point(|placement| placement.y <= visible_end);
        start..end
    }

    pub(crate) fn page_at_y(&self, y: f32) -> Option<usize> {
        let insertion = self
            .placements
            .partition_point(|placement| placement.bottom() < y);
        self.placements
            .get(insertion)
            .map(|placement| placement.page_index)
    }

    /// Captures a document position as a page and normalized two-axis offset.
    pub(crate) fn anchor_at(&self, x: f32, y: f32) -> Option<PageAnchor> {
        let page_index = self.page_at_y(y)?;
        let placement = self.placement(page_index)?;
        let page_x_fraction = ((x - placement.x) / placement.width).clamp(0.0, 1.0);
        let page_y_fraction = ((y - placement.y) / placement.height).clamp(0.0, 1.0);
        Some(PageAnchor {
            page_index,
            page_x_fraction,
            page_y_fraction,
        })
    }

    /// Returns the scroll offset that places an anchor at viewport center.
    pub(crate) fn centered_offset(
        &self,
        anchor: PageAnchor,
        viewport_width: f32,
        viewport_height: f32,
    ) -> Option<(f32, f32)> {
        let placement = self.placement(anchor.page_index)?;
        let document_x = placement.x + placement.width * anchor.page_x_fraction;
        let document_y = placement.y + placement.height * anchor.page_y_fraction;
        let desired_x = document_x - viewport_width / 2.0;
        let desired_y = document_y - viewport_height / 2.0;
        let maximum_x = (self.content_width - viewport_width).max(0.0);
        let maximum_y = (self.total_height - viewport_height).max(0.0);
        Some((
            desired_x.clamp(0.0, maximum_x),
            desired_y.clamp(0.0, maximum_y),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(width: f32, height: f32) -> PageRect {
        PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: width,
            y1: height,
        }
    }

    #[test]
    fn placement_accumulates_variable_page_heights_and_centers_width() {
        let layout = ContinuousLayout::new(&[page(100.0, 200.0), page(200.0, 100.0)], 2.0, 600.0);

        assert_eq!(layout.placement(0).unwrap().x, 200.0);
        assert_eq!(layout.placement(0).unwrap().y, PAGE_GAP);
        assert_eq!(layout.placement(1).unwrap().x, 100.0);
        assert_eq!(layout.placement(1).unwrap().y, 432.0);
        assert_eq!(layout.total_height(), 648.0);
    }

    #[test]
    fn visible_range_excludes_distant_pages_and_includes_prefetch() {
        let pages = vec![page(100.0, 100.0); 10];
        let layout = ContinuousLayout::new(&pages, 1.0, 200.0);

        assert_eq!(layout.visible_pages(250.0..350.0, 0.0), 2..3);
        assert_eq!(layout.visible_pages(250.0..350.0, 120.0), 1..4);
    }

    #[test]
    fn page_lookup_maps_page_gap_to_the_following_page() {
        let layout = ContinuousLayout::new(&[page(100.0, 100.0); 2], 1.0, 200.0);

        assert_eq!(layout.page_at_y(50.0), Some(0));
        assert_eq!(layout.page_at_y(120.0), Some(1));
        assert_eq!(layout.page_at_y(150.0), Some(1));
    }

    #[test]
    fn centered_anchor_survives_zoom_with_the_same_page_fraction() {
        let pages = vec![page(100.0, 200.0); 3];
        let before = ContinuousLayout::new(&pages, 2.0, 500.0);
        let anchor = before.anchor_at(325.0, 548.0).unwrap();
        let after = ContinuousLayout::new(&pages, 4.0, 900.0);

        let (offset_x, offset_y) = after.centered_offset(anchor, 400.0, 200.0).unwrap();
        let restored = after.anchor_at(offset_x + 200.0, offset_y + 100.0).unwrap();

        assert_eq!(restored.page_index, anchor.page_index);
        assert!((restored.page_x_fraction - anchor.page_x_fraction).abs() < 0.001);
        assert!((restored.page_y_fraction - anchor.page_y_fraction).abs() < 0.001);
    }
}
