use eframe::egui::{Color32, PointerButton, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Ui};

use crate::domain::document::{PageRect, RenderedTile};
use crate::domain::selection::{PagePoint, PageQuad, SelectionSnapshot};

#[derive(Default)]
pub(crate) struct PageViewport {
    drag_page: Option<usize>,
    drag_start: Option<PagePoint>,
    drag_current: Option<PagePoint>,
}

impl PageViewport {
    /// Paints one tile in its page-relative raster position.
    pub(crate) fn paint_tile(
        ui: &Ui,
        page_screen_rect: Rect,
        texture: &TextureHandle,
        tile: &RenderedTile,
    ) {
        let tile_rect = screen_rect_for_tile(page_screen_rect, tile);
        ui.painter().image(
            texture.id(),
            tile_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// Paints search geometry without changing the logical text selection.
    pub(crate) fn paint_search_quads(
        ui: &Ui,
        screen_rect: Rect,
        bounds: PageRect,
        quads: &[PageQuad],
    ) {
        for quad in quads {
            paint_quad(
                ui,
                screen_rect,
                bounds,
                *quad,
                Color32::from_rgba_unmultiplied(80, 170, 255, 72),
                Color32::from_rgb(35, 110, 210),
            );
        }
    }

    /// Handles selection interaction and overlays for one logical PDF page.
    pub(crate) fn interact_at(
        &mut self,
        ui: &mut Ui,
        screen_rect: Rect,
        page_index: usize,
        bounds: PageRect,
        selection: Option<&SelectionSnapshot>,
    ) -> Option<(usize, PagePoint, PagePoint)> {
        let response = ui.interact(
            screen_rect,
            ui.id().with(("pdf-page", page_index)),
            Sense::drag(),
        );

        if let Some(selection) = selection.filter(|value| value.page_index == page_index) {
            for quad in &selection.quads {
                paint_quad(
                    ui,
                    screen_rect,
                    bounds,
                    *quad,
                    Color32::from_rgba_unmultiplied(255, 210, 0, 72),
                    Color32::from_rgb(220, 150, 0),
                );
            }
        }

        if response.drag_started_by(PointerButton::Primary) {
            self.drag_page = Some(page_index);
            self.drag_start = response
                .interact_pointer_pos()
                .map(|position| page_point_from_screen(position, screen_rect, bounds));
            self.drag_current = self.drag_start;
        }
        if response.dragged_by(PointerButton::Primary) && self.drag_page == Some(page_index) {
            self.drag_current = response
                .interact_pointer_pos()
                .map(|position| page_point_from_screen(position, screen_rect, bounds));
        }

        self.paint_drag_markers(ui, screen_rect, page_index, bounds);

        if response.drag_stopped_by(PointerButton::Primary) && self.drag_page == Some(page_index) {
            let completed = self
                .drag_start
                .zip(self.drag_current)
                .map(|(start, end)| (page_index, start, end));
            self.drag_page = None;
            self.drag_start = None;
            self.drag_current = None;
            return completed;
        }
        None
    }

    fn paint_drag_markers(&self, ui: &Ui, screen_rect: Rect, page_index: usize, bounds: PageRect) {
        if self.drag_page != Some(page_index) {
            return;
        }
        let (Some(start), Some(current)) = (self.drag_start, self.drag_current) else {
            return;
        };
        let start = screen_point_from_page(start, screen_rect, bounds);
        let current = screen_point_from_page(current, screen_rect, bounds);
        ui.painter()
            .line_segment([start, current], Stroke::new(2.0, Color32::LIGHT_BLUE));
        ui.painter().circle_filled(start, 5.0, Color32::RED);
        ui.painter().circle_filled(current, 5.0, Color32::BLUE);
    }
}

fn screen_rect_for_tile(page_screen_rect: Rect, tile: &RenderedTile) -> Rect {
    let page_pixel_width = tile.page_pixel_width as f32;
    let page_pixel_height = tile.page_pixel_height as f32;
    let x0 = tile.spec.pixel_x as f32 / page_pixel_width;
    let y0 = tile.spec.pixel_y as f32 / page_pixel_height;
    let x1 = (tile.spec.pixel_x + tile.spec.pixel_width) as f32 / page_pixel_width;
    let y1 = (tile.spec.pixel_y + tile.spec.pixel_height) as f32 / page_pixel_height;
    Rect::from_min_max(
        Pos2::new(
            page_screen_rect.left() + page_screen_rect.width() * x0,
            page_screen_rect.top() + page_screen_rect.height() * y0,
        ),
        Pos2::new(
            page_screen_rect.left() + page_screen_rect.width() * x1,
            page_screen_rect.top() + page_screen_rect.height() * y1,
        ),
    )
}

fn paint_quad(
    ui: &Ui,
    screen_rect: Rect,
    bounds: PageRect,
    quad: PageQuad,
    fill: Color32,
    stroke: Color32,
) {
    let points = vec![
        screen_point_from_page(quad.upper_left, screen_rect, bounds),
        screen_point_from_page(quad.upper_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_left, screen_rect, bounds),
    ];
    ui.painter().add(Shape::convex_polygon(
        points,
        fill,
        Stroke::new(1.5, stroke),
    ));
}

fn page_point_from_screen(position: Pos2, screen_rect: Rect, bounds: PageRect) -> PagePoint {
    // Pointer positions are clamped at the rendered page edge so a drag that
    // ends just outside the widget still maps to a valid PDF selection point.
    let x = position.x.clamp(screen_rect.left(), screen_rect.right());
    let y = position.y.clamp(screen_rect.top(), screen_rect.bottom());
    let normalized_x = (x - screen_rect.left()) / screen_rect.width();
    let normalized_y = (y - screen_rect.top()) / screen_rect.height();
    PagePoint::new(
        bounds.x0 + normalized_x * bounds.width(),
        bounds.y0 + normalized_y * bounds.height(),
    )
}

fn screen_point_from_page(point: PagePoint, screen_rect: Rect, bounds: PageRect) -> Pos2 {
    let normalized_x = (point.x - bounds.x0) / bounds.width();
    let normalized_y = (point.y - bounds.y0) / bounds.height();
    Pos2::new(
        screen_rect.left() + normalized_x * screen_rect.width(),
        screen_rect.top() + normalized_y * screen_rect.height(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::domain::document::TileSpec;

    fn bounds() -> PageRect {
        PageRect {
            x0: 10.0,
            y0: 20.0,
            x1: 110.0,
            y1: 220.0,
        }
    }

    fn tile(spec: TileSpec) -> RenderedTile {
        RenderedTile {
            page_index: 0,
            zoom: 1.0,
            pixels_per_point: 1.0,
            scale: 1.0,
            generation: 0,
            revision: 0,
            spec,
            page_pixel_width: 1_024,
            page_pixel_height: 512,
            pixels_rgba: Vec::new(),
            bounds: bounds(),
            render_time: Duration::ZERO,
            physical_memory_bytes: None,
        }
    }

    #[test]
    fn screen_and_pdf_coordinates_roundtrip_with_nonzero_origin() {
        let screen_rect = Rect::from_min_size(Pos2::new(50.0, 80.0), [200.0, 400.0].into());
        let expected = PagePoint::new(35.0, 170.0);

        let screen = screen_point_from_page(expected, screen_rect, bounds());
        let actual = page_point_from_screen(screen, screen_rect, bounds());

        assert!((actual.x - expected.x).abs() < 0.001);
        assert!((actual.y - expected.y).abs() < 0.001);
    }

    #[test]
    fn adjacent_tile_rectangles_share_an_exact_screen_edge() {
        let page_rect = Rect::from_min_size(Pos2::new(10.0, 20.0), [800.0, 400.0].into());
        let left = tile(TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        });
        let right = tile(TileSpec {
            pixel_x: 512,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        });

        let left_rect = screen_rect_for_tile(page_rect, &left);
        let right_rect = screen_rect_for_tile(page_rect, &right);

        assert_eq!(left_rect.right(), right_rect.left());
        assert_eq!(left_rect.left(), page_rect.left());
        assert_eq!(right_rect.right(), page_rect.right());
    }
}
