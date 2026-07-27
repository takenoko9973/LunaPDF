use std::collections::BTreeMap;

use eframe::egui::{self, Id, Sense, Ui};

use crate::domain::annotation::AnnotationSummary;
use crate::domain::document::OutlineItem;
use crate::ui::annotation_editor::color_swatch;

const COMMENT_HEAD_CHARACTERS: usize = 48;

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

/// Draws the Rust-owned outline hierarchy and returns a selected page target.
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

/// Draws progressively indexed Highlights without retaining a selected row.
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
                    let row = ui
                        .horizontal(|ui| {
                            color_swatch(ui, summary.color, 18.0);
                            ui.label(format!("{}ページ", page_index + 1));
                            ui.label(comment_head(&summary.contents));
                        })
                        .response
                        .interact(Sense::click());
                    let row = if summary.contents.trim().is_empty() {
                        row
                    } else {
                        row.on_hover_text(&summary.contents)
                    };

                    if row.double_clicked() {
                        if summary.can_edit_contents || summary.can_edit_color {
                            action = Some(HighlightSidebarAction::Edit(summary.clone()));
                        }
                    } else if row.clicked() {
                        action = Some(HighlightSidebarAction::Jump(*page_index));
                    }
                    row.context_menu(|ui| {
                        if ui
                            .add_enabled(
                                summary.can_edit_contents || summary.can_edit_color,
                                egui::Button::new("注釈を編集"),
                            )
                            .clicked()
                        {
                            action = Some(HighlightSidebarAction::Edit(summary.clone()));
                            ui.close();
                        }
                        if ui
                            .add_enabled(summary.can_delete, egui::Button::new("注釈を削除"))
                            .clicked()
                        {
                            action = Some(HighlightSidebarAction::Delete(summary.clone()));
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

fn comment_head(contents: &str) -> String {
    let first_line = contents.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        "コメントなし".to_owned()
    } else {
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
}

#[cfg(test)]
mod tests {
    use super::{COMMENT_HEAD_CHARACTERS, comment_head};

    #[test]
    fn comment_head_uses_only_the_first_line() {
        assert_eq!(comment_head("first\nsecond"), "first");
        assert_eq!(comment_head(" \nsecond"), "コメントなし");
        assert_eq!(comment_head(""), "コメントなし");
        assert_eq!(
            comment_head(&"a".repeat(COMMENT_HEAD_CHARACTERS + 1)),
            format!("{}…", "a".repeat(COMMENT_HEAD_CHARACTERS))
        );
    }
}
