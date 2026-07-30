//! PDF ビューポートへのカーソル適用。

use eframe::egui::{self, CursorIcon};

/// PDF の操作状態で選択された論理カーソルを適用する。
pub(crate) fn set_pdf_cursor(context: &egui::Context, icon: CursorIcon) {
    context.set_cursor_icon(icon);
}
