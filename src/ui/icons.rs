use eframe::egui::{Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

const ICON_SIZE: f32 = 18.0;
const BUTTON_SIZE: f32 = 28.0;

#[derive(Clone, Copy)]
pub(crate) enum ToolbarIcon {
    Open,
    Print,
    Sidebar,
    Previous,
    Next,
    ZoomOut,
    ZoomIn,
    FitWidth,
    FitPage,
    Continuous,
    SinglePage,
    Highlight,
    Undo,
    Outline,
    Thumbnails,
}

/// Draws a vector toolbar button so its meaning does not depend on emoji fonts.
pub(crate) fn icon_button(
    ui: &mut Ui,
    icon: ToolbarIcon,
    enabled: bool,
    selected: bool,
    tooltip: &str,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(Vec2::splat(BUTTON_SIZE), sense);
    response = response.on_hover_text(tooltip);

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let background = if selected {
            ui.visuals().selection.bg_fill
        } else {
            visuals.bg_fill
        };
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            background,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
        let color = if enabled {
            visuals.fg_stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        paint_icon(
            ui,
            icon,
            rect.shrink((BUTTON_SIZE - ICON_SIZE) / 2.0),
            color,
        );
    }
    response
}

fn paint_icon(ui: &Ui, icon: ToolbarIcon, rect: Rect, color: Color32) {
    let painter = ui.painter();
    let stroke = Stroke::new(1.6, color);
    let x0 = rect.left();
    let x1 = rect.right();
    let y0 = rect.top();
    let y1 = rect.bottom();
    let cx = rect.center().x;
    let cy = rect.center().y;
    let p = |x: f32, y: f32| Pos2::new(x, y);

    match icon {
        ToolbarIcon::Open => {
            painter.line_segment([p(x0 + 1.0, y0 + 6.0), p(x0 + 6.0, y0 + 6.0)], stroke);
            painter.line_segment([p(x0 + 6.0, y0 + 6.0), p(x0 + 8.0, y0 + 3.0)], stroke);
            painter.line_segment([p(x0 + 8.0, y0 + 3.0), p(x1 - 1.0, y0 + 3.0)], stroke);
            painter.line_segment([p(x0 + 1.0, y0 + 6.0), p(x0 + 1.0, y1 - 2.0)], stroke);
            painter.line_segment([p(x0 + 1.0, y1 - 2.0), p(x1 - 3.0, y1 - 2.0)], stroke);
            painter.line_segment([p(x1 - 3.0, y1 - 2.0), p(x1 - 1.0, y0 + 8.0)], stroke);
            painter.line_segment([p(x1 - 1.0, y0 + 8.0), p(x0 + 4.0, y0 + 8.0)], stroke);
            painter.line_segment([p(x0 + 4.0, y0 + 8.0), p(x0 + 1.0, y1 - 2.0)], stroke);
        }
        ToolbarIcon::Print => {
            painter.rect_stroke(
                Rect::from_min_max(p(x0 + 4.0, y0 + 1.0), p(x1 - 4.0, y0 + 7.0)),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_min_max(p(x0 + 1.0, y0 + 6.0), p(x1 - 1.0, y1 - 4.0)),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_min_max(p(x0 + 4.0, y1 - 8.0), p(x1 - 4.0, y1 - 1.0)),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
        }
        ToolbarIcon::Sidebar => {
            painter.rect_stroke(rect.shrink(1.0), 1.0, stroke, StrokeKind::Inside);
            painter.line_segment([p(x0 + 6.0, y0 + 1.0), p(x0 + 6.0, y1 - 1.0)], stroke);
        }
        ToolbarIcon::Previous => {
            painter.line_segment([p(x1 - 4.0, y0 + 2.0), p(x0 + 4.0, cy)], stroke);
            painter.line_segment([p(x0 + 4.0, cy), p(x1 - 4.0, y1 - 2.0)], stroke);
        }
        ToolbarIcon::Next => {
            painter.line_segment([p(x0 + 4.0, y0 + 2.0), p(x1 - 4.0, cy)], stroke);
            painter.line_segment([p(x1 - 4.0, cy), p(x0 + 4.0, y1 - 2.0)], stroke);
        }
        ToolbarIcon::ZoomOut | ToolbarIcon::ZoomIn => {
            painter.circle_stroke(p(cx - 2.0, cy - 2.0), 5.0, stroke);
            painter.line_segment([p(cx + 2.0, cy + 2.0), p(x1 - 1.0, y1 - 1.0)], stroke);
            painter.line_segment([p(cx - 5.0, cy - 2.0), p(cx + 1.0, cy - 2.0)], stroke);
            if matches!(icon, ToolbarIcon::ZoomIn) {
                painter.line_segment([p(cx - 2.0, cy - 5.0), p(cx - 2.0, cy + 1.0)], stroke);
            }
        }
        ToolbarIcon::FitWidth => {
            painter.rect_stroke(rect.shrink(3.0), 0.0, stroke, StrokeKind::Inside);
            painter.line_segment([p(x0 + 1.0, cy), p(x1 - 1.0, cy)], stroke);
            painter.line_segment([p(x0 + 1.0, cy), p(x0 + 4.0, cy - 3.0)], stroke);
            painter.line_segment([p(x0 + 1.0, cy), p(x0 + 4.0, cy + 3.0)], stroke);
            painter.line_segment([p(x1 - 1.0, cy), p(x1 - 4.0, cy - 3.0)], stroke);
            painter.line_segment([p(x1 - 1.0, cy), p(x1 - 4.0, cy + 3.0)], stroke);
        }
        ToolbarIcon::FitPage => {
            painter.rect_stroke(rect.shrink(2.0), 0.0, stroke, StrokeKind::Inside);
            painter.line_segment([p(cx, y0), p(cx, y0 + 5.0)], stroke);
            painter.line_segment([p(cx, y0), p(cx - 3.0, y0 + 3.0)], stroke);
            painter.line_segment([p(cx, y0), p(cx + 3.0, y0 + 3.0)], stroke);
            painter.line_segment([p(cx, y1), p(cx, y1 - 5.0)], stroke);
            painter.line_segment([p(cx, y1), p(cx - 3.0, y1 - 3.0)], stroke);
            painter.line_segment([p(cx, y1), p(cx + 3.0, y1 - 3.0)], stroke);
        }
        ToolbarIcon::Continuous => {
            for offset in [0.0, 6.0, 12.0] {
                painter.rect_stroke(
                    Rect::from_min_size(p(x0 + 3.0, y0 + offset), Vec2::new(12.0, 4.0)),
                    0.0,
                    stroke,
                    StrokeKind::Inside,
                );
            }
        }
        ToolbarIcon::SinglePage => {
            painter.rect_stroke(rect.shrink(3.0), 0.0, stroke, StrokeKind::Inside);
        }
        ToolbarIcon::Highlight => {
            painter.line_segment([p(x0 + 3.0, y1 - 2.0), p(x1 - 2.0, y0 + 3.0)], stroke);
            painter.line_segment([p(x0 + 1.0, y1), p(x0 + 6.0, y1)], Stroke::new(3.0, color));
        }
        ToolbarIcon::Undo => {
            painter.line_segment([p(x0 + 2.0, cy), p(x0 + 7.0, y0 + 4.0)], stroke);
            painter.line_segment([p(x0 + 2.0, cy), p(x0 + 7.0, y1 - 4.0)], stroke);
            painter.line_segment([p(x0 + 2.0, cy), p(cx + 2.0, cy)], stroke);
            painter.circle_stroke(p(cx + 2.0, cy + 3.0), 6.0, stroke);
        }
        ToolbarIcon::Outline => {
            for offset in [3.0, 8.0, 13.0] {
                painter.circle_filled(p(x0 + 3.0, y0 + offset), 1.2, color);
                painter.line_segment([p(x0 + 6.0, y0 + offset), p(x1 - 1.0, y0 + offset)], stroke);
            }
        }
        ToolbarIcon::Thumbnails => {
            for row in 0..2 {
                for column in 0..2 {
                    let min = p(x0 + 2.0 + column as f32 * 8.0, y0 + 2.0 + row as f32 * 8.0);
                    painter.rect_stroke(
                        Rect::from_min_size(min, Vec2::splat(6.0)),
                        0.0,
                        stroke,
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }
}
