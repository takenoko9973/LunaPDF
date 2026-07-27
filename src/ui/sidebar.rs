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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HighlightRowGesture {
    PrimaryClick,
    PrimaryDoubleClick,
    EditMenu,
    DeleteMenu,
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

/// Maps one row gesture without letting read-only rows open a mutation path.
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
