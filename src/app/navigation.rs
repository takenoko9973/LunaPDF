use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DisplayMode {
    Continuous,
    SinglePage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ZoomMode {
    Fixed,
    FitWidth,
    FitPage,
}

pub(super) struct ViewState {
    pub(super) display_mode: DisplayMode,
    pub(super) zoom_mode: ZoomMode,
    pub(super) zoom: f32,
    pub(super) current_page: usize,
    pub(super) scroll_to_page: Option<usize>,
    pub(super) center_anchor: Option<PageAnchor>,
    pub(super) restore_anchor: Option<PageAnchor>,
    pub(super) single_center_anchor: Option<Vec2>,
    pub(super) restore_single_anchor: Option<Vec2>,
    pub(super) single_wheel: SinglePageWheelState,
    pub(super) autoscroll: Option<AutoscrollState>,
    pub(super) pan_requested_offset: Option<Vec2>,
    pub(super) render_pixels_per_point_bits: Option<u32>,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SinglePageWheelState {
    pub(super) accumulated_points: f32,
    pub(super) latched: bool,
    pub(super) direction: f32,
    pub(super) last_input_time: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AutoscrollState {
    pub(super) anchor: Pos2,
    pub(super) requested_offset: Option<Vec2>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SinglePageGeometry {
    pub(super) content_size: Vec2,
    pub(super) page_rect: Rect,
}

pub(super) fn adjacent_page_index(
    current: usize,
    page_count: usize,
    delta: isize,
) -> Option<usize> {
    let last_page = page_count.checked_sub(1)?;
    let target = current.saturating_add_signed(delta).min(last_page);
    (target != current).then_some(target)
}

/// ビューポート中心をズーム復元用のページ相対座標へ変換する。
pub(super) fn normalized_page_point(page_rect: Rect, point: Pos2) -> Vec2 {
    Vec2::new(
        ((point.x - page_rect.left()) / page_rect.width()).clamp(0.0, 1.0),
        ((point.y - page_rect.top()) / page_rect.height()).clamp(0.0, 1.0),
    )
}

/// 生のホイールイベントを、上限付きの単一ページ移動ステップへ変換する。
///
/// 端フラグはこのフレームの ScrollArea 処理前のスクロール位置を表す。
/// 端に到達しただけのイベントでページも変わることを防ぐ。
pub(super) fn single_page_wheel_steps(
    events: &[Event],
    pointer_over_view: bool,
    at_top: bool,
    at_bottom: bool,
    page_fits_vertically: bool,
    now: f64,
    state: &mut SinglePageWheelState,
) -> isize {
    expire_wheel_gesture_after_idle(now, state);
    let mut page_steps = 0_isize;

    for event in events {
        let Event::MouseWheel {
            unit,
            delta,
            phase,
            modifiers,
        } = event
        else {
            continue;
        };

        // バックエンドが最終フェーズを届ける前にポインターが PDF 領域を離れても、
        // End とキャンセルではラッチを解放する必要がある。
        if matches!(phase, TouchPhase::End | TouchPhase::Cancel) {
            reset_wheel_gesture(state);
            continue;
        }
        let vertical_input = delta.y.abs() > delta.x.abs() && delta.y.abs() > f32::EPSILON;
        if !pointer_over_view || modifiers.ctrl || !vertical_input {
            continue;
        }

        if *phase == TouchPhase::Start {
            reset_wheel_gesture(state);
        }
        let direction = delta.y.signum();
        let reversed = state.direction != 0.0 && state.direction != direction;
        if reversed {
            // 反転は意図的な新しい操作であり、累積距離も前ページのラッチも
            // 引き継いではならない。
            reset_wheel_gesture(state);
        }
        state.direction = direction;
        state.last_input_time = Some(now);

        let moves_to_previous = direction > 0.0 && at_top;
        let moves_to_next = direction < 0.0 && at_bottom;
        if !moves_to_previous && !moves_to_next {
            state.accumulated_points = 0.0;
            continue;
        }
        let page_delta = if moves_to_next { 1 } else { -1 };

        match unit {
            MouseWheelUnit::Line | MouseWheelUnit::Page => {
                // 生の離散イベントは 1 回の物理ホイール操作を表すため、プラットフォーム固有の
                // 数値の大きさでページ数を増やしてはならない。
                state.accumulated_points = 0.0;
                state.latched = false;
                page_steps += page_delta;
            }
            MouseWheelUnit::Point => {
                if state.latched {
                    continue;
                }
                state.accumulated_points += delta.y;
                if state.accumulated_points.abs() >= TRACKPAD_PAGE_THRESHOLD_POINTS {
                    state.accumulated_points = 0.0;
                    state.latched = true;
                    page_steps += page_delta;
                }
            }
        }
    }

    if page_fits_vertically {
        page_steps
    } else {
        // 1 回の端遷移後、拡大された隣接ページは反対側の端から始まるため、
        // 次のフレームまで評価できない。
        page_steps.signum()
    }
}

pub(super) fn expire_wheel_gesture_after_idle(now: f64, state: &mut SinglePageWheelState) {
    let idle = state
        .last_input_time
        .is_some_and(|last| now - last >= WHEEL_GESTURE_IDLE_SECONDS);
    if idle {
        reset_wheel_gesture(state);
    }
}

pub(super) fn reset_wheel_gesture(state: &mut SinglePageWheelState) {
    state.accumulated_points = 0.0;
    state.latched = false;
    state.direction = 0.0;
    state.last_input_time = None;
}

pub(super) struct AutoscrollFrame {
    pub(super) anchor: Pos2,
}

pub(super) struct AutoscrollOffsets {
    pub(super) current: Vec2,
    pub(super) maximum: Vec2,
}

/// 1 つの PDF ScrollArea のブラウザ風 autoscroll を開始・更新・停止する。
pub(super) fn update_autoscroll(
    context: &egui::Context,
    view: &mut ViewState,
    view_rect: Rect,
    excluded_rects: &[Rect],
    view_layer: LayerId,
    offsets: AutoscrollOffsets,
    primary_interaction_in_progress: bool,
) -> Option<AutoscrollFrame> {
    if view.display_mode != DisplayMode::Continuous {
        view.autoscroll = None;
        return None;
    }
    let input = context.input(|input| {
        (
            input.pointer.button_clicked(PointerButton::Middle),
            input.pointer.button_clicked(PointerButton::Primary),
            input.pointer.button_clicked(PointerButton::Secondary),
            input.key_pressed(Key::Escape),
            input.focused,
            input.pointer.hover_pos(),
            input.stable_dt.min(AUTOSCROLL_MAX_FRAME_SECONDS),
        )
    });
    let (middle_clicked, primary_clicked, secondary_clicked, escape, focused, pointer, dt) = input;

    if view.autoscroll.is_some() {
        let stop_requested =
            middle_clicked || primary_clicked || secondary_clicked || escape || !focused;
        if stop_requested {
            view.autoscroll = None;
            return None;
        }
    } else if middle_clicked
        && !primary_interaction_in_progress
        && pointer.is_some_and(|position| {
            // 前面ウィンドウや注釈エディターが中央矩形に重なることがある。
            // 中央レイヤーの除外されていない領域だけが中クリック開始を所有する。
            view_rect.contains(position)
                && excluded_rects.iter().all(|rect| !rect.contains(position))
                && context.layer_id_at(position) == Some(view_layer)
        })
    {
        let anchor = pointer.expect("the start condition requires a pointer position");
        view.autoscroll = Some(AutoscrollState {
            anchor,
            requested_offset: Some(offsets.current),
        });
    }

    let autoscroll = view.autoscroll.as_mut()?;
    let velocity = pointer.map_or(Vec2::ZERO, |position| {
        autoscroll_velocity(autoscroll.anchor, position)
    });
    let desired_offset = offsets.current + velocity * dt;
    autoscroll.requested_offset = Some(clamp_scroll_offset(desired_offset, offsets.maximum));
    context.request_repaint();
    Some(AutoscrollFrame {
        anchor: autoscroll.anchor,
    })
}

pub(super) fn autoscroll_velocity(anchor: Pos2, pointer: Pos2) -> Vec2 {
    let displacement = pointer - anchor;
    let distance = displacement.length();
    if distance <= AUTOSCROLL_DEAD_ZONE_POINTS {
        return Vec2::ZERO;
    }

    // 放射方向のスケーリングは、速度上限付近で軸ごとに独立してクリップして方向が
    // ずれる場合と異なり、斜め移動の方向を安定させる。
    let requested_speed = (distance - AUTOSCROLL_DEAD_ZONE_POINTS) * AUTOSCROLL_SPEED_PER_POINT;
    let speed = requested_speed.min(AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND);
    displacement / distance * speed
}

pub(super) fn clamp_scroll_offset(offset: Vec2, maximum: Vec2) -> Vec2 {
    Vec2::new(
        offset.x.clamp(0.0, maximum.x.max(0.0)),
        offset.y.clamp(0.0, maximum.y.max(0.0)),
    )
}

pub(super) fn pointer_over_any_rect(context: &egui::Context, rects: &[Rect]) -> bool {
    context.input(|input| {
        input
            .pointer
            .hover_pos()
            .is_some_and(|position| rects.iter().any(|rect| rect.contains(position)))
    })
}

/// `ScrollArea::id_salt` が保存する永続 ID を正確に生成する。
pub(super) fn scroll_area_state_id(ui: &egui::Ui, id_salt: impl egui::AsIdSalt) -> Id {
    // ScrollArea は呼び出し側のソルトを IdSalt にハッシュしてから親と結合する。
    // 生のタプルを make_persistent_id に直接渡すと別の値になり、常に空のスクロール状態を読む。
    ui.make_persistent_id(egui::IdSalt::new(id_salt))
}

pub(super) fn paint_autoscroll_marker(ui: &egui::Ui, view_rect: Rect, anchor: Pos2) {
    let painter = ui.painter().with_clip_rect(view_rect);
    let stroke = Stroke::new(1.5, ui.visuals().strong_text_color());
    painter.circle_filled(
        anchor,
        AUTOSCROLL_MARKER_RADIUS_POINTS,
        ui.visuals().panel_fill,
    );
    painter.circle_stroke(anchor, AUTOSCROLL_MARKER_RADIUS_POINTS, stroke);
    painter.circle_filled(anchor, 2.0, stroke.color);
}

/// 現在ページと先読みページで同じ中央揃えの ScrollArea 座標を構築する。
pub(super) fn single_page_geometry(
    bounds: crate::domain::document::PageRect,
    zoom: f32,
    viewport_size: Vec2,
) -> SinglePageGeometry {
    let display_size = Vec2::new(bounds.width() * zoom, bounds.height() * zoom);
    let content_size = Vec2::new(
        viewport_size.x.max(display_size.x + PAGE_GAP * 2.0),
        viewport_size.y.max(display_size.y + PAGE_GAP * 2.0),
    );
    let page_x = ((content_size.x - display_size.x) / 2.0).max(PAGE_GAP);
    let page_y = ((content_size.y - display_size.y) / 2.0).max(PAGE_GAP);
    SinglePageGeometry {
        content_size,
        page_rect: Rect::from_min_size(Pos2::new(page_x, page_y), display_size),
    }
}

pub(super) fn single_page_centered_offset(
    page_rect: Rect,
    normalized_anchor: Vec2,
    viewport_size: Vec2,
    content_size: Vec2,
) -> Vec2 {
    let anchor = Pos2::new(
        page_rect.left() + page_rect.width() * normalized_anchor.x,
        page_rect.top() + page_rect.height() * normalized_anchor.y,
    );
    let desired = anchor.to_vec2() - viewport_size / 2.0;
    let maximum = (content_size - viewport_size).max(Vec2::ZERO);
    Vec2::new(
        desired.x.clamp(0.0, maximum.x),
        desired.y.clamp(0.0, maximum.y),
    )
}

impl ViewState {
    /// 有効な表示密度を記録し、既存の描画要求が古い密度のデバイスピクセル座標を
    /// 使っているかを報告する。
    pub(super) fn update_render_density(&mut self, pixels_per_point: f32) -> bool {
        let current_bits = pixels_per_point.to_bits();
        let changed = self
            .render_pixels_per_point_bits
            .is_some_and(|previous_bits| previous_bits != current_bits);
        self.render_pixels_per_point_bits = Some(current_bits);
        changed
    }

    pub(super) fn stop_autoscroll(&mut self) {
        self.autoscroll = None;
    }

    pub(super) fn new() -> Self {
        Self {
            display_mode: DisplayMode::Continuous,
            zoom_mode: ZoomMode::FitWidth,
            zoom: 1.0,
            current_page: 0,
            scroll_to_page: Some(0),
            center_anchor: None,
            restore_anchor: None,
            single_center_anchor: None,
            restore_single_anchor: None,
            single_wheel: SinglePageWheelState::default(),
            autoscroll: None,
            pan_requested_offset: None,
            render_pixels_per_point_bits: None,
            generation: 1,
        }
    }

    pub(super) fn from_session(saved: SessionView) -> Self {
        let display_mode = match saved.display {
            SessionDisplayMode::Continuous => DisplayMode::Continuous,
            SessionDisplayMode::SinglePage => DisplayMode::SinglePage,
        };
        let zoom_mode = match saved.zoom_mode {
            SessionZoomMode::Fixed => ZoomMode::Fixed,
            SessionZoomMode::FitWidth => ZoomMode::FitWidth,
            SessionZoomMode::FitPage => ZoomMode::FitPage,
        };
        let page_anchor = PageAnchor {
            page_index: saved.page_index,
            page_x_fraction: saved.page_x,
            page_y_fraction: saved.page_y,
        };
        let single_anchor = Vec2::new(saved.page_x, saved.page_y);

        Self {
            display_mode,
            zoom_mode,
            zoom: saved.zoom,
            current_page: saved.page_index,
            scroll_to_page: None,
            center_anchor: (display_mode == DisplayMode::Continuous).then_some(page_anchor),
            restore_anchor: (display_mode == DisplayMode::Continuous).then_some(page_anchor),
            single_center_anchor: (display_mode == DisplayMode::SinglePage)
                .then_some(single_anchor),
            restore_single_anchor: (display_mode == DisplayMode::SinglePage)
                .then_some(single_anchor),
            single_wheel: SinglePageWheelState::default(),
            autoscroll: None,
            pan_requested_offset: None,
            render_pixels_per_point_bits: None,
            generation: 1,
        }
    }

    pub(super) fn to_session(&self) -> SessionView {
        let (page_index, page_x, page_y) = match self.display_mode {
            DisplayMode::Continuous => {
                // キューされたページ移動はまだスクロール領域の中心を更新していないため、
                // 前フレームのアンカーより優先する必要がある。
                let anchor = self
                    .scroll_to_page
                    .map(|page_index| PageAnchor {
                        page_index,
                        page_x_fraction: 0.5,
                        page_y_fraction: 0.5,
                    })
                    .or(self.center_anchor)
                    .or(self.restore_anchor)
                    .unwrap_or(PageAnchor {
                        page_index: self.current_page,
                        page_x_fraction: 0.5,
                        page_y_fraction: 0.5,
                    });
                (
                    anchor.page_index,
                    anchor.page_x_fraction,
                    anchor.page_y_fraction,
                )
            }
            DisplayMode::SinglePage => {
                let anchor = self
                    .single_center_anchor
                    .or(self.restore_single_anchor)
                    .unwrap_or(Vec2::splat(0.5));
                (self.current_page, anchor.x, anchor.y)
            }
        };

        SessionView {
            page_index,
            page_x,
            page_y,
            display: match self.display_mode {
                DisplayMode::Continuous => SessionDisplayMode::Continuous,
                DisplayMode::SinglePage => SessionDisplayMode::SinglePage,
            },
            zoom_mode: match self.zoom_mode {
                ZoomMode::Fixed => SessionZoomMode::Fixed,
                ZoomMode::FitWidth => SessionZoomMode::FitWidth,
                ZoomMode::FitPage => SessionZoomMode::FitPage,
            },
            zoom: self.zoom,
        }
    }

    pub(super) fn clamp_to_page_count(&mut self, page_count: usize) {
        let last_page = page_count.saturating_sub(1);
        self.current_page = self.current_page.min(last_page);
        self.scroll_to_page = self.scroll_to_page.map(|page| page.min(last_page));
        for anchor in [&mut self.center_anchor, &mut self.restore_anchor]
            .into_iter()
            .flatten()
        {
            // セッション位置は次のオープンがページ数を知る前に取得されるため、
            // より短い置換 PDF でも範囲内に収める。
            anchor.page_index = anchor.page_index.min(last_page);
        }
    }

    /// 2 つの表示モード間で PDF の中央座標を引き継ぐ。
    pub(super) fn switch_display_mode(&mut self, mode: DisplayMode) -> bool {
        if self.display_mode == mode {
            return false;
        }

        match (self.display_mode, mode) {
            (DisplayMode::Continuous, DisplayMode::SinglePage) => {
                let anchor = self.center_anchor.unwrap_or(PageAnchor {
                    page_index: self.current_page,
                    page_x_fraction: 0.5,
                    page_y_fraction: 0.5,
                });
                self.current_page = anchor.page_index;
                let normalized = Vec2::new(anchor.page_x_fraction, anchor.page_y_fraction);
                self.single_center_anchor = Some(normalized);
                self.restore_single_anchor = Some(normalized);
                self.scroll_to_page = None;
            }
            (DisplayMode::SinglePage, DisplayMode::Continuous) => {
                let normalized = self.single_center_anchor.unwrap_or(Vec2::splat(0.5));
                let anchor = PageAnchor {
                    page_index: self.current_page,
                    page_x_fraction: normalized.x,
                    page_y_fraction: normalized.y,
                };
                self.center_anchor = Some(anchor);
                self.restore_anchor = Some(anchor);
                self.scroll_to_page = None;
            }
            _ => unreachable!("LunaPDF has exactly two display modes"),
        }
        self.display_mode = mode;
        // どちらのポインタースクロールモードも現在の ScrollArea の座標系でオフセットを
        // 保存する。モード切り替え時は別レイアウトを表示する前にアンカーと保留中の
        // 2 軸移動を破棄しなければならない。
        self.autoscroll = None;
        self.pan_requested_offset = None;
        true
    }
}
