//! Cursor application for the PDF viewport.

use eframe::egui::{self, CursorIcon};

/// Applies the logical cursor selected by the PDF interaction state.
pub(crate) fn set_pdf_cursor(context: &egui::Context, icon: CursorIcon) {
    context.set_cursor_icon(icon);
}
