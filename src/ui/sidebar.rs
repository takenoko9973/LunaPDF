use std::collections::BTreeMap;

use eframe::egui::{self, CursorIcon, Id, Rect, Sense, Ui, Vec2, WidgetInfo, WidgetType};

use crate::domain::annotation::AnnotationSummary;
use crate::domain::document::OutlineItem;
use crate::ui::annotation_editor::paint_color_swatch;

const COMMENT_HEAD_CHARACTERS: usize = 48;
// 18pt のスウォッチに5ptの垂直余白を加えると、行が読みやすくクリックしやすい。
const HIGHLIGHT_ROW_HEIGHT: f32 = 28.0;
// 4pt の余白なら、ジェスチャー行を狭めずにホバー用の余白を見える形で残せる。
const HIGHLIGHT_ROW_HORIZONTAL_PADDING: f32 = 4.0;
const HIGHLIGHT_ROW_SWATCH_SIZE: f32 = 18.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SidebarTab {
    Outline,
    Thumbnails,
    Highlights,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HighlightSidebarAction {
    Jump(usize),
    Edit(AnnotationSummary),
    Delete(AnnotationSummary),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HighlightRowGesture {
    PrimaryClick,
    PrimaryDoubleClick,
    EditMenu,
    DeleteMenu,
}

/// Rust 側が所有するアウトライン階層を描画し、選択されたページの対象を返す。
pub(crate) fn show_outline(ui: &mut Ui, items: &[OutlineItem]) -> Option<usize> {
    show_outline_level(ui, items, Id::new("pdf-outline-root"))
}

fn show_outline_level(ui: &mut Ui, items: &[OutlineItem], parent_id: Id) -> Option<usize> {
    let mut selected_page = None;
    for (index, item) in items.iter().enumerate() {
        let item_id = parent_id.with(index);
        if item.children.is_empty() {
            if outline_label(ui, item).clicked() {
                selected_page = item.page_index;
            }
            continue;
        }

        let response = egui::CollapsingHeader::new(&item.title)
            .id_salt(item_id)
            .show(ui, |ui| show_outline_level(ui, &item.children, item_id));
        if response.header_response.clicked() && item.page_index.is_some() {
            selected_page = item.page_index;
        }
        if response.body_returned.is_some() {
            selected_page = response.body_returned.flatten().or(selected_page);
        }
    }
    selected_page
}

fn outline_label(ui: &mut Ui, item: &OutlineItem) -> egui::Response {
    ui.add_enabled(
        item.page_index.is_some(),
        egui::Button::new(&item.title).frame(false),
    )
}

/// インデックス順のハイライトを描画し、選択された行を保持しない。
pub(crate) fn show_highlights(
    ui: &mut Ui,
    pages: &BTreeMap<usize, Vec<AnnotationSummary>>,
    total_pages: usize,
    in_progress: bool,
    error: Option<&str>,
) -> Option<HighlightSidebarAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if in_progress {
            ui.spinner();
        }
        ui.label(format!("{} / {total_pages}ページ", pages.len()));
    });
    if let Some(message) = error {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (page_index, highlights) in pages {
            for summary in highlights {
                ui.push_id((summary.id.page_index, summary.id.xref), |ui| {
                    let (row_rect, row_response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), HIGHLIGHT_ROW_HEIGHT),
                        Sense::click(),
                    );
                    let row = row_response.on_hover_cursor(CursorIcon::PointingHand);
                    if row.hovered() {
                        ui.painter().rect_filled(
                            row_rect,
                            2.0,
                            ui.visuals().widgets.hovered.bg_fill,
                        );
                    }
                    let row_label = paint_highlight_row(ui, row_rect, *page_index, summary);
                    // Painter の内容には子 Response がないため、すべてのポインタ操作で使う
                    // 行の Response 1つに意味を付与する。
                    row.widget_info(|| {
                        WidgetInfo::labeled(WidgetType::Button, true, row_label.clone())
                    });
                    let row = if summary.contents.trim().is_empty() {
                        row
                    } else {
                        row.on_hover_text(&summary.contents)
                    };

                    if row.double_clicked() {
                        action =
                            highlight_row_action(HighlightRowGesture::PrimaryDoubleClick, summary);
                    } else if row.clicked() {
                        action = highlight_row_action(HighlightRowGesture::PrimaryClick, summary);
                    }
                    row.context_menu(|ui| {
                        if ui
                            .add_enabled(
                                summary.can_edit_contents || summary.can_edit_color,
                                egui::Button::new("注釈を編集"),
                            )
                            .clicked()
                        {
                            action = highlight_row_action(HighlightRowGesture::EditMenu, summary);
                            ui.close();
                        }
                        if ui
                            .add_enabled(summary.can_delete, egui::Button::new("注釈を削除"))
                            .clicked()
                        {
                            action = highlight_row_action(HighlightRowGesture::DeleteMenu, summary);
                            ui.close();
                        }
                    });
                });
            }
        }
        if pages.len() == total_pages
            && pages.values().all(|highlights| highlights.is_empty())
            && error.is_none()
        {
            ui.label("ハイライトはありません");
        }
    });
    action
}

fn paint_highlight_row(
    ui: &mut Ui,
    row_rect: Rect,
    page_index: usize,
    summary: &AnnotationSummary,
) -> String {
    let item_spacing = ui.spacing().item_spacing.x;
    let (swatch_rect, text_rect) = highlight_row_content_rects(row_rect, item_spacing);
    paint_color_swatch(ui, swatch_rect, summary.color);

    let text = format!(
        "{}ページ  {}",
        page_index + 1,
        comment_head(&summary.contents)
    );
    let mut layout_job = egui::text::LayoutJob::single_section(
        text.clone(),
        egui::TextFormat {
            font_id: egui::TextStyle::Body.resolve(ui.style()),
            color: ui.visuals().text_color(),
            ..Default::default()
        },
    );
    layout_job.wrap = egui::epaint::text::TextWrapping::truncate_at_width(text_rect.width());
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(layout_job));
    // Painter が所有するテキストにより行の Response を正とし、固定ベースラインの
    // オフセットではなく `Galley` の矩形で垂直方向の中央を決める。
    let text_position = egui::Pos2::new(
        text_rect.left(),
        text_rect.center().y - galley.size().y / 2.0,
    );
    ui.painter()
        .with_clip_rect(text_rect)
        .galley(text_position, galley, ui.visuals().text_color());
    text
}

fn highlight_row_content_rects(row_rect: Rect, item_spacing: f32) -> (Rect, Rect) {
    let content_rect = row_rect.shrink2(Vec2::new(HIGHLIGHT_ROW_HORIZONTAL_PADDING, 0.0));
    let swatch_rect = Rect::from_center_size(
        egui::Pos2::new(
            content_rect.left() + HIGHLIGHT_ROW_SWATCH_SIZE / 2.0,
            content_rect.center().y,
        ),
        Vec2::splat(HIGHLIGHT_ROW_SWATCH_SIZE),
    );
    let text_rect = Rect::from_min_max(
        egui::Pos2::new(swatch_rect.right() + item_spacing, content_rect.top()),
        content_rect.right_bottom(),
    );
    (swatch_rect, text_rect)
}

/// 読み取り専用の行から変更経路を開かないように、1行のジェスチャーを対応付ける。
fn highlight_row_action(
    gesture: HighlightRowGesture,
    summary: &AnnotationSummary,
) -> Option<HighlightSidebarAction> {
    match gesture {
        HighlightRowGesture::PrimaryClick => {
            Some(HighlightSidebarAction::Jump(summary.id.page_index))
        }
        HighlightRowGesture::PrimaryDoubleClick | HighlightRowGesture::EditMenu
            if summary.can_edit_contents || summary.can_edit_color =>
        {
            Some(HighlightSidebarAction::Edit(summary.clone()))
        }
        HighlightRowGesture::DeleteMenu if summary.can_delete => {
            Some(HighlightSidebarAction::Delete(summary.clone()))
        }
        HighlightRowGesture::PrimaryDoubleClick
        | HighlightRowGesture::EditMenu
        | HighlightRowGesture::DeleteMenu => None,
    }
}

fn comment_head(contents: &str) -> String {
    let first_line = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty());
    let Some(first_line) = first_line else {
        return "コメントなし".to_owned();
    };
    let mut characters = first_line.chars();
    let head = characters
        .by_ref()
        .take(COMMENT_HEAD_CHARACTERS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::annotation::{AnnotationId, AnnotationKind};

    fn summary() -> AnnotationSummary {
        AnnotationSummary {
            id: AnnotationId {
                page_index: 4,
                xref: 17,
            },
            kind: AnnotationKind::Highlight,
            contents: "comment".to_owned(),
            color: None,
            can_edit_contents: true,
            can_edit_color: true,
            can_delete: true,
        }
    }

    #[test]
    fn comment_head_uses_only_the_first_line() {
        assert_eq!(comment_head("first\nsecond"), "first");
        assert_eq!(comment_head(" \nsecond"), "second");
        assert_eq!(comment_head(" \n\t"), "コメントなし");
        assert_eq!(comment_head(""), "コメントなし");
        assert_eq!(
            comment_head(&"a".repeat(COMMENT_HEAD_CHARACTERS + 1)),
            format!("{}…", "a".repeat(COMMENT_HEAD_CHARACTERS))
        );
    }

    #[test]
    fn row_content_uses_the_allocated_rows_vertical_center() {
        let row_rect = Rect::from_min_size(
            egui::Pos2::new(10.0, 20.0),
            Vec2::new(240.0, HIGHLIGHT_ROW_HEIGHT),
        );

        let (swatch_rect, text_rect) = highlight_row_content_rects(row_rect, 8.0);

        assert_eq!(swatch_rect.center().y, row_rect.center().y);
        assert_eq!(text_rect.center().y, row_rect.center().y);
        assert!(text_rect.left() > swatch_rect.right());
        assert_eq!(
            text_rect.right(),
            row_rect.right() - HIGHLIGHT_ROW_HORIZONTAL_PADDING
        );
    }

    #[test]
    fn row_gestures_keep_navigation_edit_and_delete_distinct() {
        let summary = summary();

        assert_eq!(
            highlight_row_action(HighlightRowGesture::PrimaryClick, &summary),
            Some(HighlightSidebarAction::Jump(4))
        );
        assert_eq!(
            highlight_row_action(HighlightRowGesture::PrimaryDoubleClick, &summary),
            Some(HighlightSidebarAction::Edit(summary.clone()))
        );
        assert_eq!(
            highlight_row_action(HighlightRowGesture::EditMenu, &summary),
            Some(HighlightSidebarAction::Edit(summary.clone()))
        );
        assert_eq!(
            highlight_row_action(HighlightRowGesture::DeleteMenu, &summary),
            Some(HighlightSidebarAction::Delete(summary))
        );
    }

    #[test]
    fn row_gestures_disable_unsupported_edit_and_delete_actions() {
        let mut summary = summary();
        summary.can_edit_contents = false;
        summary.can_edit_color = false;
        summary.can_delete = false;

        assert!(highlight_row_action(HighlightRowGesture::PrimaryDoubleClick, &summary).is_none());
        assert!(highlight_row_action(HighlightRowGesture::EditMenu, &summary).is_none());
        assert!(highlight_row_action(HighlightRowGesture::DeleteMenu, &summary).is_none());
        assert_eq!(
            highlight_row_action(HighlightRowGesture::PrimaryClick, &summary),
            Some(HighlightSidebarAction::Jump(4))
        );
    }
}
