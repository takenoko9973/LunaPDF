use eframe::egui::{Color32, Rect, Response, Sense, Ui, Vec2};

const ICON_SIZE: f32 = 18.0;
// Sumatra は18pxのアイコンを基準に小さな垂直パディングだけを加える。24ptの
// コントロールは同程度の比率を保ちつつ、実用的なクリック領域を確保する。
pub(crate) const TOOLBAR_CONTROL_HEIGHT: f32 = 24.0;

macro_rules! embedded_icon {
    ($name:literal) => {
        eframe::egui::ImageSource::Bytes {
            uri: std::borrow::Cow::Borrowed(concat!("bytes://assets/icons/", $name, ".svg")),
            bytes: eframe::egui::load::Bytes::Static(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                ".svg"
            ))),
        }
    };
}

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
            Self::Open => embedded_icon!("open"),
            Self::Print => embedded_icon!("print"),
            Self::Sidebar => embedded_icon!("sidebar"),
            Self::Previous => embedded_icon!("previous"),
            Self::Next => embedded_icon!("next"),
            Self::ZoomOut => embedded_icon!("zoom-out"),
            Self::ZoomIn => embedded_icon!("zoom-in"),
            Self::FitWidth => embedded_icon!("fit-width"),
            Self::FitPage => embedded_icon!("fit-page"),
            Self::Continuous => embedded_icon!("continuous"),
            Self::SinglePage => embedded_icon!("single-page"),
            Self::Highlight => embedded_icon!("highlight"),
            Self::Outline => embedded_icon!("outline"),
            Self::Thumbnails => embedded_icon!("thumbnails"),
        }
    }
}

/// 固定されたツールバーのクリック領域を確保し、中央に SVG を描画して Response を返す。
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
    let (rect, mut response) = ui.allocate_exact_size(Vec2::splat(TOOLBAR_CONTROL_HEIGHT), sense);
    response = response.on_hover_text(tooltip);

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        // 通常状態と無効状態のボタンは透明に保ち、操作状態だけ塗りつぶす。
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
            // SVG の `currentColor` は白に解決されるため、egui の乗算色調で
            // テーマ別のアイコンファイルなしに現在のテーマ色を与えられる。
            .tint(color);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(ICON_SIZE));
        image.paint_at(ui, icon_rect);
    }
    response
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_toolbar_icon_has_embedded_bytes_and_a_stable_asset_uri() {
        let icons = [
            ToolbarIcon::Open,
            ToolbarIcon::Print,
            ToolbarIcon::Sidebar,
            ToolbarIcon::Previous,
            ToolbarIcon::Next,
            ToolbarIcon::ZoomOut,
            ToolbarIcon::ZoomIn,
            ToolbarIcon::FitWidth,
            ToolbarIcon::FitPage,
            ToolbarIcon::Continuous,
            ToolbarIcon::SinglePage,
            ToolbarIcon::Highlight,
            ToolbarIcon::Outline,
            ToolbarIcon::Thumbnails,
        ];
        let mut uris = HashSet::new();

        for icon in icons {
            let eframe::egui::ImageSource::Bytes { uri, bytes } = icon.source() else {
                panic!("toolbar icons must remain compile-time embedded bytes");
            };
            assert!(uri.starts_with("bytes://assets/icons/"));
            assert!(uri.ends_with(".svg"));
            assert!(!uri.contains(".."));
            assert!(uris.insert(uri.into_owned()));
            let eframe::egui::load::Bytes::Static(bytes) = bytes else {
                panic!("toolbar icon bytes must have static storage");
            };
            assert!(!bytes.is_empty());
        }
    }
}
