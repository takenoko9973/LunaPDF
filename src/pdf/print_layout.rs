use crate::domain::document::PageRect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PrintStrip {
    pub(super) pixel_y: u32,
    pub(super) pixel_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PrintLayout {
    pub(super) scale: f32,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
    pub(super) offset_x: i32,
    pub(super) offset_y: i32,
}

impl PrintLayout {
    /// Fits one PDF page within the printer's printable device-pixel area.
    pub(super) fn fit(
        bounds: PageRect,
        printable_width: u32,
        printable_height: u32,
    ) -> Option<Self> {
        // Printer caps and page boxes cross OS/PDF boundaries. Rejecting all
        // non-physical values here avoids ambiguous scaling later in GDI.
        if printable_width == 0
            || printable_height == 0
            || !bounds.width().is_finite()
            || !bounds.height().is_finite()
            || bounds.width() <= 0.0
            || bounds.height() <= 0.0
        {
            return None;
        }
        let mut scale = (printable_width as f32 / bounds.width())
            .min(printable_height as f32 / bounds.height());
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }

        // MuPDF rounds the transformed page box rather than only its width.
        // Matching that rule keeps the requested strips inside non-zero page boxes.
        let mut pixel_width = scaled_extent(bounds.x0, bounds.x1, scale)?;
        let mut pixel_height = scaled_extent(bounds.y0, bounds.y1, scale)?;
        if pixel_width > printable_width || pixel_height > printable_height {
            // Transforming a non-zero page box can round its two edges outward
            // by one pixel. Reduce only that rounding excess before giving up.
            let width_correction = printable_width as f32 / pixel_width as f32;
            let height_correction = printable_height as f32 / pixel_height as f32;
            scale *= width_correction.min(height_correction);
            pixel_width = scaled_extent(bounds.x0, bounds.x1, scale)?;
            pixel_height = scaled_extent(bounds.y0, bounds.y1, scale)?;
        }
        if pixel_width > printable_width || pixel_height > printable_height {
            return None;
        }
        let offset_x = i32::try_from((printable_width - pixel_width) / 2).ok()?;
        let offset_y = i32::try_from((printable_height - pixel_height) / 2).ok()?;
        Some(Self {
            scale,
            pixel_width,
            pixel_height,
            offset_x,
            offset_y,
        })
    }

    /// Splits the raster into complete scanline strips under a fixed RGBA budget.
    pub(super) fn strips(self, byte_budget: usize) -> Option<Vec<PrintStrip>> {
        let row_bytes = usize::try_from(self.pixel_width).ok()?.checked_mul(4)?;
        if row_bytes == 0 || row_bytes > byte_budget {
            return None;
        }
        let rows_per_strip = u32::try_from(byte_budget / row_bytes).ok()?.max(1);
        let mut strips = Vec::new();
        let mut pixel_y = 0_u32;
        while pixel_y < self.pixel_height {
            let pixel_height = rows_per_strip.min(self.pixel_height - pixel_y);
            strips.push(PrintStrip {
                pixel_y,
                pixel_height,
            });
            pixel_y = pixel_y.checked_add(pixel_height)?;
        }
        Some(strips)
    }
}

fn scaled_extent(start: f32, end: f32, scale: f32) -> Option<u32> {
    let scaled_start = (start * scale).round();
    let scaled_end = (end * scale).round();
    if !scaled_start.is_finite() || !scaled_end.is_finite() {
        return None;
    }
    let extent = f64::from(scaled_end) - f64::from(scaled_start);
    if extent <= 0.0 || extent > f64::from(u32::MAX) {
        return None;
    }
    Some(extent as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_is_centered_and_preserves_aspect_ratio() {
        let layout = PrintLayout::fit(
            PageRect {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            2_400,
            2_400,
        )
        .unwrap();

        assert_eq!(layout.pixel_width, 1_800);
        assert_eq!(layout.pixel_height, 2_400);
        assert_eq!(layout.offset_x, 300);
        assert_eq!(layout.offset_y, 0);
        assert!((layout.scale - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn strips_cover_page_once_without_exceeding_budget() {
        let layout = PrintLayout {
            scale: 1.0,
            pixel_width: 1_000,
            pixel_height: 2_501,
            offset_x: 0,
            offset_y: 0,
        };
        let byte_budget = 1_000 * 4 * 600;
        let strips = layout.strips(byte_budget).unwrap();

        assert_eq!(strips.first().unwrap().pixel_y, 0);
        assert_eq!(strips.last().unwrap().pixel_y, 2_400);
        assert_eq!(strips.last().unwrap().pixel_height, 101);
        assert_eq!(
            strips.iter().map(|strip| strip.pixel_height).sum::<u32>(),
            layout.pixel_height
        );
        assert!(strips.iter().all(|strip| {
            strip.pixel_height as usize * layout.pixel_width as usize * 4 <= byte_budget
        }));
    }

    #[test]
    fn transformed_nonzero_box_uses_mupdf_rounding() {
        let layout = PrintLayout::fit(
            PageRect {
                x0: 10.25,
                y0: 20.25,
                x1: 110.75,
                y1: 220.75,
            },
            1_000,
            1_000,
        )
        .unwrap();

        assert!(layout.pixel_width <= 1_000);
        assert!(layout.pixel_height <= 1_000);
        assert_eq!(layout.offset_x, (1_000 - layout.pixel_width as i32) / 2);
        assert_eq!(layout.offset_y, (1_000 - layout.pixel_height as i32) / 2);
    }
}
