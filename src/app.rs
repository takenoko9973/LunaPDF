mod events;
mod navigation;
mod rendering;
mod search;
#[cfg(test)]
mod tests;
mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{
    self, Color32, Event, Id, Key, LayerId, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, TouchPhase, UiBuilder, Vec2,
    ViewportCommand, pos2,
};

use crate::domain::annotation::{
    AnnotationDeleteRequest, AnnotationId, AnnotationPageRequest, AnnotationPageSnapshot,
    AnnotationSummary, HighlightIndexBatch, HighlightIndexRequest,
};
use crate::domain::document::{
    DocumentInfo, DocumentVersion, EditAction, HighlightRequest, OutlineItem, RenderPriority,
    RenderedThumbnail, RenderedTile, SearchMatch, SearchPageResult, ThumbnailRequest, TileRequest,
    TileSpec,
};
use crate::domain::selection::{
    PagePoint, SelectionSnapshot, TextPageSnapshot, TextSnapshotRequest,
};
use crate::domain::session::{
    DisplayMode as SessionDisplayMode, SessionEntry, SessionLayout, SessionState, SessionTab,
    SessionView, SidebarTab as SessionSidebarTab, SplitDirection as SessionSplitDirection,
    ZoomMode as SessionZoomMode,
};
use crate::domain::tabs::{
    OpenTabResult, RestoredTabEntry, SplitDirection, SplitGroupId, SplitSide, TabEntry, TabId,
    TabState,
};
use crate::pdf::{DocumentCommand, DocumentEvent, DocumentService, read_document_version};
use crate::persistence::session_store::SessionStore;
#[cfg(windows)]
use crate::platform::windows::default_apps::{
    DefaultAppState, default_app_menu_item, open_default_apps_settings, query_default_app_state,
};
use crate::render::cache::WeightedLruCache;
use crate::render::layout::{ContinuousLayout, PAGE_GAP, PageAnchor};
use crate::render::tiles::TileGrid;
use crate::ui::annotation_editor::{
    AnnotationEditorAction, AnnotationEditorState, AnnotationMenuCandidate, AnnotationUiAction,
    annotation_comment_id, annotation_overlay_rect, show_annotation_candidate_button,
    show_annotation_editor,
};
use crate::ui::cursor::set_pdf_cursor;
use crate::ui::fonts::install_ui_font;
use crate::ui::icons::{TOOLBAR_CONTROL_HEIGHT, ToolbarIcon, icon_button};
use crate::ui::sidebar::{HighlightSidebarAction, SidebarTab, show_highlights, show_outline};
use crate::ui::viewport::{
    PageInteraction, PageInteractionInput, PageViewport, pdf_cursor_icon, screen_rect_for_tile,
};

use navigation::{
    AutoscrollOffsets, DisplayMode, ViewState, ZoomMode, adjacent_page_index, clamp_scroll_offset,
    normalized_page_point, paint_autoscroll_marker, pointer_over_any_rect, scroll_area_state_id,
    single_page_centered_offset, single_page_geometry, single_page_wheel_steps, update_autoscroll,
};
#[cfg(test)]
use navigation::{AutoscrollState, SinglePageWheelState, autoscroll_velocity};
#[cfg(test)]
use rendering::{
    closest_provisional_tile_keys, logical_tile_rect, prioritized_tile_specs,
    tile_specs_intersecting_viewport,
};
use rendering::{paint_page_tiles, single_page_tile_requests, tile_requests_for_page};
#[cfg(test)]
use search::{
    SearchCursor, next_search_match, search_match_anchor, search_page_order,
    search_result_is_current,
};
use search::{SearchState, search_match_ordinal, search_query_id};
use workspace::{
    SplitRects, split_drop_highlight, split_drop_placement, split_rects, tab_insertion_index,
};

// 詳細確認と全体表示の両方を覆いつつ、誤ったホイール操作で
// ラスター割り当てが無制限に要求されない範囲に制限する。
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;

// パネルサイズが確定する過程ではサブピクセル丸めで Fit モードが変化しうる。
// 0.1% 未満の差を無視し、見た目が同じ連続フレームで全ページを無効化しない。
const ZOOM_CHANGE_EPSILON: f32 = 0.001;

// egui の操作領域の最小高さ 18 ポイントを保ち、24 ポイントの閉じる操作領域と
// 最小幅でも読めるタイトル領域を確保し、従来無制限だったファイル名を
// 一般的なデスクトップのタブ幅に収める。
const TAB_MIN_WIDTH: f32 = 96.0;
const TAB_MAX_WIDTH: f32 = 240.0;
const TAB_HEIGHT: f32 = 24.0;
const TAB_HORIZONTAL_PADDING: f32 = 8.0;
const TAB_CLOSE_WIDTH: f32 = 24.0;
const TAB_CONTENT_GAP: f32 = 4.0;
const TAB_ITEM_SPACING: f32 = 1.0;

// 8 ポイントのベクター X は 24 ポイントの閉じる操作領域内で判読でき、
// 最小タブ高さでも周囲に十分なホバー時の塗りを残せる。
const TAB_CLOSE_ICON_HALF_SIZE: f32 = 4.0;
const TAB_CLOSE_ICON_STROKE_WIDTH: f32 = 1.5;

// ドラッグ中のタブ影がポインターとdrop境界を隠さず、通常タブと同程度の
// 情報量を保つための論理ポイント値。
const TAB_DRAG_PREVIEW_OFFSET: f32 = 12.0;
const TAB_DRAG_PREVIEW_PADDING: f32 = 8.0;

// 最初の 3 桁分を確保してツールバーの列幅を安定させる。長い文書では必要な
// 桁数をすべて測定し、ページ入力を切り詰めたり拒否したりしない。
const PAGE_INPUT_MINIMUM_COLUMNS: usize = 3;

// 設計上の予算は全タブで共有し、タブごとに固定量を分割せず、アクティブ文書が
// 利用可能な GPU メモリを使えるようにする。
const GPU_TILE_BUDGET_BYTES: usize = 192 * 1_024 * 1_024;

// サムネイルには専用予算を設け、長いサイドバーが 192 MiB のレンダーキャッシュ
// からアクティブページの表示タイルを追い出さないようにする。
const THUMBNAIL_BUDGET_BYTES: usize = 32 * 1_024 * 1_024;
const THUMBNAIL_MAX_WIDTH: u32 = 160;
const THUMBNAIL_MAX_HEIGHT: u32 = 220;
const THUMBNAIL_ROW_HEIGHT: f32 = 248.0;

// リリース性能マトリクスでは、8 ページが最初の結果とキャンセル境界を最短にし、
// 全体のスキャン時間も増やさなかった。
const HIGHLIGHT_INDEX_BATCH_PAGES: usize = 8;

// 高精度デバイスは離散的なホイール段階ではなくポイント差分を送る。
// 24 論理ポイントなら端での偶発的な動きを除外しつつ、意図した短いトラックパッド
// 操作を一般的な 1 行分以上遅延させない。
const TRACKPAD_PAGE_THRESHOLD_POINTS: f32 = 24.0;

// バックエンドが差分 0 のフレームを正確に送らない場合でも、短いアイドル区間で
// トラックパッドの慣性と次の意図的な操作を分離する。
const WHEEL_GESTURE_IDLE_SECONDS: f64 = 0.150;

// 1 論理ポイントで PAGE_GAP と ScrollArea の小数丸めを吸収するが、可視ページの
// 内容を飛ばすには小さすぎる。
const SINGLE_PAGE_EDGE_TOLERANCE_POINTS: f32 = 1.0;

// ブラウザ風 autoscroll はアンカー付近で停止したままにする。12 論理ポイントなら
// 一般的なデスクトップ DPI でのクリックの揺れを許容できる。
const AUTOSCROLL_DEAD_ZONE_POINTS: f32 = 12.0;
const AUTOSCROLL_SPEED_PER_POINT: f32 = 12.0;

// 長いポインター移動と停止したフレームの双方を制限する。前者は制御不能なジャンプを
// 防ぎ、100 ms なら 1 フレームを 480 論理ポイント未満に保てる。
const AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND: f32 = 4_800.0;
const AUTOSCROLL_MAX_FRAME_SECONDS: f32 = 0.100;

const AUTOSCROLL_MARKER_RADIUS_POINTS: f32 = 8.0;

// 250 ms 以内の入力をデバッグ計測上は 1 つの連続ズーム操作とみなす。最新倍率の
// 描画を遅らせない十分短い間隔である。
#[cfg(debug_assertions)]
const ZOOM_INPUT_GROUP_IDLE_SECONDS: f64 = 0.250;

// N-05 はプロセスの安定目標を 512 MiB と定める。サスペンドはこの上限を超えた後だけ
// 許可し、通常のタブ切り替えでは文書を保持する。
const RESIDENT_MEMORY_SUSPEND_THRESHOLD_BYTES: usize = 512 * 1_024 * 1_024;

pub(crate) struct PrototypeApp {
    tabs: TabState,
    documents: Vec<DocumentTab>,
    viewports: HashMap<SplitSide, PageViewport>,
    status: String,
    error: Option<String>,
    #[cfg(windows)]
    default_apps_state: DefaultAppState,
    #[cfg(windows)]
    default_apps_menu_open: bool,
    #[cfg(windows)]
    auto_rotate_print: bool,
    close_confirmation: Option<CloseConfirmation>,
    approved_window_documents: HashSet<PathBuf>,
    allow_window_close: bool,
    window_close_pending: bool,
    close_all_pending: bool,
    session_close_failure: Option<String>,
    saved_tab_to_close: Option<PathBuf>,
    session_store: SessionStore,
    restore_enabled: bool,
    session_restore_progress: Option<SessionRestoreProgress>,
    external_open_events: Receiver<std::result::Result<Vec<PathBuf>, String>>,
    next_document_id: u64,
    activity_sequence: u64,
    // 表示要求は次のタブバーのフレームで消費する。一度だけにすることで、その後に
    // ユーザーがスクロールして非アクティブなタブを確認できる。
    tab_to_reveal: Option<usize>,
    tab_drag: Option<TabDragState>,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    gpu_lru: WeightedLruCache<TileCacheKey, ()>,
    thumbnail_lru: WeightedLruCache<ThumbnailCacheKey, ()>,
    annotation_editor: Option<AnnotationEditorState>,
    annotation_picker: Option<AnnotationPickerState>,
    recent_annotation_colors: Vec<[u8; 3]>,
    // egui-winit は対応する押下状態の Key イベントなしに Event::Copy を送る。
    // 解放イベントでラッチを解除し、OS のキーリピートで PDF を再コピーしない。
    copy_shortcut_active: bool,
    last_external_check: Instant,
}

struct DocumentTab {
    document_id: u64,
    service: Option<DocumentService>,
    state: DocumentState,
    last_selected_sequence: u64,
    info: Option<DocumentInfo>,
    error: Option<String>,
    outline: Option<Vec<OutlineItem>>,
    outline_requested: bool,
    tiles: HashMap<TileCacheKey, CachedTile>,
    pending_tiles: HashMap<TileCacheKey, TileRequest>,
    wanted_tiles: HashSet<TileCacheKey>,
    visible_tiles: HashSet<TileCacheKey>,
    text_snapshots: HashMap<TextSnapshotKey, TextPageSnapshot>,
    pending_text_snapshots: HashSet<TextSnapshotKey>,
    failed_text_snapshots: HashSet<TextSnapshotKey>,
    wanted_text_snapshots: HashSet<TextSnapshotKey>,
    annotation_pages: HashMap<AnnotationPageRequest, AnnotationPageSnapshot>,
    pending_annotation_pages: HashSet<AnnotationPageRequest>,
    failed_annotation_pages: HashSet<AnnotationPageRequest>,
    wanted_annotation_pages: HashSet<AnnotationPageRequest>,
    highlight_index: HighlightIndexState,
    pending_highlight_refresh_page: Option<usize>,
    selection: Option<SelectionSnapshot>,
    selection_generation: u64,
    pending_edits: usize,
    edit_history: Vec<EditAction>,
    undo_in_flight: bool,
    save_in_flight: bool,
    print_in_flight: bool,
    thumbnails: HashMap<ThumbnailCacheKey, CachedThumbnail>,
    pending_thumbnails: HashSet<ThumbnailCacheKey>,
    failed_thumbnails: HashSet<ThumbnailCacheKey>,
    thumbnail_generation: u64,
    search: SearchState,
    page_input: String,
    page_input_error: Option<String>,
    view: ViewState,
    #[cfg(debug_assertions)]
    render_performance: RenderPerformance,
    restoring_from_session: bool,
    external_candidate: Option<(DocumentVersion, u8)>,
    external_conflict_reported: bool,
    reload_in_flight: bool,
    saved_as_path: Option<PathBuf>,
}

struct HighlightIndexState {
    generation: u64,
    revision: Option<u64>,
    total_pages: usize,
    pages: BTreeMap<usize, Vec<AnnotationSummary>>,
    in_flight: Option<HighlightIndexRequest>,
    refresh_page: Option<usize>,
    started: bool,
    error: Option<String>,
}

impl Default for HighlightIndexState {
    fn default() -> Self {
        Self {
            generation: 1,
            revision: None,
            total_pages: 0,
            pages: BTreeMap::new(),
            in_flight: None,
            refresh_page: None,
            started: false,
            error: None,
        }
    }
}

fn next_highlight_index_request(index: &HighlightIndexState) -> Option<HighlightIndexRequest> {
    if !index.started || index.in_flight.is_some() || index.error.is_some() {
        return None;
    }
    let revision = index.revision?;
    let (first_page, page_count) = if let Some(page_index) = index.refresh_page {
        (page_index, 1)
    } else {
        let first_page =
            (0..index.total_pages).find(|page_index| !index.pages.contains_key(page_index))?;
        let page_count = (first_page..index.total_pages)
            .take_while(|page_index| !index.pages.contains_key(page_index))
            .take(HIGHLIGHT_INDEX_BATCH_PAGES)
            .count();
        (first_page, page_count)
    };
    Some(HighlightIndexRequest {
        generation: index.generation,
        expected_revision: revision,
        first_page,
        page_count,
    })
}

/// 保留中の要求と完全に一致する応答の場合だけページ行を置き換える。
fn apply_highlight_index_batch(
    index: &mut HighlightIndexState,
    batch: HighlightIndexBatch,
) -> bool {
    let request = HighlightIndexRequest {
        generation: batch.generation,
        expected_revision: batch.revision,
        first_page: batch
            .pages
            .first()
            .map_or(batch.total_pages, |page| page.page_index),
        page_count: batch.pages.len(),
    };
    let current = index.in_flight == Some(request)
        && index.generation == batch.generation
        && index.revision == Some(batch.revision)
        && index.total_pages == batch.total_pages;
    if !current {
        return false;
    }

    index.in_flight = None;
    for page in batch.pages {
        if index.refresh_page == Some(page.page_index) {
            index.refresh_page = None;
        }
        index.pages.insert(page.page_index, page.highlights);
    }
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentState {
    Opening,
    ReadyClean,
    ReadyDirty,
    Saving,
    Suspended,
    Error,
}

struct CachedTile {
    tile: RenderedTile,
    texture: TextureHandle,
    #[cfg(debug_assertions)]
    was_prefetched: bool,
}

struct CachedThumbnail {
    thumbnail: RenderedThumbnail,
    texture: TextureHandle,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ThumbnailCacheKey {
    document_id: u64,
    page_index: usize,
    max_pixel_width: u32,
    max_pixel_height: u32,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TextSnapshotKey {
    page_index: usize,
    revision: u64,
}

struct AnnotationPickerState {
    document_id: u64,
    revision: u64,
    candidates: Vec<AnnotationMenuCandidate>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TileCacheKey {
    document_id: u64,
    page_index: usize,
    zoom_bits: u32,
    pixels_per_point_bits: u32,
    rotation_quarter_turns: u8,
    spec: TileSpec,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseScope {
    Tab,
    AllTabs,
    Window,
}

struct CloseConfirmation {
    scope: CloseScope,
    path: PathBuf,
    save_in_flight: bool,
}

#[derive(Clone, Copy)]
enum CloseDecision {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone, Copy)]
enum SessionCloseDecision {
    Retry,
    ExitWithoutSession,
    Cancel,
}

enum OpenIntent {
    User,
    Restored { view: SessionView },
}

#[derive(Clone, Copy)]
enum OpenDocumentResult {
    Pending(usize),
    Existing(usize),
}

struct SessionRestoreProgress {
    requested: usize,
    pending: usize,
    restored: usize,
    skipped: usize,
}

fn restored_runtime_layout(
    saved: &SessionLayout,
    runtime_tab_ids: &[Option<TabId>],
) -> (Vec<RestoredTabEntry>, Option<TabId>) {
    let mut seen = HashSet::new();
    let saved_active_entry = saved.active_tab.and_then(|active_tab| {
        saved.entries.iter().position(|entry| match entry {
            SessionEntry::Single { tab_index } => *tab_index == active_tab,
            SessionEntry::Split { tab_indices, .. } => tab_indices.contains(&active_tab),
        })
    });
    let mut restored = Vec::new();
    let mut source_positions = Vec::new();
    for (source_position, entry) in saved.entries.iter().enumerate() {
        let restored_entry = match entry {
            SessionEntry::Single { tab_index } => runtime_tab_ids
                .get(*tab_index)
                .copied()
                .flatten()
                .filter(|tab_id| seen.insert(*tab_id))
                .map(RestoredTabEntry::Single),
            SessionEntry::Split {
                tab_indices,
                direction,
                ratio,
                focused_tab,
            } => {
                let members = tab_indices.map(|tab_index| {
                    runtime_tab_ids
                        .get(tab_index)
                        .copied()
                        .flatten()
                        .filter(|tab_id| seen.insert(*tab_id))
                });
                match members {
                    [Some(first), Some(second)] => Some(RestoredTabEntry::Split {
                        tabs: [first, second],
                        direction: match direction {
                            SessionSplitDirection::Horizontal => SplitDirection::Horizontal,
                            SessionSplitDirection::Vertical => SplitDirection::Vertical,
                        },
                        ratio: *ratio,
                        focused: if *focused_tab == tab_indices[0] {
                            SplitSide::First
                        } else {
                            SplitSide::Second
                        },
                    }),
                    [Some(tab_id), None] | [None, Some(tab_id)] => {
                        Some(RestoredTabEntry::Single(tab_id))
                    }
                    [None, None] => None,
                }
            }
        };
        if let Some(entry) = restored_entry {
            restored.push(entry);
            source_positions.push(source_position);
        }
    }

    let saved_active = saved
        .active_tab
        .and_then(|index| runtime_tab_ids.get(index).copied().flatten())
        .filter(|tab_id| seen.contains(tab_id));
    let active = saved_active.or_else(|| {
        let target_position = saved_active_entry.unwrap_or(0);
        let restored_index = source_positions
            .iter()
            .position(|position| *position >= target_position)
            .or_else(|| source_positions.len().checked_sub(1))?;
        restored_entry_focus(&restored[restored_index])
    });
    (restored, active)
}

fn restored_entry_focus(entry: &RestoredTabEntry) -> Option<TabId> {
    match entry {
        RestoredTabEntry::Single(tab_id) => Some(*tab_id),
        RestoredTabEntry::Split { tabs, focused, .. } => Some(tabs[focused.index()]),
    }
}

fn foreground_layer_blocks_pane_input(layer: Option<LayerId>) -> bool {
    layer.is_some_and(|layer| {
        // MiddleはWindow、Foregroundは注釈editorやpopupが使用する。Tooltipは
        // 操作対象ではないため、タブdrop先の探索を妨げない。
        matches!(layer.order, egui::Order::Middle | egui::Order::Foreground)
    })
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct RenderPerformance {
    page_transition: Option<PageTransitionPerformance>,
    zoom: Option<ZoomPerformance>,
}

#[cfg(debug_assertions)]
struct PageTransitionPerformance {
    target_page: usize,
    input_at: Instant,
    first_exact_tile: Option<Duration>,
    full_exact_viewport: Option<Duration>,
    cache_hit: Option<bool>,
    prefetch_used: Option<bool>,
}

#[cfg(debug_assertions)]
struct ZoomPerformance {
    target_zoom: f32,
    last_input_at: Instant,
    provisional_display: Option<Duration>,
    first_exact_tile: Option<Duration>,
    full_exact_viewport: Option<Duration>,
    discarded_intermediate_requests: usize,
}

#[derive(Clone, Copy, Debug)]
struct TabContentRects {
    selection: Rect,
    title: Rect,
    close: Rect,
}

#[derive(Clone, Copy, Debug)]
struct TabDragState {
    source: TabDragSource,
}

#[derive(Clone, Copy, Debug)]
enum TabDragSource {
    Tab(TabId),
    Group(SplitGroupId),
}

#[derive(Clone, Copy, Debug)]
enum SplitMenuAction {
    SetDirection(SplitGroupId, SplitDirection),
    Swap(SplitGroupId),
    Unsplit(SplitGroupId),
}

#[derive(Debug)]
struct TabBarOutput {
    bar_rect: Rect,
    entry_rects: Vec<Rect>,
    tab_rects: Vec<Rect>,
    tab_ids: Vec<TabId>,
    group_rects: Vec<(SplitGroupId, Rect)>,
    select: Option<TabId>,
    close: Option<TabId>,
    drag_started: Option<TabDragSource>,
    drag_released: bool,
    menu_action: Option<SplitMenuAction>,
}

#[derive(Debug)]
struct PaneUiOutput {
    side: SplitSide,
    tab_id: TabId,
    pdf_rect: Rect,
}

struct TabPaintState<'a> {
    selected: bool,
    focused: bool,
    can_close: bool,
    select_response: &'a egui::Response,
    close_response: &'a egui::Response,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TabPointerAction {
    Select,
    Close,
}

fn tab_pointer_action(primary_clicked: bool, middle_clicked: bool) -> Option<TabPointerAction> {
    if middle_clicked {
        Some(TabPointerAction::Close)
    } else if primary_clicked {
        Some(TabPointerAction::Select)
    } else {
        None
    }
}

/// 水平スクロールへ移行する場合も含め、均等なタブ幅を返す。
fn tab_width_for_count(
    available_width: f32,
    tab_count: usize,
    item_spacing: f32,
    minimum_width: f32,
    maximum_width: f32,
) -> f32 {
    if tab_count == 0 {
        return 0.0;
    }
    let spacing_total = item_spacing * tab_count.saturating_sub(1) as f32;
    let width_for_tabs = (available_width - spacing_total).max(0.0);
    let equal_width = width_for_tabs / tab_count as f32;
    equal_width.clamp(minimum_width, maximum_width)
}

fn tab_reveal_for_selection_change(previous: Option<usize>, selected: usize) -> Option<usize> {
    (previous != Some(selected)).then_some(selected)
}

fn tab_reveal_after_close(
    previous: Option<usize>,
    closed: usize,
    current: Option<usize>,
) -> Option<usize> {
    let selected_document_changed = previous == Some(closed);
    selected_document_changed.then_some(current).flatten()
}

/// 残りのタブ幅をテキストへ割り当てる前に閉じる操作領域を確保する。
fn tab_content_rects(
    tab_rect: Rect,
    horizontal_padding: f32,
    close_width: f32,
    content_gap: f32,
) -> TabContentRects {
    let close = Rect::from_min_max(
        Pos2::new(tab_rect.right() - close_width, tab_rect.top()),
        tab_rect.right_bottom(),
    );
    let title = Rect::from_min_max(
        Pos2::new(tab_rect.left() + horizontal_padding, tab_rect.top()),
        Pos2::new(close.left() - content_gap, tab_rect.bottom()),
    );
    let selection = Rect::from_min_max(tab_rect.min, Pos2::new(close.left(), tab_rect.bottom()));
    TabContentRects {
        selection,
        title,
        close,
    }
}

fn paint_document_tab(
    ui: &egui::Ui,
    tab_rect: Rect,
    content: TabContentRects,
    title: &str,
    state: TabPaintState<'_>,
) {
    let pointer_over_tab = state.select_response.hovered() || state.close_response.hovered();
    let (background, outline) = if state.selected {
        (
            ui.visuals().selection.bg_fill,
            ui.visuals().selection.stroke,
        )
    } else if pointer_over_tab {
        let hovered = &ui.visuals().widgets.hovered;
        (hovered.weak_bg_fill, hovered.bg_stroke)
    } else {
        let inactive = &ui.visuals().widgets.inactive;
        (inactive.weak_bg_fill, inactive.bg_stroke)
    };
    ui.painter()
        .rect(tab_rect, 4.0, background, outline, StrokeKind::Inside);
    if state.selected && state.focused {
        // 両ペインにはそれぞれ選択タブがあるため、共通UIの対象だけを下線で区別する。
        ui.painter().line_segment(
            [tab_rect.left_bottom(), tab_rect.right_bottom()],
            Stroke::new(2.0, ui.visuals().selection.stroke.color),
        );
    }

    let text_color = if state.selected {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let mut title_job = egui::text::LayoutJob::single_section(
        title.to_owned(),
        egui::TextFormat {
            font_id: egui::TextStyle::Button.resolve(ui.style()),
            color: text_color,
            ..Default::default()
        },
    );
    title_job.wrap = egui::epaint::text::TextWrapping::truncate_at_width(content.title.width());
    let title_galley = ui.fonts_mut(|fonts| fonts.layout_job(title_job));
    let title_position = Pos2::new(
        content.title.left(),
        content.title.center().y - title_galley.size().y / 2.0,
    );
    ui.painter()
        .with_clip_rect(content.title)
        .galley(title_position, title_galley, text_color);

    // 閉じる領域はタブ背景を共有し、ホバー時だけ局所的に塗る。旧来の枠付きボタンを
    // 再現せずに、操作箇所を見つけられるようにする。
    if state.can_close && state.close_response.hovered() {
        ui.painter().rect_filled(
            content.close.shrink(2.0),
            3.0,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    }
    let close_color = if state.can_close {
        text_color
    } else {
        ui.visuals().weak_text_color()
    };
    let close_stroke = Stroke::new(TAB_CLOSE_ICON_STROKE_WIDTH, close_color);
    for segment in close_icon_segments(content.close) {
        ui.painter().line_segment(segment, close_stroke);
    }
}

fn close_icon_segments(close_rect: Rect) -> [[Pos2; 2]; 2] {
    let center = close_rect.center();
    let offset = Vec2::splat(TAB_CLOSE_ICON_HALF_SIZE);
    [
        [center - offset, center + offset],
        [
            center + Vec2::new(-offset.x, offset.y),
            center + Vec2::new(offset.x, -offset.y),
        ],
    ]
}

impl PrototypeApp {
    /// アプリケーションを生成し、コマンドラインで指定された各 PDF を開く。
    pub(crate) fn new(
        creation_context: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        session_store: SessionStore,
        external_open_events: Receiver<std::result::Result<Vec<PathBuf>, String>>,
    ) -> Self {
        install_ui_font(&creation_context.egui_ctx);
        egui_extras::install_image_loaders(&creation_context.egui_ctx);
        let mut app = Self::from_startup(paths, session_store);
        app.external_open_events = external_open_events;
        app
    }

    fn from_startup(paths: Vec<PathBuf>, session_store: SessionStore) -> Self {
        let (saved_session, session_load_error) = match session_store.load() {
            Ok(session) => (session, None),
            Err(error) => (
                None,
                Some(format!(
                    "前回のセッションを復元できませんでした。今回は復元を省略します。詳細: {error}"
                )),
            ),
        };
        let restore_enabled = saved_session
            .as_ref()
            .is_none_or(|session| session.restore_enabled);
        // 色設定はタブとは独立して復元し、コマンドライン指定の PDF を開いても
        // ユーザーの編集履歴を破棄しない。
        let recent_annotation_colors = saved_session
            .as_ref()
            .map(|session| session.recent_annotation_colors.clone())
            .unwrap_or_default();
        let tabs = TabState::new();
        let mut viewports = HashMap::new();
        viewports.insert(SplitSide::First, PageViewport::default());
        let mut app = Self {
            tabs,
            documents: Vec::new(),
            viewports,
            status: "Drop a PDF into the window to open it".to_owned(),
            error: session_load_error,
            #[cfg(windows)]
            default_apps_state: DefaultAppState::Unavailable("まだ照会していません".to_owned()),
            #[cfg(windows)]
            default_apps_menu_open: false,
            #[cfg(windows)]
            auto_rotate_print: true,
            close_confirmation: None,
            approved_window_documents: HashSet::new(),
            allow_window_close: false,
            window_close_pending: false,
            close_all_pending: false,
            session_close_failure: None,
            saved_tab_to_close: None,
            session_store,
            restore_enabled,
            session_restore_progress: None,
            external_open_events: crossbeam_channel::never(),
            next_document_id: 1,
            activity_sequence: 0,
            tab_to_reveal: None,
            tab_drag: None,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Outline,
            gpu_lru: WeightedLruCache::new(GPU_TILE_BUDGET_BYTES),
            thumbnail_lru: WeightedLruCache::new(THUMBNAIL_BUDGET_BYTES),
            annotation_editor: None,
            annotation_picker: None,
            recent_annotation_colors,
            copy_shortcut_active: false,
            last_external_check: Instant::now(),
        };
        if restore_enabled && let Some(session) = saved_session {
            app.restore_session(session);
        }
        // 起動引数の PDF は、セッション復元後に通常のユーザーオープン経路へ渡す。
        for path in paths {
            app.open_document(path);
        }
        app
    }

    #[cfg(windows)]
    fn refresh_default_apps_state(&mut self) {
        self.default_apps_state = match query_default_app_state() {
            Ok(state) => state,
            Err(error) => DefaultAppState::Unavailable(error.to_string()),
        };
    }

    fn open_document(&mut self, path: PathBuf) {
        let _opened_index = self.open_document_with_intent(path, OpenIntent::User);
    }

    /// 短いポーリングで安定した外部版だけを採用する。通知 API の一回限りのイベントでは
    /// 0 byte・delete/recreate・atomic replace の途中状態を区別できないためである。
    fn check_external_changes(&mut self) {
        if self.last_external_check.elapsed() < Duration::from_millis(300) {
            return;
        }
        self.last_external_check = Instant::now();
        for index in 0..self.documents.len() {
            let Some(info) = self.documents[index].info.as_ref() else {
                continue;
            };
            if self.documents[index].state == DocumentState::Suspended {
                continue;
            }
            let path = self.tabs.tabs()[index].path().to_path_buf();
            let expected = info.version;
            let current = read_document_version(&path).ok();
            if current == Some(expected) {
                self.documents[index].external_candidate = None;
                continue;
            }

            if current.is_none() {
                if let Some(renamed) = find_same_folder_rename(&path, expected) {
                    if self.documents[index].send(DocumentCommand::RebindPath(renamed)) {
                        self.status = "PDF の名前変更を追跡しています…".to_owned();
                    }
                }
                continue;
            }
            let current = current.expect("checked above");
            let stable_count = match self.documents[index].external_candidate {
                Some((candidate, count)) if candidate == current => count.saturating_add(1),
                _ => 1,
            };
            self.documents[index].external_candidate = Some((current, stable_count));
            if stable_count < 2 || self.documents[index].reload_in_flight {
                continue;
            }
            let document_id = self.documents[index].document_id;
            let editor_dirty = self.annotation_editor.as_ref().is_some_and(|editor| {
                editor.document_id == document_id
                    && (editor.is_dirty() || editor.mutation_in_flight)
            });
            if self.documents[index].has_unsaved_changes() || editor_dirty {
                if !self.documents[index].external_conflict_reported {
                    self.documents[index].external_conflict_reported = true;
                    self.documents[index].error = Some(
                        "外部でPDFが更新されました。未保存の編集を保護するため、自動再読み込みを停止しています。編集版を別名保存してから外部版を再読み込みしてください。".to_owned(),
                    );
                }
                continue;
            }
            self.documents[index].reload_in_flight =
                self.documents[index].send(DocumentCommand::Reload(path));
            if self.documents[index].reload_in_flight {
                self.documents[index].invalidate_rendering();
                self.documents[index].invalidate_text_snapshots();
                self.documents[index].invalidate_annotation_pages();
                self.status = "外部更新されたPDFを再読み込みしています…".to_owned();
            }
        }
    }

    fn save_conflicted_document_as(&mut self, index: usize) {
        let source = self.tabs.tabs()[index].path();
        let selected = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(
                source
                    .file_name()
                    .unwrap_or(source.as_os_str())
                    .to_string_lossy()
                    .into_owned(),
            )
            .save_file();
        let Some(path) = selected else {
            return;
        };
        if self.documents[index].send(DocumentCommand::SaveAs(path.clone())) {
            self.documents[index].saved_as_path = Some(path);
            self.status = "編集版を別名保存しています…".to_owned();
        }
    }

    /// ネイティブピッカーを開き、明示的に選択された PDF だけを渡す。
    fn pick_pdf_and_open(&mut self) {
        // ネイティブピッカーの間は PDF の操作面が一時的に置き換わるため、
        // ダイアログから戻ったときにアンカーを再開してはならない。
        self.stop_visible_autoscroll();
        self.cancel_all_viewport_interactions();
        let selected = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file();
        if let Some(path) = selected {
            self.open_document(path);
        }
    }

    fn stop_visible_autoscroll(&mut self) {
        for index in self.visible_indices() {
            self.documents[index].view.stop_autoscroll();
        }
    }

    fn cancel_all_viewport_interactions(&mut self) {
        for viewport in self.viewports.values_mut() {
            viewport.cancel_primary_interaction();
        }
    }

    fn cancel_active_viewport_interaction(&mut self) {
        let side = self.focused_side();
        if let Some(viewport) = self.viewports.get_mut(&side) {
            viewport.cancel_primary_interaction();
        }
    }

    fn restore_session(&mut self, session: SessionState) {
        self.sidebar_open = session.sidebar_open;
        self.sidebar_tab = match session.sidebar_tab {
            SessionSidebarTab::Outline => SidebarTab::Outline,
            SessionSidebarTab::Thumbnails => SidebarTab::Thumbnails,
            SessionSidebarTab::Highlights => SidebarTab::Highlights,
        };

        let saved_tab_count = session.tabs.len();
        let mut runtime_tab_ids = vec![None; saved_tab_count];
        let mut pending_count = 0;
        let mut restored_count = 0;
        let mut skipped_count = 0;
        for (saved_index, tab) in session.tabs.into_iter().enumerate() {
            let opened =
                self.open_document_with_intent(tab.path, OpenIntent::Restored { view: tab.view });
            match opened {
                Some(OpenDocumentResult::Pending(index)) => {
                    runtime_tab_ids[saved_index] = self.tabs.tabs().get(index).map(|tab| tab.id());
                    pending_count += 1;
                }
                Some(OpenDocumentResult::Existing(index)) => {
                    runtime_tab_ids[saved_index] = self.tabs.tabs().get(index).map(|tab| tab.id());
                    restored_count += 1;
                }
                None => skipped_count += 1,
            }
        }

        let (entries, active) = restored_runtime_layout(&session.layout, &runtime_tab_ids);
        // 保存パスが同じ実体へ正規化される場合や消失した場合も、開けたタブだけで
        // 一度に配置を確定し、非同期 Opened の到着順にフォーカスを揺らさない。
        let restored = self.tabs.restore_layout(entries, active);
        debug_assert!(
            restored,
            "validated session must produce a valid runtime layout"
        );
        self.sync_pane_viewports();
        let progress = SessionRestoreProgress {
            requested: saved_tab_count,
            pending: pending_count,
            restored: restored_count,
            skipped: skipped_count,
        };
        self.status = progress.status();
        if progress.pending > 0 {
            self.session_restore_progress = Some(progress);
        }
    }

    fn open_document_with_intent(
        &mut self,
        path: PathBuf,
        intent: OpenIntent,
    ) -> Option<OpenDocumentResult> {
        let report_to_user = matches!(&intent, OpenIntent::User);
        let restored_view = match intent {
            OpenIntent::User => None,
            OpenIntent::Restored { view } => Some(view),
        };
        if !is_pdf_path(&path) {
            if report_to_user {
                self.error = Some(format!(
                    "PDFファイルではないため開けません。拡張子が.pdfのファイルを選択してください。対象: {}",
                    path.display()
                ));
            }
            return None;
        }

        let previously_active = self.active_index();
        let previously_visible = self.visible_indices();
        match self.tabs.open(&path) {
            Ok(OpenTabResult::Opened(index)) => {
                let canonical_path = self.tabs.tabs()[index].path().to_path_buf();
                let document_id = self.next_document_id;
                self.next_document_id = self
                    .next_document_id
                    .checked_add(1)
                    .expect("document IDs cannot exhaust u64");
                let activity_sequence = self.next_activity_sequence();
                self.documents.push(DocumentTab::new(
                    document_id,
                    canonical_path,
                    activity_sequence,
                    restored_view,
                ));
                self.activate_document(index, previously_active, &previously_visible);
                if report_to_user {
                    self.status = format!("Opening {}…", path.display());
                    self.error = None;
                }
                Some(OpenDocumentResult::Pending(index))
            }
            Ok(OpenTabResult::SelectedExisting(index)) => {
                self.activate_document(index, previously_active, &previously_visible);
                if report_to_user {
                    self.status = format!("Selected existing tab: {}", path.display());
                    self.error = None;
                }
                Some(OpenDocumentResult::Existing(index))
            }
            Err(error) => {
                if report_to_user {
                    self.error = Some(format!(
                        "PDFを開けませんでした。ファイルの場所とアクセス権限を確認してください。対象: {}。詳細: {error}",
                        path.display()
                    ));
                }
                None
            }
        }
    }

    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let dropped_files = context.input(|input| input.raw.dropped_files.clone());
        for file in dropped_files {
            if let Some(path) = file.path {
                self.open_document(path);
            }
        }
    }

    /// 既存プロセスから届いたPDFパスを、通常のユーザー操作と同じタブ追加経路へ渡す。
    ///
    /// 戻り値は、同じフレームに残っているOSの終了要求を抑止すべきかを表す。
    fn receive_external_open_events(&mut self, context: &egui::Context) -> bool {
        let mut external_request_received = false;
        let events = self.external_open_events.try_iter().collect::<Vec<_>>();
        for event in events {
            match event {
                Ok(paths) => {
                    external_request_received = true;
                    self.cancel_bulk_close_for_external_open();
                    for path in paths {
                        self.open_document(path);
                    }
                    // Windows側の前面化制限には従いつつ、最小化解除とフォーカス要求を
                    // eguiのウィンドウ経路へまとめて送る。
                    context.send_viewport_cmd(ViewportCommand::Minimized(false));
                    context.send_viewport_cmd(ViewportCommand::Focus);
                }
                Err(error) => {
                    self.error = Some(error);
                }
            }
        }
        external_request_received
    }

    fn cancel_bulk_close_for_external_open(&mut self) {
        let bulk_confirmation = self
            .close_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.scope != CloseScope::Tab);
        let bulk_close_in_progress = self.window_close_pending
            || self.close_all_pending
            || bulk_confirmation
            || self.allow_window_close
            || self.session_close_failure.is_some();
        if !bulk_close_in_progress {
            return;
        }

        // 追加の起動要求は「アプリを使い続ける」意図である。保存中の処理自体は
        // 完了させるが、その結果から終了シーケンスを再開させない。
        if bulk_confirmation {
            self.close_confirmation = None;
        }
        self.approved_window_documents.clear();
        self.allow_window_close = false;
        self.window_close_pending = false;
        self.close_all_pending = false;
        self.session_close_failure = None;
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        let open_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::O));
        // 未解決のクローズ処理の下でネイティブダイアログを開始してはならない。
        // ユーザーが決定するまで、その処理が文書集合を所有する。
        if open_pressed
            && !self.window_close_pending
            && !self.close_all_pending
            && self.close_confirmation.is_none()
        {
            self.pick_pdf_and_open();
        }
        let find_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::F));
        if find_pressed && let Some(index) = self.active_index() {
            let document_id = self.documents[index].document_id;
            context.memory_mut(|memory| {
                memory.request_focus(search_query_id(document_id));
            });
            if !self.documents[index].search.query.trim().is_empty()
                && !self.documents[index].search.in_progress
                && self.documents[index].search.pages.is_empty()
            {
                self.begin_search(index);
            }
        }
        let escape_pressed =
            context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
        if escape_pressed && let Some(index) = self.active_index() {
            // PDF ビューが生入力を読む前に Escape をここで消費するため、
            // autoscroll はフレーム更新だけでなくこの分岐でも停止する必要がある。
            self.documents[index].view.stop_autoscroll();
            let document_id = self.documents[index].document_id;
            let query_id = search_query_id(document_id);
            let page_id = page_number_id(document_id);
            let annotation_input_id = self
                .annotation_editor
                .as_ref()
                .filter(|editor| editor.document_id == document_id)
                .map(|editor| annotation_comment_id(document_id, editor.annotation_id));
            if annotation_input_id
                .is_some_and(|input_id| context.memory(|memory| memory.has_focus(input_id)))
            {
                // 最初の Escape は複数行エディターからフォーカスを外すだけにし、
                // バッファを破棄したり背後の PDF を操作したりしてはならない。
                context.memory_mut(|memory| {
                    memory.surrender_focus(annotation_input_id.expect("checked above"));
                });
            } else if self
                .annotation_editor
                .as_ref()
                .is_some_and(|editor| editor.document_id == document_id)
            {
                let editor = self
                    .annotation_editor
                    .as_mut()
                    .expect("active document editor checked above");
                if editor.mutation_in_flight {
                    editor.notice = Some("注釈処理の完了を待っています。".to_owned());
                } else if editor.is_dirty() {
                    editor.notice = Some(
                        "未保存の変更があります。保存または変更を破棄してください。".to_owned(),
                    );
                } else {
                    self.annotation_editor = None;
                }
            } else if context.memory(|memory| memory.has_focus(page_id)) {
                self.documents[index].page_input =
                    (self.documents[index].view.current_page + 1).to_string();
                self.documents[index].page_input_error = None;
                context.memory_mut(|memory| memory.surrender_focus(page_id));
            } else if context.memory(|memory| memory.has_focus(query_id)) {
                context.memory_mut(|memory| memory.surrender_focus(query_id));
            } else if !self.documents[index].search.query.is_empty()
                || !self.documents[index].search.pages.is_empty()
            {
                self.documents[index].search.query.clear();
                self.cancel_search(index);
            }
        }

        let next_tab = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Tab));
        if next_tab {
            self.select_next_tab();
        }

        let text_input_has_focus = self.active_text_input_has_focus(context);
        if !text_input_has_focus {
            let page_up =
                context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::PageUp));
            if page_up {
                self.move_page(-1);
            }
            let page_down =
                context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::PageDown));
            if page_down {
                self.move_page(1);
            }
        }

        let close_flow_active = self.window_close_pending
            || self.close_all_pending
            || self.close_confirmation.is_some();
        let save_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::S));
        if save_pressed && !close_flow_active {
            let annotation_editor_can_save = self.active_index().is_some_and(|index| {
                let document_id = self.documents[index].document_id;
                self.annotation_editor
                    .as_ref()
                    .is_some_and(|editor| editor.document_id == document_id && editor.can_save())
            });
            if annotation_editor_can_save {
                self.request_annotation_update(
                    self.active_index()
                        .expect("an active annotation editor belongs to an active tab"),
                );
            } else {
                self.save();
            }
        }
        let print_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::P));
        if print_pressed && !close_flow_active && cfg!(windows) && self.can_print() {
            self.print();
        }
        let undo_pressed = !text_input_has_focus
            && context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Z));
        if undo_pressed && !close_flow_active {
            let editor_blocks_undo = self
                .annotation_editor
                .as_ref()
                .is_some_and(|editor| editor.is_dirty() || editor.mutation_in_flight);
            if editor_blocks_undo {
                if let Some(editor) = &mut self.annotation_editor {
                    editor.notice = Some(
                        "PDFの編集を元に戻す前に、注釈の変更を保存または破棄してください。"
                            .to_owned(),
                    );
                }
            } else {
                self.undo();
            }
        }
        let active_text_input = self.active_text_input_id(context);
        let highlight_pressed = consume_highlight_shortcut(context, active_text_input);
        if highlight_pressed && !close_flow_active {
            self.create_highlight();
        }
        let text_input_owns_copy =
            active_text_input.is_some_and(|input_id| text_edit_has_selection(context, input_id));
        let pdf_selection_available = self.can_copy_selection();
        let copy_pressed = consume_pdf_copy_event(
            context,
            &mut self.copy_shortcut_active,
            text_input_owns_copy,
            pdf_selection_available,
        );
        if copy_pressed {
            self.copy_selection(context);
        }
    }

    fn active_text_input_id(&self, context: &egui::Context) -> Option<Id> {
        let index = self.active_index()?;
        let document_id = self.documents[index].document_id;
        let annotation_id = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.document_id == document_id)
            .map(|editor| annotation_comment_id(document_id, editor.annotation_id));
        [
            Some(search_query_id(document_id)),
            Some(page_number_id(document_id)),
            annotation_id,
        ]
        .into_iter()
        .flatten()
        .find(|id| context.memory(|memory| memory.has_focus(*id)))
    }

    fn active_text_input_has_focus(&self, context: &egui::Context) -> bool {
        self.active_text_input_id(context).is_some()
    }

    fn active_index(&self) -> Option<usize> {
        self.tabs.selected_index()
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.tabs
            .visible_tab_ids()
            .into_iter()
            .filter_map(|tab_id| self.tabs.tab_registry_index(tab_id))
            .collect()
    }

    fn is_visible_index(&self, index: usize) -> bool {
        self.tabs
            .tabs()
            .get(index)
            .is_some_and(|tab| self.tabs.visible_tab_ids().contains(&tab.id()))
    }

    fn side_for_index(&self, index: usize) -> Option<SplitSide> {
        let tab_id = self.tabs.tabs().get(index)?.id();
        match self
            .tabs
            .visible_tab_ids()
            .iter()
            .position(|id| *id == tab_id)?
        {
            0 => Some(SplitSide::First),
            1 => Some(SplitSide::Second),
            _ => None,
        }
    }

    fn focused_side(&self) -> SplitSide {
        let Some(active) = self.tabs.active_tab_id() else {
            return SplitSide::First;
        };
        let Some(group) = self.tabs.active_split() else {
            return SplitSide::First;
        };
        if group.tab(SplitSide::First) == active {
            SplitSide::First
        } else {
            SplitSide::Second
        }
    }

    fn cancel_viewport_for_index(&mut self, index: usize) {
        if let Some(side) = self.side_for_index(index)
            && let Some(viewport) = self.viewports.get_mut(&side)
        {
            viewport.cancel_primary_interaction();
        }
    }

    fn sync_pane_viewports(&mut self) {
        let split_visible = self.tabs.active_split().is_some();
        self.viewports.retain(|side, viewport| {
            let retained = *side == SplitSide::First || split_visible;
            if !retained {
                viewport.cancel_primary_interaction();
            }
            retained
        });
        self.viewports.entry(SplitSide::First).or_default();
        if split_visible {
            self.viewports.entry(SplitSide::Second).or_default();
        }
    }

    fn next_activity_sequence(&mut self) -> u64 {
        self.activity_sequence = self.activity_sequence.wrapping_add(1);
        self.activity_sequence
    }

    fn activate_document(
        &mut self,
        index: usize,
        previous: Option<usize>,
        previously_visible: &[usize],
    ) {
        if let Some(tab_to_reveal) = tab_reveal_for_selection_change(previous, index) {
            self.tab_to_reveal = Some(tab_to_reveal);
        }
        if let Some(previous) = previous.filter(|previous| *previous != index) {
            self.documents[previous].view.stop_autoscroll();
            // 表示セットの切替では同じスロットへ別文書が入るため、古い文書の未完了操作を
            // どちらの新しいPDF面にも引き継がない。
            self.cancel_all_viewport_interactions();
        }

        let currently_visible = self.visible_indices();
        for previous_visible in previously_visible
            .iter()
            .copied()
            .filter(|previous| !currently_visible.contains(previous))
        {
            // ペイン内選択が変わって画面から外れた文書だけを取り消す。フォーカスだけが
            // 変わった反対側ペインの可視要求は残す。
            self.documents[previous_visible].cancel_rendering_requests();
            self.documents[previous_visible].invalidate_text_snapshots();
            self.documents[previous_visible].invalidate_annotation_pages();
            self.documents[previous_visible].search.generation = self.documents[previous_visible]
                .search
                .generation
                .wrapping_add(1);
            let generation = self.documents[previous_visible].search.generation;
            let _queued = self.documents[previous_visible]
                .send(DocumentCommand::SetSearchGeneration(generation));
            self.documents[previous_visible].search.in_progress = false;
        }

        self.sync_pane_viewports();
        for visible_index in currently_visible {
            if self.documents[visible_index].state == DocumentState::Suspended {
                let path = self.tabs.tabs()[visible_index].path().to_path_buf();
                self.documents[visible_index].resume(path);
                self.status = "Reopening suspended PDF after external-change check…".to_owned();
            }
        }
        let sequence = self.next_activity_sequence();
        self.documents[index].last_selected_sequence = sequence;
        if !self.documents[index].search.query.trim().is_empty()
            && !self.documents[index].search.in_progress
        {
            self.begin_search(index);
        }
    }

    fn select_tab(&mut self, index: usize) {
        let previous = self.active_index();
        let previously_visible = self.visible_indices();
        if !self.tabs.select(index) {
            return;
        }
        self.activate_document(index, previous, &previously_visible);
    }

    fn focus_side(&mut self, side: SplitSide) {
        let previous = self.active_index();
        let previously_visible = self.visible_indices();
        let Some(tab_id) = self.tabs.visible_tab_ids().get(side.index()).copied() else {
            return;
        };
        if !self.tabs.select_tab(tab_id) {
            return;
        }
        if let Some(index) = self.active_index() {
            self.activate_document(index, previous, &previously_visible);
        }
    }

    fn maybe_suspend_inactive_document(&mut self) {
        let Some(memory) = memory_stats::memory_stats() else {
            return;
        };
        if memory.physical_mem <= RESIDENT_MEMORY_SUSPEND_THRESHOLD_BYTES {
            return;
        }

        let candidates = self
            .documents
            .iter()
            .map(|document| {
                let owns_editor = self
                    .annotation_editor
                    .as_ref()
                    .is_some_and(|editor| editor.document_id == document.document_id);
                (
                    document.is_suspendable() && !owns_editor,
                    document.last_selected_sequence,
                )
            })
            .collect::<Vec<_>>();
        let visible_indices = self.visible_indices();
        let Some(index) = oldest_suspendable_index(&visible_indices, &candidates) else {
            return;
        };

        let keys = self.documents[index]
            .tiles
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            self.gpu_lru.remove(&key);
        }
        let thumbnail_keys = self.documents[index]
            .thumbnails
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for key in thumbnail_keys {
            self.thumbnail_lru.remove(&key);
        }
        self.documents[index].suspend();
        self.status = format!(
            "Suspended inactive tab to reduce resident memory ({:.1} MiB)",
            memory.physical_mem as f64 / 1_048_576.0
        );
    }

    fn select_next_tab(&mut self) {
        let next_tab = self
            .tabs
            .next_entry_tab()
            .and_then(|tab_id| self.tabs.tab_registry_index(tab_id));
        if let Some(index) = next_tab {
            self.select_tab(index);
        }
    }

    fn focus_blocking_annotation_editor(&mut self, only_index: Option<usize>) -> bool {
        let Some(editor) = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.is_dirty() || editor.mutation_in_flight)
        else {
            return false;
        };
        let Some(index) = self
            .documents
            .iter()
            .position(|document| document.document_id == editor.document_id)
        else {
            return false;
        };
        if only_index.is_some_and(|only_index| only_index != index) {
            return false;
        }
        self.select_tab(index);
        let editor = self
            .annotation_editor
            .as_mut()
            .expect("the blocking editor was found above");
        editor.notice = Some(if editor.mutation_in_flight {
            "注釈処理の完了後にタブを閉じてください。".to_owned()
        } else {
            "タブを閉じる前に、注釈の変更を保存または破棄してください。".to_owned()
        });
        self.status = "注釈編集オーバーレイで変更を確定してください".to_owned();
        true
    }

    fn close_tab(&mut self, index: usize) {
        if let Some(document) = self.documents.get_mut(index) {
            document.view.stop_autoscroll();
        }
        if self.is_visible_index(index)
            && let Some(side) = self.side_for_index(index)
            && let Some(viewport) = self.viewports.get_mut(&side)
        {
            viewport.cancel_primary_interaction();
        }
        let Some(document) = self.documents.get(index) else {
            return;
        };
        // キューでは保存が Shutdown より前に置かれるため、Discard で正直に取り消せない。
        // 完了を待つ。
        if document.is_saving() {
            self.status = "Waiting for the current save before closing…".to_owned();
            return;
        }
        if document.is_printing() {
            self.status = "印刷処理の完了を待ってからタブを閉じます…".to_owned();
            return;
        }
        if self.focus_blocking_annotation_editor(Some(index)) {
            return;
        }
        let Some(document) = self.documents.get(index) else {
            return;
        };
        if document.has_unsaved_changes() {
            self.close_confirmation = Some(CloseConfirmation {
                scope: CloseScope::Tab,
                path: self.tabs.tabs()[index].path().to_path_buf(),
                save_in_flight: false,
            });
            return;
        }
        self.close_tab_now(index);
    }

    fn request_close_all(&mut self) {
        if self.window_close_pending || self.close_confirmation.is_some() {
            return;
        }
        if self.focus_blocking_annotation_editor(None) {
            return;
        }
        self.close_all_pending = true;
        self.prompt_next_close_all_document();
    }

    fn prompt_next_close_all_document(&mut self) {
        if self
            .documents
            .iter()
            .any(|document| document.is_saving() || document.is_printing())
        {
            self.status = "保存または印刷の完了を待ってからすべて閉じます…".to_owned();
            return;
        }
        let next_path = self
            .documents
            .iter()
            .enumerate()
            .find(|(index, document)| {
                let path = self.tabs.tabs()[*index].path();
                document.has_unsaved_changes() && !self.approved_window_documents.contains(path)
            })
            .map(|(index, _)| self.tabs.tabs()[index].path().to_path_buf());

        if let Some(path) = next_path {
            self.close_confirmation = Some(CloseConfirmation {
                scope: CloseScope::AllTabs,
                path,
                save_in_flight: false,
            });
            return;
        }

        while !self.documents.is_empty() {
            self.remove_tab_now(self.documents.len() - 1);
        }
        self.approved_window_documents.clear();
        self.close_all_pending = false;
        self.status = "すべてのタブを閉じました".to_owned();
    }

    fn close_tab_now(&mut self, index: usize) {
        if self.remove_tab_now(index) {
            self.status = "Tab closed".to_owned();
        }
    }

    fn remove_tab_now(&mut self, index: usize) -> bool {
        let previous_selection = self.active_index();
        let closing_side = self.side_for_index(index);
        let split_sibling_transition = self.tabs.tabs().get(index).and_then(|tab| {
            let group = self.tabs.split_for_tab(tab.id())?;
            // 第1面を閉じたときだけ、残る第2面のScrollArea IDが単独表示の
            // 第1面へ変わる。非表示セットでも次回選択に備えてアンカーを残す。
            (group.tab(SplitSide::First) == tab.id()).then_some(group.tab(SplitSide::Second))
        });
        if self.is_visible_index(index)
            && let Some(viewport) = closing_side.and_then(|side| self.viewports.get_mut(&side))
        {
            viewport.cancel_primary_interaction();
        }
        let document_id = self
            .documents
            .get(index)
            .map(|document| document.document_id);
        if self.tabs.close(index).is_none() {
            return false;
        }
        let was_restoring = self.documents[index].restoring_from_session;
        for key in self.documents[index].tiles.keys() {
            self.gpu_lru.remove(key);
        }
        for key in self.documents[index].thumbnails.keys() {
            self.thumbnail_lru.remove(key);
        }
        self.documents.remove(index);
        self.sync_pane_viewports();
        if let Some(sibling) = split_sibling_transition
            && let Some(sibling_index) = self.tabs.tab_registry_index(sibling)
        {
            self.documents[sibling_index].prepare_for_pane_transition();
        }
        if document_id.is_some_and(|document_id| {
            self.annotation_editor
                .as_ref()
                .is_some_and(|editor| editor.document_id == document_id)
        }) {
            self.annotation_editor = None;
        }
        if document_id.is_some_and(|document_id| {
            self.annotation_picker
                .as_ref()
                .is_some_and(|picker| picker.document_id == document_id)
        }) {
            self.annotation_picker = None;
        }
        self.tab_to_reveal = tab_reveal_after_close(previous_selection, index, self.active_index());
        for visible_index in self.visible_indices() {
            if self.documents[visible_index].state == DocumentState::Suspended {
                let path = self.tabs.tabs()[visible_index].path().to_path_buf();
                self.documents[visible_index].resume(path);
            }
        }
        if let Some(active_index) = self.active_index() {
            let sequence = self.next_activity_sequence();
            self.documents[active_index].last_selected_sequence = sequence;
            if !self.documents[active_index].search.query.trim().is_empty()
                && !self.documents[active_index].search.in_progress
            {
                self.begin_search(active_index);
            }
        }
        if was_restoring {
            // 開く途中の復元タブを閉じると保留結果を消費する。ワーカーもタブとともに
            // 破棄されるため、後からイベントが完了させることはない。
            self.finish_session_restore(false);
        }
        true
    }

    fn remove_evicted_gpu_tiles(&mut self, evicted: Vec<(TileCacheKey, ())>) {
        for (key, ()) in evicted {
            if let Some(document) = self
                .documents
                .iter_mut()
                .find(|document| document.document_id == key.document_id)
            {
                document.tiles.remove(&key);
            }
        }
    }

    fn remove_evicted_thumbnails(&mut self, evicted: Vec<(ThumbnailCacheKey, ())>) {
        for (key, ()) in evicted {
            if let Some(document) = self
                .documents
                .iter_mut()
                .find(|document| document.document_id == key.document_id)
            {
                document.thumbnails.remove(&key);
            }
        }
    }

    fn close_tab_by_path(&mut self, path: &Path) {
        let index = self.tabs.tabs().iter().position(|tab| tab.path() == path);
        if let Some(index) = index {
            self.close_tab_now(index);
        }
    }

    fn handle_window_close(&mut self, context: &egui::Context, suppress_close: bool) {
        let close_requested = context.input(|input| input.viewport().close_requested());
        if close_requested {
            self.stop_visible_autoscroll();
            self.cancel_all_viewport_interactions();
        }
        if !close_requested {
            return;
        }

        // OS のクローズ要求を検知した同じフレーム中にキャンセルを送らない限り、
        // eframe はネイティブウィンドウを閉じる。
        if suppress_close {
            context.send_viewport_cmd(ViewportCommand::CancelClose);
            return;
        }
        if self.allow_window_close {
            return;
        }
        context.send_viewport_cmd(ViewportCommand::CancelClose);
        if self.focus_blocking_annotation_editor(None) {
            self.window_close_pending = false;
            return;
        }
        self.window_close_pending = true;
        if self.close_confirmation.is_none() {
            self.prompt_next_window_document(context);
        }
    }

    fn prompt_next_window_document(&mut self, context: &egui::Context) {
        if self.session_close_failure.is_some() {
            return;
        }
        if self
            .documents
            .iter()
            .any(|document| document.is_saving() || document.is_printing())
        {
            self.status = "保存または印刷の完了を待ってから終了します…".to_owned();
            return;
        }
        if self.session_restore_progress.is_some() {
            // 復元した全オープンが成功または失敗を報告するまでセッション状態を取得しては
            // ならない。最後の保留結果を消費したとき receive_document_events がクローズ処理を再試行する。
            self.status = "Waiting for session restore before closing…".to_owned();
            return;
        }

        let next_path = self
            .documents
            .iter()
            .enumerate()
            .find(|(index, document)| {
                let path = self.tabs.tabs()[*index].path();
                document.has_unsaved_changes() && !self.approved_window_documents.contains(path)
            })
            .map(|(index, _)| self.tabs.tabs()[index].path().to_path_buf());

        if let Some(path) = next_path {
            self.close_confirmation = Some(CloseConfirmation {
                scope: CloseScope::Window,
                path,
                save_in_flight: false,
            });
            return;
        }

        self.finalize_session_and_close(context);
    }

    fn finalize_session_and_close(&mut self, context: &egui::Context) {
        let session = self.current_session();
        if let Err(error) = self.session_store.save(&session) {
            self.session_close_failure = Some(format!(
                "セッションを保存できませんでした。保存先の書き込み権限を確認してください。詳細: {error}"
            ));
            self.status = "セッションを保存できませんでした。終了方法を選択してください".to_owned();
            return;
        }

        self.allow_window_close = true;
        self.window_close_pending = false;
        self.close_confirmation = None;
        context.send_viewport_cmd(ViewportCommand::Close);
    }

    fn finish_session_restore(&mut self, opened: bool) {
        let progress = self
            .session_restore_progress
            .as_mut()
            .expect("only pending restored tabs emit completion events");
        progress.finish_one(opened);
        let finished = progress.pending == 0;
        self.status = progress.status();
        if finished {
            self.session_restore_progress = None;
        }
    }

    fn current_session(&self) -> SessionState {
        let mut session = SessionState {
            restore_enabled: self.restore_enabled,
            sidebar_open: self.sidebar_open,
            sidebar_tab: match self.sidebar_tab {
                SidebarTab::Outline => SessionSidebarTab::Outline,
                SidebarTab::Thumbnails => SessionSidebarTab::Thumbnails,
                SidebarTab::Highlights => SessionSidebarTab::Highlights,
            },
            recent_annotation_colors: self.recent_annotation_colors.clone(),
            ..SessionState::default()
        };
        session.tabs = self
            .tabs
            .tabs()
            .iter()
            .zip(&self.documents)
            .map(|(tab, document)| SessionTab {
                path: tab.path().to_path_buf(),
                view: document.view.to_session(),
            })
            .collect();
        session.layout.entries = self
            .tabs
            .entries()
            .iter()
            .map(|entry| match entry {
                TabEntry::Single(tab_id) => SessionEntry::Single {
                    tab_index: self
                        .tabs
                        .tab_registry_index(*tab_id)
                        .expect("displayed tab must exist in registry"),
                },
                TabEntry::Split(group) => {
                    let tab_indices = group.tabs().map(|tab_id| {
                        self.tabs
                            .tab_registry_index(tab_id)
                            .expect("split member must exist in registry")
                    });
                    SessionEntry::Split {
                        tab_indices,
                        direction: match group.direction() {
                            SplitDirection::Horizontal => SessionSplitDirection::Horizontal,
                            SplitDirection::Vertical => SessionSplitDirection::Vertical,
                        },
                        ratio: group.ratio(),
                        focused_tab: tab_indices[group.focused().index()],
                    }
                }
            })
            .collect();
        session.layout.active_tab = self
            .tabs
            .active_tab_id()
            .and_then(|tab_id| self.tabs.tab_registry_index(tab_id));
        session
    }

    fn finish_save_for_close(&mut self, path: &Path, context: &egui::Context) {
        let Some(confirmation) = &self.close_confirmation else {
            return;
        };
        let save_matches = confirmation.save_in_flight && confirmation.path == path;
        if !save_matches {
            return;
        }

        let scope = confirmation.scope;
        self.close_confirmation = None;
        match scope {
            CloseScope::Tab => self.saved_tab_to_close = Some(path.to_path_buf()),
            CloseScope::AllTabs => {
                self.approved_window_documents.insert(path.to_path_buf());
                self.prompt_next_close_all_document();
            }
            CloseScope::Window => {
                self.approved_window_documents.insert(path.to_path_buf());
                self.prompt_next_window_document(context);
            }
        }
    }

    fn apply_close_decision(&mut self, decision: CloseDecision, context: &egui::Context) {
        let Some(confirmation) = &self.close_confirmation else {
            return;
        };
        let scope = confirmation.scope;
        let path = confirmation.path.clone();

        match decision {
            CloseDecision::Save => {
                let Some(index) = self
                    .tabs
                    .tabs()
                    .iter()
                    .position(|tab| tab.path() == path.as_path())
                else {
                    self.close_confirmation = None;
                    return;
                };
                if self.documents[index].is_saving() {
                    self.status = "Waiting for the current save before closing…".to_owned();
                    return;
                }
                let queued = self.documents[index].send(DocumentCommand::Save);
                if queued {
                    self.documents[index].save_in_flight = true;
                    self.documents[index].state = DocumentState::Saving;
                    if let Some(confirmation) = &mut self.close_confirmation {
                        confirmation.save_in_flight = true;
                    }
                    self.status = "Saving PDF before close…".to_owned();
                } else {
                    self.error = Some(
                        "PDFを保存できませんでした。文書処理が停止しているため、タブを開き直してください。"
                            .to_owned(),
                    );
                }
            }
            CloseDecision::Discard => {
                self.close_confirmation = None;
                match scope {
                    CloseScope::Tab => self.close_tab_by_path(&path),
                    CloseScope::AllTabs => {
                        self.approved_window_documents.insert(path);
                        self.prompt_next_close_all_document();
                    }
                    CloseScope::Window => {
                        self.approved_window_documents.insert(path);
                        self.prompt_next_window_document(context);
                    }
                }
            }
            CloseDecision::Cancel => {
                self.close_confirmation = None;
                self.approved_window_documents.clear();
                self.allow_window_close = false;
                self.window_close_pending = false;
                self.close_all_pending = false;
                self.status = "閉じる操作をキャンセルしました".to_owned();
            }
        }
    }

    fn close_confirmation_dialog(&mut self, context: &egui::Context) {
        let Some(confirmation) = &self.close_confirmation else {
            return;
        };
        let file_name = confirmation
            .path
            .file_name()
            .unwrap_or(confirmation.path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let save_in_flight = confirmation.save_in_flight;

        let modal = egui::Modal::new(Id::new("unsaved-document-close")).show(context, |ui| {
            ui.heading("未保存のPDF編集があります");
            ui.label(format!("{file_name} の変更を保存しますか？"));
            ui.horizontal(|ui| {
                let save = ui
                    .add_enabled(!save_in_flight, egui::Button::new("保存"))
                    .clicked()
                    .then_some(CloseDecision::Save);
                let discard = ui
                    .add_enabled(!save_in_flight, egui::Button::new("破棄"))
                    .clicked()
                    .then_some(CloseDecision::Discard);
                let cancel = ui
                    .button(if save_in_flight {
                        "開いたままにする"
                    } else {
                        "キャンセル"
                    })
                    .clicked()
                    .then_some(CloseDecision::Cancel);
                save.or(discard).or(cancel)
            })
            .inner
        });

        if let Some(decision) = modal.inner {
            self.apply_close_decision(decision, context);
        } else if modal.should_close() && !save_in_flight {
            self.apply_close_decision(CloseDecision::Cancel, context);
        }
    }

    fn session_close_failure_dialog(&mut self, context: &egui::Context) {
        let Some(message) = self.session_close_failure.clone() else {
            return;
        };
        let modal = egui::Modal::new(Id::new("session-save-close-failure")).show(context, |ui| {
            ui.heading("セッションを保存できませんでした");
            ui.label(&message);
            ui.label("このエラーによってPDF文書は変更されていません。");
            ui.horizontal(|ui| {
                let retry = ui
                    .button("再試行")
                    .clicked()
                    .then_some(SessionCloseDecision::Retry);
                let exit = ui
                    .button("セッションを保存せず終了")
                    .clicked()
                    .then_some(SessionCloseDecision::ExitWithoutSession);
                let cancel = ui
                    .button("キャンセル")
                    .clicked()
                    .then_some(SessionCloseDecision::Cancel);
                retry.or(exit).or(cancel)
            })
            .inner
        });

        let decision = modal
            .inner
            .or_else(|| modal.should_close().then_some(SessionCloseDecision::Cancel));
        if let Some(decision) = decision {
            self.apply_session_close_decision(decision, message, context);
        }
    }

    fn apply_session_close_decision(
        &mut self,
        decision: SessionCloseDecision,
        message: String,
        context: &egui::Context,
    ) {
        self.session_close_failure = None;
        match decision {
            SessionCloseDecision::Retry => self.prompt_next_window_document(context),
            SessionCloseDecision::ExitWithoutSession => {
                // この明示的な選択だけが、通常のクローズで必要なアトミックなセッション更新なしに
                // 終了できる経路である。
                self.allow_window_close = true;
                self.window_close_pending = false;
                self.close_confirmation = None;
                context.send_viewport_cmd(ViewportCommand::Close);
            }
            SessionCloseDecision::Cancel => {
                self.approved_window_documents.clear();
                self.allow_window_close = false;
                self.window_close_pending = false;
                self.error = Some(message);
                self.status = "Close canceled".to_owned();
            }
        }
    }

    fn move_page(&mut self, delta: isize) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let Some(page_count) = tab.info.as_ref().map(|info| info.page_bounds.len()) else {
            return;
        };
        let last_page = page_count.saturating_sub(1);
        let target = tab
            .view
            .current_page
            .saturating_add_signed(delta)
            .min(last_page);
        if target == tab.view.current_page {
            return;
        }
        tab.jump_to_page(target);
    }

    fn zoom_by(&mut self, factor: f32) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let zoom = (tab.view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        tab.set_zoom(zoom, ZoomMode::Fixed);
    }

    fn copy_selection(&mut self, context: &egui::Context) {
        let Some(selection) = self.active_tab_mut().and_then(|tab| tab.selection.as_ref()) else {
            return;
        };
        context.copy_text(selection.text.clone());
        self.status = "Selected text copied".to_owned();
    }

    fn handle_annotation_ui_action(
        &mut self,
        index: usize,
        revision: u64,
        action: AnnotationUiAction,
        context: &egui::Context,
    ) {
        if self.active_index() != Some(index)
            || self.documents[index]
                .info
                .as_ref()
                .is_none_or(|info| info.revision != revision)
        {
            return;
        }
        match action {
            AnnotationUiAction::CopySelection => self.copy_selection(context),
            AnnotationUiAction::CreateHighlight => self.create_highlight(),
            AnnotationUiAction::EditAnnotation(annotation_id) => {
                self.open_annotation_editor(index, revision, annotation_id);
            }
            AnnotationUiAction::DeleteAnnotation(annotation_id) => {
                self.request_annotation_delete(index, revision, annotation_id);
            }
            AnnotationUiAction::ChooseAnnotation(candidates) => {
                self.annotation_picker = Some(AnnotationPickerState {
                    document_id: self.documents[index].document_id,
                    revision,
                    candidates,
                });
            }
        }
    }

    fn open_annotation_editor(&mut self, index: usize, revision: u64, annotation_id: AnnotationId) {
        let request = AnnotationPageRequest {
            page_index: annotation_id.page_index,
            expected_revision: revision,
        };
        let annotation = self.documents[index]
            .annotation_pages
            .get(&request)
            .and_then(|page| {
                page.annotations
                    .iter()
                    .find(|annotation| annotation.id == annotation_id)
            })
            .cloned();
        let Some(annotation) = annotation else {
            self.error = Some(
                "注釈を開けませんでした。ページを再表示してから選択し直してください。".to_owned(),
            );
            return;
        };
        self.open_annotation_editor_from_summary(index, revision, &annotation.summary());
    }

    fn open_annotation_editor_from_summary(
        &mut self,
        index: usize,
        revision: u64,
        annotation: &AnnotationSummary,
    ) {
        let document_id = self.documents[index].document_id;
        if let Some(editor) = &mut self.annotation_editor {
            let same_target = editor.document_id == document_id
                && editor.revision == revision
                && editor.annotation_id == annotation.id;
            if same_target {
                editor.notice = None;
                return;
            }
            if editor.is_dirty() || editor.mutation_in_flight {
                // 対象を切り替えるとき、長いコメントを黙って破棄したり別の注釈へ暗黙に
                // 保存したりしてはならない。
                editor.notice =
                    Some("別の注釈を開く前に、現在の変更を保存または破棄してください。".to_owned());
                return;
            }
        }

        self.annotation_editor = Some(AnnotationEditorState::from_summary(
            document_id,
            revision,
            annotation,
        ));
        self.annotation_picker = None;
    }

    fn handle_highlight_sidebar_action(
        &mut self,
        index: usize,
        revision: u64,
        action: HighlightSidebarAction,
    ) {
        match action {
            HighlightSidebarAction::Jump(page_index) => {
                self.documents[index].jump_to_page(page_index);
            }
            HighlightSidebarAction::Edit(annotation) => {
                self.documents[index].jump_to_page(annotation.id.page_index);
                self.open_annotation_editor_from_summary(index, revision, &annotation);
            }
            HighlightSidebarAction::Delete(annotation) => {
                self.request_annotation_delete_known(
                    index,
                    revision,
                    annotation.id,
                    annotation.can_delete,
                );
            }
        }
    }

    fn request_annotation_update(&mut self, index: usize) {
        let document_id = self.documents[index].document_id;
        let Some(editor) = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.document_id == document_id && editor.can_save())
        else {
            return;
        };
        let request = editor
            .update_request()
            .expect("the filtered editor has a valid update request");
        if self.documents[index].send(DocumentCommand::UpdateAnnotation(request)) {
            let tab = &mut self.documents[index];
            tab.pending_edits += 1;
            tab.state = DocumentState::ReadyDirty;
            let editor = self
                .annotation_editor
                .as_mut()
                .expect("the update request was built from this editor");
            editor.mutation_in_flight = true;
            editor.notice = Some("注釈の変更を反映しています…".to_owned());
            self.status = "注釈の変更を反映しています…".to_owned();
        } else {
            self.error = Some(
                "注釈を更新できません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
        }
    }

    fn request_annotation_delete(
        &mut self,
        index: usize,
        revision: u64,
        annotation_id: AnnotationId,
    ) {
        let request_key = AnnotationPageRequest {
            page_index: annotation_id.page_index,
            expected_revision: revision,
        };
        let can_delete = self.documents[index]
            .annotation_pages
            .get(&request_key)
            .and_then(|page| {
                page.annotations
                    .iter()
                    .find(|annotation| annotation.id == annotation_id)
            })
            .is_some_and(|annotation| annotation.can_delete);
        self.request_annotation_delete_known(index, revision, annotation_id, can_delete);
    }

    fn request_annotation_delete_known(
        &mut self,
        index: usize,
        revision: u64,
        annotation_id: AnnotationId,
        can_delete: bool,
    ) {
        let document_id = self.documents[index].document_id;
        if let Some(editor) = &mut self.annotation_editor {
            let same_target = editor.document_id == document_id
                && editor.revision == revision
                && editor.annotation_id == annotation_id;
            if !same_target && (editor.is_dirty() || editor.mutation_in_flight) {
                // 別の注釈をコンテキストメニューから削除しても、無関係な編集バッファを
                // 消したり新しい対象へ付け替えたりしてはならない。
                editor.notice = Some(
                    "別の注釈を削除する前に、現在の変更を保存または破棄してください。".to_owned(),
                );
                return;
            }
        }
        if !can_delete {
            self.error = Some(
                "注釈を削除できません。PDFの編集制限または注釈ロックを確認してください。"
                    .to_owned(),
            );
            return;
        }
        let request = AnnotationDeleteRequest {
            id: annotation_id,
            expected_revision: revision,
        };
        if self.documents[index].send(DocumentCommand::DeleteAnnotation(request)) {
            let tab = &mut self.documents[index];
            tab.pending_edits += 1;
            tab.state = DocumentState::ReadyDirty;
            if let Some(editor) = self.annotation_editor.as_mut().filter(|editor| {
                editor.document_id == document_id && editor.annotation_id == annotation_id
            }) {
                editor.mutation_in_flight = true;
                editor.notice = Some("注釈を削除しています…".to_owned());
            }
            self.status = "注釈を削除しています…".to_owned();
        } else {
            self.error = Some(
                "注釈を削除できません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
        }
    }

    fn create_highlight(&mut self) {
        if self.window_close_pending || self.close_all_pending || self.close_confirmation.is_some()
        {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.is_printing() {
            return;
        }
        let highlight_capability = tab.info.as_ref().map(|info| info.highlight_capability);
        let Some(highlight_capability) = highlight_capability else {
            return;
        };
        if let Some(restriction) = highlight_capability.restriction() {
            // アダプターが具体的な制限を報告するため、UI が保存不能な `dirty` タブを残す
            // 独自の保存代替経路を作らない。
            self.error = Some(format!(
                "Highlightを作成できません。PDFの編集制限を確認してください。詳細: {restriction}"
            ));
            return;
        }
        let Some(selection) = &tab.selection else {
            self.error =
                Some("Highlightを作成できません。先に本文テキストを選択してください。".to_owned());
            return;
        };
        if selection.quads.is_empty() {
            self.error =
                Some("Highlightを作成できません。テキストを選択し直してください。".to_owned());
            return;
        }

        let request = HighlightRequest {
            page_index: selection.page_index,
            quads: selection.quads.clone(),
        };
        if tab.send(DocumentCommand::CreateHighlight(request)) {
            // このローカル保留数で、MuPDF が文書ワーカーから `dirty` フラグを返す前の
            // 競合を塞ぐ。
            tab.pending_edits += 1;
            tab.state = DocumentState::ReadyDirty;
            self.status = "Creating PDF Highlight annotation…".to_owned();
        } else {
            self.error = Some(
                "Highlightを作成できません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
        }
    }

    fn undo(&mut self) {
        if !self.can_undo() {
            return;
        }
        let tab = self
            .active_tab_mut()
            .expect("can_undo requires an active tab");
        let action = tab
            .edit_history
            .last()
            .expect("can_undo requires a history entry")
            .clone();
        if tab.send(DocumentCommand::Undo(action)) {
            // バックエンドが正確な安定 ID の削除を確認するまで履歴の操作を保持し、
            // 失敗時に再試行できるようにする。
            tab.undo_in_flight = true;
            self.status = "編集を元に戻しています…".to_owned();
        } else {
            self.error = Some(
                "編集を元に戻せません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
        }
    }

    fn save(&mut self) {
        if self.window_close_pending || self.close_all_pending || self.close_confirmation.is_some()
        {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.is_saving() {
            self.status = "A save is already in progress".to_owned();
            return;
        }
        if tab.is_printing() {
            self.status = "印刷処理の完了後に保存してください".to_owned();
            return;
        }
        let Some(info) = &tab.info else {
            return;
        };
        if !info.dirty && tab.pending_edits == 0 {
            self.status = "保存されていないPDF編集はありません".to_owned();
            return;
        }

        if tab.send(DocumentCommand::Save) {
            tab.save_in_flight = true;
            tab.state = DocumentState::Saving;
            self.status = "Saving PDF and reopening for verification…".to_owned();
        } else {
            self.error = Some(
                "PDFを保存できませんでした。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
        }
    }

    fn active_tab_mut(&mut self) -> Option<&mut DocumentTab> {
        let index = self.active_index()?;
        self.documents.get_mut(index)
    }

    fn tab_bar(&mut self, root_ui: &mut egui::Ui) -> Option<TabBarOutput> {
        if self.documents.is_empty() {
            return None;
        }

        let entries = self.tabs.entries().to_vec();
        let active_tab = self.tabs.active_tab_id();
        let tab_to_reveal = self.tab_to_reveal.take();
        let mut output = None;
        egui::Panel::top("tabs").show(root_ui, |ui| {
            let bar_size = Vec2::new(ui.available_width(), TAB_HEIGHT);
            let (bar_rect, _) = ui.allocate_exact_size(bar_size, Sense::hover());
            let member_count = entries.iter().map(|entry| entry.tab_ids().len()).sum();
            let tab_width = tab_width_for_count(
                ui.available_width(),
                entries.len(),
                TAB_ITEM_SPACING,
                TAB_MIN_WIDTH,
                TAB_MAX_WIDTH,
            );
            let mut result = TabBarOutput {
                bar_rect,
                entry_rects: Vec::with_capacity(entries.len()),
                tab_rects: Vec::with_capacity(member_count),
                tab_ids: Vec::with_capacity(member_count),
                group_rects: Vec::new(),
                select: None,
                close: None,
                drag_started: None,
                drag_released: false,
                menu_action: None,
            };
            ui.scope_builder(
                UiBuilder::new()
                    .id_salt("shared-tab-bar")
                    .max_rect(bar_rect),
                |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("shared-tabs-scroll")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = TAB_ITEM_SPACING;
                                for entry in &entries {
                                    match entry {
                                        TabEntry::Single(tab_id) => {
                                            let (rect, response) = ui.allocate_exact_size(
                                                Vec2::new(tab_width, TAB_HEIGHT),
                                                Sense::hover(),
                                            );
                                            result.entry_rects.push(rect);
                                            result.tab_rects.push(rect);
                                            result.tab_ids.push(*tab_id);
                                            let selected = active_tab == Some(*tab_id);
                                            let (select, close, can_close) = self.paint_tab_member(
                                                ui, *tab_id, rect, selected, selected,
                                            );
                                            if selected
                                                && tab_to_reveal
                                                    == self.tabs.tab_registry_index(*tab_id)
                                            {
                                                response.scroll_to_me(None);
                                            }
                                            Self::collect_tab_pointer_actions(
                                                *tab_id,
                                                &select,
                                                &close,
                                                can_close,
                                                &mut result,
                                            );
                                        }
                                        TabEntry::Split(group) => {
                                            const GROUP_HANDLE_HIT_WIDTH: f32 = 18.0;
                                            // 分割セットは通常タブと同じ外幅を二等分する。中央の見た目は
                                            // 境界線だけにしつつ、セット移動用の当たり判定は広く保つ。
                                            let member_width = tab_width / 2.0;
                                            let (group_rect, response) = ui.allocate_exact_size(
                                                Vec2::new(tab_width, TAB_HEIGHT),
                                                Sense::hover(),
                                            );
                                            let first_rect = Rect::from_min_max(
                                                group_rect.min,
                                                pos2(
                                                    group_rect.left() + member_width,
                                                    group_rect.bottom(),
                                                ),
                                            );
                                            let handle_rect = Rect::from_center_size(
                                                group_rect.center(),
                                                Vec2::new(GROUP_HANDLE_HIT_WIDTH, TAB_HEIGHT),
                                            );
                                            let second_rect = Rect::from_min_max(
                                                pos2(first_rect.right(), group_rect.top()),
                                                group_rect.max,
                                            );
                                            result.entry_rects.push(group_rect);
                                            result.group_rects.push((group.id(), group_rect));
                                            let active_group = group
                                                .tabs()
                                                .contains(&active_tab.unwrap_or(group.tabs()[0]));
                                            for (side, tab_rect) in [
                                                (SplitSide::First, first_rect),
                                                (SplitSide::Second, second_rect),
                                            ] {
                                                let tab_id = group.tab(side);
                                                result.tab_rects.push(tab_rect);
                                                result.tab_ids.push(tab_id);
                                                let focused = active_tab == Some(tab_id);
                                                let (select, close, can_close) = self
                                                    .paint_tab_member(
                                                        ui, tab_id, tab_rect, focused, focused,
                                                    );
                                                Self::split_context_menu(
                                                    &select,
                                                    group.id(),
                                                    &mut result.menu_action,
                                                );
                                                Self::collect_tab_pointer_actions(
                                                    tab_id,
                                                    &select,
                                                    &close,
                                                    can_close,
                                                    &mut result,
                                                );
                                            }
                                            let group_stroke = if active_group {
                                                ui.visuals().selection.stroke
                                            } else {
                                                ui.visuals().widgets.inactive.bg_stroke
                                            };
                                            ui.painter().rect_stroke(
                                                group_rect,
                                                4.0,
                                                group_stroke,
                                                StrokeKind::Inside,
                                            );
                                            ui.painter().line_segment(
                                                [
                                                    pos2(group_rect.center().x, group_rect.top()),
                                                    pos2(
                                                        group_rect.center().x,
                                                        group_rect.bottom(),
                                                    ),
                                                ],
                                                group_stroke,
                                            );
                                            let handle = ui
                                                .interact(
                                                    handle_rect,
                                                    ui.id()
                                                        .with(("split-group-handle", group.id())),
                                                    Sense::click_and_drag(),
                                                )
                                                .on_hover_text("分割セットを移動");
                                            Self::split_context_menu(
                                                &handle,
                                                group.id(),
                                                &mut result.menu_action,
                                            );
                                            if handle.drag_started_by(PointerButton::Primary) {
                                                result.drag_started =
                                                    Some(TabDragSource::Group(group.id()));
                                            }
                                            result.drag_released |=
                                                handle.drag_stopped_by(PointerButton::Primary);
                                            if active_group && tab_to_reveal.is_some() {
                                                response.scroll_to_me(None);
                                            }
                                        }
                                    }
                                }
                            });
                        });
                },
            );
            self.apply_tab_bar_actions(&result);
            self.paint_tab_insertion_feedback(ui, &result);
            output = Some(result);
        });
        output
    }

    fn paint_tab_member(
        &self,
        ui: &mut egui::Ui,
        tab_id: TabId,
        tab_rect: Rect,
        selected: bool,
        focused: bool,
    ) -> (egui::Response, egui::Response, bool) {
        let index = self
            .tabs
            .tab_registry_index(tab_id)
            .expect("displayed tab must exist in document registry");
        let tab = &self.tabs.tabs()[index];
        let editor_dirty_document = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.is_dirty() || editor.mutation_in_flight)
            .map(|editor| editor.document_id);
        let dirty = self.documents[index].has_unsaved_changes()
            || editor_dirty_document == Some(self.documents[index].document_id);
        let marker = if dirty { "● " } else { "" };
        let title = tab
            .path()
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| tab.path().as_os_str().to_string_lossy());
        let content = tab_content_rects(
            tab_rect,
            TAB_HORIZONTAL_PADDING,
            TAB_CLOSE_WIDTH,
            TAB_CONTENT_GAP,
        );
        let widget_id = ui.id().with(("document-tab", tab_id));
        let select_response = ui
            .interact(
                content.selection,
                widget_id.with("select"),
                Sense::click_and_drag(),
            )
            .on_hover_text(tab.path().display().to_string());
        let can_close = !self.documents[index].is_printing();
        let close_response = ui.interact(
            content.close,
            widget_id.with("close"),
            if can_close {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        paint_document_tab(
            ui,
            tab_rect,
            content,
            &format!("{marker}{title}"),
            TabPaintState {
                selected,
                focused,
                can_close,
                select_response: &select_response,
                close_response: &close_response,
            },
        );
        (select_response, close_response, can_close)
    }

    fn collect_tab_pointer_actions(
        tab_id: TabId,
        select: &egui::Response,
        close: &egui::Response,
        can_close: bool,
        output: &mut TabBarOutput,
    ) {
        if select.drag_started_by(PointerButton::Primary) {
            output.drag_started = Some(TabDragSource::Tab(tab_id));
        }
        output.drag_released |= select.drag_stopped_by(PointerButton::Primary);
        match tab_pointer_action(select.clicked(), select.clicked_by(PointerButton::Middle)) {
            Some(TabPointerAction::Select) => output.select = Some(tab_id),
            Some(TabPointerAction::Close) if can_close => output.close = Some(tab_id),
            Some(TabPointerAction::Close) | None => {}
        }
        if can_close && close.clicked() {
            output.close = Some(tab_id);
        }
    }

    fn split_context_menu(
        response: &egui::Response,
        group_id: SplitGroupId,
        action: &mut Option<SplitMenuAction>,
    ) {
        response.context_menu(|ui| {
            if ui.button("左右に並べる").clicked() {
                *action = Some(SplitMenuAction::SetDirection(
                    group_id,
                    SplitDirection::Horizontal,
                ));
                ui.close();
            }
            if ui.button("上下に並べる").clicked() {
                *action = Some(SplitMenuAction::SetDirection(
                    group_id,
                    SplitDirection::Vertical,
                ));
                ui.close();
            }
            if ui.button("配置を入れ替える").clicked() {
                *action = Some(SplitMenuAction::Swap(group_id));
                ui.close();
            }
            ui.separator();
            if ui.button("分割を解除").clicked() {
                *action = Some(SplitMenuAction::Unsplit(group_id));
                ui.close();
            }
        });
    }

    fn menu_bar(&mut self, root_ui: &mut egui::Ui) {
        let mut open_requested = false;
        let mut print_requested = false;
        let mut close_current_requested = false;
        let mut close_all_requested = false;
        let mut exit_requested = false;
        let mut copy_requested = false;
        let mut highlight_requested = false;
        let mut undo_requested = false;
        #[cfg(windows)]
        let mut default_apps_requested = false;

        egui::Panel::top("menu-bar").show(root_ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let file_menu = ui.menu_button("ファイル", |ui| {
                    #[cfg(windows)]
                    if !self.default_apps_menu_open {
                        self.refresh_default_apps_state();
                    }
                    if ui.button("PDFを開く…").clicked() {
                        open_requested = true;
                        ui.close();
                    }
                    let print_label = if cfg!(windows) {
                        "印刷…    Ctrl+P"
                    } else {
                        "印刷…（このOSでは非対応）"
                    };
                    if ui
                        .add_enabled(
                            cfg!(windows) && self.can_print(),
                            egui::Button::new(print_label),
                        )
                        .clicked()
                    {
                        print_requested = true;
                        ui.close();
                    }
                    #[cfg(windows)]
                    ui.checkbox(&mut self.auto_rotate_print, "印刷時に用紙の向きを自動回転");
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.active_index()
                                .is_some_and(|index| !self.documents[index].is_printing()),
                            egui::Button::new("現在のタブを閉じる"),
                        )
                        .clicked()
                    {
                        close_current_requested = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.documents.is_empty(),
                            egui::Button::new("すべてのタブを閉じる"),
                        )
                        .clicked()
                    {
                        close_all_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.checkbox(&mut self.restore_enabled, "前回のセッションを復元");
                    #[cfg(windows)]
                    {
                        if let DefaultAppState::Unavailable(error) = &self.default_apps_state {
                            ui.label("既定のPDFアプリ状態を確認できませんでした。");
                            ui.label(error);
                        }
                        let (label, enabled) = default_app_menu_item(&self.default_apps_state);
                        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                            default_apps_requested = true;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("終了").clicked() {
                        exit_requested = true;
                        ui.close();
                    }
                });
                #[cfg(windows)]
                {
                    self.default_apps_menu_open = file_menu.inner.is_some();
                }
                #[cfg(not(windows))]
                let _ = file_menu;
                ui.menu_button("編集", |ui| {
                    if ui
                        .add_enabled(self.can_undo(), egui::Button::new("元に戻す    Ctrl+Z"))
                        .clicked()
                    {
                        undo_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.can_copy_selection(),
                            egui::Button::new("コピー    Ctrl+C"),
                        )
                        .clicked()
                    {
                        copy_requested = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.can_create_highlight(),
                            egui::Button::new("ハイライト    H"),
                        )
                        .clicked()
                    {
                        highlight_requested = true;
                        ui.close();
                    }
                });
                ui.menu_button("表示", |ui| {
                    ui.checkbox(&mut self.sidebar_open, "サイドバー");
                    ui.separator();
                    if let Some(index) = self.active_index() {
                        if ui.button("目次").clicked() {
                            self.sidebar_open = true;
                            self.sidebar_tab = SidebarTab::Outline;
                            ui.close();
                        }
                        if ui.button("サムネイル").clicked() {
                            self.sidebar_open = true;
                            self.sidebar_tab = SidebarTab::Thumbnails;
                            ui.close();
                        }
                        if ui.button("ハイライト一覧").clicked() {
                            self.sidebar_open = true;
                            self.sidebar_tab = SidebarTab::Highlights;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("幅に合わせる").clicked() {
                            self.documents[index].view.zoom_mode = ZoomMode::FitWidth;
                            ui.close();
                        }
                        if ui.button("ページ全体").clicked() {
                            self.documents[index].view.zoom_mode = ZoomMode::FitPage;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("連続表示").clicked() {
                            self.cancel_active_viewport_interaction();
                            self.documents[index].set_display_mode(DisplayMode::Continuous);
                            ui.close();
                        }
                        if ui.button("単一ページ表示").clicked() {
                            self.cancel_active_viewport_interaction();
                            self.documents[index].set_display_mode(DisplayMode::SinglePage);
                            ui.close();
                        }
                    }
                });
            });
        });

        if open_requested {
            self.pick_pdf_and_open();
        }
        if print_requested {
            self.print();
        }
        if close_current_requested && let Some(index) = self.active_index() {
            self.close_tab(index);
        }
        if close_all_requested {
            self.request_close_all();
        }
        if exit_requested {
            root_ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
        if copy_requested {
            self.copy_selection(root_ui.ctx());
        }
        if highlight_requested {
            self.create_highlight();
        }
        if undo_requested {
            self.undo();
        }
        #[cfg(windows)]
        if default_apps_requested && let Err(error) = open_default_apps_settings() {
            self.error = Some(format!(
                "Windowsの既定のアプリ設定を開けませんでした。詳細: {error}"
            ));
        }
    }

    fn toolbar(&mut self, root_ui: &mut egui::Ui) {
        let mut search_changed = false;
        let mut search_navigation = None;
        let mut submitted_page = None;
        let mut page_delta_requested = None;
        let mut open_requested = false;
        let mut print_requested = false;

        let toolbar_frame = egui::Frame::side_top_panel(root_ui.style()).stroke(egui::Stroke::new(
            1.0,
            root_ui.visuals().widgets.noninteractive.bg_stroke.color,
        ));
        let toolbar_panel = egui::Panel::top("toolbar").frame(toolbar_frame);
        toolbar_panel.show(root_ui, |ui| {
            // ウィンドウが狭くてもスクロールで 1 行を保つ。折り返すと機能上のグループ内の
            // コントロールが分離される。
            egui::ScrollArea::horizontal()
                .id_salt("toolbar-scroll")
                .show(ui, |ui| {
                    // 中央揃えの前にスクロール内容を制約する。そうしないと上部パネルが
                    // ここで残りのウィンドウ高さを提供してしまう。
                    ui.set_height(TOOLBAR_CONTROL_HEIGHT);
                    // 固定行の中で中央揃えにし、ラベル、TextEdit、区切り、アイコンの対象を
                    // 1 本の視覚軸にそろえる。
                    ui.horizontal_centered(|ui| {
                        // PDF を開いていない間もサイドバーの枠を安定させ、最初の文書を読み込んだ
                        // ときに残りのツールバー群が移動しないようにする。
                        let sidebar_enabled = self
                            .active_index()
                            .is_some_and(|index| self.documents[index].info.is_some());
                        if icon_button(
                            ui,
                            ToolbarIcon::Sidebar,
                            sidebar_enabled,
                            sidebar_enabled && self.sidebar_open,
                            "サイドバーを表示/非表示",
                        )
                        .clicked()
                        {
                            self.sidebar_open = !self.sidebar_open;
                        }
                        open_requested =
                            icon_button(ui, ToolbarIcon::Open, true, false, "PDFを開く (Ctrl+O)")
                                .clicked();
                        let print_enabled = cfg!(windows) && self.can_print();
                        let print_tooltip = if cfg!(windows) {
                            "印刷 (Ctrl+P)"
                        } else {
                            "このOSでは印刷に対応していません"
                        };
                        print_requested = icon_button(
                            ui,
                            ToolbarIcon::Print,
                            print_enabled,
                            false,
                            print_tooltip,
                        )
                        .clicked();
                        ui.separator();

                        let Some(index) = self.active_index() else {
                            ui.label("PDFをドロップするか、開くボタンから選択してください");
                            return;
                        };
                        let document_id = self.documents[index].document_id;
                        let page_count = self.documents[index]
                            .info
                            .as_ref()
                            .map(|info| info.page_bounds.len())
                            .unwrap_or(0);
                        let current_page = self.documents[index].view.current_page;

                        ui.label("ページ:");
                        let page_id = page_number_id(document_id);
                        if !ui.memory(|memory| memory.has_focus(page_id)) {
                            self.documents[index].page_input = (current_page + 1).to_string();
                        }
                        let page_input_width = page_number_input_width(ui, page_count);
                        let page_response = ui.add_sized(
                            [page_input_width, TOOLBAR_CONTROL_HEIGHT],
                            toolbar_singleline_text_edit(&mut self.documents[index].page_input)
                                .id(page_id)
                                .desired_width(page_input_width)
                                .horizontal_align(egui::Align::Center),
                        );
                        let enter_pressed = page_response.has_focus()
                            && ui.input(|input| input.key_pressed(Key::Enter));
                        if enter_pressed || page_response.lost_focus() {
                            submitted_page = Some(self.documents[index].page_input.clone());
                        }
                        ui.label(format!("/ {page_count}"));
                        if icon_button(
                            ui,
                            ToolbarIcon::Previous,
                            current_page > 0,
                            false,
                            "前のページ (PageUp)",
                        )
                        .clicked()
                        {
                            page_delta_requested = Some(-1);
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::Next,
                            current_page + 1 < page_count,
                            false,
                            "次のページ (PageDown)",
                        )
                        .clicked()
                        {
                            page_delta_requested = Some(1);
                        }
                        ui.separator();
                        if icon_button(ui, ToolbarIcon::ZoomOut, true, false, "縮小").clicked() {
                            self.zoom_by(1.0 / 1.1);
                        }
                        ui.label(format!("{:.0}%", self.documents[index].view.zoom * 100.0));
                        if icon_button(ui, ToolbarIcon::ZoomIn, true, false, "拡大").clicked() {
                            self.zoom_by(1.1);
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::FitWidth,
                            true,
                            self.documents[index].view.zoom_mode == ZoomMode::FitWidth,
                            "幅に合わせる",
                        )
                        .clicked()
                        {
                            self.documents[index].view.zoom_mode = ZoomMode::FitWidth;
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::FitPage,
                            true,
                            self.documents[index].view.zoom_mode == ZoomMode::FitPage,
                            "ページ全体を表示",
                        )
                        .clicked()
                        {
                            self.documents[index].view.zoom_mode = ZoomMode::FitPage;
                        }
                        ui.separator();
                        let continuous =
                            self.documents[index].view.display_mode == DisplayMode::Continuous;
                        if icon_button(ui, ToolbarIcon::Continuous, true, continuous, "連続表示")
                            .clicked()
                        {
                            self.cancel_active_viewport_interaction();
                            self.documents[index].set_display_mode(DisplayMode::Continuous);
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::SinglePage,
                            true,
                            !continuous,
                            "単一ページ表示",
                        )
                        .clicked()
                        {
                            self.cancel_active_viewport_interaction();
                            self.documents[index].set_display_mode(DisplayMode::SinglePage);
                        }
                        let highlight_tooltip = self.documents[index]
                            .info
                            .as_ref()
                            .and_then(|info| info.highlight_capability.restriction())
                            .unwrap_or("選択範囲をハイライト (H)");
                        if icon_button(
                            ui,
                            ToolbarIcon::Highlight,
                            self.can_create_highlight(),
                            false,
                            highlight_tooltip,
                        )
                        .clicked()
                        {
                            self.create_highlight();
                        }
                        ui.separator();

                        let search = &mut self.documents[index].search;
                        let response = ui.add_sized(
                            [180.0, TOOLBAR_CONTROL_HEIGHT],
                            toolbar_singleline_text_edit(&mut search.query)
                                .id(search_query_id(document_id))
                                .desired_width(180.0)
                                .hint_text("PDF内を検索"),
                        );
                        search_changed = response.changed();
                        let enter = ui.input(|input| input.key_pressed(Key::Enter));
                        if response.has_focus() && enter && !search_changed {
                            search_navigation = Some(!ui.input(|input| input.modifiers.shift));
                        }
                        let match_count = search.pages.values().map(Vec::len).sum::<usize>();
                        let selected = search
                            .selected
                            .and_then(|cursor| search_match_ordinal(&search.pages, cursor));
                        let count = format!("{} / {match_count}", selected.unwrap_or(0));
                        let progress = if search.in_progress {
                            format!("{count} · {}/{}", search.completed_pages, page_count)
                        } else {
                            count
                        };
                        ui.label(progress);
                        if search.truncated {
                            ui.label("上限に到達");
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::Previous,
                            match_count > 0,
                            false,
                            "前の検索結果 (Shift+Enter)",
                        )
                        .clicked()
                        {
                            search_navigation = Some(false);
                        }
                        if icon_button(
                            ui,
                            ToolbarIcon::Next,
                            match_count > 0,
                            false,
                            "次の検索結果 (Enter)",
                        )
                        .clicked()
                        {
                            search_navigation = Some(true);
                        }
                    });
                });
        });

        if open_requested {
            self.pick_pdf_and_open();
        }
        if print_requested {
            self.print();
        }
        if let Some(index) = self.active_index() {
            if let Some(page) = submitted_page {
                self.submit_page_number(index, &page);
            }
            if let Some(delta) = page_delta_requested {
                self.move_page(delta);
            }
            if search_changed {
                self.begin_search(index);
            } else if let Some(forward) = search_navigation {
                self.navigate_search(index, forward);
            }
        }
    }

    fn submit_page_number(&mut self, index: usize, input: &str) {
        let page_count = self.documents[index]
            .info
            .as_ref()
            .map_or(0, |info| info.page_bounds.len());
        if let Some(page_index) = page_index_from_input(input, page_count) {
            let page_number = page_index + 1;
            self.documents[index].jump_to_page(page_index);
            self.documents[index].page_input = page_number.to_string();
            self.documents[index].page_input_error = None;
            return;
        }
        self.documents[index].page_input =
            (self.documents[index].view.current_page + 1).to_string();
        self.documents[index].page_input_error = Some(format!(
            "ページ番号は 1 から {page_count} の範囲で入力してください"
        ));
    }

    fn can_copy_selection(&self) -> bool {
        self.active_index()
            .and_then(|index| self.documents[index].selection.as_ref())
            .is_some()
    }

    fn can_create_highlight(&self) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        self.documents[index]
            .info
            .as_ref()
            .is_some_and(|info| info.highlight_capability.is_allowed())
            && !self.documents[index].print_in_flight
            && self.documents[index]
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.quads.is_empty())
    }

    fn can_undo(&self) -> bool {
        if self.window_close_pending || self.close_all_pending || self.close_confirmation.is_some()
        {
            return false;
        }
        if self.active_index().is_some_and(|index| {
            let document_id = self.documents[index].document_id;
            self.annotation_editor.as_ref().is_some_and(|editor| {
                editor.document_id == document_id
                    && (editor.is_dirty() || editor.mutation_in_flight)
            })
        }) {
            return false;
        }
        self.active_index().is_some_and(|index| {
            let tab = &self.documents[index];
            !tab.edit_history.is_empty()
                && !tab.undo_in_flight
                && !tab.save_in_flight
                && !tab.print_in_flight
                && tab.service.is_some()
        })
    }

    fn can_print(&self) -> bool {
        // ネイティブダイアログは文書ワーカー上で動くため、オープン、保存、印刷、終了確認、
        // ワーカー不在は排他的に扱う。
        self.active_index().is_some_and(|index| {
            let tab = &self.documents[index];
            tab.state != DocumentState::Opening
                && tab.state != DocumentState::Suspended
                && tab.state != DocumentState::Error
                && !tab.save_in_flight
                && !tab.print_in_flight
                && tab.service.is_some()
                && tab.info.is_some()
        }) && !self.window_close_pending
            && !self.close_all_pending
            && self.close_confirmation.is_none()
    }

    fn print(&mut self) {
        #[cfg(windows)]
        {
            if !self.can_print() {
                return;
            }
            let auto_rotate = self.auto_rotate_print;
            let tab = self
                .active_tab_mut()
                .expect("can_print requires an active tab");
            if tab.send(DocumentCommand::Print { auto_rotate }) {
                tab.print_in_flight = true;
                self.status = "印刷ダイアログを準備しています…".to_owned();
            } else {
                self.error = Some("印刷: 文書ワーカーを利用できません".to_owned());
            }
        }
        #[cfg(not(windows))]
        {
            self.error = Some("このOSでは印刷に対応していません".to_owned());
        }
    }

    fn status_panel(&self, _root_ui: &mut egui::Ui) {
        #[cfg(debug_assertions)]
        egui::Panel::bottom("debug-status").show(_root_ui, |ui| {
            egui::CollapsingHeader::new("デバッグ情報")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(&self.status);
                        if let Some(index) = self.active_index() {
                            let tab = &self.documents[index];
                            if let Some(info) = &tab.info {
                                ui.separator();
                                ui.label(format!("open {:.1} ms", milliseconds(info.open_time)));
                                ui.label(format!("Highlights: {}", info.highlight_count));
                                // 明示した方式により、開発時の診断で遅い全体書き換えと停止した保存を
                                // 区別できる。
                                if info.can_save_incrementally {
                                    ui.label("incremental save");
                                } else {
                                    ui.label("full-file rewrite");
                                }
                                if let Some(memory) = info.physical_memory_bytes {
                                    ui.label(format_memory(memory));
                                }
                            }
                            let current_tile = tab
                                .tiles
                                .values()
                                .find(|cached| cached.tile.page_index == tab.view.current_page);
                            if let Some(cached) = current_tile {
                                ui.label(format!(
                                    "render {:.1} ms @ {:.2}x",
                                    milliseconds(cached.tile.render_time),
                                    cached.tile.scale
                                ));
                                if let Some(memory) = cached.tile.physical_memory_bytes {
                                    ui.label(format_memory(memory));
                                }
                            }
                            if let Some(selection) = &tab.selection {
                                ui.label(format!(
                                    "text {:.1} ms",
                                    milliseconds(selection.extraction_time)
                                ));
                            }
                            if let Some(measurement) = &tab.render_performance.page_transition {
                                ui.separator();
                                ui.label(format!(
                                    "page {}: first {}, full {}, cache {}, prefetch {}",
                                    measurement.target_page + 1,
                                    debug_duration(measurement.first_exact_tile),
                                    debug_duration(measurement.full_exact_viewport),
                                    debug_yes_no(measurement.cache_hit),
                                    debug_yes_no(measurement.prefetch_used),
                                ));
                            }
                            if let Some(measurement) = &tab.render_performance.zoom {
                                ui.separator();
                                ui.label(format!(
                                    "zoom {:.0}%: provisional {}, final-input→first {}, full {}, discarded {}",
                                    measurement.target_zoom * 100.0,
                                    debug_duration(measurement.provisional_display),
                                    debug_duration(measurement.first_exact_tile),
                                    debug_duration(measurement.full_exact_viewport),
                                    measurement.discarded_intermediate_requests,
                                ));
                            }
                        }
                        ui.separator();
                        if self.gpu_lru.is_empty() {
                            ui.label("GPU tiles: empty");
                        } else {
                            ui.label(format!(
                                "GPU tiles: {} ({:.1}/{:.0} MiB)",
                                self.gpu_lru.len(),
                                self.gpu_lru.current_bytes() as f64 / 1_048_576.0,
                                self.gpu_lru.budget() as f64 / 1_048_576.0,
                            ));
                        }
                    });
                });
        });
    }

    fn error_banner(&mut self, root_ui: &mut egui::Ui) {
        let app_error = self.error.clone();
        let document_error = self
            .active_index()
            .and_then(|index| self.documents[index].error.clone());
        let page_input_error = self
            .active_index()
            .and_then(|index| self.documents[index].page_input_error.clone());
        if app_error.is_none() && document_error.is_none() && page_input_error.is_none() {
            return;
        }
        let conflict_index = self.active_index().filter(|index| {
            self.documents[*index].external_conflict_reported
                && self.documents[*index].has_unsaved_changes()
        });
        egui::Panel::top("persistent-error-banner").show(root_ui, |ui| {
            if let Some(error) = app_error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if let Some(error) = document_error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if let Some(error) = page_input_error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if let Some(index) = conflict_index
                && ui.button("編集版を別名保存して外部版を読み込む").clicked()
            {
                self.save_conflicted_document_as(index);
            }
        });
    }

    fn sidebar_panel(&mut self, root_ui: &mut egui::Ui) {
        if !self.sidebar_open {
            return;
        }
        let mut selected_page = None;
        let mut highlight_action = None;
        egui::Panel::left("document-sidebar")
            .default_size(220.0)
            .size_range(180.0..=360.0)
            .resizable(true)
            .show(root_ui, |ui| {
                ui.horizontal(|ui| {
                    if icon_button(
                        ui,
                        ToolbarIcon::Outline,
                        true,
                        self.sidebar_tab == SidebarTab::Outline,
                        "目次",
                    )
                    .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Outline;
                    }
                    if icon_button(
                        ui,
                        ToolbarIcon::Thumbnails,
                        true,
                        self.sidebar_tab == SidebarTab::Thumbnails,
                        "サムネイル",
                    )
                    .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Thumbnails;
                    }
                    if icon_button(
                        ui,
                        ToolbarIcon::Highlight,
                        true,
                        self.sidebar_tab == SidebarTab::Highlights,
                        "ハイライト一覧",
                    )
                    .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Highlights;
                    }
                });
                ui.separator();
                let Some(index) = self.active_index() else {
                    return;
                };
                match self.sidebar_tab {
                    SidebarTab::Outline => {
                        if let Some(outline) = &self.documents[index].outline {
                            if outline.is_empty() {
                                ui.label("目次はありません");
                            } else {
                                selected_page = show_outline(ui, outline);
                            }
                        } else {
                            ui.spinner();
                            ui.label("目次を読み込み中…");
                        }
                    }
                    SidebarTab::Thumbnails => {
                        selected_page = self.thumbnail_sidebar(ui, index);
                    }
                    SidebarTab::Highlights => {
                        self.documents[index].start_highlight_index();
                        let index_state = &self.documents[index].highlight_index;
                        let action = show_highlights(
                            ui,
                            &index_state.pages,
                            index_state.total_pages,
                            index_state.in_flight.is_some(),
                            index_state.error.as_deref(),
                        );
                        if let (Some(revision), Some(action)) = (index_state.revision, action) {
                            highlight_action = Some((revision, action));
                        }
                    }
                }
            });
        if let Some(index) = self.active_index()
            && let Some(page) = selected_page
        {
            self.documents[index].jump_to_page(page);
        }
        if let Some(index) = self.active_index()
            && let Some((revision, action)) = highlight_action
        {
            self.handle_highlight_sidebar_action(index, revision, action);
        }
    }

    fn thumbnail_sidebar(&mut self, ui: &mut egui::Ui, index: usize) -> Option<usize> {
        let (page_count, revision) = self.documents[index]
            .info
            .as_ref()
            .map(|info| (info.page_bounds.len(), info.revision))?;
        let tab = &mut self.documents[index];
        let thumbnail_lru = &mut self.thumbnail_lru;
        let mut selected_page = None;
        egui::ScrollArea::vertical().show_rows(
            ui,
            THUMBNAIL_ROW_HEIGHT,
            page_count,
            |ui, visible_rows| {
                for page_index in visible_rows {
                    let key = ThumbnailCacheKey::for_page(tab.document_id, page_index, revision);
                    tab.request_thumbnail(page_index, key);
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), THUMBNAIL_ROW_HEIGHT),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            if thumbnail_lru.get(&key).is_some()
                                && let Some(cached) = tab.thumbnails.get(&key)
                            {
                                let available_width = ui.available_width().max(1.0);
                                let scale = (available_width / cached.thumbnail.pixel_width as f32)
                                    .min(1.0);
                                let size = Vec2::new(
                                    cached.thumbnail.pixel_width as f32 * scale,
                                    cached.thumbnail.pixel_height as f32 * scale,
                                );
                                let image = egui::Image::new((cached.texture.id(), size))
                                    .sense(egui::Sense::click());
                                if ui.add(image).clicked() {
                                    selected_page = Some(page_index);
                                }
                            } else if tab.failed_thumbnails.contains(&key) {
                                ui.add_space(THUMBNAIL_MAX_HEIGHT as f32 / 2.0);
                                ui.label("サムネイルを読み込めませんでした");
                                if ui.button("再試行").clicked() {
                                    // 永続的な PDF エラーで毎フレーム再試行してはならない。
                                    // 明示的なユーザー操作だけが再キューする。
                                    tab.failed_thumbnails.remove(&key);
                                    tab.request_thumbnail(page_index, key);
                                }
                            } else {
                                ui.add_space(THUMBNAIL_MAX_HEIGHT as f32 / 2.0);
                                ui.spinner();
                            }
                            if ui
                                .selectable_label(
                                    tab.view.current_page == page_index,
                                    format!("{}ページ", page_index + 1),
                                )
                                .clicked()
                            {
                                selected_page = Some(page_index);
                            }
                        },
                    );
                }
            },
        );
        selected_page
    }

    fn central_panel(&mut self, root_ui: &mut egui::Ui, tab_bar: Option<&TabBarOutput>) -> Rect {
        let mut focused_pdf_rect = Rect::NOTHING;
        egui::CentralPanel::default().show(root_ui, |ui| {
            if self.documents.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("PDFをこのウィンドウへドロップしてください");
                });
                return;
            }

            let available = ui.available_rect_before_wrap();
            let visible_tabs = self.tabs.visible_tab_ids();
            let active_split = self.tabs.active_split().cloned();
            let (pane_rects, split_layout) = if let Some(group) = &active_split {
                let split = split_rects(available, group.ratio(), group.direction());
                (
                    split.panes.to_vec(),
                    Some((group.id(), group.direction(), split)),
                )
            } else {
                (vec![available], None)
            };

            let (press_origin, hover_position, pointer_navigation, zoom_delta) =
                ui.input(|input| {
                    let wheel_input = input
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::MouseWheel { .. }));
                    let zoom_delta = input.zoom_delta();
                    (
                        input.pointer.press_origin(),
                        input.pointer.hover_pos(),
                        wheel_input || (zoom_delta - 1.0).abs() > f32::EPSILON,
                        zoom_delta,
                    )
                });
            let pointer =
                press_origin.or_else(|| pointer_navigation.then_some(hover_position).flatten());
            let foreground_owns_pointer = pointer.is_some_and(|pointer| {
                foreground_layer_blocks_pane_input(ui.ctx().layer_id_at(pointer))
            });
            let pointer_side = (!foreground_owns_pointer)
                .then(|| {
                    pointer.and_then(|pointer| {
                        pane_rects.iter().position(|rect| rect.contains(pointer))
                    })
                })
                .flatten()
                .and_then(|index| match index {
                    0 => Some(SplitSide::First),
                    1 => Some(SplitSide::Second),
                    _ => None,
                });
            if let Some(side) = pointer_side {
                self.focus_side(side);
            }
            if (zoom_delta - 1.0).abs() > f32::EPSILON && !foreground_owns_pointer {
                self.zoom_by(zoom_delta);
            }

            if let Some((group_id, direction, split)) = split_layout {
                self.separator(ui, available, group_id, direction, split);
            }

            let mut outputs = Vec::with_capacity(visible_tabs.len());
            for (index, (tab_id, pane_rect)) in visible_tabs.into_iter().zip(pane_rects).enumerate()
            {
                let side = if index == 0 {
                    SplitSide::First
                } else {
                    SplitSide::Second
                };
                ui.scope_builder(
                    UiBuilder::new()
                        .id_salt(("document-pane", side))
                        .max_rect(pane_rect),
                    |pane_ui| self.document_view(pane_ui, side, tab_id),
                );
                outputs.push(PaneUiOutput {
                    side,
                    tab_id,
                    pdf_rect: pane_rect,
                });
                if self.tabs.active_tab_id() == Some(tab_id) {
                    focused_pdf_rect = pane_rect;
                }
            }

            self.paint_pdf_drop_feedback(ui, tab_bar, &outputs);
            self.paint_tab_drag_preview(ui);
            self.finish_tab_drag(ui, tab_bar, &outputs);
            ui.allocate_rect(available, Sense::hover());
        });
        focused_pdf_rect
    }

    fn separator(
        &mut self,
        ui: &mut egui::Ui,
        available: Rect,
        group_id: SplitGroupId,
        direction: SplitDirection,
        split: SplitRects,
    ) {
        let response = ui
            .interact(
                split.separator,
                ui.id().with("document-pane-separator"),
                Sense::drag(),
            )
            .on_hover_cursor(match direction {
                SplitDirection::Horizontal => egui::CursorIcon::ResizeHorizontal,
                SplitDirection::Vertical => egui::CursorIcon::ResizeVertical,
            });
        let color = if response.dragged() || response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        ui.painter().rect_filled(split.separator, 0.0, color);
        if response.dragged_by(PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let ratio = match direction {
                SplitDirection::Horizontal => {
                    let extent = (available.width() - split.separator.width()).max(1.0);
                    (pointer.x - available.left()) / extent
                }
                SplitDirection::Vertical => {
                    let extent = (available.height() - split.separator.height()).max(1.0);
                    (pointer.y - available.top()) / extent
                }
            };
            let _applied = self.tabs.set_split_ratio(group_id, ratio.clamp(0.1, 0.9));
        } else if self
            .tabs
            .split_group(group_id)
            .is_some_and(|group| group.ratio() != split.ratio)
        {
            // ウィンドウ縮小でpoint最小寸法へclampした比率を保存し、拡大時に操作不能な寸法へ戻さない。
            let _applied = self.tabs.set_split_ratio(group_id, split.ratio);
        }
    }

    fn document_view(&mut self, ui: &mut egui::Ui, side: SplitSide, tab_id: TabId) {
        let index = self
            .tabs
            .tab_registry_index(tab_id)
            .expect("selected pane tab must exist in document registry");
        if self.documents[index].state == DocumentState::Opening {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        }
        if self.documents[index].state == DocumentState::Error {
            ui.centered_and_justified(|ui| {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    self.documents[index]
                        .error
                        .as_deref()
                        .unwrap_or("PDF文書ワーカーが停止しました"),
                );
            });
            return;
        }
        if self.documents[index].info.is_none() {
            return;
        }

        let pixels_per_point = ui.ctx().pixels_per_point();
        if self.documents[index]
            .view
            .update_render_density(pixels_per_point)
        {
            self.documents[index].invalidate_rendering();
        }
        self.update_fit_zoom(index, ui.available_size());
        match self.documents[index].view.display_mode {
            DisplayMode::Continuous => self.continuous_view(ui, side, index),
            DisplayMode::SinglePage => self.single_page_view(ui, side, index),
        }
    }

    fn apply_tab_bar_actions(&mut self, output: &TabBarOutput) {
        if let Some(source) = output.drag_started {
            self.tab_drag = Some(TabDragState { source });
        }
        if let Some(tab_id) = output.select
            && let Some(index) = self.tabs.tab_registry_index(tab_id)
        {
            self.select_tab(index);
        }
        if let Some(tab_id) = output.close
            && let Some(index) = self.tabs.tab_registry_index(tab_id)
        {
            self.close_tab(index);
        }
        let Some(action) = output.menu_action else {
            return;
        };
        let previous = self.active_index();
        let previously_visible = self.visible_indices();
        let transition_tabs = match action {
            SplitMenuAction::Swap(group_id) => self
                .tabs
                .split_group(group_id)
                .map(|group| group.tabs().to_vec())
                .unwrap_or_default(),
            SplitMenuAction::Unsplit(group_id) => self
                .tabs
                .split_group(group_id)
                .map(|group| vec![group.tab(SplitSide::Second)])
                .unwrap_or_default(),
            SplitMenuAction::SetDirection(_, _) => Vec::new(),
        };
        let changed = match action {
            SplitMenuAction::SetDirection(group_id, direction) => {
                self.tabs.set_split_direction(group_id, direction)
            }
            SplitMenuAction::Swap(group_id) => self.tabs.swap_split_members(group_id),
            SplitMenuAction::Unsplit(group_id) => self.tabs.unsplit(group_id),
        };
        if changed {
            for tab_id in transition_tabs {
                if let Some(index) = self.tabs.tab_registry_index(tab_id) {
                    self.documents[index].prepare_for_pane_transition();
                }
            }
            self.cancel_all_viewport_interactions();
            self.sync_pane_viewports();
            if let Some(index) = self.active_index() {
                self.activate_document(index, previous, &previously_visible);
            }
        }
    }

    fn paint_tab_insertion_feedback(&self, ui: &egui::Ui, output: &TabBarOutput) {
        if self.tab_drag.is_none() {
            return;
        }
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if foreground_layer_blocks_pane_input(ui.ctx().layer_id_at(pointer)) {
            return;
        }
        if output.bar_rect.contains(pointer) {
            let insertion = tab_insertion_index(&output.entry_rects, pointer.x);
            let line_x = match insertion {
                0 => output
                    .entry_rects
                    .first()
                    .map_or(output.bar_rect.left(), Rect::left),
                index if index == output.entry_rects.len() => output
                    .entry_rects
                    .last()
                    .map_or(output.bar_rect.left(), Rect::right),
                index => {
                    let previous = output.entry_rects[index - 1].right();
                    let next = output.entry_rects[index].left();
                    (previous + next) / 2.0
                }
            };
            ui.painter().line_segment(
                [
                    pos2(line_x, output.bar_rect.top()),
                    pos2(line_x, output.bar_rect.bottom()),
                ],
                Stroke::new(2.0, ui.visuals().selection.stroke.color),
            );
        }
    }

    fn paint_pdf_drop_feedback(
        &self,
        ui: &egui::Ui,
        tab_bar: Option<&TabBarOutput>,
        outputs: &[PaneUiOutput],
    ) {
        let Some(drag) = self.tab_drag else {
            return;
        };
        let TabDragSource::Tab(dragged_tab) = drag.source else {
            return;
        };
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if foreground_layer_blocks_pane_input(ui.ctx().layer_id_at(pointer))
            || tab_bar.is_some_and(|bar| bar.bar_rect.contains(pointer))
        {
            return;
        }
        if outputs.len() == 1
            && outputs[0].tab_id != dragged_tab
            && let Some(placement) = split_drop_placement(outputs[0].pdf_rect, pointer)
        {
            let highlight = split_drop_highlight(outputs[0].pdf_rect, placement);
            ui.painter().rect_filled(
                highlight,
                4.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.35),
            );
        } else if outputs.len() == 2
            && let Some(target) = outputs
                .iter()
                .find(|output| output.tab_id != dragged_tab && output.pdf_rect.contains(pointer))
        {
            ui.painter().rect_filled(
                target.pdf_rect,
                4.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.25),
            );
        }
    }

    fn tab_drag_preview_label(&self, source: TabDragSource) -> Option<String> {
        let title = |tab_id| {
            let index = self.tabs.tab_registry_index(tab_id)?;
            let path = self.tabs.tabs()[index].path();
            Some(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.as_os_str().to_string_lossy().into_owned()),
            )
        };
        match source {
            TabDragSource::Tab(tab_id) => title(tab_id),
            TabDragSource::Group(group_id) => {
                let group = self.tabs.split_group(group_id)?;
                Some(format!(
                    "{} ｜ {}",
                    title(group.tab(SplitSide::First))?,
                    title(group.tab(SplitSide::Second))?
                ))
            }
        }
    }

    fn paint_tab_drag_preview(&self, ui: &egui::Ui) {
        let Some(drag) = self.tab_drag else {
            return;
        };
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        let Some(label) = self.tab_drag_preview_label(drag.source) else {
            return;
        };

        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let text_color = ui.visuals().strong_text_color();
        let mut title_job = egui::text::LayoutJob::single_section(
            label,
            egui::TextFormat {
                font_id,
                color: text_color,
                ..Default::default()
            },
        );
        title_job.wrap = egui::epaint::text::TextWrapping::truncate_at_width(
            TAB_MAX_WIDTH - TAB_DRAG_PREVIEW_PADDING * 2.0,
        );
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(title_job));
        let preview_size = Vec2::new(galley.size().x + TAB_DRAG_PREVIEW_PADDING * 2.0, TAB_HEIGHT);
        let bounds = ui.ctx().content_rect();
        let desired = pointer + Vec2::splat(TAB_DRAG_PREVIEW_OFFSET);
        // 画面端でも保持中のタイトルが見えるように、ポインターからずらした矩形だけを
        // content rect内へ制限する。drop判定に使うポインター座標は変更しない。
        let preview_min = pos2(
            desired.x.clamp(
                bounds.left(),
                (bounds.right() - preview_size.x).max(bounds.left()),
            ),
            desired.y.clamp(
                bounds.top(),
                (bounds.bottom() - preview_size.y).max(bounds.top()),
            ),
        );
        let preview_rect = Rect::from_min_size(preview_min, preview_size);
        let painter = ui.ctx().layer_painter(LayerId::new(
            egui::Order::Tooltip,
            Id::new("tab-drag-preview"),
        ));
        painter.rect(
            preview_rect,
            4.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.92),
            ui.visuals().selection.stroke,
            StrokeKind::Inside,
        );
        painter.galley(
            pos2(
                preview_rect.left() + TAB_DRAG_PREVIEW_PADDING,
                preview_rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            text_color,
        );
    }

    fn finish_tab_drag(
        &mut self,
        ui: &egui::Ui,
        tab_bar: Option<&TabBarOutput>,
        outputs: &[PaneUiOutput],
    ) {
        let Some(tab_bar) = tab_bar else {
            return;
        };
        if !tab_bar.drag_released {
            return;
        }
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if foreground_layer_blocks_pane_input(ui.ctx().layer_id_at(pointer)) {
            return;
        }
        let previous = self.active_index();
        let previously_visible = self.visible_indices();
        let mut changed = false;
        let mut transitioned_tabs = Vec::new();
        if tab_bar.bar_rect.contains(pointer) {
            let insertion = tab_insertion_index(&tab_bar.entry_rects, pointer.x);
            changed = match drag.source {
                TabDragSource::Group(group_id) => self.tabs.reorder_split(group_id, insertion),
                TabDragSource::Tab(tab_id) => {
                    let same_group = self.tabs.split_for_tab(tab_id).map(|group| group.id());
                    let pointer_member =
                        tab_bar.tab_ids.iter().zip(&tab_bar.tab_rects).find_map(
                            |(candidate, rect)| rect.contains(pointer).then_some(*candidate),
                        );
                    let pointer_group = tab_bar
                        .group_rects
                        .iter()
                        .find_map(|(group_id, rect)| rect.contains(pointer).then_some(*group_id));
                    match same_group {
                        Some(group_id) if pointer_group == Some(group_id) => {
                            let swap_requested = pointer_member.is_some_and(|candidate| {
                                candidate != tab_id
                                    && self.tabs.split_for_tab(candidate).map(|group| group.id())
                                        == Some(group_id)
                            });
                            if swap_requested {
                                if let Some(group) = self.tabs.split_group(group_id) {
                                    transitioned_tabs.extend(group.tabs());
                                }
                                self.tabs.swap_split_members(group_id)
                            } else {
                                false
                            }
                        }
                        Some(_) => {
                            let second_tab = self
                                .tabs
                                .split_for_tab(tab_id)
                                .map(|group| group.tab(SplitSide::Second));
                            let extracted = self.tabs.extract_split_member(tab_id, insertion);
                            if extracted {
                                // 解除後は両方とも単独表示の第1面になるため、元の第2面だけ
                                // 新しいScrollArea IDへ中央アンカーを引き継ぐ。
                                transitioned_tabs.extend(second_tab);
                            }
                            extracted
                        }
                        None => self.tabs.reorder_single(tab_id, insertion),
                    }
                }
            };
        } else if let TabDragSource::Tab(tab_id) = drag.source {
            if outputs.len() == 1 {
                if let Some(placement) = split_drop_placement(outputs[0].pdf_rect, pointer) {
                    let collapsed_second_tab = self
                        .tabs
                        .split_for_tab(tab_id)
                        .map(|group| group.tab(SplitSide::Second));
                    changed = self.tabs.create_split(tab_id, outputs[0].tab_id, placement);
                    if changed {
                        // 別セットから第1面を持ち出す場合、残る第2面も単独表示の
                        // 第1面へ移るためアンカー引継ぎが必要になる。
                        transitioned_tabs.extend(collapsed_second_tab);
                        transitioned_tabs.extend([tab_id, outputs[0].tab_id]);
                    }
                }
            } else if let Some(target) = outputs
                .iter()
                .find(|output| output.pdf_rect.contains(pointer))
            {
                let group_id = self
                    .tabs
                    .active_split()
                    .expect("two visible tabs require an active split")
                    .id();
                if let Some(displaced) =
                    self.tabs
                        .replace_split_member(group_id, target.side, tab_id)
                {
                    changed = true;
                    transitioned_tabs.extend([tab_id, displaced]);
                }
            }
        }
        if !changed {
            return;
        }

        for tab_id in transitioned_tabs {
            if let Some(index) = self.tabs.tab_registry_index(tab_id) {
                self.documents[index].prepare_for_pane_transition();
            }
        }
        self.cancel_all_viewport_interactions();
        self.sync_pane_viewports();
        if let Some(index) = self.active_index() {
            self.activate_document(index, previous, &previously_visible);
        }
    }

    fn annotation_candidate_picker(&mut self, context: &egui::Context) {
        let Some(index) = self.active_index() else {
            return;
        };
        let document_id = self.documents[index].document_id;
        let revision = self.documents[index]
            .info
            .as_ref()
            .map(|info| info.revision);
        let Some(picker) = self.annotation_picker.as_ref().filter(|picker| {
            picker.document_id == document_id && revision == Some(picker.revision)
        }) else {
            return;
        };
        let picker_revision = picker.revision;
        let candidates = picker.candidates.clone();
        let mut open = true;
        let mut selected = None;
        egui::Window::new("注釈を選択")
            .id(Id::new(("annotation-picker", document_id)))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                ui.label("同じ位置に複数の注釈があります。");
                for candidate in &candidates {
                    if show_annotation_candidate_button(ui, candidate, candidate.can_edit).clicked()
                    {
                        selected = Some(candidate.id);
                    }
                }
            });
        if let Some(annotation_id) = selected {
            self.annotation_picker = None;
            self.open_annotation_editor(index, picker_revision, annotation_id);
        } else if !open {
            self.annotation_picker = None;
        }
    }

    fn annotation_editor_overlay(&mut self, context: &egui::Context, bounds: Rect) {
        let Some(index) = self.active_index() else {
            return;
        };
        let document_id = self.documents[index].document_id;
        let revision = self.documents[index]
            .info
            .as_ref()
            .map(|info| info.revision);
        let Some(editor) = self
            .annotation_editor
            .as_mut()
            .filter(|editor| editor.document_id == document_id)
        else {
            return;
        };
        editor.stale = revision != Some(editor.revision);
        if editor.stale {
            // revision に束縛された xref は、別の編集や保存/再オープン後に更新してはならない。
            // 表示中のバッファは明示的に破棄できる。
            editor.notice = Some(
                "PDFが変更されたため、この編集内容は保存できません。変更を破棄してください。"
                    .to_owned(),
            );
        }
        let action =
            show_annotation_editor(context, bounds, editor, &mut self.recent_annotation_colors);
        match action {
            Some(AnnotationEditorAction::Close | AnnotationEditorAction::Discard) => {
                self.annotation_editor = None;
            }
            Some(AnnotationEditorAction::Save) => {
                self.request_annotation_update(index);
            }
            Some(AnnotationEditorAction::Delete) => {
                let editor = self
                    .annotation_editor
                    .as_ref()
                    .expect("delete action requires an open editor");
                self.request_annotation_delete(index, editor.revision, editor.annotation_id);
            }
            None => {}
        }
    }

    fn update_fit_zoom(&mut self, index: usize, available: Vec2) {
        let tab = &mut self.documents[index];
        let Some(info) = &tab.info else {
            return;
        };
        let Some(bounds) = info.page_bounds.get(tab.view.current_page) else {
            return;
        };
        let Some(desired) = Self::fit_zoom_for_page(*bounds, available, tab.view.zoom_mode) else {
            return;
        };

        if (tab.view.zoom - desired).abs() > ZOOM_CHANGE_EPSILON {
            let mode = tab.view.zoom_mode;
            tab.set_zoom(desired, mode);
        }
    }

    /// 現在のビューポートから Fit ズームを計算し、リサイズ時の無効化をテスト可能にする。
    fn fit_zoom_for_page(
        bounds: crate::domain::document::PageRect,
        available: Vec2,
        zoom_mode: ZoomMode,
    ) -> Option<f32> {
        let usable_width = (available.x - PAGE_GAP * 2.0).max(1.0);
        let usable_height = (available.y - PAGE_GAP * 2.0).max(1.0);
        let desired = match zoom_mode {
            ZoomMode::Fixed => return None,
            ZoomMode::FitWidth => usable_width / bounds.width(),
            ZoomMode::FitPage => {
                (usable_width / bounds.width()).min(usable_height / bounds.height())
            }
        }
        .clamp(MIN_ZOOM, MAX_ZOOM);
        Some(desired)
    }

    fn continuous_view(&mut self, ui: &mut egui::Ui, side: SplitSide, index: usize) {
        let document_id = self.documents[index].document_id;
        if self.documents[index].view.autoscroll.is_some() {
            // 停止入力を受けるまで autoscroll がナビゲーションを所有するため、古い主ジェスチャーを
            // アンカーと同時に残してはならない。
            self.viewports
                .entry(side)
                .or_default()
                .cancel_primary_interaction();
        }
        let editor_input_rect = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.document_id == document_id)
            .map(|_| annotation_overlay_rect(ui.max_rect()));
        let suppress_annotation_hover = self.documents[index].view.autoscroll.is_some()
            || self
                .annotation_editor
                .as_ref()
                .is_some_and(|editor| editor.document_id == document_id)
            || self
                .annotation_picker
                .as_ref()
                .is_some_and(|picker| picker.document_id == document_id);
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport_size = ui.available_size();
        let viewport = self.viewports.entry(side).or_default();
        let gpu_lru = &mut self.gpu_lru;
        let tab = &mut self.documents[index];
        let info = tab.info.as_ref().expect("checked before drawing");
        let path = info.path.clone();
        let page_bounds = info.page_bounds.clone();
        let revision = info.revision;
        let can_create_highlight = info.highlight_capability.is_allowed() && !tab.print_in_flight;
        let widest_page = page_bounds
            .iter()
            .map(|bounds| bounds.width() * tab.view.zoom)
            .fold(0.0_f32, f32::max);
        let content_width = viewport_size.x.max(widest_page + PAGE_GAP * 2.0);
        let layout = ContinuousLayout::new(&page_bounds, tab.view.zoom, content_width);
        let jump_offset = if let Some(page) = tab.view.scroll_to_page.take() {
            tab.view.restore_anchor = None;
            layout
                .placement(page)
                .map(|placement| Vec2::new(0.0, placement.y))
        } else {
            tab.view.restore_anchor.take().and_then(|anchor| {
                layout
                    .centered_offset(anchor, viewport_size.x, viewport_size.y)
                    .map(|(x, y)| Vec2::new(x, y))
            })
        };
        let autoscroll_offset = tab
            .view
            .autoscroll
            .and_then(|autoscroll| autoscroll.requested_offset);
        let pan_offset = tab.view.pan_requested_offset.take();
        let mut scroll_area = egui::ScrollArea::both()
            .id_salt(("continuous-pdf", &path))
            .auto_shrink([false, false]);
        if let Some(offset) = jump_offset.or(pan_offset).or(autoscroll_offset) {
            scroll_area = scroll_area.scroll_offset(offset);
        }

        let mut completed_drag = None;
        let mut annotation_action = None;
        let mut pan_delta = None;
        let mut clear_selection = false;
        let mut cursor_target = None;
        let mut page_screen_rects = Vec::new();
        let output = scroll_area.show_viewport(ui, |ui, visible_viewport| {
            ui.set_min_size(Vec2::new(content_width, layout.total_height()));
            let visible_text_pages =
                layout.visible_pages(visible_viewport.min.y..visible_viewport.max.y, 0.0);
            tab.prepare_text_snapshots(visible_text_pages.clone(), revision);
            tab.prepare_annotation_pages(visible_text_pages, revision);
            // 1 ビューポート分を先読みして通常のホイールスクロールを滑らかにし、共有バイト LRU で
            // 厳密なメモリ上限を確保する。
            let wanted_pages = layout.visible_pages(
                visible_viewport.min.y..visible_viewport.max.y,
                visible_viewport.height(),
            );
            let mut requests = Vec::new();
            for page_index in wanted_pages.clone() {
                let Some(placement) = layout.placement(page_index) else {
                    continue;
                };
                let page_content_rect = Rect::from_min_size(
                    Pos2::new(placement.x, placement.y),
                    Vec2::new(placement.width, placement.height),
                );
                let Some(mut page_requests) = tile_requests_for_page(
                    tab,
                    page_index,
                    page_bounds[page_index],
                    page_content_rect,
                    visible_viewport,
                    pixels_per_point,
                ) else {
                    continue;
                };
                requests.append(&mut page_requests);
            }
            tab.prepare_tiles(requests);

            for page_index in wanted_pages {
                let Some(placement) = layout.placement(page_index) else {
                    continue;
                };
                let screen_rect = Rect::from_min_size(
                    ui.max_rect().min + Vec2::new(placement.x, placement.y),
                    Vec2::new(placement.width, placement.height),
                );
                page_screen_rects.push(screen_rect);
                paint_page_tiles(ui, screen_rect, page_index, tab, gpu_lru);
                if let Some(matches) = tab.search.pages.get(&page_index) {
                    let selected_match = tab
                        .search
                        .selected
                        .filter(|cursor| cursor.page_index == page_index)
                        .map(|cursor| cursor.match_index);
                    PageViewport::paint_search_matches(
                        ui,
                        screen_rect,
                        page_bounds[page_index],
                        matches,
                        selected_match,
                    );
                }
                let interaction = ui
                    .scope_builder(UiBuilder::new().max_rect(screen_rect), |page_ui| {
                        let text_key = TextSnapshotKey {
                            page_index,
                            revision,
                        };
                        let annotation_key = AnnotationPageRequest {
                            page_index,
                            expected_revision: revision,
                        };
                        viewport.interact_at(
                            page_ui,
                            PageInteractionInput {
                                screen_rect,
                                page_index,
                                bounds: page_bounds[page_index],
                                text_snapshot: tab.text_snapshots.get(&text_key),
                                selection: tab.selection.as_ref(),
                                annotation_page: tab.annotation_pages.get(&annotation_key),
                                can_create_highlight,
                                suppress_annotation_hover,
                                input_excluded_rect: editor_input_rect,
                            },
                        )
                    })
                    .inner;
                if interaction.completed_drag.is_some() {
                    completed_drag = interaction.completed_drag;
                }
                if interaction.annotation_action.is_some() {
                    annotation_action = interaction.annotation_action;
                }
                if interaction.pan_delta.is_some() {
                    pan_delta = interaction.pan_delta;
                }
                if interaction.cursor_target.is_some() {
                    cursor_target = interaction.cursor_target;
                }
                clear_selection |= interaction.clear_selection;
            }
        });

        if let Some(page) = layout.page_at_y(output.state.offset.y + 1.0) {
            tab.view.current_page = page;
        }
        let viewport_center = output.state.offset + output.inner_rect.size() / 2.0;
        tab.view.center_anchor = layout.anchor_at(viewport_center.x, viewport_center.y);
        let maximum_offset = (output.content_size - output.inner_rect.size()).max(Vec2::ZERO);
        let excluded_rects = editor_input_rect.into_iter().collect::<Vec<_>>();
        let background_interaction = viewport.interact_background(
            ui,
            output.inner_rect,
            &page_screen_rects,
            &excluded_rects,
            tab.selection.is_some(),
        );
        pan_delta = pan_delta.or(background_interaction.pan_delta);
        cursor_target = cursor_target.or(background_interaction.cursor_target);
        clear_selection |= background_interaction.clear_selection;
        if let Some(delta) = pan_delta {
            let requested_offset = output.state.offset - delta;
            tab.view.pan_requested_offset =
                Some(clamp_scroll_offset(requested_offset, maximum_offset));
        }
        let autoscroll_frame = update_autoscroll(
            ui.ctx(),
            &mut tab.view,
            output.inner_rect,
            &excluded_rects,
            ui.layer_id(),
            AutoscrollOffsets {
                current: output.state.offset,
                maximum: maximum_offset,
            },
            viewport.primary_interaction_in_progress(),
        );
        let autoscroll_active = autoscroll_frame.is_some();
        if let Some(frame) = autoscroll_frame {
            paint_autoscroll_marker(ui, output.inner_rect, frame.anchor);
        }
        let blank_pan_active = viewport.blank_pan_in_progress();
        let dedicated_cursor_owner = pointer_over_any_rect(ui.ctx(), &excluded_rects);
        if !dedicated_cursor_owner
            && (cursor_target.is_some() || autoscroll_active || blank_pan_active)
        {
            // 注釈エディターは PDF の後に描画される。ここで PDF カーソルを省略すると、ウィジェットが
            // Default を使うエディター領域も対象にできる。
            set_pdf_cursor(
                ui.ctx(),
                pdf_cursor_icon(cursor_target, autoscroll_active, blank_pan_active),
            );
        }
        if let Some((page_index, start, end)) = completed_drag {
            tab.request_selection(page_index, start, end);
            self.status = "Resolving selection on the document worker…".to_owned();
        } else if clear_selection {
            tab.clear_selection();
        }
        if let Some(action) = annotation_action {
            self.handle_annotation_ui_action(index, revision, action, ui.ctx());
        }
    }

    fn single_page_view(&mut self, ui: &mut egui::Ui, side: SplitSide, index: usize) {
        let document_id = self.documents[index].document_id;
        let suppress_annotation_hover = self.documents[index].view.autoscroll.is_some()
            || self
                .annotation_editor
                .as_ref()
                .is_some_and(|editor| editor.document_id == document_id)
            || self
                .annotation_picker
                .as_ref()
                .is_some_and(|picker| picker.document_id == document_id);
        let editor_input_rect = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.document_id == document_id)
            .map(|_| annotation_overlay_rect(ui.max_rect()));
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport = self.viewports.entry(side).or_default();
        let gpu_lru = &mut self.gpu_lru;
        let tab = &mut self.documents[index];
        let info = tab.info.as_ref().expect("checked before drawing");
        let path = info.path.clone();
        let revision = info.revision;
        let page_bounds = info.page_bounds.clone();
        let page_count = page_bounds.len();
        let page_index = tab.view.current_page;
        let bounds = page_bounds[page_index];
        let can_create_highlight = info.highlight_capability.is_allowed() && !tab.print_in_flight;
        let viewport_size = ui.available_size();
        let geometry = single_page_geometry(bounds, tab.view.zoom, viewport_size);
        let content_size = geometry.content_size;
        let page_content_rect = geometry.page_rect;
        let restored_offset = tab.view.restore_single_anchor.take().map(|anchor| {
            single_page_centered_offset(page_content_rect, anchor, viewport_size, content_size)
        });
        let maximum_offset = (content_size - viewport_size).max(Vec2::ZERO);
        let scroll_id = scroll_area_state_id(ui, ("single-pdf", &path));
        let stored_offset = egui::scroll_area::State::load(ui.ctx(), scroll_id)
            .map(|state| state.offset)
            .unwrap_or(Vec2::ZERO);
        let starting_offset =
            clamp_scroll_offset(restored_offset.unwrap_or(stored_offset), maximum_offset);
        let was_at_top = starting_offset.y <= SINGLE_PAGE_EDGE_TOLERANCE_POINTS;
        let was_at_bottom =
            starting_offset.y >= maximum_offset.y - SINGLE_PAGE_EDGE_TOLERANCE_POINTS;
        let page_fits_vertically = maximum_offset.y <= SINGLE_PAGE_EDGE_TOLERANCE_POINTS;
        let pan_offset = tab.view.pan_requested_offset.take();
        let mut interaction = PageInteraction::default();
        let mut page_screen_rect = None;
        let mut scroll_area = egui::ScrollArea::both()
            .id_salt(("single-pdf", &path))
            .auto_shrink([false, false]);
        if let Some(offset) = restored_offset.or(pan_offset) {
            scroll_area = scroll_area.scroll_offset(offset);
        }
        let output = scroll_area.show_viewport(ui, |ui, visible_viewport| {
            ui.set_min_size(content_size);
            tab.prepare_text_snapshots(std::iter::once(page_index), revision);
            tab.prepare_annotation_pages(std::iter::once(page_index), revision);
            let screen_rect = Rect::from_min_size(
                ui.max_rect().min + page_content_rect.min.to_vec2(),
                page_content_rect.size(),
            );
            page_screen_rect = Some(screen_rect);
            let requests = single_page_tile_requests(
                tab,
                &page_bounds,
                page_index,
                page_content_rect,
                visible_viewport,
                viewport_size,
                pixels_per_point,
            );
            tab.prepare_tiles(requests);
            paint_page_tiles(ui, screen_rect, page_index, tab, gpu_lru);
            if let Some(matches) = tab.search.pages.get(&page_index) {
                let selected_match = tab
                    .search
                    .selected
                    .filter(|cursor| cursor.page_index == page_index)
                    .map(|cursor| cursor.match_index);
                PageViewport::paint_search_matches(
                    ui,
                    screen_rect,
                    bounds,
                    matches,
                    selected_match,
                );
            }
            interaction = ui
                .scope_builder(UiBuilder::new().max_rect(screen_rect), |page_ui| {
                    let text_key = TextSnapshotKey {
                        page_index,
                        revision,
                    };
                    let annotation_key = AnnotationPageRequest {
                        page_index,
                        expected_revision: revision,
                    };
                    viewport.interact_at(
                        page_ui,
                        PageInteractionInput {
                            screen_rect,
                            page_index,
                            bounds,
                            text_snapshot: tab.text_snapshots.get(&text_key),
                            selection: tab.selection.as_ref(),
                            annotation_page: tab.annotation_pages.get(&annotation_key),
                            can_create_highlight,
                            suppress_annotation_hover,
                            input_excluded_rect: editor_input_rect,
                        },
                    )
                })
                .inner;
        });

        let viewport_center = output.state.offset + output.inner_rect.size() / 2.0;
        tab.view.single_center_anchor = Some(normalized_page_point(
            page_content_rect,
            viewport_center.to_pos2(),
        ));

        let (raw_events, pointer_position, now) = ui.ctx().input(|input| {
            (
                input.raw.events.clone(),
                input.pointer.hover_pos(),
                input.time,
            )
        });
        let pointer_over_view = pointer_position.is_some_and(|position| {
            output.inner_rect.contains(position)
                && editor_input_rect.is_none_or(|rect| !rect.contains(position))
        });
        let wheel_page_delta = single_page_wheel_steps(
            &raw_events,
            pointer_over_view,
            was_at_top,
            was_at_bottom,
            page_fits_vertically,
            now,
            &mut tab.view.single_wheel,
        );

        let maximum_output_offset =
            (output.content_size - output.inner_rect.size()).max(Vec2::ZERO);
        let excluded_rects = editor_input_rect.into_iter().collect::<Vec<_>>();
        let page_screen_rects = page_screen_rect.into_iter().collect::<Vec<_>>();
        let background_interaction = viewport.interact_background(
            ui,
            output.inner_rect,
            &page_screen_rects,
            &excluded_rects,
            tab.selection.is_some(),
        );
        let cursor_target = interaction
            .cursor_target
            .or(background_interaction.cursor_target);
        let pan_delta = interaction.pan_delta.or(background_interaction.pan_delta);
        if let Some(delta) = pan_delta {
            let requested_offset = output.state.offset - delta;
            tab.view.pan_requested_offset =
                Some(clamp_scroll_offset(requested_offset, maximum_output_offset));
        }
        if wheel_page_delta != 0
            && let Some(target) = adjacent_page_index(page_index, page_count, wheel_page_delta)
        {
            let x = tab.view.single_center_anchor.unwrap_or(Vec2::splat(0.5)).x;
            // 次のページは上端、前のページは下端から入り、ホイールの移動方向を
            // 継続する。
            let y = if wheel_page_delta > 0 { 0.0 } else { 1.0 };
            tab.jump_to_single_page_edge(target, Vec2::new(x, y));
        }

        let blank_pan_active = viewport.blank_pan_in_progress();
        let dedicated_cursor_owner = pointer_over_any_rect(ui.ctx(), &excluded_rects);
        if !dedicated_cursor_owner && (cursor_target.is_some() || blank_pan_active) {
            set_pdf_cursor(
                ui.ctx(),
                pdf_cursor_icon(cursor_target, false, blank_pan_active),
            );
        }

        if let Some((page_index, start, end)) = interaction.completed_drag {
            tab.request_selection(page_index, start, end);
            self.status = "Resolving selection on the document worker…".to_owned();
        } else if interaction.clear_selection || background_interaction.clear_selection {
            tab.clear_selection();
        }
        if let Some(action) = interaction.annotation_action {
            self.handle_annotation_ui_action(index, revision, action, ui.ctx());
        }
    }
}

impl SessionRestoreProgress {
    fn finish_one(&mut self, opened: bool) {
        self.pending = self
            .pending
            .checked_sub(1)
            .expect("a restore result must correspond to one pending tab");
        if opened {
            self.restored += 1;
        } else {
            self.skipped += 1;
        }
    }

    fn status(&self) -> String {
        if self.pending > 0 {
            let completed = self.restored + self.skipped;
            format!("Restoring session: {completed}/{} checked", self.requested)
        } else {
            format!(
                "Restored {} tabs; skipped {} unavailable files",
                self.restored, self.skipped
            )
        }
    }
}

impl DocumentTab {
    fn new(
        document_id: u64,
        path: PathBuf,
        last_selected_sequence: u64,
        restored_view: Option<SessionView>,
    ) -> Self {
        let restoring_from_session = restored_view.is_some();
        Self {
            document_id,
            service: Some(DocumentService::spawn(path)),
            state: DocumentState::Opening,
            last_selected_sequence,
            info: None,
            error: None,
            outline: None,
            outline_requested: false,
            tiles: HashMap::new(),
            pending_tiles: HashMap::new(),
            wanted_tiles: HashSet::new(),
            visible_tiles: HashSet::new(),
            text_snapshots: HashMap::new(),
            pending_text_snapshots: HashSet::new(),
            failed_text_snapshots: HashSet::new(),
            wanted_text_snapshots: HashSet::new(),
            annotation_pages: HashMap::new(),
            pending_annotation_pages: HashSet::new(),
            failed_annotation_pages: HashSet::new(),
            wanted_annotation_pages: HashSet::new(),
            highlight_index: HighlightIndexState::default(),
            pending_highlight_refresh_page: None,
            selection: None,
            selection_generation: 0,
            pending_edits: 0,
            edit_history: Vec::new(),
            undo_in_flight: false,
            save_in_flight: false,
            print_in_flight: false,
            thumbnails: HashMap::new(),
            pending_thumbnails: HashSet::new(),
            failed_thumbnails: HashSet::new(),
            thumbnail_generation: 1,
            search: SearchState::default(),
            page_input: "1".to_owned(),
            page_input_error: None,
            view: restored_view.map_or_else(ViewState::new, ViewState::from_session),
            #[cfg(debug_assertions)]
            render_performance: RenderPerformance::default(),
            restoring_from_session,
            external_candidate: None,
            external_conflict_reported: false,
            reload_in_flight: false,
            saved_as_path: None,
        }
    }

    fn send(&self, command: DocumentCommand) -> bool {
        self.service
            .as_ref()
            .is_some_and(|service| service.send(command))
    }

    fn cancel_render(&self, request: &TileRequest) {
        if let Some(service) = &self.service {
            service.cancel_render(request);
        }
    }

    fn cancel_text_snapshot(&self, key: TextSnapshotKey) {
        if let Some(service) = &self.service {
            service.cancel_text_snapshot(&TextSnapshotRequest {
                page_index: key.page_index,
                expected_revision: key.revision,
            });
        }
    }

    fn is_saving(&self) -> bool {
        document_save_blocks_close(self.save_in_flight)
    }

    fn is_printing(&self) -> bool {
        self.print_in_flight
    }

    fn mark_worker_disconnected(&mut self) {
        // チャネル切断後に完了イベントは届かない。応答待ちのフラグをすべて解除し、
        // クローズと復旧操作を利用可能にする。
        self.cancel_highlight_index_work();
        self.pending_edits = 0;
        self.pending_annotation_pages.clear();
        self.undo_in_flight = false;
        self.save_in_flight = false;
        self.print_in_flight = false;
        if self.state != DocumentState::Error {
            self.error = Some(
                "文書処理が予期せず終了しました。タブを閉じ、PDFを開き直してください。".to_owned(),
            );
            self.state = DocumentState::Error;
        }
        self.service = None;
    }

    fn is_suspendable(&self) -> bool {
        // 印刷中のタブを破棄すると完了受信器も失われ、UI が印刷処理中だと永久に
        // 誤認するためである。
        self.state == DocumentState::ReadyClean
            && !self.has_unsaved_changes()
            && !self.is_printing()
    }

    fn suspend(&mut self) {
        self.cancel_highlight_index_work();
        self.invalidate_rendering();
        self.invalidate_text_snapshots();
        self.invalidate_annotation_pages();
        self.tiles.clear();
        self.thumbnail_generation = self.thumbnail_generation.wrapping_add(1);
        self.pending_thumbnails.clear();
        self.failed_thumbnails.clear();
        self.thumbnails.clear();
        self.search.generation = self.search.generation.wrapping_add(1);
        let _queued = self.send(DocumentCommand::SetSearchGeneration(self.search.generation));
        self.search.in_progress = false;
        self.service = None;
        self.state = DocumentState::Suspended;
    }

    fn resume(&mut self, path: PathBuf) {
        let expected_version = self
            .info
            .as_ref()
            .expect("only an opened clean document can be suspended")
            .version;
        self.service = Some(DocumentService::resume(path, expected_version));
        self.state = DocumentState::Opening;
        self.invalidate_rendering();
        // 再開したワーカーは generation 0 で開始する。キャッシュ済みインデックスの継続が
        // キューに届く前に、保持していたタブのトークンを設定する。
        self.reconnect_highlight_index();
    }

    fn set_zoom(&mut self, zoom: f32, mode: ZoomMode) {
        let scale_changed = self.view.zoom.to_bits() != zoom.to_bits();
        if !scale_changed {
            self.view.zoom_mode = mode;
            return;
        }
        match self.view.display_mode {
            DisplayMode::Continuous => self.view.restore_anchor = self.view.center_anchor,
            DisplayMode::SinglePage => {
                self.view.restore_single_anchor = self.view.single_center_anchor
            }
        }
        self.view.zoom = zoom;
        self.view.zoom_mode = mode;
        #[cfg(debug_assertions)]
        let canceled_requests = self.invalidate_rendering();
        #[cfg(not(debug_assertions))]
        self.invalidate_rendering();
        #[cfg(debug_assertions)]
        self.render_performance
            .begin_zoom(zoom, canceled_requests, Instant::now());
    }

    fn set_display_mode(&mut self, mode: DisplayMode) {
        if self.view.switch_display_mode(mode) {
            // 表示モードごとに ScrollArea の座標系が異なるため、古いモードのアンカーを
            // 安全に再開できない。
            self.view.stop_autoscroll();
            self.invalidate_rendering();
        }
    }

    fn prepare_for_pane_transition(&mut self) {
        self.view.stop_autoscroll();
        // ScrollArea の永続IDはペイン階層を含む。所属変更後の新しいIDへ、文書が
        // 最後に描画した中央アンカーを明示的に引き継ぐ。
        match self.view.display_mode {
            DisplayMode::Continuous => self.view.restore_anchor = self.view.center_anchor,
            DisplayMode::SinglePage => {
                self.view.restore_single_anchor = self.view.single_center_anchor
            }
        }
    }

    fn jump_to_page(&mut self, page_index: usize) {
        if !self.set_page_index(page_index) {
            return;
        }
        match self.view.display_mode {
            DisplayMode::Continuous => {
                self.view.scroll_to_page = Some(page_index);
                self.view.restore_anchor = None;
            }
            DisplayMode::SinglePage => {
                let center = Vec2::splat(0.5);
                self.view.single_center_anchor = Some(center);
                self.view.restore_single_anchor = Some(center);
            }
        }
    }

    fn jump_to_single_page_edge(&mut self, page_index: usize, anchor: Vec2) {
        if !self.set_page_index(page_index) {
            return;
        }
        self.view.single_center_anchor = Some(anchor);
        self.view.restore_single_anchor = Some(anchor);
    }

    fn jump_to_search_match(&mut self, anchor: PageAnchor) {
        if !self.set_page_index(anchor.page_index) {
            return;
        }
        match self.view.display_mode {
            DisplayMode::Continuous => {
                // ページ上端へのスクロールではページ下部のヒットが隠れる。保留中の中央
                // アンカー経路を再利用し、ヒット全体を表示したままにする。
                self.view.scroll_to_page = None;
                self.view.restore_anchor = Some(anchor);
            }
            DisplayMode::SinglePage => {
                let center = Vec2::new(anchor.page_x_fraction, anchor.page_y_fraction);
                self.view.single_center_anchor = Some(center);
                self.view.restore_single_anchor = Some(center);
            }
        }
    }

    /// 範囲を確認し共有ページ入力状態を更新してから、呼び出し側が表示モード固有の
    /// 復元位置を設定できるようにする。
    fn set_page_index(&mut self, page_index: usize) -> bool {
        let page_count = self.info.as_ref().map_or(0, |info| info.page_bounds.len());
        if page_index >= page_count {
            return false;
        }
        #[cfg(debug_assertions)]
        if page_index != self.view.current_page {
            self.render_performance
                .begin_page_transition(page_index, Instant::now());
        }
        self.page_input_error = None;
        self.view.current_page = page_index;
        true
    }

    fn request_thumbnail(&mut self, page_index: usize, key: ThumbnailCacheKey) {
        if self.thumbnails.contains_key(&key)
            || self.pending_thumbnails.contains(&key)
            || self.failed_thumbnails.contains(&key)
        {
            return;
        }
        let request = ThumbnailRequest {
            page_index,
            max_pixel_width: key.max_pixel_width,
            max_pixel_height: key.max_pixel_height,
            generation: self.thumbnail_generation,
            expected_revision: key.revision,
        };
        if self.send(DocumentCommand::LoadThumbnail(request)) {
            self.pending_thumbnails.insert(key);
        }
    }

    fn has_unsaved_changes(&self) -> bool {
        self.state == DocumentState::ReadyDirty
            || self.state == DocumentState::Saving
            || self.pending_edits > 0
            || self.info.as_ref().is_some_and(|info| info.dirty)
    }

    fn invalidate_thumbnails(&mut self) -> Vec<ThumbnailCacheKey> {
        self.thumbnail_generation = self.thumbnail_generation.wrapping_add(1);
        self.pending_thumbnails.clear();
        self.failed_thumbnails.clear();
        let keys = self.thumbnails.keys().copied().collect();
        self.thumbnails.clear();
        keys
    }

    fn invalidate_rendering(&mut self) -> usize {
        self.view.generation = self.view.generation.wrapping_add(1);
        self.cancel_rendering_requests()
    }

    fn cancel_rendering_requests(&mut self) -> usize {
        let pending = self
            .pending_tiles
            .drain()
            .map(|(_, request)| request)
            .collect::<Vec<_>>();
        let canceled_count = pending.len();
        for request in pending {
            self.cancel_render(&request);
        }
        self.wanted_tiles.clear();
        self.visible_tiles.clear();
        canceled_count
    }

    fn invalidate_text_snapshots(&mut self) {
        self.text_snapshots.clear();
        let pending = self.pending_text_snapshots.drain().collect::<Vec<_>>();
        for key in pending {
            self.cancel_text_snapshot(key);
        }
        self.failed_text_snapshots.clear();
        self.wanted_text_snapshots.clear();
    }

    fn invalidate_annotation_pages(&mut self) {
        self.annotation_pages.clear();
        self.pending_annotation_pages.clear();
        self.failed_annotation_pages.clear();
        self.wanted_annotation_pages.clear();
    }

    /// サイドバーが初めて表示された後に、そのタブの Highlight スキャンを開始する。
    fn start_highlight_index(&mut self) {
        if !self.highlight_index.started {
            let Some(info) = &self.info else {
                return;
            };
            self.highlight_index.started = true;
            self.highlight_index.revision = Some(info.revision);
            self.highlight_index.total_pages = info.page_bounds.len();
            if !self.send(DocumentCommand::SetHighlightIndexGeneration(
                self.highlight_index.generation,
            )) {
                self.highlight_index.error =
                    Some("ハイライト一覧の読み込みを開始できませんでした。".to_owned());
                return;
            }
        }
        self.queue_next_highlight_index_batch();
    }

    /// xref が変化した可能性があるとき、古いバッチをすべて取り消してインデックスを再構築する。
    fn reset_highlight_index(&mut self, revision: u64, total_pages: usize) {
        if !self.highlight_index.started {
            return;
        }
        self.highlight_index.generation = self.highlight_index.generation.wrapping_add(1);
        self.highlight_index.revision = Some(revision);
        self.highlight_index.total_pages = total_pages;
        self.highlight_index.pages.clear();
        self.highlight_index.in_flight = None;
        self.highlight_index.refresh_page = None;
        self.highlight_index.error = None;
        let _queued = self.send(DocumentCommand::SetHighlightIndexGeneration(
            self.highlight_index.generation,
        ));
        self.queue_next_highlight_index_batch();
    }

    /// 同じ文書の完了済みページを保持しつつ、編集された 1 ページを更新する。
    fn refresh_highlight_index_page(
        &mut self,
        page_index: usize,
        revision: u64,
        total_pages: usize,
    ) {
        if !self.highlight_index.started {
            return;
        }
        self.highlight_index.generation = self.highlight_index.generation.wrapping_add(1);
        self.highlight_index.revision = Some(revision);
        self.highlight_index.total_pages = total_pages;
        self.highlight_index
            .pages
            .retain(|indexed_page, _| *indexed_page < total_pages);
        self.highlight_index.pages.remove(&page_index);
        self.highlight_index.in_flight = None;
        self.highlight_index.refresh_page = (page_index < total_pages).then_some(page_index);
        self.highlight_index.error = None;
        let _queued = self.send(DocumentCommand::SetHighlightIndexGeneration(
            self.highlight_index.generation,
        ));
        self.queue_next_highlight_index_batch();
    }

    fn cancel_highlight_index_work(&mut self) {
        if !self.highlight_index.started {
            return;
        }
        self.highlight_index.generation = self.highlight_index.generation.wrapping_add(1);
        self.highlight_index.in_flight = None;
        let _queued = self.send(DocumentCommand::SetHighlightIndexGeneration(
            self.highlight_index.generation,
        ));
    }

    fn reconnect_highlight_index(&mut self) {
        if !self.highlight_index.started {
            return;
        }
        if !self.send(DocumentCommand::SetHighlightIndexGeneration(
            self.highlight_index.generation,
        )) {
            self.highlight_index.error =
                Some("ハイライト一覧の読み込みを再開できませんでした。".to_owned());
            return;
        }
        self.highlight_index.error = None;
        self.queue_next_highlight_index_batch();
    }

    fn queue_next_highlight_index_batch(&mut self) {
        let Some(request) = next_highlight_index_request(&self.highlight_index) else {
            return;
        };

        if self.send(DocumentCommand::LoadHighlightIndexBatch(request)) {
            self.highlight_index.in_flight = Some(request);
        } else {
            self.highlight_index.error = Some("ハイライト一覧を読み込めませんでした。".to_owned());
        }
    }

    fn receive_highlight_index_batch(&mut self, batch: HighlightIndexBatch) {
        if !apply_highlight_index_batch(&mut self.highlight_index, batch) {
            return;
        }
        self.queue_next_highlight_index_batch();
    }

    fn prepare_text_snapshots(&mut self, page_indices: impl Iterator<Item = usize>, revision: u64) {
        let wanted = page_indices
            .map(|page_index| TextSnapshotKey {
                page_index,
                revision,
            })
            .collect::<HashSet<_>>();
        let canceled = self
            .pending_text_snapshots
            .iter()
            .filter(|key| !wanted.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in canceled {
            self.pending_text_snapshots.remove(&key);
            self.cancel_text_snapshot(key);
        }
        self.text_snapshots.retain(|key, _| wanted.contains(key));
        retain_visible_text_failures(&mut self.failed_text_snapshots, &mut self.error, &wanted);
        self.wanted_text_snapshots = wanted;

        let requests = self
            .wanted_text_snapshots
            .iter()
            .filter(|key| {
                !self.text_snapshots.contains_key(key)
                    && !self.pending_text_snapshots.contains(key)
                    && !self.failed_text_snapshots.contains(key)
            })
            .copied()
            .collect::<Vec<_>>();
        for key in requests {
            let request = TextSnapshotRequest {
                page_index: key.page_index,
                expected_revision: key.revision,
            };
            if self.send(DocumentCommand::LoadTextSnapshot(request)) {
                self.pending_text_snapshots.insert(key);
            }
        }
    }

    fn prepare_annotation_pages(
        &mut self,
        page_indices: impl Iterator<Item = usize>,
        revision: u64,
    ) {
        let wanted = page_indices
            .map(|page_index| AnnotationPageRequest {
                page_index,
                expected_revision: revision,
            })
            .collect::<HashSet<_>>();
        self.annotation_pages
            .retain(|request, _| wanted.contains(request));
        self.failed_annotation_pages
            .retain(|request| wanted.contains(request));
        self.wanted_annotation_pages = wanted;

        // 表示中に失敗したページは要求対象集合から離れて再び入るまでブロックする。
        // 保留中の古いページは revision 確認でワーカー応答を拒否するため無害である。
        let requests = self
            .wanted_annotation_pages
            .iter()
            .filter(|request| {
                !self.annotation_pages.contains_key(request)
                    && !self.pending_annotation_pages.contains(request)
                    && !self.failed_annotation_pages.contains(request)
            })
            .copied()
            .collect::<Vec<_>>();
        for request in requests {
            if self.send(DocumentCommand::LoadAnnotations(request)) {
                self.pending_annotation_pages.insert(request);
            }
        }
    }

    fn prepare_tiles(&mut self, requests: Vec<TileRequest>) {
        let wanted_tiles = requests
            .iter()
            .map(|request| TileCacheKey::from_request(self.document_id, request))
            .collect::<HashSet<_>>();
        let visible_tiles = requests
            .iter()
            .filter(|request| request.priority == RenderPriority::Visible)
            .map(|request| TileCacheKey::from_request(self.document_id, request))
            .collect::<HashSet<_>>();
        let canceled_keys = self
            .pending_tiles
            .keys()
            .filter(|key| !wanted_tiles.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in canceled_keys {
            if let Some(request) = self.pending_tiles.remove(&key) {
                self.cancel_render(&request);
            }
        }
        self.wanted_tiles = wanted_tiles;
        self.visible_tiles = visible_tiles;

        #[cfg(debug_assertions)]
        {
            let cache_hit = self
                .visible_tiles
                .iter()
                .any(|key| self.tiles.contains_key(key));
            let prefetch_used = self.visible_tiles.iter().any(|key| {
                self.tiles
                    .get(key)
                    .is_some_and(|cached| cached.was_prefetched)
            });
            self.render_performance.note_page_cache_state(
                self.view.current_page,
                cache_hit,
                prefetch_used,
            );
        }

        for request in requests {
            let key = TileCacheKey::from_request(self.document_id, &request);
            if self.tiles.contains_key(&key) {
                continue;
            }
            // enum の小さい順位ほど優先度が高い。同じタイルがより緊急なビューポート
            // クラスへ移った場合だけ再キューする。
            if let Some(pending) = self.pending_tiles.get(&key)
                && request.priority >= pending.priority
            {
                continue;
            }
            let queued = self.send(DocumentCommand::RenderTile(request));
            if queued {
                self.pending_tiles.insert(key, request);
            }
        }
    }

    fn request_selection(&mut self, page_index: usize, start: PagePoint, end: PagePoint) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        let _queued = self.send(DocumentCommand::Select {
            page_index,
            generation: self.selection_generation,
            start,
            end,
        });
    }

    fn clear_selection(&mut self) {
        // generation を進め、以前のドラッグに対するワーカー結果が明示的なクリック消去後に
        // 選択を復元するのを防ぐ。
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection = None;
    }
}

#[cfg(debug_assertions)]
impl RenderPerformance {
    fn begin_page_transition(&mut self, target_page: usize, now: Instant) {
        self.page_transition = Some(PageTransitionPerformance {
            target_page,
            input_at: now,
            first_exact_tile: None,
            full_exact_viewport: None,
            cache_hit: None,
            prefetch_used: None,
        });
    }

    fn note_page_cache_state(&mut self, page_index: usize, cache_hit: bool, prefetch_used: bool) {
        let Some(measurement) = self
            .page_transition
            .as_mut()
            .filter(|measurement| measurement.target_page == page_index)
        else {
            return;
        };
        if measurement.cache_hit.is_none() {
            measurement.cache_hit = Some(cache_hit);
            measurement.prefetch_used = Some(prefetch_used);
        }
    }

    fn begin_zoom(&mut self, target_zoom: f32, canceled_requests: usize, now: Instant) {
        let continuing_gesture = self.zoom.as_ref().is_some_and(|measurement| {
            now.saturating_duration_since(measurement.last_input_at)
                .as_secs_f64()
                <= ZOOM_INPUT_GROUP_IDLE_SECONDS
        });
        let discarded_intermediate_requests = if continuing_gesture {
            self.zoom
                .as_ref()
                .map_or(0, |measurement| measurement.discarded_intermediate_requests)
                .saturating_add(canceled_requests)
        } else {
            // 安定した古いズームへの要求は「中間」処理ではない。同じジェスチャーの後続入力が
            // あった場合だけ、以前の対象が古くなる。
            0
        };
        self.zoom = Some(ZoomPerformance {
            target_zoom,
            last_input_at: now,
            provisional_display: None,
            first_exact_tile: None,
            full_exact_viewport: None,
            discarded_intermediate_requests,
        });
    }

    fn note_paint(
        &mut self,
        page_index: usize,
        zoom: f32,
        provisional_painted: bool,
        exact_visible_painted: bool,
        exact_visible_complete: bool,
        now: Instant,
    ) {
        if let Some(measurement) = self
            .page_transition
            .as_mut()
            .filter(|measurement| measurement.target_page == page_index)
        {
            let elapsed = now.saturating_duration_since(measurement.input_at);
            if exact_visible_painted {
                measurement.first_exact_tile.get_or_insert(elapsed);
            }
            if exact_visible_complete {
                measurement.full_exact_viewport.get_or_insert(elapsed);
            }
        }

        if let Some(measurement) = self
            .zoom
            .as_mut()
            .filter(|measurement| measurement.target_zoom.to_bits() == zoom.to_bits())
        {
            let elapsed = now.saturating_duration_since(measurement.last_input_at);
            if provisional_painted {
                measurement.provisional_display.get_or_insert(elapsed);
            }
            if exact_visible_painted {
                measurement.first_exact_tile.get_or_insert(elapsed);
            }
            if exact_visible_complete {
                measurement.full_exact_viewport.get_or_insert(elapsed);
            }
        }
    }
}

impl eframe::App for PrototypeApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if !context.input(|input| input.focused) {
            // ネイティブのフォーカス喪失では対応するボタン解放イベントが省略されることがある。
            self.stop_visible_autoscroll();
            self.cancel_all_viewport_interactions();
            self.copy_shortcut_active = false;
        }
        let modal_open = self.close_confirmation.is_some() || self.session_close_failure.is_some();
        if modal_open {
            // モーダルが閉じるまでポインターの意図を所有する。背景の autoscroll アンカーを
            // 保持するとダイアログの下で文書が動いてしまう。
            self.stop_visible_autoscroll();
            self.cancel_all_viewport_interactions();
        }
        self.receive_document_events(context);
        self.check_external_changes();
        self.maybe_suspend_inactive_document();
        let external_request_received = self.receive_external_open_events(context);
        self.handle_dropped_files(context);
        self.handle_shortcuts(context);
        self.handle_window_close(context, external_request_received);
        context.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.menu_bar(ui);
        let tab_bar = self.tab_bar(ui);
        self.toolbar(ui);
        self.error_banner(ui);
        self.status_panel(ui);
        self.sidebar_panel(ui);
        let central_rect = self.central_panel(ui, tab_bar.as_ref());
        self.annotation_candidate_picker(ui.ctx());
        self.annotation_editor_overlay(ui.ctx(), central_rect);
        self.close_confirmation_dialog(ui.ctx());
        self.session_close_failure_dialog(ui.ctx());
    }
}

/// 最も長く未使用で完全にサスペンド可能な文書を選び、表示中タブはすべて除外する。
fn oldest_suspendable_index(
    visible_indices: &[usize],
    candidates: &[(bool, u64)],
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(index, (is_suspendable, _))| !visible_indices.contains(index) && *is_suspendable)
        .min_by_key(|(_, (_, last_selected))| *last_selected)
        .map(|(index, _)| index)
}

/// 内部ワーカー操作タグを、リリース UI で案内できるメッセージへ変換する。
fn document_failure_message(operation: &str, detail: &str) -> String {
    // バックエンドの詳細は診断用に保持するが、先頭の要約では失敗内容とユーザーが
    // 次にできることを常に日本語で説明する。
    let guidance = match operation {
        "open" => "PDFを開けませんでした。ファイルが破損していないか確認してください。",
        "resume" | "document-info" => {
            "PDFを再開できませんでした。ファイルの変更を確認し、タブを開き直してください。"
        }
        "render" => "ページを表示できませんでした。PDFを開き直してください。",
        "selection" => "テキストを選択できませんでした。選択し直してください。",
        "highlight" | "highlight-state" => {
            "Highlightを作成できませんでした。PDFの編集制限を確認してください。"
        }
        "annotation-update" | "annotation-update-state" => {
            "注釈を更新できませんでした。PDFの編集制限と注釈ロックを確認してください。"
        }
        "annotation-delete" | "annotation-delete-state" => {
            "注釈を削除できませんでした。PDFの編集制限と注釈ロックを確認してください。"
        }
        "undo" | "undo-state" => {
            "編集を元に戻せませんでした。PDFを開き直して状態を確認してください。"
        }
        "outline" => "目次を読み込めませんでした。PDFを開き直してください。",
        "search" => "PDF内を検索できませんでした。検索語を確認して再実行してください。",
        "print" => "PDFを印刷できませんでした。プリンターの状態と設定を確認してください。",
        "save" => "PDFを保存できませんでした。ファイルの書き込み権限と空き容量を確認してください。",
        _ => "PDFの処理に失敗しました。タブを開き直してください。",
    };
    format!("{guidance} 詳細: {detail}")
}

fn document_save_blocks_close(save_in_flight: bool) -> bool {
    save_in_flight
}

/// `dirty` な Highlight イベントが、すでにキューされた Save の完了に先行することがある。
fn state_after_document_info(current: DocumentState, dirty: bool) -> DocumentState {
    if dirty && current == DocumentState::Saving {
        DocumentState::Saving
    } else if dirty {
        DocumentState::ReadyDirty
    } else {
        DocumentState::ReadyClean
    }
}

/// 同一フォルダだけを走査し、元の file identity と一致する候補を名前変更として扱う。
/// 元パスへ新しい PDF が作られた場合は identity が異なるため、この経路で誤認しない。
fn find_same_folder_rename(path: &Path, expected: DocumentVersion) -> Option<PathBuf> {
    let parent = path.parent()?;
    std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let candidate = entry.path();
            let version = read_document_version(&candidate).ok()?;
            (version.identity_primary == expected.identity_primary
                && version.identity_secondary == expected.identity_secondary)
                .then(|| candidate)
        })
}

fn text_snapshot_result_is_current(
    is_visible: bool,
    key: TextSnapshotKey,
    current_revision: Option<u64>,
    page_count: usize,
    wanted: &HashSet<TextSnapshotKey>,
) -> bool {
    // スクロール、タブ切り替え、注釈変更の後に抽出が完了することがある。UI やエラーに
    // 影響できるのは完全に一致する表示中の文書状態だけである。
    is_visible
        && current_revision == Some(key.revision)
        && key.page_index < page_count
        && wanted.contains(&key)
}

fn annotation_page_result_is_current(
    is_visible: bool,
    request: AnnotationPageRequest,
    current_revision: Option<u64>,
    wanted: &HashSet<AnnotationPageRequest>,
) -> bool {
    // 注釈 xref は文書内で可変な同一性である。非表示タブ、古い revision、表示外の
    // ページからの結果を現在の UI の編集対象にしてはならない。
    is_visible && current_revision == Some(request.expected_revision) && wanted.contains(&request)
}

fn retain_visible_text_failures(
    failed: &mut HashSet<TextSnapshotKey>,
    error: &mut Option<String>,
    wanted: &HashSet<TextSnapshotKey>,
) {
    failed.retain(|key| wanted.contains(key));
    if failed.is_empty()
        && error
            .as_ref()
            .is_some_and(|message| message.starts_with("text snapshot:"))
    {
        // ページが表示選択範囲を離れた後まで、ページ固有の抽出エラーをタブに残してはならない。
        *error = None;
    }
}

fn mark_thumbnail_failed(
    pending: &mut HashSet<ThumbnailCacheKey>,
    failed: &mut HashSet<ThumbnailCacheKey>,
    key: ThumbnailCacheKey,
) {
    // 失敗したキーはユーザーが明示的に再試行するまでブロックする。他の保留ページは
    // ワーカーコマンドが有効なのでそのまま保持する。
    pending.remove(&key);
    failed.insert(key);
}

/// 表示中の文書状態に属しているラスターだけを受け入れる。
///
/// ワーカーは進行中の MuPDF 描画をキャンセルできないため、結果が GPU テクスチャを
/// 割り当てる前に 4 つの同一性要素をすべて確認する。
fn tile_result_is_current(
    is_visible: bool,
    key: TileCacheKey,
    result_generation: u64,
    current_generation: u64,
    current_revision: Option<u64>,
    wanted_tiles: &HashSet<TileCacheKey>,
) -> bool {
    is_visible
        && result_generation == current_generation
        && current_revision == Some(key.revision)
        && wanted_tiles.contains(&key)
}

/// 検証済み RGBA ピクセルを egui にコピーし、ワーカー転送用の割り当てを解放する。
fn take_rgba_image(pixels_rgba: &mut Vec<u8>, size: [usize; 2]) -> egui::ColorImage {
    // `Vec::clear` では GPU/サムネイルキャッシュ予算全体まで CPU 上に保持し続ける。
    // 割り当てを移動して取り出し、egui のコピー後に破棄できるようにする。
    let transferred_pixels = std::mem::take(pixels_rgba);
    egui::ColorImage::from_rgba_unmultiplied(size, &transferred_pixels)
}

/// 1 始まりのユーザーページ番号を既存の 0 始まりページインデックスへ変換する。
fn page_index_from_input(input: &str, page_count: usize) -> Option<usize> {
    let page_number = input.trim().parse::<usize>().ok()?;
    if page_number > page_count {
        return None;
    }
    page_number.checked_sub(1)
}

fn page_number_input_columns(page_count: usize) -> usize {
    page_count
        .max(1)
        .to_string()
        .len()
        .max(PAGE_INPUT_MINIMUM_COLUMNS)
}

/// 固定高さのクリップ矩形内で `Galley` を中央揃えにするツールバー用 1 行エディターを構築する。
///
/// フォント調整は `Galley` 内の字形を移動する。この整列は TextEdit 内の `Galley` の位置だけを
/// 決めるため、固定字形オフセットを別途加えない。
fn toolbar_singleline_text_edit(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).vertical_align(egui::Align::Center)
}

fn page_number_input_width(ui: &egui::Ui, page_count: usize) -> f32 {
    let sample = "9".repeat(page_number_input_columns(page_count));
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let text_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(sample, font_id, Color32::WHITE)
            .size()
            .x
    });
    let frame_padding = ui.spacing().button_padding.x * 2.0;
    (text_width + frame_padding).max(ui.spacing().interact_size.x + frame_padding)
}

fn page_number_id(document_id: u64) -> Id {
    Id::new(("pdf-page-number", document_id))
}

fn consume_highlight_shortcut(context: &egui::Context, active_text_input: Option<Id>) -> bool {
    if active_text_input.is_some() {
        // 単独の `h` はフォーカス中のエディターに属する。ここで消費すると文字を落とし、
        // 無関係な注釈まで作ってしまう。
        return false;
    }
    context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::H))
}

fn text_edit_has_selection(context: &egui::Context, id: Id) -> bool {
    egui::TextEdit::load_state(context, id)
        .and_then(|state| state.cursor.char_range())
        .is_some_and(|range| !range.is_empty())
}

/// PDF の選択がコマンドを所有するときだけ、プラットフォームのコピーイベントを消費する。
fn consume_pdf_copy_event(
    context: &egui::Context,
    shortcut_active: &mut bool,
    text_input_owns_copy: bool,
    pdf_selection_available: bool,
) -> bool {
    context.input_mut(|input| {
        let mut copy_pdf_selection = false;
        let mut retained_events = Vec::with_capacity(input.events.len());
        for event in input.events.drain(..) {
            match event {
                Event::Copy => {
                    let first_copy_event = !*shortcut_active;
                    *shortcut_active = true;
                    if text_input_owns_copy {
                        // PDF 選択を検討する前に TextEdit 自身の選択範囲がプラットフォームの
                        // クリップボードへ届くよう、TextEdit 用の Event::Copy は保持する。
                        retained_events.push(Event::Copy);
                    } else if first_copy_event && pdf_selection_available {
                        copy_pdf_selection = true;
                    }
                }
                Event::Key {
                    key: Key::C | Key::Insert | Key::Copy,
                    pressed: false,
                    ..
                } => {
                    // egui-winit は Ctrl+Insert と専用 Copy キーも Event::Copy に変換するため、
                    // それらの解放で同じラッチを再有効化する。
                    *shortcut_active = false;
                    retained_events.push(event);
                }
                _ => retained_events.push(event),
            }
        }
        input.events = retained_events;
        copy_pdf_selection
    })
}

fn is_pdf_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

#[cfg(debug_assertions)]
fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(debug_assertions)]
fn debug_duration(duration: Option<Duration>) -> String {
    duration.map_or_else(
        || "pending".to_owned(),
        |duration| format!("{:.1} ms", milliseconds(duration)),
    )
}

#[cfg(debug_assertions)]
fn debug_yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "pending",
    }
}

#[cfg(debug_assertions)]
fn format_memory(bytes: usize) -> String {
    format!("resident memory: {:.1} MiB", bytes as f64 / 1_048_576.0)
}
