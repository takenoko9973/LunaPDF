use super::*;

impl TileCacheKey {
    pub(super) fn from_request(document_id: u64, request: &TileRequest) -> Self {
        Self {
            document_id,
            page_index: request.page_index,
            zoom_bits: request.zoom.to_bits(),
            pixels_per_point_bits: request.pixels_per_point.to_bits(),
            rotation_quarter_turns: 0,
            spec: request.spec,
            revision: request.expected_revision,
        }
    }

    pub(super) fn from_tile(document_id: u64, tile: &RenderedTile) -> Self {
        Self {
            document_id,
            page_index: tile.page_index,
            zoom_bits: tile.zoom.to_bits(),
            pixels_per_point_bits: tile.pixels_per_point.to_bits(),
            rotation_quarter_turns: 0,
            spec: tile.spec,
            revision: tile.revision,
        }
    }
}

impl ThumbnailCacheKey {
    pub(super) fn for_page(document_id: u64, page_index: usize, revision: u64) -> Self {
        Self {
            document_id,
            page_index,
            max_pixel_width: THUMBNAIL_MAX_WIDTH,
            max_pixel_height: THUMBNAIL_MAX_HEIGHT,
            revision,
        }
    }

    pub(super) fn from_thumbnail(document_id: u64, thumbnail: &RenderedThumbnail) -> Self {
        Self {
            document_id,
            page_index: thumbnail.page_index,
            max_pixel_width: thumbnail.max_pixel_width,
            max_pixel_height: thumbnail.max_pixel_height,
            revision: thumbnail.revision,
        }
    }

    pub(super) fn from_request(document_id: u64, request: &ThumbnailRequest) -> Self {
        Self {
            document_id,
            page_index: request.page_index,
            max_pixel_width: request.max_pixel_width,
            max_pixel_height: request.max_pixel_height,
            revision: request.expected_revision,
        }
    }
}

impl TextSnapshotKey {
    pub(super) fn from_snapshot(snapshot: &TextPageSnapshot) -> Self {
        Self {
            page_index: snapshot.page_index,
            revision: snapshot.revision,
        }
    }

    pub(super) fn from_request(request: &TextSnapshotRequest) -> Self {
        Self {
            page_index: request.page_index,
            revision: request.expected_revision,
        }
    }
}

/// 1 ページに必要なラスタータイルを列挙し、表示優先度を割り当てる。
pub(super) fn tile_requests_for_page(
    tab: &DocumentTab,
    page_index: usize,
    bounds: crate::domain::document::PageRect,
    page_screen_rect: Rect,
    visible_viewport: Rect,
    pixels_per_point: f32,
) -> Option<Vec<TileRequest>> {
    let scale = tab.view.zoom * pixels_per_point;
    let grid = TileGrid::new(bounds, scale)?;
    let prioritized_specs = prioritized_tile_specs(grid, page_screen_rect, visible_viewport)?;
    let revision = tab.info.as_ref()?.revision;
    let mut requests = Vec::with_capacity(prioritized_specs.len());
    for (spec, priority) in prioritized_specs {
        requests.push(TileRequest {
            page_index,
            zoom: tab.view.zoom,
            pixels_per_point,
            scale,
            generation: tab.view.generation,
            expected_revision: revision,
            spec,
            priority,
        });
    }
    Some(requests)
}

/// 現在ページと 2 つの遷移ビューを並べ、ナビゲーション後に現れる範囲外の
/// 拡大隣接ページはラスタライズしない。
pub(super) fn single_page_tile_requests(
    tab: &DocumentTab,
    page_bounds: &[crate::domain::document::PageRect],
    page_index: usize,
    current_page_rect: Rect,
    visible_viewport: Rect,
    viewport_size: Vec2,
    pixels_per_point: f32,
) -> Vec<TileRequest> {
    let Some(current_bounds) = page_bounds.get(page_index).copied() else {
        return Vec::new();
    };
    let mut requests = tile_requests_for_page(
        tab,
        page_index,
        current_bounds,
        current_page_rect,
        visible_viewport,
        pixels_per_point,
    )
    .unwrap_or_default();
    for request in &mut requests {
        if request.priority != RenderPriority::Visible {
            // スクロール方向にかかわらず、現在ページの 1 ビューポート余白を
            // どちらの隣接遷移範囲より先に完了させる。
            request.priority = RenderPriority::CurrentViewport;
        }
    }

    let horizontal_anchor = tab.view.single_center_anchor.unwrap_or(Vec2::splat(0.5)).x;
    if let Some(next_page) = page_index.checked_add(1)
        && let Some(bounds) = page_bounds.get(next_page).copied()
    {
        let mut next_requests = transition_tile_requests_for_page(
            tab,
            next_page,
            bounds,
            viewport_size,
            Vec2::new(horizontal_anchor, 0.0),
            pixels_per_point,
            RenderPriority::NextViewport,
        )
        .unwrap_or_default();
        requests.append(&mut next_requests);
    }
    if let Some(previous_page) = page_index.checked_sub(1)
        && let Some(bounds) = page_bounds.get(previous_page).copied()
    {
        let mut previous_requests = transition_tile_requests_for_page(
            tab,
            previous_page,
            bounds,
            viewport_size,
            Vec2::new(horizontal_anchor, 1.0),
            pixels_per_point,
            RenderPriority::PreviousViewport,
        )
        .unwrap_or_default();
        requests.append(&mut previous_requests);
    }
    requests
}

/// 端の遷移直後に見えるページ領域だけを要求する。
pub(super) fn transition_tile_requests_for_page(
    tab: &DocumentTab,
    page_index: usize,
    bounds: crate::domain::document::PageRect,
    viewport_size: Vec2,
    transition_anchor: Vec2,
    pixels_per_point: f32,
    priority: RenderPriority,
) -> Option<Vec<TileRequest>> {
    let geometry = single_page_geometry(bounds, tab.view.zoom, viewport_size);
    let offset = single_page_centered_offset(
        geometry.page_rect,
        transition_anchor,
        viewport_size,
        geometry.content_size,
    );
    let transition_viewport = Rect::from_min_size(offset.to_pos2(), viewport_size);
    let scale = tab.view.zoom * pixels_per_point;
    let grid = TileGrid::new(bounds, scale)?;
    let specs = tile_specs_intersecting_viewport(grid, geometry.page_rect, transition_viewport)?;
    let revision = tab.info.as_ref()?.revision;
    Some(
        specs
            .into_iter()
            .map(|spec| TileRequest {
                page_index,
                zoom: tab.view.zoom,
                pixels_per_point,
                scale,
                generation: tab.view.generation,
                expected_revision: revision,
                spec,
                priority,
            })
            .collect(),
    )
}

/// ラスター処理を可視領域とその周囲ちょうど 1 ビューポートに制限する。
pub(super) fn prioritized_tile_specs(
    grid: TileGrid,
    page_rect: Rect,
    visible_viewport: Rect,
) -> Option<Vec<(TileSpec, RenderPriority)>> {
    let margin = visible_viewport.size();
    let request_viewport =
        Rect::from_min_max(visible_viewport.min - margin, visible_viewport.max + margin);
    let specs = tile_specs_intersecting_viewport(grid, page_rect, request_viewport)?;
    Some(
        specs
            .into_iter()
            .map(|spec| {
                let tile_rect = logical_tile_rect(page_rect, grid, spec);
                (spec, tile_priority(tile_rect, visible_viewport))
            })
            .collect(),
    )
}

/// 要求された論理ページ領域と交差するタイルだけを列挙する。
pub(super) fn tile_specs_intersecting_viewport(
    grid: TileGrid,
    page_rect: Rect,
    request_viewport: Rect,
) -> Option<Vec<TileSpec>> {
    let requested_page_rect = page_rect.intersect(request_viewport);
    if !requested_page_rect.is_positive() {
        return Some(Vec::new());
    }
    let min_x = logical_edge_to_pixel(
        requested_page_rect.left(),
        page_rect.left(),
        page_rect.width(),
        grid.pixel_width(),
        false,
    )?;
    let min_y = logical_edge_to_pixel(
        requested_page_rect.top(),
        page_rect.top(),
        page_rect.height(),
        grid.pixel_height(),
        false,
    )?;
    let max_x = logical_edge_to_pixel(
        requested_page_rect.right(),
        page_rect.left(),
        page_rect.width(),
        grid.pixel_width(),
        true,
    )?;
    let max_y = logical_edge_to_pixel(
        requested_page_rect.bottom(),
        page_rect.top(),
        page_rect.height(),
        grid.pixel_height(),
        true,
    )?;
    let specs = grid.specs_in_pixel_rect(min_x, min_y, max_x, max_y)?;
    Some(
        specs
            .into_iter()
            .filter(|spec| logical_tile_rect(page_rect, grid, *spec).intersects(request_viewport))
            .collect(),
    )
}

/// 論理ページの端を、ページ内で上限を設けたデバイスピクセル端へ変換する。
pub(super) fn logical_edge_to_pixel(
    value: f32,
    page_start: f32,
    logical_extent: f32,
    pixel_extent: u32,
    round_up: bool,
) -> Option<u32> {
    if !value.is_finite() || !page_start.is_finite() || logical_extent <= 0.0 {
        return None;
    }
    let scaled = (value - page_start) / logical_extent * pixel_extent as f32;
    if !scaled.is_finite() {
        return None;
    }
    // 外向き丸めで、論理先読み矩形が触れるすべての端ピクセル（右端・下端の
    // 部分タイルを含む）を含める。
    let rounded = if round_up {
        scaled.ceil()
    } else {
        scaled.floor()
    };
    Some(rounded.clamp(0.0, pixel_extent as f32) as u32)
}

pub(super) fn logical_tile_rect(page_rect: Rect, grid: TileGrid, spec: TileSpec) -> Rect {
    let width = grid.pixel_width() as f32;
    let height = grid.pixel_height() as f32;
    let left = page_rect.left() + page_rect.width() * spec.pixel_x as f32 / width;
    let top = page_rect.top() + page_rect.height() * spec.pixel_y as f32 / height;
    let right =
        page_rect.left() + page_rect.width() * (spec.pixel_x + spec.pixel_width) as f32 / width;
    let bottom =
        page_rect.top() + page_rect.height() * (spec.pixel_y + spec.pixel_height) as f32 / height;
    Rect::from_min_max(Pos2::new(left, top), Pos2::new(right, bottom))
}

pub(super) fn tile_priority(tile_rect: Rect, visible_viewport: Rect) -> RenderPriority {
    if tile_rect.intersects(visible_viewport) {
        return RenderPriority::Visible;
    }
    // 右/下は連続表示とズーム単一ページのどちらでも通常の読み進め方向であり、
    // 左/上は低い順位として残す。
    if tile_rect.top() >= visible_viewport.bottom() || tile_rect.left() >= visible_viewport.right()
    {
        RenderPriority::NextViewport
    } else {
        RenderPriority::PreviousViewport
    }
}

/// 文書、ページ、revision、回転を正確に保ったまま、現在のズームに最も近い
/// キャッシュ済みラスターの同一性を 1 つ選ぶ。
pub(super) fn closest_provisional_tile_keys(
    keys: impl Iterator<Item = TileCacheKey>,
    document_id: u64,
    page_index: usize,
    revision: u64,
    rotation_quarter_turns: u8,
    current_zoom: f32,
    current_pixels_per_point_bits: u32,
) -> Vec<TileCacheKey> {
    let current_identity = (current_zoom.to_bits(), current_pixels_per_point_bits);
    let candidates = keys
        .filter(|key| {
            key.document_id == document_id
                && key.page_index == page_index
                && key.revision == revision
                && key.rotation_quarter_turns == rotation_quarter_turns
                && (key.zoom_bits, key.pixels_per_point_bits) != current_identity
        })
        .collect::<Vec<_>>();
    let current_pixels_per_point = f32::from_bits(current_pixels_per_point_bits);
    let best_identity = candidates
        .iter()
        .map(|key| (key.zoom_bits, key.pixels_per_point_bits))
        .min_by(|left, right| {
            let left_zoom = f32::from_bits(left.0);
            let right_zoom = f32::from_bits(right.0);
            let left_density = f32::from_bits(left.1);
            let right_density = f32::from_bits(right.1);
            // 対数比により半分と 2 倍の倍率を等距離にする。ズームを主に一致させ、
            // 論理対応が安全なため密度で同率を決める。
            let left_zoom_distance = (left_zoom / current_zoom).ln().abs();
            let right_zoom_distance = (right_zoom / current_zoom).ln().abs();
            let left_density_distance = (left_density / current_pixels_per_point).ln().abs();
            let right_density_distance = (right_density / current_pixels_per_point).ln().abs();
            left_zoom_distance
                .total_cmp(&right_zoom_distance)
                .then_with(|| left_density_distance.total_cmp(&right_density_distance))
        });
    let Some(best_identity) = best_identity else {
        return Vec::new();
    };
    let mut selected = candidates
        .into_iter()
        .filter(|key| (key.zoom_bits, key.pixels_per_point_bits) == best_identity)
        .collect::<Vec<_>>();
    selected.sort_by_key(|key| (key.spec.pixel_y, key.spec.pixel_x));
    selected
}

pub(super) fn paint_page_tiles(
    ui: &egui::Ui,
    screen_rect: Rect,
    page_index: usize,
    tab: &mut DocumentTab,
    gpu_lru: &mut WeightedLruCache<TileCacheKey, ()>,
) {
    ui.painter()
        .rect_filled(screen_rect, 2.0, Color32::from_gray(245));
    let exact_visible_total = tab
        .visible_tiles
        .iter()
        .filter(|key| key.page_index == page_index)
        .count();
    let exact_visible_cached = tab
        .visible_tiles
        .iter()
        .filter(|key| key.page_index == page_index && tab.tiles.contains_key(key))
        .count();
    let exact_visible_already_complete =
        exact_visible_total > 0 && exact_visible_cached == exact_visible_total;
    let provisional_keys = if exact_visible_already_complete {
        Vec::new()
    } else {
        tab.info
            .as_ref()
            .map(|info| info.revision)
            .zip(tab.view.render_pixels_per_point_bits)
            .map(|(revision, pixels_per_point_bits)| {
                let clip_rect = ui.clip_rect();
                let visible_cached_keys = tab.tiles.iter().filter_map(|(key, cached)| {
                    screen_rect_for_tile(screen_rect, &cached.tile)
                        .intersects(clip_rect)
                        .then_some(*key)
                });
                closest_provisional_tile_keys(
                    visible_cached_keys,
                    tab.document_id,
                    page_index,
                    revision,
                    0,
                    tab.view.zoom,
                    pixels_per_point_bits,
                )
            })
            .unwrap_or_default()
    };
    let mut provisional_painted = false;
    for key in provisional_keys {
        let retained = gpu_lru.get(&key).is_some();
        if retained && let Some(cached) = tab.tiles.get(&key) {
            // PageViewport は正規化ページ空間を介してデバイスピクセルを対応付けるため、
            // ここでは古い DPI でも安全で、下で正確なタイルに置き換えられる。
            PageViewport::paint_tile(ui, screen_rect, &cached.texture, &cached.tile);
            provisional_painted = true;
        }
    }

    let mut exact_keys = tab
        .wanted_tiles
        .iter()
        .filter(|key| key.page_index == page_index)
        .copied()
        .collect::<Vec<_>>();
    exact_keys.sort_by_key(|key| (key.spec.pixel_y, key.spec.pixel_x));

    #[cfg(debug_assertions)]
    let mut exact_visible_painted_count = 0;
    let mut exact_painted = false;
    for key in exact_keys {
        let retained = gpu_lru.get(&key).is_some();
        if retained && let Some(cached) = tab.tiles.get(&key) {
            PageViewport::paint_tile(ui, screen_rect, &cached.texture, &cached.tile);
            exact_painted = true;
            #[cfg(debug_assertions)]
            if tab.visible_tiles.contains(&key) {
                exact_visible_painted_count += 1;
                // 先読みタイルが遷移に使われた後の再訪は、別の先読み利用ではなく
                // 通常のキャッシュヒットとして扱う。
                if let Some(cached) = tab.tiles.get_mut(&key) {
                    cached.was_prefetched = false;
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let exact_visible_complete =
            exact_visible_total > 0 && exact_visible_painted_count == exact_visible_total;
        tab.render_performance.note_paint(
            page_index,
            tab.view.zoom,
            provisional_painted,
            exact_visible_painted_count > 0,
            exact_visible_complete,
            Instant::now(),
        );
    }

    let painted_any = provisional_painted || exact_painted;
    if !painted_any {
        ui.painter().text(
            screen_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("Rendering page {}…", page_index + 1),
            egui::TextStyle::Body.resolve(ui.style()),
            Color32::DARK_GRAY,
        );
    }
}
