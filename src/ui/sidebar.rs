use eframe::egui::{self, Id, Ui};

use crate::domain::document::OutlineItem;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SidebarTab {
    Outline,
    Thumbnails,
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
