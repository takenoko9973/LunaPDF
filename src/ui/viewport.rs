use eframe::egui::{
    Color32, CursorIcon, PointerButton, Popup, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Ui,
    Vec2,
};

use crate::domain::annotation::{
    AnnotationCandidateDecision, AnnotationPageSnapshot, annotations_at_point,
    decide_annotation_candidates,
};
use crate::domain::document::{PageRect, RenderedTile, SearchMatch};
use crate::domain::selection::{
    NonTextTargetKind, PagePoint, PageQuad, SelectionSnapshot, TextPageSnapshot,
    selected_display_quads, selection_contains_point, snap_to_glyph,
    snap_to_glyph_with_max_distance,
};
use crate::ui::annotation_editor::{
    AnnotationContextTarget, AnnotationUiAction, annotation_hover_comments,
    annotation_menu_candidates, show_annotation_context_menu,
};

#[derive(Default)]
pub(crate) struct PageViewport {
    drag_page: Option<usize>,
    drag_start: Option<PagePoint>,
    drag_current: Option<PagePoint>,
    drag_origin_screen: Option<Pos2>,
    drag_active: bool,
    context_page: Option<usize>,
    context_target: Option<AnnotationContextTarget>,
    primary_press: Option<PrimaryPressSource>,
    clear_selection_on_click: bool,
    blank_pan: Option<BlankPanState>,
}

#[derive(Default)]
pub(crate) struct PageInteraction {
    pub(crate) completed_drag: Option<(usize, PagePoint, PagePoint)>,
    pub(crate) annotation_action: Option<AnnotationUiAction>,
    pub(crate) pan_delta: Option<Vec2>,
    pub(crate) clear_selection: bool,
    pub(crate) cursor_target: Option<PageCursorTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageCursorTarget {
    Text,
    Link,
    Annotation,
    OtherInteractive,
    Blank,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimaryPressSource {
    Page(usize),
    Background,
}

#[derive(Clone, Copy, Debug)]
struct BlankPanState {
    source: PrimaryPressSource,
    origin: Pos2,
    last_position: Pos2,
    active: bool,
}

impl BlankPanState {
    fn new(source: PrimaryPressSource, origin: Pos2) -> Self {
        Self {
            source,
            origin,
            last_position: origin,
            active: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PagePressKind {
    Text(PagePoint),
    Blank,
    Selection,
    Link,
    Annotation,
    OtherInteractive,
    Unavailable,
}

impl PagePressKind {
    fn cursor_target(self) -> PageCursorTarget {
        match self {
            Self::Text(_) | Self::Selection => PageCursorTarget::Text,
            Self::Link => PageCursorTarget::Link,
            Self::Annotation => PageCursorTarget::Annotation,
            Self::OtherInteractive | Self::Unavailable => PageCursorTarget::OtherInteractive,
            Self::Blank => PageCursorTarget::Blank,
        }
    }
}

/// 操作状態を変更せず、1つの PDF フレームに対するカーソルを返す。
pub(crate) fn pdf_cursor_icon(
    target: Option<PageCursorTarget>,
    _autoscroll_active: bool,
    blank_pan_active: bool,
) -> CursorIcon {
    if blank_pan_active {
        // 開始済みのパンではテキスト上を通過しても4方向矢印を維持し、
        // カスタム画像ではなく egui の標準カーソルを使う。
        CursorIcon::AllScroll
    } else {
        // 中ボタンのオートスクロールでは意図的に対象カーソルを上書きしないため、
        // ホイールを押してもカーソルだけが視覚的に変化することはない。
        match target {
            Some(PageCursorTarget::Text) => CursorIcon::Text,
            Some(PageCursorTarget::Link) => CursorIcon::PointingHand,
            Some(
                PageCursorTarget::Annotation
                | PageCursorTarget::OtherInteractive
                | PageCursorTarget::Blank
                | PageCursorTarget::Background,
            )
            | None => CursorIcon::Default,
        }
    }
}

pub(crate) struct PageInteractionInput<'a> {
    pub(crate) screen_rect: Rect,
    pub(crate) page_index: usize,
    pub(crate) bounds: PageRect,
    pub(crate) text_snapshot: Option<&'a TextPageSnapshot>,
    pub(crate) selection: Option<&'a SelectionSnapshot>,
    pub(crate) annotation_page: Option<&'a AnnotationPageSnapshot>,
    pub(crate) can_create_highlight: bool,
    pub(crate) suppress_annotation_hover: bool,
    pub(crate) input_excluded_rect: Option<Rect>,
}

impl PageViewport {
    /// ページ相対のラスタ位置に1つのタイルを描画する。
    pub(crate) fn paint_tile(
        ui: &Ui,
        page_screen_rect: Rect,
        texture: &TextureHandle,
        tile: &RenderedTile,
    ) {
        let tile_rect = screen_rect_for_tile(page_screen_rect, tile);
        ui.painter().image(
            texture.id(),
            tile_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// 論理的なテキスト選択を変更せず、検索ジオメトリを描画する。
    pub(crate) fn paint_search_matches(
        ui: &Ui,
        screen_rect: Rect,
        bounds: PageRect,
        matches: &[SearchMatch],
        selected_match: Option<usize>,
    ) {
        for (match_index, search_match) in matches.iter().enumerate() {
            // 別の色で Enter 選択中の結果を見えるようにしつつ、他のヒットは
            // 文書全体の検索コンテキストとして残す。
            let selected = selected_match == Some(match_index);
            let (fill, stroke) = if selected {
                (
                    Color32::from_rgba_unmultiplied(255, 185, 30, 96),
                    Color32::from_rgb(220, 120, 0),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(80, 170, 255, 72),
                    Color32::from_rgb(35, 110, 210),
                )
            };
            for quad in &search_match.quads {
                paint_quad(
                    ui,
                    screen_rect,
                    bounds,
                    *quad,
                    fill,
                    Stroke::new(1.5, stroke),
                );
            }
        }
    }

    /// 1つの論理 PDF ページの選択操作とオーバーレイを処理する。
    pub(crate) fn interact_at(
        &mut self,
        ui: &mut Ui,
        input: PageInteractionInput<'_>,
    ) -> PageInteraction {
        let PageInteractionInput {
            screen_rect,
            page_index,
            bounds,
            text_snapshot,
            selection,
            annotation_page,
            can_create_highlight,
            suppress_annotation_hover,
            input_excluded_rect,
        } = input;
        let response = ui.interact(
            screen_rect,
            ui.id().with(("pdf-page", page_index)),
            Sense::click_and_drag(),
        );

        if let Some(selection) = selection.filter(|value| value.page_index == page_index) {
            for quad in &selection.display_quads {
                paint_quad(
                    ui,
                    screen_rect,
                    bounds,
                    *quad,
                    Color32::from_rgba_unmultiplied(255, 210, 0, 72),
                    Stroke::NONE,
                );
            }
        }
        let mut interaction = PageInteraction::default();
        let pointer_page_point = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.latest_pos()))
            .filter(|position| response.rect.contains(*position))
            .filter(|position| input_excluded_rect.is_none_or(|rect| !rect.contains(*position)))
            .filter(|position| ui.ctx().layer_id_at(*position) == Some(ui.layer_id()))
            .map(|position| page_point_from_screen(position, screen_rect, bounds));
        let (secondary_released, secondary_position) = ui.input(|input| {
            (
                input.pointer.button_released(PointerButton::Secondary),
                input.pointer.latest_pos(),
            )
        });
        let secondary_released_here = secondary_released
            && secondary_position.is_some_and(|position| {
                response.rect.contains(position)
                    && input_excluded_rect.is_none_or(|rect| !rect.contains(position))
                    && ui.ctx().layer_id_at(position) == Some(ui.layer_id())
            });
        if secondary_released_here {
            let selection_available =
                pointer_page_point
                    .zip(selection)
                    .is_some_and(|(point, selection)| {
                        selection_contains_point(selection, page_index, point)
                    });
            let annotation_hits = pointer_page_point
                .zip(annotation_page)
                .map_or_else(Vec::new, |(point, page)| {
                    annotations_at_point(&page.annotations, point)
                });
            if selection_available || !annotation_hits.is_empty() {
                self.context_page = Some(page_index);
                self.context_target = Some(AnnotationContextTarget {
                    selection_available,
                    candidates: annotation_menu_candidates(&annotation_hits),
                });
            } else {
                self.context_page = None;
                self.context_target = None;
                Popup::close_id(ui.ctx(), Popup::default_response_id(&response));
            }
        }

        if response.double_clicked_by(PointerButton::Primary)
            && let Some(point) = pointer_page_point
            && let Some(page) = annotation_page
        {
            let hits = annotations_at_point(&page.annotations, point);
            interaction.annotation_action = match decide_annotation_candidates(&hits) {
                AnnotationCandidateDecision::None => None,
                AnnotationCandidateDecision::Open(id) => {
                    Some(AnnotationUiAction::EditAnnotation(id))
                }
                AnnotationCandidateDecision::Choose => Some(AnnotationUiAction::ChooseAnnotation(
                    annotation_menu_candidates(&hits),
                )),
            };
        }

        let (primary_pressed, primary_released, pointer_position, press_origin) =
            ui.input(|input| {
                (
                    input.pointer.button_pressed(PointerButton::Primary),
                    input.pointer.button_released(PointerButton::Primary),
                    input.pointer.latest_pos(),
                    input.pointer.press_origin(),
                )
            });
        let origin = press_origin.or(pointer_position);
        if primary_pressed
            && origin.is_some_and(|position| {
                response.rect.contains(position)
                    && input_excluded_rect.is_none_or(|rect| !rect.contains(position))
                    && ui.ctx().layer_id_at(position) == Some(ui.layer_id())
            })
        {
            let origin = origin.expect("the page contains the checked pointer origin");
            let point = page_point_from_screen(origin, screen_rect, bounds);
            let inside_selection = selection
                .is_some_and(|selection| selection_contains_point(selection, page_index, point));
            let logical_tolerance = ui
                .ctx()
                .options(|options| options.input_options.max_click_dist);
            let page_tolerance =
                glyph_hit_tolerance_in_page_points(screen_rect, bounds, logical_tolerance);
            let press_kind = classify_page_press(
                page_index,
                point,
                page_tolerance,
                text_snapshot,
                selection,
                annotation_page,
            );

            self.primary_press = Some(PrimaryPressSource::Page(page_index));
            self.clear_selection_on_click = selection.is_some() && !inside_selection;
            self.blank_pan = match press_kind {
                PagePressKind::Blank => Some(BlankPanState::new(
                    PrimaryPressSource::Page(page_index),
                    origin,
                )),
                PagePressKind::Text(_)
                | PagePressKind::Selection
                | PagePressKind::Link
                | PagePressKind::Annotation
                | PagePressKind::OtherInteractive
                | PagePressKind::Unavailable => None,
            };
            self.drag_start = match press_kind {
                PagePressKind::Text(start) => Some(start),
                PagePressKind::Blank
                | PagePressKind::Selection
                | PagePressKind::Link
                | PagePressKind::Annotation
                | PagePressKind::OtherInteractive
                | PagePressKind::Unavailable => None,
            };
            self.drag_page = self.drag_start.map(|_| page_index);
            self.drag_current = self.drag_start;
            self.drag_origin_screen = self.drag_start.map(|_| origin);
            self.drag_active = false;
        }

        let drag_threshold = ui
            .ctx()
            .options(|options| options.input_options.max_click_dist);
        if self.drag_page == Some(page_index)
            && let (Some(origin), Some(position)) = (self.drag_origin_screen, pointer_position)
        {
            self.drag_active |= selection_drag_exceeds_threshold(origin, position, drag_threshold);
        }
        if self.drag_active && self.drag_page == Some(page_index) {
            self.drag_current = response
                .interact_pointer_pos()
                .or(pointer_position)
                .map(|position| page_point_from_screen(position, screen_rect, bounds))
                .and_then(|point| {
                    text_snapshot.and_then(|snapshot| snap_to_glyph(&snapshot.glyphs, point))
                });
        }
        if let Some(pan) = self
            .blank_pan
            .as_mut()
            .filter(|pan| pan.source == PrimaryPressSource::Page(page_index))
            && let Some(position) = pointer_position
        {
            interaction.pan_delta = update_blank_pan(pan, position, drag_threshold);
        }

        self.paint_drag_preview(ui, screen_rect, page_index, bounds, text_snapshot);

        if primary_released && self.primary_press == Some(PrimaryPressSource::Page(page_index)) {
            let pan_was_active = self.blank_pan.is_some_and(|pan| pan.active);
            let completed = self.drag_active.then(|| {
                self.drag_start
                    .zip(self.drag_current)
                    .map(|(start, end)| (page_index, start, end))
            });
            interaction.clear_selection =
                self.clear_selection_on_click && !self.drag_active && !pan_was_active;
            self.cancel_primary_interaction();
            interaction.completed_drag = completed.flatten();
        }
        if self.context_page == Some(page_index)
            && let Some(target) = &self.context_target
            && let Some(action) = show_annotation_context_menu(
                &response,
                target,
                can_create_highlight,
                secondary_released_here,
            )
        {
            interaction.annotation_action = Some(action);
        }
        let context_menu_open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&response));
        if annotation_hover_is_allowed(
            suppress_annotation_hover,
            self.drag_active || self.blank_pan.is_some(),
            context_menu_open,
        ) && let Some(point) = pointer_page_point
            && let Some(page) = annotation_page
        {
            let hits = annotations_at_point(&page.annotations, point);
            let comments = annotation_hover_comments(&hits);
            if !comments.is_empty() {
                response.clone().on_hover_ui_at_pointer(|ui| {
                    for (index, comment) in comments.iter().enumerate() {
                        if index > 0 {
                            ui.separator();
                        }
                        ui.label(*comment);
                    }
                });
            }
        }
        interaction.cursor_target = pointer_page_point.map(|point| {
            let logical_tolerance = ui
                .ctx()
                .options(|options| options.input_options.max_click_dist);
            let page_tolerance =
                glyph_hit_tolerance_in_page_points(screen_rect, bounds, logical_tolerance);
            classify_page_press(
                page_index,
                point,
                page_tolerance,
                text_snapshot,
                selection,
                annotation_page,
            )
            .cursor_target()
        });
        interaction
    }

    /// すべての描画済み PDF ページの外側にある灰色のビューポート領域を処理する。
    pub(crate) fn interact_background(
        &mut self,
        ui: &mut Ui,
        view_rect: Rect,
        page_rects: &[Rect],
        excluded_rects: &[Rect],
        selection_present: bool,
    ) -> PageInteraction {
        let mut interaction = PageInteraction::default();
        let (primary_pressed, primary_released, pointer_position, press_origin) =
            ui.input(|input| {
                (
                    input.pointer.button_pressed(PointerButton::Primary),
                    input.pointer.button_released(PointerButton::Primary),
                    input.pointer.latest_pos(),
                    input.pointer.press_origin(),
                )
            });
        let origin = press_origin.or(pointer_position);
        let starts_on_background = origin.is_some_and(|position| {
            view_rect.contains(position)
                && page_rects.iter().all(|rect| !rect.contains(position))
                && excluded_rects.iter().all(|rect| !rect.contains(position))
                && ui.ctx().layer_id_at(position) == Some(ui.layer_id())
        });
        if primary_pressed && self.primary_press.is_none() && starts_on_background {
            let origin = origin.expect("the checked background press has an origin");
            self.primary_press = Some(PrimaryPressSource::Background);
            self.clear_selection_on_click = selection_present;
            self.blank_pan = Some(BlankPanState::new(PrimaryPressSource::Background, origin));
        }

        let drag_threshold = ui
            .ctx()
            .options(|options| options.input_options.max_click_dist);
        if let Some(pan) = self
            .blank_pan
            .as_mut()
            .filter(|pan| pan.source == PrimaryPressSource::Background)
            && let Some(position) = pointer_position
        {
            interaction.pan_delta = update_blank_pan(pan, position, drag_threshold);
        }

        if primary_released && self.primary_press == Some(PrimaryPressSource::Background) {
            let pan_was_active = self.blank_pan.is_some_and(|pan| pan.active);
            interaction.clear_selection = self.clear_selection_on_click && !pan_was_active;
            self.cancel_primary_interaction();
        }

        if pointer_position.is_some_and(|position| {
            view_rect.contains(position)
                && page_rects.iter().all(|rect| !rect.contains(position))
                && excluded_rects.iter().all(|rect| !rect.contains(position))
                && ui.ctx().layer_id_at(position) == Some(ui.layer_id())
        }) {
            interaction.cursor_target = Some(PageCursorTarget::Background);
        }
        interaction
    }

    /// 共有ビューポートが現在プライマリボタンのジェスチャーを所有しているか報告する。
    pub(crate) fn primary_interaction_in_progress(&self) -> bool {
        self.primary_press.is_some()
    }

    /// 空白ページのパンがドラッグしきい値を越えたか報告する。
    pub(crate) fn blank_pan_in_progress(&self) -> bool {
        self.blank_pan.is_some_and(|pan| pan.active)
    }

    /// 選択やスクロール位置を変更せず、ページが所有するプライマリ入力をキャンセルする。
    pub(crate) fn cancel_primary_interaction(&mut self) {
        self.drag_page = None;
        self.drag_start = None;
        self.drag_current = None;
        self.drag_origin_screen = None;
        self.drag_active = false;
        self.primary_press = None;
        self.clear_selection_on_click = false;
        self.blank_pan = None;
    }

    fn paint_drag_preview(
        &self,
        ui: &Ui,
        screen_rect: Rect,
        page_index: usize,
        bounds: PageRect,
        text_snapshot: Option<&TextPageSnapshot>,
    ) {
        if !self.drag_active || self.drag_page != Some(page_index) {
            return;
        }
        let (Some(start), Some(current), Some(text_snapshot)) =
            (self.drag_start, self.drag_current, text_snapshot)
        else {
            return;
        };
        for quad in selected_display_quads(&text_snapshot.glyphs, start, current) {
            paint_quad(
                ui,
                screen_rect,
                bounds,
                quad,
                Color32::from_rgba_unmultiplied(255, 210, 0, 56),
                Stroke::NONE,
            );
        }
    }
}

fn annotation_hover_is_allowed(
    externally_suppressed: bool,
    selection_drag_active: bool,
    context_menu_open: bool,
) -> bool {
    // 別のポインタ所有の注釈操作または文書操作が有効な間はツールチップを作らず、
    // 同じヒットを取り合わないようにする。
    !externally_suppressed && !selection_drag_active && !context_menu_open
}

fn classify_page_press(
    page_index: usize,
    point: PagePoint,
    glyph_tolerance: f32,
    text_snapshot: Option<&TextPageSnapshot>,
    selection: Option<&SelectionSnapshot>,
    annotation_page: Option<&AnnotationPageSnapshot>,
) -> PagePressKind {
    if selection.is_some_and(|selection| selection_contains_point(selection, page_index, point)) {
        return PagePressKind::Selection;
    }
    let Some(text_snapshot) = text_snapshot else {
        return PagePressKind::Unavailable;
    };
    if let Some(glyph) =
        snap_to_glyph_with_max_distance(&text_snapshot.glyphs, point, glyph_tolerance)
    {
        return PagePressKind::Text(glyph);
    }
    let Some(annotation_page) = annotation_page else {
        // 注釈メタデータが届くまでは、テキスト以外のページ領域を占有済みとして扱う。
        // これにより、ワーカーがまだ記述していない Highlight を通過してパンすることを防ぐ。
        // グリフ選択は引き続き利用できる。
        return PagePressKind::Unavailable;
    };
    let annotation_hit = !annotations_at_point(&annotation_page.annotations, point).is_empty();
    if annotation_hit {
        return PagePressKind::Annotation;
    }
    let link_hit = text_snapshot
        .non_text_targets
        .iter()
        .any(|target| target.kind == NonTextTargetKind::Link && target.quad.contains(point));
    if link_hit {
        return PagePressKind::Link;
    }
    let independent_hit = text_snapshot
        .non_text_targets
        .iter()
        .any(|target| target.quad.contains(point));
    // このビューアーが現在すべての種類に専用の左クリック操作を提供していなくても、
    // フォームと画像はポインタ領域を引き続き所有する。
    if independent_hit {
        PagePressKind::OtherInteractive
    } else {
        PagePressKind::Blank
    }
}

fn glyph_hit_tolerance_in_page_points(
    screen_rect: Rect,
    bounds: PageRect,
    logical_tolerance: f32,
) -> f32 {
    if screen_rect.width() <= 0.0 || screen_rect.height() <= 0.0 {
        return 0.0;
    }
    // egui のクリック許容値は DPI 非依存の論理ポイントで表されている。描画済みページの
    // スケールを通じて変換することで、固定デバイスピクセルしきい値なしに PDF の各ズームで
    // 同じ画面上のヒット半径を保つ。
    let page_units_per_screen_x = bounds.width() / screen_rect.width();
    let page_units_per_screen_y = bounds.height() / screen_rect.height();
    logical_tolerance * page_units_per_screen_x.max(page_units_per_screen_y)
}

fn update_blank_pan(
    pan: &mut BlankPanState,
    pointer_position: Pos2,
    drag_threshold: f32,
) -> Option<Vec2> {
    if !pan.active {
        if !selection_drag_exceeds_threshold(pan.origin, pointer_position, drag_threshold) {
            return None;
        }
        pan.active = true;
        let delta = pointer_position - pan.origin;
        pan.last_position = pointer_position;
        return (delta != Vec2::ZERO).then_some(delta);
    }

    let delta = pointer_position - pan.last_position;
    pan.last_position = pointer_position;
    (delta != Vec2::ZERO).then_some(delta)
}

fn selection_drag_exceeds_threshold(origin: Pos2, current: Pos2, threshold: f32) -> bool {
    // このしきい値は egui の論理ポイントによるクリック許容値なので、ネイティブ DPI と
    // アプリケーションのズーム設定をまたいでも判定が一貫する。
    origin.distance_sq(current) > threshold * threshold
}

/// ラスタタイルのデバイスピクセル範囲を正規化されたページ画面座標へ対応付ける。
pub(crate) fn screen_rect_for_tile(page_screen_rect: Rect, tile: &RenderedTile) -> Rect {
    let page_pixel_width = tile.page_pixel_width as f32;
    let page_pixel_height = tile.page_pixel_height as f32;
    let x0 = tile.spec.pixel_x as f32 / page_pixel_width;
    let y0 = tile.spec.pixel_y as f32 / page_pixel_height;
    let x1 = (tile.spec.pixel_x + tile.spec.pixel_width) as f32 / page_pixel_width;
    let y1 = (tile.spec.pixel_y + tile.spec.pixel_height) as f32 / page_pixel_height;
    Rect::from_min_max(
        Pos2::new(
            page_screen_rect.left() + page_screen_rect.width() * x0,
            page_screen_rect.top() + page_screen_rect.height() * y0,
        ),
        Pos2::new(
            page_screen_rect.left() + page_screen_rect.width() * x1,
            page_screen_rect.top() + page_screen_rect.height() * y1,
        ),
    )
}

fn paint_quad(
    ui: &Ui,
    screen_rect: Rect,
    bounds: PageRect,
    quad: PageQuad,
    fill: Color32,
    stroke: Stroke,
) {
    let points = vec![
        screen_point_from_page(quad.upper_left, screen_rect, bounds),
        screen_point_from_page(quad.upper_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_right, screen_rect, bounds),
        screen_point_from_page(quad.lower_left, screen_rect, bounds),
    ];
    ui.painter()
        .add(Shape::convex_polygon(points, fill, stroke));
}

fn page_point_from_screen(position: Pos2, screen_rect: Rect, bounds: PageRect) -> PagePoint {
    // ポインタ位置を描画済みページの端でクランプし、ウィジェットのすぐ外で終わるドラッグも
    // 有効な PDF 選択点に対応付けられるようにする。
    let x = position.x.clamp(screen_rect.left(), screen_rect.right());
    let y = position.y.clamp(screen_rect.top(), screen_rect.bottom());
    let normalized_x = (x - screen_rect.left()) / screen_rect.width();
    let normalized_y = (y - screen_rect.top()) / screen_rect.height();
    PagePoint::new(
        bounds.x0 + normalized_x * bounds.width(),
        bounds.y0 + normalized_y * bounds.height(),
    )
}

fn screen_point_from_page(point: PagePoint, screen_rect: Rect, bounds: PageRect) -> Pos2 {
    let normalized_x = (point.x - bounds.x0) / bounds.width();
    let normalized_y = (point.y - bounds.y0) / bounds.height();
    Pos2::new(
        screen_rect.left() + normalized_x * screen_rect.width(),
        screen_rect.top() + normalized_y * screen_rect.height(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use eframe::egui;

    use super::*;
    use crate::domain::annotation::{
        AnnotationId, AnnotationKind, AnnotationPageSnapshot, AnnotationSnapshot,
    };
    use crate::domain::document::TileSpec;
    use crate::domain::selection::{GlyphSnapshot, NonTextTarget};

    fn bounds() -> PageRect {
        PageRect {
            x0: 10.0,
            y0: 20.0,
            x1: 110.0,
            y1: 220.0,
        }
    }

    fn tile(spec: TileSpec) -> RenderedTile {
        RenderedTile {
            page_index: 0,
            zoom: 1.0,
            pixels_per_point: 1.0,
            #[cfg(debug_assertions)]
            scale: 1.0,
            generation: 0,
            revision: 0,
            spec,
            page_pixel_width: 1_024,
            page_pixel_height: 512,
            pixels_rgba: Vec::new(),
            bounds: bounds(),
            #[cfg(debug_assertions)]
            render_time: Duration::ZERO,
            #[cfg(debug_assertions)]
            physical_memory_bytes: None,
        }
    }

    fn text_snapshot() -> TextPageSnapshot {
        TextPageSnapshot {
            page_index: 0,
            revision: 0,
            glyphs: vec![GlyphSnapshot {
                character: 'A',
                quad: PageQuad {
                    upper_left: PagePoint::new(0.0, 0.0),
                    upper_right: PagePoint::new(20.0, 0.0),
                    lower_left: PagePoint::new(0.0, 20.0),
                    lower_right: PagePoint::new(20.0, 20.0),
                },
                line_index: 0,
            }],
            non_text_targets: Vec::new(),
        }
    }

    fn selection_frame(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
    ) -> Option<(usize, PagePoint, PagePoint)> {
        selection_frame_at(context, viewport, events, None)
    }

    fn selection_frame_at(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        time: Option<f64>,
    ) -> Option<(usize, PagePoint, PagePoint)> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
            events,
            time,
            ..Default::default()
        };
        let mut completed = None;
        let snapshot = text_snapshot();
        let _output = context.run_ui(input, |ui| {
            completed = viewport
                .interact_at(
                    ui,
                    PageInteractionInput {
                        screen_rect: Rect::from_min_size(
                            Pos2::new(20.0, 20.0),
                            [100.0, 100.0].into(),
                        ),
                        page_index: 0,
                        bounds: PageRect {
                            x0: 0.0,
                            y0: 0.0,
                            x1: 100.0,
                            y1: 100.0,
                        },
                        text_snapshot: Some(&snapshot),
                        selection: None,
                        annotation_page: None,
                        can_create_highlight: false,
                        suppress_annotation_hover: false,
                        input_excluded_rect: None,
                    },
                )
                .completed_drag;
        });
        completed
    }

    fn primary_button_event(position: Pos2, pressed: bool) -> egui::Event {
        pointer_button_event(position, PointerButton::Primary, pressed)
    }

    fn pointer_button_event(position: Pos2, button: PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn annotation_page(xrefs: &[i32]) -> AnnotationPageSnapshot {
        AnnotationPageSnapshot {
            page_index: 0,
            revision: 0,
            annotations: xrefs
                .iter()
                .map(|xref| AnnotationSnapshot {
                    id: AnnotationId {
                        page_index: 0,
                        xref: *xref,
                    },
                    kind: AnnotationKind::Highlight,
                    quads: vec![PageQuad {
                        upper_left: PagePoint::new(0.0, 0.0),
                        upper_right: PagePoint::new(20.0, 0.0),
                        lower_left: PagePoint::new(0.0, 20.0),
                        lower_right: PagePoint::new(20.0, 20.0),
                    }],
                    contents: String::new(),
                    color: None,
                    opacity: 1.0,
                    can_edit_contents: true,
                    can_edit_color: true,
                    can_delete: true,
                })
                .collect(),
        }
    }

    fn annotation_frame(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        time: f64,
        page: &AnnotationPageSnapshot,
    ) -> PageInteraction {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
            events,
            time: Some(time),
            ..Default::default()
        };
        let mut interaction = PageInteraction::default();
        let text = text_snapshot();
        let _output = context.run_ui(input, |ui| {
            interaction = viewport.interact_at(
                ui,
                PageInteractionInput {
                    screen_rect: Rect::from_min_size(Pos2::new(20.0, 20.0), [100.0, 100.0].into()),
                    page_index: 0,
                    bounds: PageRect {
                        x0: 0.0,
                        y0: 0.0,
                        x1: 100.0,
                        y1: 100.0,
                    },
                    text_snapshot: Some(&text),
                    selection: None,
                    annotation_page: Some(page),
                    can_create_highlight: true,
                    suppress_annotation_hover: false,
                    input_excluded_rect: None,
                },
            );
        });
        interaction
    }

    fn selected_glyph() -> SelectionSnapshot {
        SelectionSnapshot {
            page_index: 0,
            generation: 1,
            text: "A".to_owned(),
            display_quads: vec![PageQuad {
                upper_left: PagePoint::new(0.0, 0.0),
                upper_right: PagePoint::new(20.0, 0.0),
                lower_left: PagePoint::new(0.0, 20.0),
                lower_right: PagePoint::new(20.0, 20.0),
            }],
            quads: Vec::new(),
            extraction_time: Duration::ZERO,
        }
    }

    fn page_frame_with_selection(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        selection: &SelectionSnapshot,
    ) -> PageInteraction {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0))),
            events,
            ..Default::default()
        };
        let text = text_snapshot();
        let annotations = annotation_page(&[]);
        let mut interaction = PageInteraction::default();
        let _output = context.run_ui(input, |ui| {
            interaction = viewport.interact_at(
                ui,
                PageInteractionInput {
                    screen_rect: Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::splat(100.0)),
                    page_index: 0,
                    bounds: PageRect {
                        x0: 0.0,
                        y0: 0.0,
                        x1: 100.0,
                        y1: 100.0,
                    },
                    text_snapshot: Some(&text),
                    selection: Some(selection),
                    annotation_page: Some(&annotations),
                    can_create_highlight: true,
                    suppress_annotation_hover: false,
                    input_excluded_rect: None,
                },
            );
        });
        interaction
    }

    fn background_frame(
        context: &egui::Context,
        viewport: &mut PageViewport,
        events: Vec<egui::Event>,
        selection_present: bool,
    ) -> PageInteraction {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(240.0))),
            events,
            ..Default::default()
        };
        let mut interaction = PageInteraction::default();
        let _output = context.run_ui(input, |ui| {
            interaction = viewport.interact_background(
                ui,
                Rect::from_min_size(Pos2::ZERO, Vec2::splat(220.0)),
                &[Rect::from_min_size(
                    Pos2::new(20.0, 20.0),
                    Vec2::splat(100.0),
                )],
                &[],
                selection_present,
            );
        });
        interaction
    }

    #[test]
    fn screen_and_pdf_coordinates_roundtrip_with_nonzero_origin() {
        let screen_rect = Rect::from_min_size(Pos2::new(50.0, 80.0), [200.0, 400.0].into());
        let expected = PagePoint::new(35.0, 170.0);

        let screen = screen_point_from_page(expected, screen_rect, bounds());
        let actual = page_point_from_screen(screen, screen_rect, bounds());

        assert!((actual.x - expected.x).abs() < 0.001);
        assert!((actual.y - expected.y).abs() < 0.001);
    }

    #[test]
    fn annotation_hover_is_suppressed_by_every_competing_interaction() {
        assert!(annotation_hover_is_allowed(false, false, false));
        assert!(!annotation_hover_is_allowed(true, false, false));
        assert!(!annotation_hover_is_allowed(false, true, false));
        assert!(!annotation_hover_is_allowed(false, false, true));
    }

    #[test]
    fn glyph_hit_tolerance_tracks_logical_points_across_zoom() {
        let page = PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        let at_one_to_one = glyph_hit_tolerance_in_page_points(
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            page,
            6.0,
        );
        let at_two_times_zoom = glyph_hit_tolerance_in_page_points(
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0)),
            page,
            6.0,
        );

        assert_eq!(at_one_to_one, 6.0);
        assert_eq!(at_two_times_zoom, 3.0);
    }

    #[test]
    fn page_press_distinguishes_text_blank_and_non_text_targets() {
        let mut text = text_snapshot();
        let annotations = annotation_page(&[]);
        assert!(matches!(
            classify_page_press(
                0,
                PagePoint::new(10.0, 10.0),
                6.0,
                Some(&text),
                None,
                Some(&annotations),
            ),
            PagePressKind::Text(_)
        ));
        assert_eq!(
            classify_page_press(
                0,
                PagePoint::new(80.0, 80.0),
                6.0,
                Some(&text),
                None,
                Some(&annotations),
            ),
            PagePressKind::Blank
        );

        text.non_text_targets.push(NonTextTarget {
            kind: NonTextTargetKind::Image,
            quad: PageQuad {
                upper_left: PagePoint::new(60.0, 60.0),
                upper_right: PagePoint::new(90.0, 60.0),
                lower_left: PagePoint::new(60.0, 90.0),
                lower_right: PagePoint::new(90.0, 90.0),
            },
        });
        assert_eq!(
            classify_page_press(
                0,
                PagePoint::new(80.0, 80.0),
                6.0,
                Some(&text),
                None,
                Some(&annotations),
            ),
            PagePressKind::OtherInteractive
        );

        let mut no_glyphs = text.clone();
        no_glyphs.glyphs.clear();
        no_glyphs.non_text_targets.clear();
        no_glyphs.non_text_targets.push(NonTextTarget {
            kind: NonTextTargetKind::Link,
            quad: PageQuad {
                upper_left: PagePoint::new(60.0, 60.0),
                upper_right: PagePoint::new(90.0, 60.0),
                lower_left: PagePoint::new(60.0, 90.0),
                lower_right: PagePoint::new(90.0, 90.0),
            },
        });
        assert_eq!(
            classify_page_press(
                0,
                PagePoint::new(80.0, 80.0),
                6.0,
                Some(&no_glyphs),
                None,
                Some(&annotations),
            ),
            PagePressKind::Link
        );
        no_glyphs.non_text_targets.clear();
        let highlight = annotation_page(&[17]);
        assert_eq!(
            classify_page_press(
                0,
                PagePoint::new(10.0, 10.0),
                6.0,
                Some(&no_glyphs),
                None,
                Some(&highlight),
            ),
            PagePressKind::Annotation
        );

        let selection = SelectionSnapshot {
            page_index: 0,
            generation: 1,
            text: "selected".to_owned(),
            display_quads: vec![PageQuad {
                upper_left: PagePoint::new(70.0, 70.0),
                upper_right: PagePoint::new(90.0, 70.0),
                lower_left: PagePoint::new(70.0, 90.0),
                lower_right: PagePoint::new(90.0, 90.0),
            }],
            quads: Vec::new(),
            extraction_time: Duration::ZERO,
        };
        assert_eq!(
            classify_page_press(
                0,
                PagePoint::new(80.0, 80.0),
                6.0,
                Some(&no_glyphs),
                Some(&selection),
                Some(&annotations),
            ),
            PagePressKind::Selection
        );
    }

    #[test]
    fn idle_cursor_distinguishes_selectable_text_links_and_other_targets() {
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Text), false, false),
            CursorIcon::Text
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Link), false, false),
            CursorIcon::PointingHand
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Annotation), false, false),
            CursorIcon::Default
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::OtherInteractive), false, false),
            CursorIcon::Default
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Blank), false, false),
            CursorIcon::Default
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Background), false, false),
            CursorIcon::Default
        );
    }

    #[test]
    fn cursor_transitions_cover_all_scroll_pan_and_unchanged_autoscroll() {
        for target in [PageCursorTarget::Blank, PageCursorTarget::Background] {
            assert_eq!(
                pdf_cursor_icon(Some(target), false, false),
                CursorIcon::Default
            );
            assert_eq!(
                pdf_cursor_icon(Some(target), false, true),
                CursorIcon::AllScroll
            );
            assert_eq!(
                pdf_cursor_icon(Some(target), true, false),
                CursorIcon::Default
            );
        }
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Text), false, false),
            CursorIcon::Text
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Text), false, true),
            CursorIcon::AllScroll
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Text), true, false),
            CursorIcon::Text
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Link), true, false),
            CursorIcon::PointingHand
        );
        assert_eq!(pdf_cursor_icon(None, false, false), CursorIcon::Default);
    }

    #[test]
    fn selection_target_uses_the_i_beam_without_an_active_pan() {
        let selection = selected_glyph();
        let target = classify_page_press(
            0,
            PagePoint::new(10.0, 10.0),
            6.0,
            Some(&text_snapshot()),
            Some(&selection),
            Some(&annotation_page(&[])),
        )
        .cursor_target();

        assert_eq!(target, PageCursorTarget::Text);
        assert_eq!(
            pdf_cursor_icon(Some(target), false, false),
            CursorIcon::Text
        );
    }

    #[test]
    fn cancelling_primary_interaction_removes_pan_and_selection_drag_state() {
        let mut viewport = PageViewport {
            drag_page: Some(0),
            drag_start: Some(PagePoint::new(1.0, 2.0)),
            drag_current: Some(PagePoint::new(3.0, 4.0)),
            drag_origin_screen: Some(Pos2::new(10.0, 20.0)),
            drag_active: true,
            primary_press: Some(PrimaryPressSource::Page(0)),
            clear_selection_on_click: true,
            blank_pan: Some(BlankPanState {
                source: PrimaryPressSource::Page(0),
                origin: Pos2::new(10.0, 20.0),
                last_position: Pos2::new(30.0, 40.0),
                active: true,
            }),
            ..Default::default()
        };

        viewport.cancel_primary_interaction();

        assert!(!viewport.primary_interaction_in_progress());
        assert!(!viewport.blank_pan_in_progress());
        assert_eq!(viewport.drag_page, None);
        assert!(!viewport.drag_active);
    }

    #[test]
    fn blank_pan_waits_for_threshold_then_reports_two_axis_pointer_delta() {
        let mut pan = BlankPanState::new(PrimaryPressSource::Page(0), Pos2::new(100.0, 100.0));

        assert_eq!(
            update_blank_pan(&mut pan, Pos2::new(104.0, 103.0), 6.0),
            None
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Blank), false, pan.active),
            CursorIcon::Default
        );
        assert_eq!(
            update_blank_pan(&mut pan, Pos2::new(110.0, 108.0), 6.0),
            Some(Vec2::new(10.0, 8.0))
        );
        assert_eq!(
            pdf_cursor_icon(Some(PageCursorTarget::Blank), false, pan.active),
            CursorIcon::AllScroll
        );
        assert_eq!(
            update_blank_pan(&mut pan, Pos2::new(106.0, 115.0), 6.0),
            Some(Vec2::new(-4.0, 7.0))
        );
    }

    #[test]
    fn blank_click_clears_selection_but_blank_pan_keeps_it() {
        let selection = selected_glyph();
        let click_context = egui::Context::default();
        let mut click_viewport = PageViewport::default();
        let blank = Pos2::new(100.0, 100.0);
        page_frame_with_selection(
            &click_context,
            &mut click_viewport,
            vec![primary_button_event(blank, true)],
            &selection,
        );
        let click = page_frame_with_selection(
            &click_context,
            &mut click_viewport,
            vec![primary_button_event(blank, false)],
            &selection,
        );
        assert!(click.clear_selection);
        assert_eq!(click.cursor_target, Some(PageCursorTarget::Blank));
        assert_eq!(
            pdf_cursor_icon(
                click.cursor_target,
                false,
                click_viewport.blank_pan_in_progress()
            ),
            CursorIcon::Default
        );

        let pan_context = egui::Context::default();
        let mut pan_viewport = PageViewport::default();
        page_frame_with_selection(
            &pan_context,
            &mut pan_viewport,
            vec![primary_button_event(blank, true)],
            &selection,
        );
        let moved = page_frame_with_selection(
            &pan_context,
            &mut pan_viewport,
            vec![egui::Event::PointerMoved(Pos2::new(85.0, 90.0))],
            &selection,
        );
        assert_eq!(moved.pan_delta, Some(Vec2::new(-15.0, -10.0)));
        let release = page_frame_with_selection(
            &pan_context,
            &mut pan_viewport,
            vec![primary_button_event(Pos2::new(85.0, 90.0), false)],
            &selection,
        );
        assert!(!release.clear_selection);
    }

    #[test]
    fn gray_background_click_clears_and_drag_pans_in_two_axes() {
        let background = Pos2::new(180.0, 180.0);
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        background_frame(
            &context,
            &mut viewport,
            vec![primary_button_event(background, true)],
            true,
        );
        let click = background_frame(
            &context,
            &mut viewport,
            vec![primary_button_event(background, false)],
            true,
        );
        assert!(click.clear_selection);

        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let pressed = background_frame(
            &context,
            &mut viewport,
            vec![primary_button_event(background, true)],
            true,
        );
        assert_eq!(pressed.cursor_target, Some(PageCursorTarget::Background));
        assert_eq!(
            pdf_cursor_icon(
                pressed.cursor_target,
                false,
                viewport.blank_pan_in_progress()
            ),
            CursorIcon::Default
        );
        let drag = background_frame(
            &context,
            &mut viewport,
            vec![egui::Event::PointerMoved(Pos2::new(160.0, 150.0))],
            true,
        );
        assert_eq!(drag.pan_delta, Some(Vec2::new(-20.0, -30.0)));
        assert_eq!(
            pdf_cursor_icon(drag.cursor_target, false, viewport.blank_pan_in_progress()),
            CursorIcon::AllScroll
        );
    }

    #[test]
    fn adjacent_tile_rectangles_share_an_exact_screen_edge() {
        let page_rect = Rect::from_min_size(Pos2::new(10.0, 20.0), [800.0, 400.0].into());
        let left = tile(TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        });
        let right = tile(TileSpec {
            pixel_x: 512,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        });

        let left_rect = screen_rect_for_tile(page_rect, &left);
        let right_rect = screen_rect_for_tile(page_rect, &right);

        assert_eq!(left_rect.right(), right_rect.left());
        assert_eq!(left_rect.left(), page_rect.left());
        assert_eq!(right_rect.right(), page_rect.right());
    }

    #[test]
    fn primary_click_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(position),
                    primary_button_event(position, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![primary_button_event(position, false)],
            )
            .is_none()
        );
    }

    #[test]
    fn subthreshold_pointer_jitter_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let pressed = Pos2::new(25.0, 25.0);
        let jittered = Pos2::new(27.0, 26.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(pressed),
                    primary_button_event(pressed, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert_eq!(viewport.drag_origin_screen, Some(pressed));
        assert_eq!(
            context.options(|options| options.input_options.max_click_dist),
            6.0
        );
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![egui::Event::PointerMoved(jittered)],
            )
            .is_none()
        );
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![primary_button_event(jittered, false)],
            )
            .is_none()
        );
    }

    #[test]
    fn stationary_long_press_does_not_complete_a_text_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);

        assert!(
            selection_frame_at(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(position),
                    primary_button_event(position, true),
                ],
                Some(0.0),
            )
            .is_none()
        );
        assert!(selection_frame_at(&context, &mut viewport, Vec::new(), Some(1.0)).is_none());
        assert!(
            selection_frame_at(
                &context,
                &mut viewport,
                vec![primary_button_event(position, false)],
                Some(1.01),
            )
            .is_none()
        );
    }

    #[test]
    fn intentional_short_drag_can_select_one_glyph() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let pressed = Pos2::new(25.0, 25.0);
        let dragged = Pos2::new(33.0, 25.0);

        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![
                    egui::Event::PointerMoved(pressed),
                    primary_button_event(pressed, true),
                ],
            )
            .is_none()
        );
        assert_eq!(viewport.drag_page, Some(0));
        assert_eq!(viewport.drag_origin_screen, Some(pressed));
        assert!(
            selection_frame(
                &context,
                &mut viewport,
                vec![egui::Event::PointerMoved(dragged)],
            )
            .is_none()
        );
        assert!(viewport.drag_active);
        let completed = selection_frame(
            &context,
            &mut viewport,
            vec![
                egui::Event::PointerMoved(dragged),
                primary_button_event(dragged, false),
            ],
        )
        .expect("a drag beyond egui's click tolerance should complete");

        assert_eq!(completed.0, 0);
        assert_eq!(completed.1, PagePoint::new(10.0, 10.0));
        assert_eq!(completed.2, PagePoint::new(10.0, 10.0));
    }

    #[test]
    fn drag_threshold_requires_distance_beyond_egui_click_tolerance() {
        let origin = Pos2::new(10.0, 10.0);

        assert!(!selection_drag_exceeds_threshold(
            origin,
            Pos2::new(16.0, 10.0),
            6.0,
        ));
        assert!(selection_drag_exceeds_threshold(
            origin,
            Pos2::new(16.01, 10.0),
            6.0,
        ));
    }

    #[test]
    fn annotation_double_click_opens_one_stable_id_without_selecting_text() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41]);
        let position = Pos2::new(25.0, 25.0);

        for (time, pressed) in [(0.0, true), (0.01, false), (0.1, true)] {
            let interaction = annotation_frame(
                &context,
                &mut viewport,
                vec![pointer_button_event(
                    position,
                    PointerButton::Primary,
                    pressed,
                )],
                time,
                &page,
            );
            assert!(interaction.completed_drag.is_none());
        }
        let interaction = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Primary,
                false,
            )],
            0.11,
            &page,
        );

        assert_eq!(
            interaction.annotation_action,
            Some(AnnotationUiAction::EditAnnotation(AnnotationId {
                page_index: 0,
                xref: 41,
            }))
        );
        assert!(interaction.completed_drag.is_none());
    }

    #[test]
    fn overlapping_annotation_double_click_requests_candidate_choice() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41, 52]);
        let position = Pos2::new(25.0, 25.0);

        for (time, pressed) in [(0.0, true), (0.01, false), (0.1, true)] {
            let _interaction = annotation_frame(
                &context,
                &mut viewport,
                vec![pointer_button_event(
                    position,
                    PointerButton::Primary,
                    pressed,
                )],
                time,
                &page,
            );
        }
        let interaction = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Primary,
                false,
            )],
            0.11,
            &page,
        );

        let Some(AnnotationUiAction::ChooseAnnotation(candidates)) = interaction.annotation_action
        else {
            panic!("overlapping annotations should preserve a candidate choice");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id.xref, 41);
        assert_eq!(candidates[1].id.xref, 52);
    }

    #[test]
    fn secondary_click_targets_annotation_without_starting_selection() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let page = annotation_page(&[41]);
        let position = Pos2::new(25.0, 25.0);

        let _pressed = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Secondary,
                true,
            )],
            0.0,
            &page,
        );
        let released = annotation_frame(
            &context,
            &mut viewport,
            vec![pointer_button_event(
                position,
                PointerButton::Secondary,
                false,
            )],
            0.01,
            &page,
        );

        assert!(released.completed_drag.is_none());
        assert_eq!(viewport.drag_page, None);
        assert_eq!(viewport.context_page, Some(0));
        assert_eq!(
            viewport
                .context_target
                .as_ref()
                .map(|target| target.candidates[0].id.xref),
            Some(41)
        );
    }

    #[test]
    fn secondary_click_on_current_selection_enables_selection_actions_only() {
        let context = egui::Context::default();
        let mut viewport = PageViewport::default();
        let position = Pos2::new(25.0, 25.0);
        let selected_quad = PageQuad {
            upper_left: PagePoint::new(0.0, 0.0),
            upper_right: PagePoint::new(20.0, 0.0),
            lower_left: PagePoint::new(0.0, 20.0),
            lower_right: PagePoint::new(20.0, 20.0),
        };
        let selection = SelectionSnapshot {
            page_index: 0,
            generation: 1,
            text: "A".to_owned(),
            display_quads: vec![selected_quad],
            quads: vec![selected_quad],
            extraction_time: Duration::ZERO,
        };
        let text = text_snapshot();
        for (time, pressed) in [(0.0, true), (0.01, false)] {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, [200.0, 200.0].into())),
                events: vec![pointer_button_event(
                    position,
                    PointerButton::Secondary,
                    pressed,
                )],
                time: Some(time),
                ..Default::default()
            };
            let _output = context.run_ui(input, |ui| {
                let interaction = viewport.interact_at(
                    ui,
                    PageInteractionInput {
                        screen_rect: Rect::from_min_size(
                            Pos2::new(20.0, 20.0),
                            [100.0, 100.0].into(),
                        ),
                        page_index: 0,
                        bounds: PageRect {
                            x0: 0.0,
                            y0: 0.0,
                            x1: 100.0,
                            y1: 100.0,
                        },
                        text_snapshot: Some(&text),
                        selection: Some(&selection),
                        annotation_page: None,
                        can_create_highlight: true,
                        suppress_annotation_hover: false,
                        input_excluded_rect: None,
                    },
                );
                assert!(interaction.completed_drag.is_none());
            });
        }

        let target = viewport
            .context_target
            .as_ref()
            .expect("selection context target");
        assert!(target.selection_available);
        assert!(target.candidates.is_empty());
    }
}
