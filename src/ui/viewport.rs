use eframe::egui::{
    Color32, PointerButton, Popup, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Ui,
};

use crate::domain::annotation::{
    AnnotationCandidateDecision, AnnotationPageSnapshot, annotations_at_point,
    decide_annotation_candidates,
};
use crate::domain::document::{PageRect, RenderedTile, SearchMatch};
use crate::domain::selection::{
    PagePoint, PageQuad, SelectionSnapshot, TextPageSnapshot, selected_display_quads,
    selection_contains_point, snap_to_glyph,
};
use crate::ui::annotation_editor::{
    AnnotationContextTarget, AnnotationUiAction, annotation_menu_candidates,
    show_annotation_context_menu,
};

#[derive(Default)]
pub(crate) struct PageViewport {
    drag_page: Option<usize>,
    drag_start: Option<PagePoint>,
    drag_current: Option<PagePoint>,
    drag_origin_screen: Option<Pos2>,
    drag_active: bool,
    context_page: Option<usize>,
    context_target: Option<AnnotationContextTarget>,
}

#[derive(Default)]
pub(crate) struct PageInteraction {
    pub(crate) completed_drag: Option<(usize, PagePoint, PagePoint)>,
    pub(crate) annotation_action: Option<AnnotationUiAction>,
}

pub(crate) struct PageInteractionInput<'a> {
    pub(crate) screen_rect: Rect,
    pub(crate) page_index: usize,
    pub(crate) bounds: PageRect,
    pub(crate) text_snapshot: Option<&'a TextPageSnapshot>,
    pub(crate) selection: Option<&'a SelectionSnapshot>,
    pub(crate) annotation_page: Option<&'a AnnotationPageSnapshot>,
    pub(crate) can_create_highlight: bool,
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
    pub(crate) fn paint_search_matches(
        ui: &Ui,
        screen_rect: Rect,
        bounds: PageRect,
        matches: &[SearchMatch],
        selected_match: Option<usize>,
    ) {
        for (match_index, search_match) in matches.iter().enumerate() {
            // A distinct color makes the result Enter selected visible while
            // retaining every other hit as document-wide search context.
            let selected = selected_match == Some(match_index);
            let (fill, stroke) = if selected {
                (
                    Color32::from_rgba_unmultiplied(255, 185, 30, 96),
                    Color32::from_rgb(220, 120, 0),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(80, 170, 255, 72),
                    Color32::from_rgb(35, 110, 210),
                )
            };
            for quad in &search_match.quads {
                paint_quad(
                    ui,
                    screen_rect,
                    bounds,
                    *quad,
                    fill,
                    Stroke::new(1.5, stroke),
                );
            }
        }
    }

    /// Handles selection interaction and overlays for one logical PDF page.
    pub(crate) fn interact_at(
        &mut self,
        ui: &mut Ui,
        input: PageInteractionInput<'_>,
    ) -> PageInteraction {
        let PageInteractionInput {
            screen_rect,
            page_index,
            bounds,
            text_snapshot,
            selection,
            annotation_page,
            can_create_highlight,
        } = input;
        let response = ui.interact(
            screen_rect,
            ui.id().with(("pdf-page", page_index)),
            Sense::click_and_drag(),
        );

        if let Some(selection) = selection.filter(|value| value.page_index == page_index) {
            for quad in &selection.display_quads {
                paint_quad(
                    ui,
                    screen_rect,
                    bounds,
                    *quad,
                    Color32::from_rgba_unmultiplied(255, 210, 0, 72),
                    Stroke::NONE,
                );
            }
        }
        let mut interaction = PageInteraction::default();
        let pointer_page_point = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.latest_pos()))
            .filter(|position| response.rect.contains(*position))
            .map(|position| page_point_from_screen(position, screen_rect, bounds));
        let secondary_released_here = ui.input(|input| {
            input.pointer.button_released(PointerButton::Secondary)
                && input
                    .pointer
                    .latest_pos()
                    .is_some_and(|position| response.rect.contains(position))
        });
        if secondary_released_here {
            let selection_available =
                pointer_page_point
                    .zip(selection)
                    .is_some_and(|(point, selection)| {
                        selection_contains_point(selection, page_index, point)
                    });
            let annotation_hits = pointer_page_point
                .zip(annotation_page)
                .map_or_else(Vec::new, |(point, page)| {
                    annotations_at_point(&page.annotations, point)
                });
            if selection_available || !annotation_hits.is_empty() {
                self.context_page = Some(page_index);
                self.context_target = Some(AnnotationContextTarget {
                    selection_available,
                    candidates: annotation_menu_candidates(&annotation_hits),
                });
            } else {
                self.context_page = None;
                self.context_target = None;
                Popup::close_id(ui.ctx(), Popup::default_response_id(&response));
            }
        }

        if response.double_clicked_by(PointerButton::Primary)
            && let Some(point) = pointer_page_point
            && let Some(page) = annotation_page
        {
            let hits = annotations_at_point(&page.annotations, point);
            interaction.annotation_action = match decide_annotation_candidates(&hits) {
                AnnotationCandidateDecision::None => None,
                AnnotationCandidateDecision::Open(id) => {
                    Some(AnnotationUiAction::EditAnnotation(id))
                }
                AnnotationCandidateDecision::Choose => Some(AnnotationUiAction::ChooseAnnotation(
                    annotation_menu_candidates(&hits),
                )),
            };
        }

        let (primary_pressed, primary_released, pointer_position, press_origin) =
            ui.input(|input| {
                (
                    input.pointer.button_pressed(PointerButton::Primary),
                    input.pointer.button_released(PointerButton::Primary),
                    input.pointer.latest_pos(),
                    input.pointer.press_origin(),
                )
            });
        let origin = press_origin.or(pointer_position);
        if primary_pressed && origin.is_some_and(|position| response.rect.contains(position)) {
            self.drag_start = origin
                .map(|position| page_point_from_screen(position, screen_rect, bounds))
                .and_then(|point| {
                    text_snapshot.and_then(|snapshot| snap_to_glyph(&snapshot.glyphs, point))
                });
            self.drag_page = self.drag_start.map(|_| page_index);
            self.drag_current = self.drag_start;
            // A press without a text glyph must not leave an origin that a
            // later move could activate as a selection on another page.
            self.drag_origin_screen = if self.drag_page.is_some() {
                origin
            } else {
                None
            };
            self.drag_active = false;
        }

        if self.drag_page == Some(page_index)
            && let (Some(origin), Some(position)) = (self.drag_origin_screen, pointer_position)
        {
            let drag_threshold = ui
                .ctx()
                .options(|options| options.input_options.max_click_dist);
            self.drag_active |= selection_drag_exceeds_threshold(origin, position, drag_threshold);
        }
        if self.drag_active && self.drag_page == Some(page_index) {
            self.drag_current = response
                .interact_pointer_pos()
                .or(pointer_position)
                .map(|position| page_point_from_screen(position, screen_rect, bounds))
                .and_then(|point| {
                    text_snapshot.and_then(|snapshot| snap_to_glyph(&snapshot.glyphs, point))
                });
        }

        self.paint_drag_preview(ui, screen_rect, page_index, bounds, text_snapshot);

        if primary_released && self.drag_page == Some(page_index) {
            let completed = self.drag_active.then(|| {
                self.drag_start
                    .zip(self.drag_current)
                    .map(|(start, end)| (page_index, start, end))
            });
            self.drag_page = None;
            self.drag_start = None;
            self.drag_current = None;
            self.drag_origin_screen = None;
            self.drag_active = false;
            interaction.completed_drag = completed.flatten();
        }
        if self.context_page == Some(page_index)
            && let Some(target) = &self.context_target
            && let Some(action) = show_annotation_context_menu(
                &response,
                target,
                can_create_highlight,
                secondary_released_here,
            )
        {
            interaction.annotation_action = Some(action);
        }
        interaction
    }

    fn paint_drag_preview(
        &self,
        ui: &Ui,
        screen_rect: Rect,
        page_index: usize,
        bounds: PageRect,
        text_snapshot: Option<&TextPageSnapshot>,
    ) {
        if !self.drag_active || self.drag_page != Some(page_index) {
            return;
        }
        let (Some(start), Some(current), Some(text_snapshot)) =
            (self.drag_start, self.drag_current, text_snapshot)
        else {
            return;
        };
        for quad in selected_display_quads(&text_snapshot.glyphs, start, current) {
            paint_quad(
                ui,
                screen_rect,
                bounds,
                quad,
                Color32::from_rgba_unmultiplied(255, 210, 0, 56),
                Stroke::NONE,
            );
        }
    }
}

fn selection_drag_exceeds_threshold(origin: Pos2, current: Pos2, threshold: f32) -> bool {
    // The threshold is egui's logical-point click tolerance, so the decision
    // remains consistent across native DPI and application zoom settings.
    origin.distance_sq(current) > threshold * threshold
}

/// Maps a raster tile's device-pixel window into normalized page-screen coordinates.
pub(crate) fn screen_rect_for_tile(page_screen_rect: Rect, tile: &RenderedTile) -> Rect {
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
    stroke: Stroke,
) {
    let points = vec![
        screen_point_from_page(quad.upper_left, screen_rect, bounds),
        screen_point_from_page(quad.upper_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_left, screen_rect, bounds),
    ];
    ui.painter()
        .add(Shape::convex_polygon(points, fill, stroke));
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

    use eframe::egui;

    use super::*;
    use crate::domain::annotation::{
        AnnotationId, AnnotationKind, AnnotationPageSnapshot, AnnotationSnapshot,
    };
    use crate::domain::document::TileSpec;
    use crate::domain::selection::GlyphSnapshot;

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

    fn text_snapshot() -> TextPageSnapshot {
        TextPageSnapshot {
            page_index: 0,
            revision: 0,
            glyphs: vec![GlyphSnapshot {
                character: 'A',
                quad: PageQuad {
                    upper_left: PagePoint::new(0.0, 0.0),
                    upper_right: PagePoint::new(20.0, 0.0),
                    lower_left: PagePoint::new(0.0, 20.0),
                    lower_right: PagePoint::new(20.0, 20.0),
                },
                line_index: 0,
            }],
        }
    }

    fn selection_frame(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
    ) -> Option<(usize, PagePoint, PagePoint)> {
        selection_frame_at(context, viewport, events, None)
    }

    fn selection_frame_at(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        time: Option<f64>,
    ) -> Option<(usize, PagePoint, PagePoint)> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
            events,
            time,
            ..Default::default()
        };
        let mut completed = None;
        let snapshot = text_snapshot();
        let _output = context.run_ui(input, |ui| {
            completed = viewport
                .interact_at(
                    ui,
                    PageInteractionInput {
                        screen_rect: Rect::from_min_size(
                            Pos2::new(20.0, 20.0),
                            [100.0, 100.0].into(),
                        ),
                        page_index: 0,
                        bounds: PageRect {
                            x0: 0.0,
                            y0: 0.0,
                            x1: 100.0,
                            y1: 100.0,
                        },
                        text_snapshot: Some(&snapshot),
                        selection: None,
                        annotation_page: None,
                        can_create_highlight: false,
                    },
                )
                .completed_drag;
        });
        completed
    }

    fn primary_button_event(position: Pos2, pressed: bool) -> egui::Event {
        pointer_button_event(position, PointerButton::Primary, pressed)
    }

    fn pointer_button_event(position: Pos2, button: PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn annotation_page(xrefs: &[i32]) -> AnnotationPageSnapshot {
        AnnotationPageSnapshot {
            page_index: 0,
            revision: 0,
            annotations: xrefs
                .iter()
                .map(|xref| AnnotationSnapshot {
                    id: AnnotationId {
                        page_index: 0,
                        xref: *xref,
                    },
                    kind: AnnotationKind::Highlight,
                    quads: vec![PageQuad {
                        upper_left: PagePoint::new(0.0, 0.0),
                        upper_right: PagePoint::new(20.0, 0.0),
                        lower_left: PagePoint::new(0.0, 20.0),
                        lower_right: PagePoint::new(20.0, 20.0),
                    }],
                    contents: String::new(),
                    color: None,
                    opacity: 1.0,
                    can_edit_contents: true,
                    can_edit_color: true,
                    can_delete: true,
                })
                .collect(),
        }
    }

    fn annotation_frame(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        time: f64,
        page: &AnnotationPageSnapshot,
    ) -> PageInteraction {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
            events,
            time: Some(time),
            ..Default::default()
        };
        let mut interaction = PageInteraction::default();
        let text = text_snapshot();
        let _output = context.run_ui(input, |ui| {
            interaction = viewport.interact_at(
                ui,
                PageInteractionInput {
                    screen_rect: Rect::from_min_size(Pos2::new(20.0, 20.0), [100.0, 100.0].into()),
                    page_index: 0,
                    bounds: PageRect {
                        x0: 0.0,
                        y0: 0.0,
                        x1: 100.0,
                        y1: 100.0,
                    },
                    text_snapshot: Some(&text),
                    selection: None,
                    annotation_page: Some(page),
                    can_create_highlight: true,
                },
            );
        });
        interaction
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

    #[test]
    fn primary_click_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(position),
                    primary_button_event(position, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![primary_button_event(position, false)],
            )
            .is_none()
        );
    }

    #[test]
    fn subthreshold_pointer_jitter_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let pressed = Pos2::new(25.0, 25.0);
        let jittered = Pos2::new(27.0, 26.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(pressed),
                    primary_button_event(pressed, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert_eq!(viewport.drag_origin_screen, Some(pressed));
        assert_eq!(
            context.options(|options| options.input_options.max_click_dist),
            6.0
        );
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![egui::Event::PointerMoved(jittered)],
            )
            .is_none()
        );
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![primary_button_event(jittered, false)],
            )
            .is_none()
        );
    }

    #[test]
    fn stationary_long_press_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);

        assert!(
            selection_frame_at(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(position),
                    primary_button_event(position, true),
                ],
                Some(0.0),
            )
            .is_none()
        );
        assert!(selection_frame_at(&context, &mut viewport, Vec::new(), Some(1.0)).is_none());
        assert!(
            selection_frame_at(
                &context,
                &mut viewport,
                vec![primary_button_event(position, false)],
                Some(1.01),
            )
            .is_none()
        );
    }

    #[test]
    fn intentional_short_drag_can_select_one_glyph() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let pressed = Pos2::new(25.0, 25.0);
        let dragged = Pos2::new(33.0, 25.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(pressed),
                    primary_button_event(pressed, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert_eq!(viewport.drag_origin_screen, Some(pressed));
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![egui::Event::PointerMoved(dragged)],
            )
            .is_none()
        );
        assert!(viewport.drag_active);
        let completed = selection_frame(
            &context,
            &mut viewport,
            vec![
                egui::Event::PointerMoved(dragged),
                primary_button_event(dragged, false),
            ],
        )
        .expect("a drag beyond egui's click tolerance should complete");

        assert_eq!(completed.0, 0);
        assert_eq!(completed.1, PagePoint::new(10.0, 10.0));
        assert_eq!(completed.2, PagePoint::new(10.0, 10.0));
    }

    #[test]
    fn drag_threshold_requires_distance_beyond_egui_click_tolerance() {
        let origin = Pos2::new(10.0, 10.0);

        assert!(!selection_drag_exceeds_threshold(
            origin,
            Pos2::new(16.0, 10.0),
            6.0,
        ));
        assert!(selection_drag_exceeds_threshold(
            origin,
            Pos2::new(16.01, 10.0),
            6.0,
        ));
    }

    #[test]
    fn annotation_double_click_opens_one_stable_id_without_selecting_text() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41]);
        let position = Pos2::new(25.0, 25.0);

        for (time, pressed) in [(0.0, true), (0.01, false), (0.1, true)] {
            let interaction = annotation_frame(
                &context,
                &mut viewport,
                vec![pointer_button_event(
                    position,
                    PointerButton::Primary,
                    pressed,
                )],
                time,
                &page,
            );
            assert!(interaction.completed_drag.is_none());
        }
        let interaction = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Primary,
                false,
            )],
            0.11,
            &page,
        );

        assert_eq!(
            interaction.annotation_action,
            Some(AnnotationUiAction::EditAnnotation(AnnotationId {
                page_index: 0,
                xref: 41,
            }))
        );
        assert!(interaction.completed_drag.is_none());
    }

    #[test]
    fn overlapping_annotation_double_click_requests_candidate_choice() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41, 52]);
        let position = Pos2::new(25.0, 25.0);

        for (time, pressed) in [(0.0, true), (0.01, false), (0.1, true)] {
            let _interaction = annotation_frame(
                &context,
                &mut viewport,
                vec![pointer_button_event(
                    position,
                    PointerButton::Primary,
                    pressed,
                )],
                time,
                &page,
            );
        }
        let interaction = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Primary,
                false,
            )],
            0.11,
            &page,
        );

        let Some(AnnotationUiAction::ChooseAnnotation(candidates)) = interaction.annotation_action
        else {
            panic!("overlapping annotations should preserve a candidate choice");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id.xref, 41);
        assert_eq!(candidates[1].id.xref, 52);
    }

    #[test]
    fn secondary_click_targets_annotation_without_starting_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41]);
        let position = Pos2::new(25.0, 25.0);

        let _pressed = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Secondary,
                true,
            )],
            0.0,
            &page,
        );
        let released = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Secondary,
                false,
            )],
            0.01,
            &page,
        );

        assert!(released.completed_drag.is_none());
        assert_eq!(viewport.drag_page, None);
        assert_eq!(viewport.context_page, Some(0));
        assert_eq!(
            viewport
                .context_target
                .as_ref()
                .map(|target| target.candidates[0].id.xref),
            Some(41)
        );
    }

    #[test]
    fn secondary_click_on_current_selection_enables_selection_actions_only() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);
        let selected_quad = PageQuad {
            upper_left: PagePoint::new(0.0, 0.0),
            upper_right: PagePoint::new(20.0, 0.0),
            lower_left: PagePoint::new(0.0, 20.0),
            lower_right: PagePoint::new(20.0, 20.0),
        };
        let selection = SelectionSnapshot {
            page_index: 0,
            generation: 1,
            text: "A".to_owned(),
            display_quads: vec![selected_quad],
            quads: vec![selected_quad],
            extraction_time: Duration::ZERO,
        };
        let text = text_snapshot();
        for (time, pressed) in [(0.0, true), (0.01, false)] {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
                events: vec![pointer_button_event(
                    position,
                    PointerButton::Secondary,
                    pressed,
                )],
                time: Some(time),
                ..Default::default()
            };
            let _output = context.run_ui(input, |ui| {
                let interaction = viewport.interact_at(
                    ui,
                    PageInteractionInput {
                        screen_rect: Rect::from_min_size(
                            Pos2::new(20.0, 20.0),
                            [100.0, 100.0].into(),
                        ),
                        page_index: 0,
                        bounds: PageRect {
                            x0: 0.0,
                            y0: 0.0,
                            x1: 100.0,
                            y1: 100.0,
                        },
                        text_snapshot: Some(&text),
                        selection: Some(&selection),
                        annotation_page: None,
                        can_create_highlight: true,
                    },
                );
                assert!(interaction.completed_drag.is_none());
            });
        }

        let target = viewport
            .context_target
            .as_ref()
            .expect("selection context target");
        assert!(target.selection_available);
        assert!(target.candidates.is_empty());
    }
}
