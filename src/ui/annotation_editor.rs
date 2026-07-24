use eframe::egui::{
    self, Button, Color32, Id, Order, Popup, Pos2, Rect, Response, RichText, ScrollArea,
    SetOpenCommand, Vec2,
};

use crate::domain::annotation::{AnnotationId, AnnotationSnapshot, PdfAnnotationColor};

// The panel is deliberately bounded in logical points. It remains usable on
// narrow windows by shrinking inside the view and scrolling its own contents.
const OVERLAY_PREFERRED_WIDTH: f32 = 320.0;
const OVERLAY_PREFERRED_HEIGHT: f32 = 480.0;
const OVERLAY_MARGIN: f32 = 12.0;

// Candidate labels show enough of a comment to distinguish ordinary notes
// without allowing a long external Contents value to dominate the menu width.
const ANNOTATION_LABEL_COMMENT_CHARS: usize = 24;

// These explicit RGB values form the editable UI palette; they are never used
// as inferred replacements when an existing PDF color cannot be read.
const COLOR_PRESETS: [(&str, [u8; 3]); 5] = [
    ("黄色", [255, 235, 59]),
    ("緑色", [76, 175, 80]),
    ("青色", [66, 133, 244]),
    ("赤色", [239, 83, 80]),
    ("紫色", [171, 71, 188]),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationMenuCandidate {
    pub(crate) id: AnnotationId,
    pub(crate) label: String,
    pub(crate) can_edit: bool,
    pub(crate) can_delete: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationContextTarget {
    pub(crate) selection_available: bool,
    pub(crate) candidates: Vec<AnnotationMenuCandidate>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AnnotationUiAction {
    CopySelection,
    CreateHighlight,
    EditAnnotation(AnnotationId),
    DeleteAnnotation(AnnotationId),
    ChooseAnnotation(Vec<AnnotationMenuCandidate>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnnotationOverlayPlacement {
    RightEdge,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationEditorValues {
    pub(crate) contents: String,
    pub(crate) color: Option<PdfAnnotationColor>,
}

#[derive(Clone, Debug)]
pub(crate) struct AnnotationEditorState {
    pub(crate) document_id: u64,
    pub(crate) revision: u64,
    pub(crate) annotation_id: AnnotationId,
    pub(crate) original: AnnotationEditorValues,
    pub(crate) buffer: AnnotationEditorValues,
    pub(crate) can_edit_contents: bool,
    pub(crate) can_edit_color: bool,
    pub(crate) can_delete: bool,
    pub(crate) stale: bool,
    pub(crate) placement: AnnotationOverlayPlacement,
    pub(crate) notice: Option<String>,
}

impl AnnotationEditorState {
    /// Creates an edit buffer without exposing the selected text as annotation content.
    pub(crate) fn from_snapshot(
        document_id: u64,
        revision: u64,
        annotation: &AnnotationSnapshot,
    ) -> Self {
        let values = AnnotationEditorValues {
            contents: annotation.contents.clone(),
            color: annotation.color,
        };
        Self {
            document_id,
            revision,
            annotation_id: annotation.id,
            original: values.clone(),
            buffer: values,
            can_edit_contents: annotation.can_edit_contents,
            can_edit_color: annotation.can_edit_color,
            can_delete: annotation.can_delete,
            stale: false,
            placement: AnnotationOverlayPlacement::RightEdge,
            notice: None,
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.buffer != self.original
    }

    pub(crate) fn can_save(&self) -> bool {
        let contents_allowed =
            self.buffer.contents == self.original.contents || self.can_edit_contents;
        let color_allowed = self.buffer.color == self.original.color || self.can_edit_color;
        // A mixed editable/read-only annotation is saved only when every
        // changed field is allowed; silently dropping one change is forbidden.
        !self.stale && self.is_dirty() && contents_allowed && color_allowed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnnotationEditorAction {
    Close,
    Save,
    Discard,
    Delete,
}

/// Converts backend snapshots into stable, UI-owned candidate rows.
pub(crate) fn annotation_menu_candidates(
    annotations: &[&AnnotationSnapshot],
) -> Vec<AnnotationMenuCandidate> {
    annotations
        .iter()
        .map(|annotation| AnnotationMenuCandidate {
            id: annotation.id,
            label: annotation_candidate_label(annotation),
            can_edit: annotation.can_edit_contents || annotation.can_edit_color,
            can_delete: annotation.can_delete,
        })
        .collect()
}

/// Shows the page context menu and returns only a stable-ID operation request.
pub(crate) fn show_annotation_context_menu(
    response: &Response,
    target: &AnnotationContextTarget,
    can_create_highlight: bool,
    open_requested: bool,
) -> Option<AnnotationUiAction> {
    let mut action = None;
    let open_command = if open_requested {
        Some(SetOpenCommand::Bool(true))
    } else if response.clicked() {
        Some(SetOpenCommand::Bool(false))
    } else {
        None
    };
    Popup::context_menu(response)
        .open_memory(open_command)
        .show(|ui| {
            if ui
                .add_enabled(target.selection_available, Button::new("コピー"))
                .clicked()
            {
                action = Some(AnnotationUiAction::CopySelection);
                ui.close();
            }
            if ui
                .add_enabled(
                    target.selection_available && can_create_highlight,
                    Button::new("ハイライト注釈を作成"),
                )
                .clicked()
            {
                action = Some(AnnotationUiAction::CreateHighlight);
                ui.close();
            }
            ui.separator();
            candidate_action_menu(
                ui,
                "注釈を編集",
                &target.candidates,
                |candidate| candidate.can_edit,
                AnnotationUiAction::EditAnnotation,
                &mut action,
            );
            candidate_action_menu(
                ui,
                "注釈を削除",
                &target.candidates,
                |candidate| candidate.can_delete,
                AnnotationUiAction::DeleteAnnotation,
                &mut action,
            );
        });
    action
}

fn candidate_action_menu(
    ui: &mut egui::Ui,
    label: &str,
    candidates: &[AnnotationMenuCandidate],
    enabled: impl Fn(&AnnotationMenuCandidate) -> bool,
    operation: impl Fn(AnnotationId) -> AnnotationUiAction,
    action: &mut Option<AnnotationUiAction>,
) {
    match candidates {
        [] => {
            ui.add_enabled(false, Button::new(label));
        }
        [candidate] => {
            if ui
                .add_enabled(enabled(candidate), Button::new(label))
                .clicked()
            {
                *action = Some(operation(candidate.id));
                ui.close();
            }
        }
        _ => {
            ui.add_enabled_ui(candidates.iter().any(&enabled), |ui| {
                ui.menu_button(label, |ui| {
                    for candidate in candidates {
                        if ui
                            .add_enabled(enabled(candidate), Button::new(&candidate.label))
                            .clicked()
                        {
                            *action = Some(operation(candidate.id));
                            ui.close();
                        }
                    }
                });
            });
        }
    }
}

/// Computes the right-edge overlay rectangle without participating in panel layout.
pub(crate) fn annotation_overlay_rect(bounds: Rect) -> Rect {
    let available_width = (bounds.width() - OVERLAY_MARGIN * 2.0).max(1.0);
    let available_height = (bounds.height() - OVERLAY_MARGIN * 2.0).max(1.0);
    let size = Vec2::new(
        OVERLAY_PREFERRED_WIDTH.min(available_width),
        OVERLAY_PREFERRED_HEIGHT.min(available_height),
    );
    Rect::from_min_size(
        Pos2::new(
            bounds.right() - OVERLAY_MARGIN - size.x,
            bounds.top() + OVERLAY_MARGIN,
        ),
        size,
    )
}

/// Draws the editor as a foreground Area so fit and scroll layout stay unchanged.
pub(crate) fn show_annotation_editor(
    context: &egui::Context,
    bounds: Rect,
    state: &mut AnnotationEditorState,
) -> Option<AnnotationEditorAction> {
    let rect = match state.placement {
        AnnotationOverlayPlacement::RightEdge => annotation_overlay_rect(bounds),
    };
    let mut action = None;
    egui::Area::new(Id::new(("annotation-editor", state.document_id)))
        .order(Order::Foreground)
        .fixed_pos(rect.min)
        .show(context, |ui| {
            ui.set_width(rect.width());
            egui::Frame::window(ui.style()).show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt(("annotation-editor-scroll", state.document_id))
                    .max_height(rect.height())
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("注釈の編集");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("×").on_hover_text("閉じる").clicked() {
                                        if state.is_dirty() {
                                            state.notice = Some(
                                                "未保存の変更があります。保存または変更を破棄してください。"
                                                    .to_owned(),
                                            );
                                        } else {
                                            action = Some(AnnotationEditorAction::Close);
                                        }
                                    }
                                },
                            );
                        });
                        ui.separator();
                        ui.label("コメント／メモ");
                        ui.add_enabled(
                            state.can_edit_contents && !state.stale,
                            egui::TextEdit::multiline(&mut state.buffer.contents)
                                .id(annotation_comment_id(
                                    state.document_id,
                                    state.annotation_id,
                                ))
                                .desired_rows(8)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(8.0);
                        ui.label("色");
                        color_menu(ui, state);
                        if let Some(notice) = &state.notice {
                            ui.colored_label(Color32::LIGHT_RED, notice);
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(state.can_save(), Button::new("保存"))
                                .clicked()
                            {
                                action = Some(AnnotationEditorAction::Save);
                            }
                            if ui.button("変更を破棄").clicked() {
                                action = Some(AnnotationEditorAction::Discard);
                            }
                        });
                        ui.separator();
                        if ui
                            .add_enabled(
                                state.can_delete && !state.stale,
                                Button::new("注釈を削除"),
                            )
                            .clicked()
                        {
                            action = Some(AnnotationEditorAction::Delete);
                        }
                    });
            });
        });
    action
}

pub(crate) fn annotation_comment_id(document_id: u64, annotation_id: AnnotationId) -> Id {
    Id::new((
        "annotation-comment",
        document_id,
        annotation_id.page_index,
        annotation_id.xref,
    ))
}

fn color_menu(ui: &mut egui::Ui, state: &mut AnnotationEditorState) {
    let label = color_label(state.buffer.color);
    let label_color = color_preview(state.buffer.color).unwrap_or(Color32::GRAY);
    ui.add_enabled_ui(state.can_edit_color && !state.stale, |ui| {
        ui.menu_button(
            RichText::new(format!("● {label}")).color(label_color),
            |ui| {
                for (name, rgb) in COLOR_PRESETS {
                    let preview = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    if ui
                        .button(RichText::new(format!("● {name}")).color(preview))
                        .clicked()
                    {
                        state.buffer.color = Some(rgb_color(rgb));
                        ui.close();
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("任意の色を選択…");
                    let mut rgb = color_preview(state.buffer.color)
                        .map(|color| [color.r(), color.g(), color.b()])
                        .unwrap_or(COLOR_PRESETS[0].1);
                    // The fallback only seeds the picker display. It becomes PDF
                    // data solely after an explicit user change, never on open.
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        state.buffer.color = Some(rgb_color(rgb));
                    }
                });
            },
        );
    });
}

fn annotation_candidate_label(annotation: &AnnotationSnapshot) -> String {
    let color = color_label(annotation.color);
    let first_line = annotation
        .contents
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    let mut comment = first_line
        .chars()
        .take(ANNOTATION_LABEL_COMMENT_CHARS)
        .collect::<String>();
    if first_line.chars().count() > ANNOTATION_LABEL_COMMENT_CHARS {
        comment.push('…');
    }
    if comment.is_empty() {
        comment = "コメントなし".to_owned();
    }
    format!("{color}・{comment}・ID {}", annotation.id.xref)
}

fn color_label(color: Option<PdfAnnotationColor>) -> String {
    let Some(color) = color else {
        return "色を読み取れません".to_owned();
    };
    if let Some((name, _)) = COLOR_PRESETS
        .iter()
        .find(|(_, rgb)| Some(rgb_color(*rgb)) == Some(color))
    {
        return (*name).to_owned();
    }
    match color {
        PdfAnnotationColor::Gray(gray) => format!("Gray {:.0}%", gray * 100.0),
        PdfAnnotationColor::Rgb { red, green, blue } => {
            let preview = normalized_rgb_to_color32(red, green, blue);
            format!("#{:02X}{:02X}{:02X}", preview.r(), preview.g(), preview.b())
        }
        PdfAnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        } => format!(
            "CMYK {:.0}/{:.0}/{:.0}/{:.0}%",
            cyan * 100.0,
            magenta * 100.0,
            yellow * 100.0,
            key * 100.0
        ),
    }
}

fn color_preview(color: Option<PdfAnnotationColor>) -> Option<Color32> {
    match color? {
        PdfAnnotationColor::Gray(gray) => Some(normalized_rgb_to_color32(gray, gray, gray)),
        PdfAnnotationColor::Rgb { red, green, blue } => {
            Some(normalized_rgb_to_color32(red, green, blue))
        }
        PdfAnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        } => {
            // This standard subtractive conversion is display-only. The
            // original CMYK components remain untouched until the user chooses a color.
            let red = (1.0 - cyan) * (1.0 - key);
            let green = (1.0 - magenta) * (1.0 - key);
            let blue = (1.0 - yellow) * (1.0 - key);
            Some(normalized_rgb_to_color32(red, green, blue))
        }
    }
}

fn normalized_rgb_to_color32(red: f32, green: f32, blue: f32) -> Color32 {
    fn channel(value: f32) -> u8 {
        // PDF color channels are normalized floats; clipping malformed
        // external values protects the preview without changing stored data.
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    }
    Color32::from_rgb(channel(red), channel(green), channel(blue))
}

fn rgb_color(rgb: [u8; 3]) -> PdfAnnotationColor {
    PdfAnnotationColor::Rgb {
        red: rgb[0] as f32 / 255.0,
        green: rgb[1] as f32 / 255.0,
        blue: rgb[2] as f32 / 255.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::annotation::AnnotationKind;

    fn annotation(contents: &str, color: Option<PdfAnnotationColor>) -> AnnotationSnapshot {
        AnnotationSnapshot {
            id: AnnotationId {
                page_index: 2,
                xref: 41,
            },
            kind: AnnotationKind::Highlight,
            quads: Vec::new(),
            contents: contents.to_owned(),
            color,
            opacity: 0.5,
            can_edit_contents: true,
            can_edit_color: true,
            can_delete: true,
        }
    }

    #[test]
    fn candidate_label_uses_comment_head_and_stable_id() {
        let annotation = annotation(
            "最初の行\n表示しない行",
            Some(PdfAnnotationColor::Rgb {
                red: 1.0,
                green: 235.0 / 255.0,
                blue: 59.0 / 255.0,
            }),
        );

        let rows = annotation_menu_candidates(&[&annotation]);

        assert_eq!(rows[0].label, "黄色・最初の行・ID 41");
    }

    #[test]
    fn editor_keeps_unreadable_color_unset_until_explicit_selection() {
        let annotation = annotation("", None);

        let editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);

        assert_eq!(editor.original.color, None);
        assert_eq!(editor.buffer.color, None);
        assert!(!editor.is_dirty());
    }

    #[test]
    fn overlay_stays_inside_narrow_bounds() {
        let bounds = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(180.0, 220.0));

        let overlay = annotation_overlay_rect(bounds);

        assert!(bounds.contains_rect(overlay));
        assert_eq!(overlay.right(), bounds.right() - OVERLAY_MARGIN);
    }
}
