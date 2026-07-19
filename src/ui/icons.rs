use eframe::egui::{Color32, Rect, Response, Sense, Ui, Vec2};

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
    Outline,
    Thumbnails,
}

impl ToolbarIcon {
    fn source(self) -> eframe::egui::ImageSource<'static> {
        match self {
            Self::Open => eframe::egui::include_image!("../../assets/icons/open.svg"),
            Self::Print => eframe::egui::include_image!("../../assets/icons/print.svg"),
            Self::Sidebar => eframe::egui::include_image!("../../assets/icons/sidebar.svg"),
            Self::Previous => eframe::egui::include_image!("../../assets/icons/previous.svg"),
            Self::Next => eframe::egui::include_image!("../../assets/icons/next.svg"),
            Self::ZoomOut => eframe::egui::include_image!("../../assets/icons/zoom-out.svg"),
            Self::ZoomIn => eframe::egui::include_image!("../../assets/icons/zoom-in.svg"),
            Self::FitWidth => eframe::egui::include_image!("../../assets/icons/fit-width.svg"),
            Self::FitPage => eframe::egui::include_image!("../../assets/icons/fit-page.svg"),
            Self::Continuous => {
                eframe::egui::include_image!("../../assets/icons/continuous.svg")
            }
            Self::SinglePage => {
                eframe::egui::include_image!("../../assets/icons/single-page.svg")
            }
            Self::Highlight => eframe::egui::include_image!("../../assets/icons/highlight.svg"),
            Self::Outline => eframe::egui::include_image!("../../assets/icons/outline.svg"),
            Self::Thumbnails => {
                eframe::egui::include_image!("../../assets/icons/thumbnails.svg")
            }
        }
    }
}

/// Allocates a fixed toolbar click area, paints its centered SVG, and returns its response.
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
        // Keep rest and disabled buttons transparent; only interaction states paint a fill.
        let background = if !enabled {
            Color32::TRANSPARENT
        } else if response.is_pointer_button_down_on() {
            ui.visuals().widgets.active.bg_fill
        } else if selected {
            ui.visuals().selection.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, background);

        let color = if enabled {
            visuals.fg_stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        let image = eframe::egui::Image::new(icon.source())
            .fit_to_exact_size(Vec2::splat(ICON_SIZE))
            // SVG currentColor resolves to white so egui's multiplicative tint can supply
            // the active theme color without theme-specific icon files.
            .tint(color);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(ICON_SIZE));
        image.paint_at(ui, icon_rect);
    }
    response
}
