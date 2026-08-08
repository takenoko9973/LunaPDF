use eframe::egui::containers::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align2, Button, Color32, CursorIcon, Id, Order, Popup, PopupCloseBehavior, Pos2, Rect,
    Response, ScrollArea, Sense, SetOpenCommand, Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType,
};

use crate::domain::annotation::{
    AnnotationId, AnnotationSnapshot, AnnotationSummary, AnnotationUpdateRequest,
    PdfAnnotationColor,
};
use crate::domain::session::MAX_RECENT_ANNOTATION_COLORS;

// パネルは意図的に論理ポイント内に収める。狭いウィンドウでもビュー内で縮小し、
// 内容自体をスクロールすることで利用できる。
const OVERLAY_PREFERRED_WIDTH: f32 = 320.0;
const OVERLAY_PREFERRED_HEIGHT: f32 = 480.0;
const OVERLAY_MARGIN: f32 = 12.0;

// 候補ラベルには通常のメモを区別できるだけのコメントを表示し、外部の長い Contents 値が
// メニュー幅を支配しないようにする。
const ANNOTATION_LABEL_COMMENT_CHARS: usize = 24;
const COLOR_SWATCH_SIZE: f32 = 18.0;
// 大きめの対象で小さなスウォッチをクリックしやすくしつつ、内側の18ptの色領域は
// 注釈一覧と一貫させる。
const COLOR_CHOICE_SIZE: f32 = 28.0;
const COLOR_TRIGGER_HEIGHT: f32 = 32.0;
const COLOR_TRIGGER_HORIZONTAL_PADDING: f32 = 8.0;
const COLOR_TRIGGER_ARROW_HALF_SIZE: f32 = 3.0;
const COLOR_PICKER_SLIDER_WIDTH: f32 = 220.0;
// 12pt のチェックは18ptのスウォッチ内でも判読できる。3ptの暗い輪郭を1.5ptの明るい
// ストロークの下に描くことで、すべてのプリセット色で見えるようにする。
const COLOR_CHECK_SIZE: f32 = 12.0;
const COLOR_CHECK_OUTLINE_WIDTH: f32 = 3.0;
const COLOR_CHECK_FOREGROUND_WIDTH: f32 = 1.5;
// これらの論理寸法は指定された28–32ptの対象と14–16ptのアイコンに従う。
const EDITOR_CLOSE_BUTTON_SIZE: f32 = 30.0;
const EDITOR_CLOSE_ICON_SIZE: f32 = 15.0;
const EDITOR_CLOSE_ICON_STROKE: f32 = 1.5;
// ヘッダーは30ポイントの閉じる対象と区切りの間隔を確保し、通常の320x480エディター内容で
// 本体にスクロールバーを出さずに済むだけの空間を残す。
const EDITOR_HEADER_RESERVED_HEIGHT: f32 = 42.0;

// これらの明示的な RGB 値は編集可能な UI パレットを構成する。既存の PDF 色を読み取れない
// 場合でも、推測した置換値として使うことはない。
const COLOR_PRESETS: [(&str, [u8; 3]); 10] = [
    ("黄色", [255, 255, 0]),
    ("緑色", [76, 175, 80]),
    ("シアン", [0, 188, 212]),
    ("青色", [66, 133, 244]),
    ("紫色", [171, 71, 188]),
    ("ピンク", [233, 30, 99]),
    ("赤色", [239, 83, 80]),
    ("オレンジ", [255, 152, 0]),
    ("グレー", [128, 128, 128]),
    ("白色", [255, 255, 255]),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationMenuCandidate {
    pub(crate) id: AnnotationId,
    pub(crate) label: String,
    pub(crate) color: Option<PdfAnnotationColor>,
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
    pub(crate) mutation_in_flight: bool,
    pub(crate) placement: AnnotationOverlayPlacement,
    pub(crate) notice: Option<String>,
    pub(crate) custom_color_draft: Option<[u8; 3]>,
}

impl AnnotationEditorState {
    /// 選択テキストを注釈内容として露出させずに編集バッファを作成する。
    #[cfg(test)]
    pub(crate) fn from_snapshot(
        document_id: u64,
        revision: u64,
        annotation: &AnnotationSnapshot,
    ) -> Self {
        Self::from_summary(document_id, revision, &annotation.summary())
    }

    /// ページジオメトリを待たず、サイドバーのメタデータからエディターを作成する。
    pub(crate) fn from_summary(
        document_id: u64,
        revision: u64,
        annotation: &AnnotationSummary,
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
            mutation_in_flight: false,
            placement: AnnotationOverlayPlacement::RightEdge,
            notice: None,
            custom_color_draft: None,
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.buffer != self.original || self.custom_color_draft.is_some()
    }

    fn buffer_is_dirty(&self) -> bool {
        self.buffer != self.original
    }

    pub(crate) fn can_save(&self) -> bool {
        let contents_allowed =
            self.buffer.contents == self.original.contents || self.can_edit_contents;
        let color_allowed = self.buffer.color == self.original.color
            || (self.can_edit_color && self.buffer.color.is_some());
        // 編集可能と読み取り専用が混在する注釈は、変更したすべてのフィールドが許可される
        // 場合にだけ保存する。変更を1つ黙って破棄することは禁止する。
        !self.stale
            && !self.mutation_in_flight
            && self.custom_color_draft.is_none()
            && self.buffer_is_dirty()
            && contents_allowed
            && color_allowed
    }

    /// ユーザーが実際に変更したフィールドから、リビジョンに紐付く最小パッチを構築する。
    pub(crate) fn update_request(&self) -> Option<AnnotationUpdateRequest> {
        if !self.can_save() {
            return None;
        }
        let contents =
            (self.buffer.contents != self.original.contents).then(|| self.buffer.contents.clone());
        let color = if self.buffer.color != self.original.color {
            self.buffer.color
        } else {
            None
        };
        Some(AnnotationUpdateRequest {
            id: self.annotation_id,
            expected_revision: self.revision,
            contents,
            color,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnnotationEditorAction {
    Close,
    Save,
    Discard,
    Delete,
}

/// バックエンドのスナップショットを安定した UI 所有の候補行へ変換する。
pub(crate) fn annotation_menu_candidates(
    annotations: &[&AnnotationSnapshot],
) -> Vec<AnnotationMenuCandidate> {
    annotations
        .iter()
        .map(|annotation| AnnotationMenuCandidate {
            id: annotation.id,
            label: annotation_candidate_label(annotation),
            color: annotation.color,
            can_edit: annotation.can_edit_contents || annotation.can_edit_color,
            can_delete: annotation.can_delete,
        })
        .collect()
}

/// 選択されたページテキストで置き換えず、保存済みで空でないコメントを返す。
pub(crate) fn annotation_hover_comments<'a>(
    annotations: &[&'a AnnotationSnapshot],
) -> Vec<&'a str> {
    annotations
        .iter()
        .map(|annotation| annotation.contents.as_str())
        .filter(|contents| !contents.trim().is_empty())
        .collect()
}

/// ページのコンテキストメニューを表示し、安定した ID による操作要求だけを返す。
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
                        if show_annotation_candidate_button(ui, candidate, enabled(candidate))
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

/// スウォッチと通常の前景テキストを使って1つの注釈候補を描画する。
pub(crate) fn show_annotation_candidate_button(
    ui: &mut egui::Ui,
    candidate: &AnnotationMenuCandidate,
    enabled: bool,
) -> Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            color_swatch(ui, candidate.color, COLOR_SWATCH_SIZE);
            ui.add(Button::new(&candidate.label).frame(false))
        })
        .inner
    })
    .inner
}

/// パネルのレイアウトに参加せず、右端のオーバーレイ矩形を計算する。
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

/// フィットとスクロールのレイアウトを変えないよう、エディターを前景 Area として描画する。
pub(crate) fn show_annotation_editor(
    context: &egui::Context,
    bounds: Rect,
    state: &mut AnnotationEditorState,
    recent_annotation_colors: &mut Vec<[u8; 3]>,
    editing_enabled: bool,
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
            ui.set_max_height(rect.height());
            let frame_margin_height = ui.style().spacing.window_margin.sum().y;
            egui::Frame::window(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("注釈の編集");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (close_rect, close_response) = ui.allocate_exact_size(
                            Vec2::splat(EDITOR_CLOSE_BUTTON_SIZE),
                            Sense::click(),
                        );
                        let close_response = close_response
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text("閉じる");
                        if close_response.hovered() {
                            ui.painter().rect_filled(
                                close_rect,
                                2.0,
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        let stroke = Stroke::new(
                            EDITOR_CLOSE_ICON_STROKE,
                            ui.visuals().widgets.inactive.fg_stroke.color,
                        );
                        let center = close_rect.center();
                        let offset = EDITOR_CLOSE_ICON_SIZE / 2.0;
                        ui.painter().line_segment(
                            [center + Vec2::splat(-offset), center + Vec2::splat(offset)],
                            stroke,
                        );
                        ui.painter().line_segment(
                            [
                                center + Vec2::new(-offset, offset),
                                center + Vec2::new(offset, -offset),
                            ],
                            stroke,
                        );
                        if close_response.clicked() {
                            if state.mutation_in_flight {
                                state.notice = Some("注釈処理の完了を待っています。".to_owned());
                            } else if state.is_dirty() {
                                state.notice = Some(
                                    "未保存の変更があります。保存または変更を破棄してください。"
                                        .to_owned(),
                                );
                            } else {
                                action = Some(AnnotationEditorAction::Close);
                            }
                        }
                    });
                });
                ui.separator();
                ScrollArea::vertical()
                    .id_salt(("annotation-editor-scroll", state.document_id))
                    .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
                    // スクロールするのは本体だけで、タイトルと閉じる対象は表示したままにする。
                    .max_height(
                        (rect.height() - frame_margin_height - EDITOR_HEADER_RESERVED_HEIGHT)
                            .max(1.0),
                    )
                    .show(ui, |ui| {
                        ui.label("コメント／メモ");
                        let contents_enabled = editing_enabled
                            && state.can_edit_contents
                            && !state.stale
                            && !state.mutation_in_flight;
                        // 既存フォーカスへ届く入力は `add_enabled(false, TextEdit)` だけでは
                        // 保持されることがある。外部再読み込み中は作業コピーにだけ渡し、
                        // 状態バッファへ書き戻さないことで同フレームの変更を遮断する。
                        let mut contents = state.buffer.contents.clone();
                        ui.add_enabled(
                            contents_enabled,
                            egui::TextEdit::multiline(&mut contents)
                                .id(annotation_comment_id(
                                    state.document_id,
                                    state.annotation_id,
                                ))
                                .desired_rows(8)
                                .desired_width(f32::INFINITY),
                        );
                        if contents_enabled {
                            state.buffer.contents = contents;
                        }
                        ui.add_space(8.0);
                        ui.label("色");
                        color_menu(ui, state, recent_annotation_colors, editing_enabled);
                        if let Some(notice) = &state.notice {
                            ui.colored_label(Color32::LIGHT_RED, notice);
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    editing_enabled && state.can_save(),
                                    Button::new("保存"),
                                )
                                .clicked()
                            {
                                action = Some(AnnotationEditorAction::Save);
                            }
                            if ui
                                .add_enabled(!state.mutation_in_flight, Button::new("変更を破棄"))
                                .clicked()
                            {
                                action = Some(AnnotationEditorAction::Discard);
                            }
                        });
                        ui.separator();
                        if ui
                            .add_enabled(
                                editing_enabled
                                    && state.can_delete
                                    && !state.stale
                                    && !state.mutation_in_flight,
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

fn color_menu(
    ui: &mut egui::Ui,
    state: &mut AnnotationEditorState,
    recent_annotation_colors: &mut Vec<[u8; 3]>,
    editing_enabled: bool,
) {
    let enabled =
        editing_enabled && state.can_edit_color && !state.stale && !state.mutation_in_flight;
    // 権限またはリビジョン状態が変わった後は下書きを適用できないため、
    // 古い変更をピッカーから通さずに破棄する。
    if !enabled {
        state.custom_color_draft = None;
    }
    ui.add_enabled_ui(enabled, |ui| {
        let trigger = color_trigger(ui, state.buffer.color);
        let popup_id = Popup::default_response_id(&trigger);
        let popup_was_open = Popup::is_id_open(ui.ctx(), popup_id);
        if !enabled {
            Popup::close_id(ui.ctx(), popup_id);
        }
        Popup::menu(&trigger)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.label("プリセット");
                for row in 0..2 {
                    ui.horizontal(|ui| {
                        for col in 0..5 {
                            let (name, rgb) = COLOR_PRESETS[row * 5 + col];
                            show_color_choice(ui, name, rgb, state, recent_annotation_colors);
                        }
                    });
                }
                if !recent_annotation_colors.is_empty() {
                    ui.separator();
                    ui.label("最近使った色");
                    ui.horizontal_wrapped(|ui| {
                        for rgb in recent_annotation_colors.clone() {
                            show_color_choice(ui, "", rgb, state, recent_annotation_colors);
                        }
                    });
                }
                ui.separator();
                let picker_open = state.custom_color_draft.is_some();
                let picker_label = if picker_open {
                    "その他の色…  ∧"
                } else {
                    "その他の色…  ∨"
                };
                if ui
                    .add_sized(
                        [ui.available_width(), ui.spacing().interact_size.y],
                        Button::new(picker_label),
                    )
                    .clicked()
                {
                    if picker_open {
                        cancel_custom_color(state);
                    } else {
                        // 下書きは表示値から始めるが、ユーザーが適用を確認するまで
                        // 読み取れない/CMYK ソースを書き換えない。
                        state.custom_color_draft = Some(
                            color_preview(state.buffer.color)
                                .map(|color| [color.r(), color.g(), color.b()])
                                .unwrap_or(COLOR_PRESETS[0].1),
                        );
                    }
                }

                if let Some(mut draft) = state.custom_color_draft {
                    ui.separator();
                    ui.label("カスタム色");
                    ui.spacing_mut().slider_width = COLOR_PICKER_SLIDER_WIDTH;
                    let mut draft_color = Color32::from_rgb(draft[0], draft[1], draft[2]);
                    egui::color_picker::color_picker_color32(
                        ui,
                        &mut draft_color,
                        egui::color_picker::Alpha::Opaque,
                    );
                    draft = [draft_color.r(), draft_color.g(), draft_color.b()];

                    let mut apply = false;
                    let mut cancel = false;
                    ui.horizontal(|ui| {
                        apply = ui.button("適用").clicked();
                        cancel = ui.button("戻る").clicked();
                    });
                    if apply {
                        apply_custom_color(state, recent_annotation_colors, draft);
                        state.custom_color_draft = None;
                        ui.close();
                    } else if cancel {
                        cancel_custom_color(state);
                    } else {
                        state.custom_color_draft = Some(draft);
                    }
                }
            });
        let popup_is_open = Popup::is_id_open(ui.ctx(), popup_id);
        discard_custom_color_draft_after_popup_close(state, popup_was_open, popup_is_open);
    });
}

fn discard_custom_color_draft_after_popup_close(
    state: &mut AnnotationEditorState,
    popup_was_open: bool,
    popup_is_open: bool,
) {
    // 下書きは作成したポップアップセッションだけに属する。外側のクリック、Escape、
    // またはトリガーで閉じたときに次のオープンへ漏らしてはならない。
    if popup_was_open && !popup_is_open {
        cancel_custom_color(state);
    }
}

fn color_trigger(ui: &mut egui::Ui, color: Option<PdfAnnotationColor>) -> Response {
    let desired_size = Vec2::new(ui.available_width(), COLOR_TRIGGER_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), "色を選択"));
    let visuals = *ui.style().interact(&response);
    ui.painter().rect(
        rect,
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    if response.has_focus() {
        ui.painter().rect(
            rect.shrink(1.0),
            visuals.corner_radius,
            Color32::TRANSPARENT,
            ui.visuals().selection.stroke,
            StrokeKind::Inside,
        );
    }

    let swatch_rect = Rect::from_center_size(
        Pos2::new(
            rect.left() + COLOR_TRIGGER_HORIZONTAL_PADDING + COLOR_SWATCH_SIZE / 2.0,
            rect.center().y,
        ),
        Vec2::splat(COLOR_SWATCH_SIZE),
    );
    paint_color_swatch(ui, swatch_rect, color);
    let text_position = Pos2::new(
        swatch_rect.right() + ui.spacing().item_spacing.x,
        rect.center().y,
    );
    ui.painter().text(
        text_position,
        Align2::LEFT_CENTER,
        "色を選択",
        egui::TextStyle::Button.resolve(ui.style()),
        visuals.text_color(),
    );

    let arrow_center = Pos2::new(
        rect.right() - COLOR_TRIGGER_HORIZONTAL_PADDING - COLOR_TRIGGER_ARROW_HALF_SIZE,
        rect.center().y,
    );
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            arrow_center
                + Vec2::new(
                    -COLOR_TRIGGER_ARROW_HALF_SIZE,
                    -COLOR_TRIGGER_ARROW_HALF_SIZE / 2.0,
                ),
            arrow_center
                + Vec2::new(
                    COLOR_TRIGGER_ARROW_HALF_SIZE,
                    -COLOR_TRIGGER_ARROW_HALF_SIZE / 2.0,
                ),
            arrow_center + Vec2::new(0.0, COLOR_TRIGGER_ARROW_HALF_SIZE / 2.0),
        ],
        visuals.fg_stroke.color,
        Stroke::NONE,
    ));
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn show_color_choice(
    ui: &mut egui::Ui,
    name: &str,
    rgb: [u8; 3],
    state: &mut AnnotationEditorState,
    recent_annotation_colors: &mut Vec<[u8; 3]>,
) {
    let color = rgb_color(rgb);
    let selected = state.buffer.color == Some(color);
    let label = if name.is_empty() {
        format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
    } else {
        name.to_owned()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(COLOR_CHOICE_SIZE), Sense::click());
    let response = response.on_hover_text(format!(
        "{label} #{:02X}{:02X}{:02X}",
        rgb[0], rgb[1], rgb[2]
    ));
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let swatch_rect = Rect::from_center_size(rect.center(), Vec2::splat(COLOR_SWATCH_SIZE));
    paint_color_swatch(ui, swatch_rect, Some(color));
    if selected {
        paint_color_check(ui, rect.center());
    }
    if response.clicked() {
        select_color(state, recent_annotation_colors, rgb);
        ui.close();
    }
}

fn paint_color_check(ui: &egui::Ui, center: Pos2) {
    let half = COLOR_CHECK_SIZE / 2.0;
    let start = center + Vec2::new(-half, 0.0);
    let middle = center + Vec2::new(-half / 3.0, half / 2.0);
    let end = center + Vec2::new(half, -half / 2.0);
    for stroke in [
        Stroke::new(COLOR_CHECK_OUTLINE_WIDTH, Color32::BLACK),
        Stroke::new(COLOR_CHECK_FOREGROUND_WIDTH, Color32::WHITE),
    ] {
        ui.painter().line_segment([start, middle], stroke);
        ui.painter().line_segment([middle, end], stroke);
    }
}

pub(crate) fn paint_color_swatch(ui: &egui::Ui, rect: Rect, color: Option<PdfAnnotationColor>) {
    let border = Stroke::new(1.0, ui.visuals().widgets.noninteractive.fg_stroke.color);
    let fill = color_preview(color).unwrap_or(ui.visuals().extreme_bg_color);
    ui.painter()
        .rect(rect, 2.0, fill, border, StrokeKind::Inside);
    if color.is_none() {
        // 斜線により、読み取れない色と有効なグレー値を区別する。
        ui.painter()
            .line_segment([rect.left_bottom(), rect.right_top()], border);
    }
}

fn select_color(
    state: &mut AnnotationEditorState,
    recent_annotation_colors: &mut Vec<[u8; 3]>,
    rgb: [u8; 3],
) {
    state.buffer.color = Some(rgb_color(rgb));
    recent_annotation_colors.retain(|entry| *entry != rgb);
    recent_annotation_colors.insert(0, rgb);
    recent_annotation_colors.truncate(MAX_RECENT_ANNOTATION_COLORS);
}

fn apply_custom_color(
    state: &mut AnnotationEditorState,
    recent_annotation_colors: &mut Vec<[u8; 3]>,
    rgb: [u8; 3],
) {
    select_color(state, recent_annotation_colors, rgb);
}

fn cancel_custom_color(state: &mut AnnotationEditorState) {
    state.custom_color_draft = None;
}

fn annotation_candidate_label(annotation: &AnnotationSnapshot) -> String {
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
    format!("{comment}・ID {}", annotation.id.xref)
}

/// 注釈の色を描画するが、説明テキストにはその色を使わない。
pub(crate) fn color_swatch(
    ui: &mut egui::Ui,
    color: Option<PdfAnnotationColor>,
    size: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    paint_color_swatch(ui, rect, color);
    response
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
            // この標準的な減法変換は表示専用である。ユーザーが色を選ぶまで、元の CMYK
            // 成分には触れない。
            let red = (1.0 - cyan) * (1.0 - key);
            let green = (1.0 - magenta) * (1.0 - key);
            let blue = (1.0 - yellow) * (1.0 - key);
            Some(normalized_rgb_to_color32(red, green, blue))
        }
    }
}

fn normalized_rgb_to_color32(red: f32, green: f32, blue: f32) -> Color32 {
    fn channel(value: f32) -> u8 {
        // PDF の色チャンネルは正規化された浮動小数である。異常な外部値をクリップして
        // プレビューを保護しつつ、保存データは変更しない。
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
    fn candidate_label_uses_comment_head_and_stable_id_without_color_text() {
        let annotation = annotation(
            "最初の行\n表示しない行",
            Some(PdfAnnotationColor::Rgb {
                red: 1.0,
                green: 1.0,
                blue: 0.0,
            }),
        );

        let rows = annotation_menu_candidates(&[&annotation]);

        assert_eq!(rows[0].label, "最初の行・ID 41");
        assert_eq!(rows[0].color, annotation.color);
        assert!(!rows[0].label.contains('#'));
        assert!(!rows[0].label.contains("黄色"));
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

    #[test]
    fn editor_builds_only_changed_fields_and_blocks_stale_or_pending_save() {
        let annotation = annotation(
            "before",
            Some(PdfAnnotationColor::Rgb {
                red: 1.0,
                green: 1.0,
                blue: 0.0,
            }),
        );
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        editor.buffer.contents = "after".to_owned();

        let request = editor.update_request().unwrap();
        assert_eq!(request.id, annotation.id);
        assert_eq!(request.expected_revision, 3);
        assert_eq!(request.contents.as_deref(), Some("after"));
        assert_eq!(request.color, None);

        editor.mutation_in_flight = true;
        assert!(editor.update_request().is_none());
        editor.mutation_in_flight = false;
        editor.stale = true;
        assert!(editor.update_request().is_none());
    }

    #[test]
    fn preset_and_arbitrary_rgb_values_become_explicit_color_patches() {
        let annotation = annotation("", None);
        for (_, rgb) in COLOR_PRESETS {
            let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
            editor.buffer.color = Some(rgb_color(rgb));

            assert_eq!(editor.update_request().unwrap().color, Some(rgb_color(rgb)));
        }

        let arbitrary = [12, 34, 56];
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        editor.buffer.color = Some(rgb_color(arbitrary));

        assert_eq!(
            editor.update_request().unwrap().color,
            Some(rgb_color(arbitrary))
        );
    }

    #[test]
    fn yellow_preset_matches_the_backend_default_without_rewriting_legacy_yellow() {
        let legacy_yellow = PdfAnnotationColor::Rgb {
            red: 1.0,
            green: 235.0 / 255.0,
            blue: 59.0 / 255.0,
        };
        let annotation = annotation("", Some(legacy_yellow));
        let editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);

        assert_eq!(
            rgb_color(COLOR_PRESETS[0].1),
            PdfAnnotationColor::Rgb {
                red: 1.0,
                green: 1.0,
                blue: 0.0,
            }
        );
        assert_eq!(editor.buffer.color, Some(legacy_yellow));
        assert!(!editor.is_dirty());
    }

    #[test]
    fn hover_comments_keep_all_nonempty_stored_comments_in_annotation_order() {
        let first = annotation("最初\n続き", None);
        let empty = annotation(" \n\t", None);
        let third = annotation("三番目", None);

        let comments = annotation_hover_comments(&[&first, &empty, &third]);

        assert_eq!(comments, vec!["最初\n続き", "三番目"]);
    }

    #[test]
    fn color_palette_contains_ten_labeled_presets() {
        assert_eq!(COLOR_PRESETS.len(), 10);
        assert!(COLOR_PRESETS.iter().all(|(label, _)| !label.is_empty()));
    }

    #[test]
    fn recent_colors_dedupe_and_keep_newest_five() {
        let annotation = annotation("", None);
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        let mut recent = Vec::new();
        for value in 0..6 {
            select_color(&mut editor, &mut recent, [value, 1, 2]);
        }
        select_color(&mut editor, &mut recent, [2, 1, 2]);

        assert_eq!(
            recent,
            vec![[2, 1, 2], [5, 1, 2], [4, 1, 2], [3, 1, 2], [1, 1, 2]]
        );
    }

    #[test]
    fn custom_color_apply_updates_buffer_and_cancel_leaves_it_unchanged() {
        let annotation = annotation("", None);
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        let mut recent = Vec::new();
        let original = editor.buffer.clone();

        editor.custom_color_draft = Some([10, 20, 30]);
        cancel_custom_color(&mut editor);
        assert_eq!(editor.buffer, original);
        assert!(recent.is_empty());

        apply_custom_color(&mut editor, &mut recent, [10, 20, 30]);
        assert_eq!(editor.buffer.color, Some(rgb_color([10, 20, 30])));
        assert_eq!(recent, vec![[10, 20, 30]]);
    }

    #[test]
    fn closing_color_popup_discards_only_the_unapplied_draft() {
        let annotation = annotation("", None);
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        let original = editor.buffer.clone();
        editor.custom_color_draft = Some([10, 20, 30]);

        discard_custom_color_draft_after_popup_close(&mut editor, true, false);

        assert_eq!(editor.buffer, original);
        assert!(editor.custom_color_draft.is_none());
    }

    #[test]
    fn disabled_color_editor_cannot_retain_a_custom_draft() {
        let annotation = annotation("", None);
        let mut editor = AnnotationEditorState::from_snapshot(7, 3, &annotation);
        editor.can_edit_color = false;
        editor.custom_color_draft = Some([10, 20, 30]);
        let original = editor.buffer.clone();
        let mut recent = Vec::new();

        let context = egui::Context::default();
        let _output = context.run_ui(egui::RawInput::default(), |ui| {
            color_menu(ui, &mut editor, &mut recent, true);
        });

        assert_eq!(editor.buffer, original);
        assert!(editor.custom_color_draft.is_none());
        assert!(recent.is_empty());
    }
}
