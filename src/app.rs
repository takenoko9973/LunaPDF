use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
#[cfg(debug_assertions)]
use std::time::Instant;

use crossbeam_channel::TryRecvError;
use eframe::egui::{
    self, Color32, CursorIcon, Event, Id, Key, LayerId, Modifiers, MouseWheelUnit, PointerButton,
    Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, TouchPhase, UiBuilder,
    Vec2, ViewportCommand,
};

use crate::domain::annotation::{
    AnnotationDeleteRequest, AnnotationId, AnnotationPageRequest, AnnotationPageSnapshot,
    AnnotationSummary, HighlightIndexBatch, HighlightIndexRequest,
};
use crate::domain::document::{
    DocumentInfo, EditAction, HighlightRequest, OutlineItem, RenderPriority, RenderedThumbnail,
    RenderedTile, SearchMatch, SearchPageResult, ThumbnailRequest, TileRequest, TileSpec,
};
use crate::domain::selection::{
    PagePoint, SelectionSnapshot, TextPageSnapshot, TextSnapshotRequest,
};
use crate::domain::session::{
    DisplayMode as SessionDisplayMode, SessionState, SessionTab, SessionView,
    SidebarTab as SessionSidebarTab, ZoomMode as SessionZoomMode,
};
use crate::domain::tabs::{OpenTabResult, TabState};
use crate::pdf::{DocumentCommand, DocumentEvent, DocumentService};
use crate::persistence::session_store::SessionStore;
use crate::render::cache::WeightedLruCache;
use crate::render::layout::{ContinuousLayout, PAGE_GAP, PageAnchor};
use crate::render::tiles::TileGrid;
use crate::ui::annotation_editor::{
    AnnotationEditorAction, AnnotationEditorState, AnnotationMenuCandidate, AnnotationUiAction,
    annotation_comment_id, annotation_overlay_rect, show_annotation_candidate_button,
    show_annotation_editor,
};
use crate::ui::fonts::install_cjk_fallback;
use crate::ui::icons::{ToolbarIcon, icon_button};
use crate::ui::sidebar::{HighlightSidebarAction, SidebarTab, show_highlights, show_outline};
use crate::ui::viewport::{
    PageInteraction, PageInteractionInput, PageViewport, screen_rect_for_tile,
};

// These bounds cover detailed inspection and overview use without allowing an
// accidental wheel gesture to request an unbounded raster allocation.
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;

// Fit modes can change by sub-pixel rounding as panel sizes settle. Ignoring a
// difference below one tenth of a percent avoids invalidating every page on
// visually identical consecutive frames.
const ZOOM_CHANGE_EPSILON: f32 = 0.001;

// These values preserve egui's 18-point minimum interaction height, keep a
// 24-point close target and a readable title region at minimum width, and cap
// the former unbounded filename label at a conventional desktop tab width.
const TAB_MIN_WIDTH: f32 = 96.0;
const TAB_MAX_WIDTH: f32 = 240.0;
const TAB_HEIGHT: f32 = 24.0;
const TAB_HORIZONTAL_PADDING: f32 = 8.0;
const TAB_CLOSE_WIDTH: f32 = 24.0;
const TAB_CONTENT_GAP: f32 = 4.0;
const TAB_ITEM_SPACING: f32 = 1.0;

// An 8-point vector X remains legible inside the 24-point close target while
// leaving enough hover fill around it at the minimum tab height.
const TAB_CLOSE_ICON_HALF_SIZE: f32 = 4.0;
const TAB_CLOSE_ICON_STROKE_WIDTH: f32 = 1.5;

// The first three page digits keep a stable toolbar column. Longer documents
// measure all required digits instead of truncating or rejecting page input.
const PAGE_INPUT_MINIMUM_COLUMNS: usize = 3;

// The design budget is shared across all tabs so the active document can use
// available GPU memory instead of dividing a fixed allocation per tab.
const GPU_TILE_BUDGET_BYTES: usize = 192 * 1_024 * 1_024;

// Thumbnails have their own budget so a long sidebar cannot evict the active
// page's display tiles from the 192 MiB rendering cache.
const THUMBNAIL_BUDGET_BYTES: usize = 32 * 1_024 * 1_024;
const THUMBNAIL_MAX_WIDTH: u32 = 160;
const THUMBNAIL_MAX_HEIGHT: u32 = 220;
const THUMBNAIL_ROW_HEIGHT: f32 = 248.0;

// The release performance matrix showed eight pages had the shortest first
// result and cancellation boundary without increasing total scan time.
const HIGHLIGHT_INDEX_BATCH_PAGES: usize = 8;

// High-precision devices emit point deltas rather than discrete wheel steps.
// Twenty-four logical points filters incidental edge motion without delaying a
// deliberate short trackpad gesture by more than a typical line of content.
const TRACKPAD_PAGE_THRESHOLD_POINTS: f32 = 24.0;

// A short idle interval separates trackpad inertia from the next deliberate
// gesture even when the backend never emits an exact zero-delta frame.
const WHEEL_GESTURE_IDLE_SECONDS: f64 = 0.150;

// One logical point absorbs PAGE_GAP and fractional ScrollArea rounding while
// remaining too small to skip visible page content.
const SINGLE_PAGE_EDGE_TOLERANCE_POINTS: f32 = 1.0;

// Browser-style autoscroll should remain still near its anchor. Twelve logical
// points is large enough to tolerate click jitter at ordinary desktop DPI.
const AUTOSCROLL_DEAD_ZONE_POINTS: f32 = 12.0;
const AUTOSCROLL_SPEED_PER_POINT: f32 = 12.0;

// Bound both long pointer excursions and a stalled frame: the former prevents
// uncontrollable jumps, while 100 ms keeps one frame below 480 logical points.
const AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND: f32 = 4_800.0;
const AUTOSCROLL_MAX_FRAME_SECONDS: f32 = 0.100;

const AUTOSCROLL_MARKER_RADIUS_POINTS: f32 = 8.0;

// Inputs no more than 250 ms apart are one continuous zoom gesture for debug
// accounting. This is short enough not to delay rendering the latest scale.
#[cfg(debug_assertions)]
const ZOOM_INPUT_GROUP_IDLE_SECONDS: f64 = 0.250;

// N-05 sets 512 MiB as the stable process target. Suspension is only allowed
// after crossing that limit; ordinary tab switches retain documents.
const RESIDENT_MEMORY_SUSPEND_THRESHOLD_BYTES: usize = 512 * 1_024 * 1_024;

pub(crate) struct PrototypeApp {
    tabs: TabState,
    documents: Vec<DocumentTab>,
    viewport: PageViewport,
    status: String,
    error: Option<String>,
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
    next_document_id: u64,
    activity_sequence: u64,
    // A reveal request is consumed by the next tab-bar frame. Keeping it
    // one-shot lets users scroll away afterward to inspect inactive tabs.
    tab_to_reveal: Option<usize>,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    gpu_lru: WeightedLruCache<TileCacheKey, ()>,
    thumbnail_lru: WeightedLruCache<ThumbnailCacheKey, ()>,
    annotation_editor: Option<AnnotationEditorState>,
    annotation_picker: Option<AnnotationPickerState>,
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
    select_after_restore: bool,
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

/// Replaces page rows only when one response matches the exact outstanding request.
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

#[derive(Default)]
struct SearchState {
    query: String,
    generation: u64,
    pages: BTreeMap<usize, Vec<SearchMatch>>,
    selected: Option<SearchCursor>,
    completed_pages: usize,
    truncated: bool,
    in_progress: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchCursor {
    page_index: usize,
    match_index: usize,
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
    Restored {
        view: SessionView,
        select_after_open: bool,
    },
}

#[derive(Clone, Copy)]
enum OpenDocumentResult {
    Pending,
    Existing(usize),
}

struct SessionRestoreProgress {
    requested: usize,
    pending: usize,
    restored: usize,
    skipped: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayMode {
    Continuous,
    SinglePage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ZoomMode {
    Fixed,
    FitWidth,
    FitPage,
}

struct ViewState {
    display_mode: DisplayMode,
    zoom_mode: ZoomMode,
    zoom: f32,
    current_page: usize,
    scroll_to_page: Option<usize>,
    center_anchor: Option<PageAnchor>,
    restore_anchor: Option<PageAnchor>,
    single_center_anchor: Option<Vec2>,
    restore_single_anchor: Option<Vec2>,
    single_wheel: SinglePageWheelState,
    autoscroll: Option<AutoscrollState>,
    pan_requested_offset: Option<Vec2>,
    render_pixels_per_point_bits: Option<u32>,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct SinglePageWheelState {
    accumulated_points: f32,
    latched: bool,
    direction: f32,
    last_input_time: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct AutoscrollState {
    anchor: Pos2,
    requested_offset: Option<Vec2>,
}

#[derive(Clone, Copy, Debug)]
struct SinglePageGeometry {
    content_size: Vec2,
    page_rect: Rect,
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

struct TabPaintState<'a> {
    selected: bool,
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

/// Returns one equal tab width, including the transition to horizontal scroll.
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

/// Reserves the close target before assigning the remaining tab width to text.
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

    // The close region shares the tab background; a local fill appears only on
    // hover so it remains discoverable without recreating the old boxed button.
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
    /// Creates the application and opens each command-line PDF.
    pub(crate) fn new(
        creation_context: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        session_store: SessionStore,
    ) -> Self {
        install_cjk_fallback(&creation_context.egui_ctx);
        egui_extras::install_image_loaders(&creation_context.egui_ctx);
        Self::from_startup(paths, session_store)
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
        let mut app = Self {
            tabs: TabState::new(),
            documents: Vec::new(),
            viewport: PageViewport::default(),
            status: "Drop a PDF into the window to open it".to_owned(),
            error: session_load_error,
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
            next_document_id: 1,
            activity_sequence: 0,
            tab_to_reveal: None,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Outline,
            gpu_lru: WeightedLruCache::new(GPU_TILE_BUDGET_BYTES),
            thumbnail_lru: WeightedLruCache::new(THUMBNAIL_BUDGET_BYTES),
            annotation_editor: None,
            annotation_picker: None,
        };
        if paths.is_empty() && restore_enabled {
            if let Some(session) = saved_session {
                app.restore_session(session);
            }
        } else {
            // Explicit command-line files take precedence over session restore.
            for path in paths {
                app.open_document(path);
            }
        }
        app
    }

    fn open_document(&mut self, path: PathBuf) {
        let _opened_index = self.open_document_with_intent(path, OpenIntent::User);
    }

    /// Opens the native picker and forwards only an explicitly chosen PDF.
    fn pick_pdf_and_open(&mut self) {
        // The native picker temporarily replaces the PDF interaction surface,
        // so an anchor must not resume when the dialog returns.
        self.stop_active_autoscroll();
        let selected = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file();
        if let Some(path) = selected {
            self.open_document(path);
        }
    }

    fn stop_active_autoscroll(&mut self) {
        if let Some(index) = self.active_index() {
            self.documents[index].view.stop_autoscroll();
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
        let saved_selection = session.selected_tab;
        let mut restored_selection = None;
        let mut pending_count = 0;
        let mut restored_count = 0;
        let mut skipped_count = 0;
        for (saved_index, tab) in session.tabs.into_iter().enumerate() {
            let should_select = saved_selection == Some(saved_index);
            let opened = self.open_document_with_intent(
                tab.path,
                OpenIntent::Restored {
                    view: tab.view,
                    select_after_open: should_select,
                },
            );
            match opened {
                Some(OpenDocumentResult::Pending) => pending_count += 1,
                Some(OpenDocumentResult::Existing(index)) if should_select => {
                    restored_selection = Some(index);
                    restored_count += 1;
                }
                Some(OpenDocumentResult::Existing(_)) => restored_count += 1,
                None => skipped_count += 1,
            }
        }

        if let Some(index) = restored_selection {
            self.select_tab(index);
        }
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
        let (restored_view, select_after_restore) = match intent {
            OpenIntent::User => (None, false),
            OpenIntent::Restored {
                view,
                select_after_open,
            } => (Some(view), select_after_open),
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
                    select_after_restore,
                ));
                self.activate_document(index, previously_active);
                if report_to_user {
                    self.status = format!("Opening {}…", path.display());
                    self.error = None;
                }
                Some(OpenDocumentResult::Pending)
            }
            Ok(OpenTabResult::SelectedExisting(index)) => {
                self.activate_document(index, previously_active);
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

    fn receive_document_events(&mut self, context: &egui::Context) {
        let mut failed_restored_paths = Vec::new();
        for index in 0..self.documents.len() {
            while let Some(event) = self.documents[index]
                .service
                .as_ref()
                .map(DocumentService::try_recv)
            {
                match event {
                    Ok(DocumentEvent::Opened(info)) => {
                        let highlight_index_needs_reset =
                            self.documents[index].highlight_index.started
                                && self.documents[index].highlight_index.revision
                                    != Some(info.revision);
                        let revision = info.revision;
                        let page_count = info.page_bounds.len();
                        let restored_open = self.documents[index].restoring_from_session;
                        let select_after_restore = self.documents[index].select_after_restore;
                        self.documents[index].restoring_from_session = false;
                        self.documents[index].select_after_restore = false;
                        self.status = format!("Opened {}", info.path.display());
                        self.documents[index]
                            .view
                            .clamp_to_page_count(info.page_bounds.len());
                        self.documents[index].state = if info.dirty {
                            DocumentState::ReadyDirty
                        } else {
                            DocumentState::ReadyClean
                        };
                        self.documents[index].error = None;
                        self.documents[index].info = Some(info);
                        if highlight_index_needs_reset {
                            self.documents[index].reset_highlight_index(revision, page_count);
                        } else {
                            self.documents[index].reconnect_highlight_index();
                        }
                        if !self.documents[index].outline_requested
                            && self.documents[index].send(DocumentCommand::LoadOutline)
                        {
                            self.documents[index].outline_requested = true;
                        }
                        if self.active_index() == Some(index)
                            && !self.documents[index].search.query.trim().is_empty()
                        {
                            self.begin_search(index);
                        }
                        if restored_open {
                            if select_after_restore {
                                self.select_tab(index);
                            }
                            self.finish_session_restore(true);
                        }
                    }
                    Ok(DocumentEvent::DocumentChanged(info)) => {
                        let dirty = info.dirty;
                        let revision = info.revision;
                        let page_count = info.page_bounds.len();
                        let saved_path = (!info.dirty).then(|| info.path.clone());
                        let document_id = self.documents[index].document_id;
                        if !info.dirty
                            && self
                                .annotation_editor
                                .as_ref()
                                .is_some_and(|editor| editor.document_id == document_id)
                        {
                            // Save reopens the PDF and begins a new xref/revision
                            // validity interval; even a clean old editor must not survive it.
                            self.annotation_editor = None;
                        }
                        let restart_search = self.active_index() == Some(index)
                            && self.close_confirmation.is_none()
                            && !self.window_close_pending
                            && !self.close_all_pending
                            && !self.documents[index].search.query.trim().is_empty();
                        let tab = &mut self.documents[index];
                        let changed_highlight_page = tab.pending_highlight_refresh_page.take();
                        if info.dirty {
                            tab.state = state_after_document_info(tab.state, true);
                        } else {
                            tab.save_in_flight = false;
                            tab.edit_history.clear();
                            tab.state = state_after_document_info(tab.state, false);
                        }
                        tab.info = Some(info);
                        tab.invalidate_rendering();
                        tab.invalidate_text_snapshots();
                        tab.invalidate_annotation_pages();
                        if dirty {
                            if let Some(page_index) = changed_highlight_page {
                                tab.refresh_highlight_index_page(page_index, revision, page_count);
                            } else {
                                // A revision change without an application edit
                                // has no page identity whose xrefs can be retained.
                                tab.reset_highlight_index(revision, page_count);
                            }
                        } else {
                            // Save reopens MuPDF and may assign new xrefs even
                            // when every visible annotation looks unchanged.
                            tab.reset_highlight_index(revision, page_count);
                        }
                        let thumbnail_keys = tab.invalidate_thumbnails();
                        for key in thumbnail_keys {
                            self.thumbnail_lru.remove(&key);
                        }
                        if let Some(path) = saved_path {
                            self.finish_save_for_close(&path, context);
                        }
                        if restart_search {
                            self.begin_search(index);
                        }
                    }
                    Ok(DocumentEvent::TileRendered(mut tile)) => {
                        let is_active = self.active_index() == Some(index);
                        let key = TileCacheKey::from_tile(self.documents[index].document_id, &tile);
                        let tab = &mut self.documents[index];
                        #[cfg(debug_assertions)]
                        let completed_request = tab.pending_tiles.remove(&key);
                        #[cfg(not(debug_assertions))]
                        tab.pending_tiles.remove(&key);
                        #[cfg(debug_assertions)]
                        let was_prefetched = completed_request
                            .is_some_and(|request| request.priority != RenderPriority::Visible);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let result_is_current = tile_result_is_current(
                            is_active,
                            key,
                            tile.generation,
                            tab.view.generation,
                            current_revision,
                            &tab.wanted_tiles,
                        );
                        if !result_is_current {
                            continue;
                        }
                        // Texture upload trusts both byte count and page-relative
                        // dimensions, so validate the worker snapshot first.
                        let bounds_match = tab
                            .info
                            .as_ref()
                            .and_then(|info| info.page_bounds.get(tile.page_index))
                            .is_some_and(|bounds| *bounds == tile.bounds);
                        let payload_is_valid = tile.spec.rgba_bytes()
                            == Some(tile.pixels_rgba.len())
                            && tile.page_pixel_width > 0
                            && tile.page_pixel_height > 0
                            && bounds_match;
                        if !payload_is_valid {
                            tab.error = Some(
                                "ページを表示できませんでした。PDFを開き直してください。詳細: 描画データが不正です。"
                                    .to_owned(),
                            );
                            continue;
                        }

                        let image = take_rgba_image(
                            &mut tile.pixels_rgba,
                            [
                                tile.spec.pixel_width as usize,
                                tile.spec.pixel_height as usize,
                            ],
                        );
                        let texture = context.load_texture(
                            format!(
                                "pdf-{}-page-{}-tile-{}-{}-revision-{}",
                                tab.document_id,
                                tile.page_index,
                                tile.spec.pixel_x,
                                tile.spec.pixel_y,
                                tile.revision
                            ),
                            image,
                            TextureOptions::LINEAR,
                        );
                        let weight = tile
                            .spec
                            .rgba_bytes()
                            .expect("validated tile dimensions fit in memory");
                        tab.tiles.insert(
                            key,
                            CachedTile {
                                tile,
                                texture,
                                #[cfg(debug_assertions)]
                                was_prefetched,
                            },
                        );
                        let outcome = self.gpu_lru.insert(key, (), weight);
                        if !outcome.inserted {
                            self.documents[index].tiles.remove(&key);
                        }
                        self.remove_evicted_gpu_tiles(outcome.evicted);
                    }
                    Ok(DocumentEvent::SelectionReady(selection)) => {
                        let tab = &mut self.documents[index];
                        if selection.generation == tab.selection_generation {
                            tab.selection = Some(selection);
                            self.status = "Selection Quad baseline updated".to_owned();
                        }
                    }
                    Ok(DocumentEvent::EditActionCreated(action)) => {
                        let (completed_annotation, changed_page) = match &action {
                            EditAction::CreateHighlight { page_index, .. } => (None, *page_index),
                            EditAction::UpdateAnnotation { annotation_id, .. }
                            | EditAction::DeleteAnnotation { annotation_id, .. } => {
                                (Some(*annotation_id), annotation_id.page_index)
                            }
                        };
                        let document_id = self.documents[index].document_id;
                        let tab = &mut self.documents[index];
                        tab.pending_edits = tab.pending_edits.saturating_sub(1);
                        tab.pending_highlight_refresh_page = Some(changed_page);
                        tab.edit_history.push(action);
                        tab.state = DocumentState::ReadyDirty;
                        if completed_annotation.is_some_and(|annotation_id| {
                            self.annotation_editor.as_ref().is_some_and(|editor| {
                                editor.document_id == document_id
                                    && editor.annotation_id == annotation_id
                            })
                        }) {
                            self.annotation_editor = None;
                        }
                    }
                    Ok(DocumentEvent::EditActionUndone(action)) => {
                        let changed_page = match &action {
                            EditAction::CreateHighlight { page_index, .. } => *page_index,
                            EditAction::UpdateAnnotation { annotation_id, .. }
                            | EditAction::DeleteAnnotation { annotation_id, .. } => {
                                annotation_id.page_index
                            }
                        };
                        let tab = &mut self.documents[index];
                        tab.undo_in_flight = false;
                        if tab.edit_history.last() == Some(&action) {
                            tab.edit_history.pop();
                            tab.pending_highlight_refresh_page = Some(changed_page);
                            self.status = "編集を元に戻しました".to_owned();
                        } else {
                            // A worker response must correspond to the action at the
                            // top of this tab's history; silently reordering would let
                            // a later undo target the wrong annotation identity.
                            tab.error = Some(
                                "編集を元に戻せませんでした。タブを開き直してください。詳細: 編集履歴の応答順序が一致しません。"
                                    .to_owned(),
                            );
                        }
                    }
                    Ok(DocumentEvent::TextSnapshotReady(snapshot)) => {
                        let key = TextSnapshotKey::from_snapshot(&snapshot);
                        let is_active = self.active_index() == Some(index);
                        let tab = &mut self.documents[index];
                        tab.pending_text_snapshots.remove(&key);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
                        let is_current = text_snapshot_result_is_current(
                            is_active,
                            key,
                            current_revision,
                            page_count,
                            &tab.wanted_text_snapshots,
                        );
                        if is_current {
                            tab.failed_text_snapshots.remove(&key);
                            tab.text_snapshots.insert(key, snapshot);
                        }
                    }
                    Ok(DocumentEvent::TextSnapshotSkipped(request)) => {
                        let key = TextSnapshotKey::from_request(&request);
                        self.documents[index].pending_text_snapshots.remove(&key);
                    }
                    Ok(DocumentEvent::TextSnapshotFailed { request, message }) => {
                        let key = TextSnapshotKey::from_request(&request);
                        let is_active = self.active_index() == Some(index);
                        let tab = &mut self.documents[index];
                        tab.pending_text_snapshots.remove(&key);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
                        let is_current = text_snapshot_result_is_current(
                            is_active,
                            key,
                            current_revision,
                            page_count,
                            &tab.wanted_text_snapshots,
                        );
                        if is_current {
                            tab.failed_text_snapshots.insert(key);
                            tab.error = Some(format!(
                                "テキスト情報を読み取れませんでした。PDFを開き直してください。詳細: {message}"
                            ));
                        }
                    }
                    Ok(DocumentEvent::AnnotationsReady(snapshot)) => {
                        let request = AnnotationPageRequest {
                            page_index: snapshot.page_index,
                            expected_revision: snapshot.revision,
                        };
                        let is_active = self.active_index() == Some(index);
                        let tab = &mut self.documents[index];
                        tab.pending_annotation_pages.remove(&request);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let is_current = annotation_page_result_is_current(
                            is_active,
                            request,
                            current_revision,
                            &tab.wanted_annotation_pages,
                        );
                        if is_current {
                            tab.failed_annotation_pages.remove(&request);
                            tab.annotation_pages.insert(request, snapshot);
                        }
                    }
                    Ok(DocumentEvent::AnnotationsSkipped(request)) => {
                        self.documents[index]
                            .pending_annotation_pages
                            .remove(&request);
                    }
                    Ok(DocumentEvent::AnnotationsFailed { request, message }) => {
                        let is_active = self.active_index() == Some(index);
                        let tab = &mut self.documents[index];
                        tab.pending_annotation_pages.remove(&request);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let is_current = annotation_page_result_is_current(
                            is_active,
                            request,
                            current_revision,
                            &tab.wanted_annotation_pages,
                        );
                        if is_current {
                            tab.failed_annotation_pages.insert(request);
                            tab.error = Some(format!(
                                "注釈情報を読み取れませんでした。PDFを開き直してください。詳細: {message}"
                            ));
                        }
                    }
                    Ok(DocumentEvent::HighlightIndexReady(batch)) => {
                        self.documents[index].receive_highlight_index_batch(batch);
                    }
                    Ok(DocumentEvent::HighlightIndexSkipped(request)) => {
                        let tab = &mut self.documents[index];
                        if tab.highlight_index.in_flight == Some(request)
                            && tab.highlight_index.generation == request.generation
                        {
                            tab.highlight_index.in_flight = None;
                            tab.highlight_index.error = Some(
                                "文書が更新されたため、ハイライト一覧の読み込みを中止しました。"
                                    .to_owned(),
                            );
                        }
                    }
                    Ok(DocumentEvent::HighlightIndexFailed { request, message }) => {
                        let tab = &mut self.documents[index];
                        if tab.highlight_index.in_flight == Some(request)
                            && tab.highlight_index.generation == request.generation
                        {
                            tab.highlight_index.in_flight = None;
                            tab.highlight_index.error = Some(format!(
                                "ハイライト一覧を読み込めませんでした。詳細: {message}"
                            ));
                        }
                    }
                    Ok(DocumentEvent::OutlineReady(outline)) => {
                        self.documents[index].outline = Some(outline);
                    }
                    Ok(DocumentEvent::SearchPageReady(result)) => {
                        self.receive_search_page(index, result);
                    }
                    Ok(DocumentEvent::ThumbnailReady(mut thumbnail)) => {
                        let key = ThumbnailCacheKey::from_thumbnail(
                            self.documents[index].document_id,
                            &thumbnail,
                        );
                        let is_active = self.active_index() == Some(index);
                        let tab = &mut self.documents[index];
                        tab.pending_thumbnails.remove(&key);
                        tab.failed_thumbnails.remove(&key);
                        let result_is_current = is_active
                            && thumbnail.generation == tab.thumbnail_generation
                            && tab
                                .info
                                .as_ref()
                                .is_some_and(|info| info.revision == thumbnail.revision);
                        let expected_bytes = usize::try_from(thumbnail.pixel_width)
                            .ok()
                            .and_then(|width| {
                                usize::try_from(thumbnail.pixel_height)
                                    .ok()
                                    .and_then(|height| width.checked_mul(height))
                            })
                            .and_then(|pixels| pixels.checked_mul(4));
                        if !result_is_current || expected_bytes != Some(thumbnail.pixels_rgba.len())
                        {
                            continue;
                        }

                        let image = take_rgba_image(
                            &mut thumbnail.pixels_rgba,
                            [
                                thumbnail.pixel_width as usize,
                                thumbnail.pixel_height as usize,
                            ],
                        );
                        let texture = context.load_texture(
                            format!(
                                "pdf-{}-thumbnail-{}-revision-{}",
                                tab.document_id, thumbnail.page_index, thumbnail.revision
                            ),
                            image,
                            TextureOptions::LINEAR,
                        );
                        let weight = expected_bytes.expect("validated thumbnail byte count");
                        tab.thumbnails
                            .insert(key, CachedThumbnail { thumbnail, texture });
                        let outcome = self.thumbnail_lru.insert(key, (), weight);
                        if !outcome.inserted {
                            self.documents[index].thumbnails.remove(&key);
                        }
                        self.remove_evicted_thumbnails(outcome.evicted);
                    }
                    Ok(DocumentEvent::ThumbnailSkipped(request)) => {
                        let key = ThumbnailCacheKey::from_request(
                            self.documents[index].document_id,
                            &request,
                        );
                        self.documents[index].pending_thumbnails.remove(&key);
                    }
                    Ok(DocumentEvent::ThumbnailFailed { request, message }) => {
                        let tab = &mut self.documents[index];
                        let key = ThumbnailCacheKey::from_request(tab.document_id, &request);
                        mark_thumbnail_failed(
                            &mut tab.pending_thumbnails,
                            &mut tab.failed_thumbnails,
                            key,
                        );
                        tab.error = Some(format!(
                            "サムネイルを表示できませんでした。PDFを開き直してください。詳細: {message}"
                        ));
                    }
                    #[cfg(windows)]
                    Ok(DocumentEvent::PrintCompleted) => {
                        self.documents[index].print_in_flight = false;
                        self.status = "印刷データをプリンターへ送信しました".to_owned();
                    }
                    #[cfg(windows)]
                    Ok(DocumentEvent::PrintCancelled) => {
                        self.documents[index].print_in_flight = false;
                        self.status = "印刷をキャンセルしました".to_owned();
                    }
                    Ok(DocumentEvent::Status(status)) => {
                        self.status = status;
                        self.documents[index].error = None;
                    }
                    Ok(DocumentEvent::Failed { operation, message }) => {
                        if operation == "open" && self.documents[index].restoring_from_session {
                            // The tab vector cannot be shifted while its event
                            // queues are being traversed. Remove failed restore
                            // tabs only after every current index is inspected.
                            let path = self.tabs.tabs()[index].path().to_path_buf();
                            failed_restored_paths.push(path);
                            break;
                        }
                        self.documents[index].error =
                            Some(document_failure_message(operation, &message));
                        if operation == "highlight" {
                            let tab = &mut self.documents[index];
                            tab.pending_edits = tab.pending_edits.saturating_sub(1);
                            if !tab.has_unsaved_changes() && !tab.is_saving() {
                                tab.state = DocumentState::ReadyClean;
                            }
                        }
                        if operation == "highlight-state" {
                            let tab = &mut self.documents[index];
                            tab.pending_edits = tab.pending_edits.saturating_sub(1);
                            // MuPDF already created the annotation; only the
                            // follow-up snapshot failed, so remain dirty.
                            tab.state = DocumentState::ReadyDirty;
                        }
                        if operation == "annotation-update" || operation == "annotation-delete" {
                            let document_id = self.documents[index].document_id;
                            let tab = &mut self.documents[index];
                            tab.pending_edits = tab.pending_edits.saturating_sub(1);
                            if !tab.has_unsaved_changes() && !tab.is_saving() {
                                tab.state = DocumentState::ReadyClean;
                            }
                            if let Some(editor) = self
                                .annotation_editor
                                .as_mut()
                                .filter(|editor| editor.document_id == document_id)
                            {
                                editor.mutation_in_flight = false;
                                editor.notice = Some(
                                    "注釈を変更できませんでした。PDFの編集制限を確認してください。"
                                        .to_owned(),
                                );
                            }
                        }
                        if operation == "search" {
                            // A page error normally repeats for every queued page.
                            // Advancing the generation stops the remaining work instead
                            // of flooding the UI with identical failures.
                            self.cancel_search(index);
                        }
                        if operation == "undo" {
                            self.documents[index].undo_in_flight = false;
                        }
                        if operation == "print" {
                            self.documents[index].print_in_flight = false;
                        }
                        if operation == "save" {
                            self.documents[index].save_in_flight = false;
                            self.documents[index].state = DocumentState::ReadyDirty;
                            let failed_path = self.tabs.tabs()[index].path();
                            if let Some(confirmation) = &mut self.close_confirmation
                                && confirmation.path == failed_path
                            {
                                confirmation.save_in_flight = false;
                            }
                        } else if operation == "open"
                            || operation == "resume"
                            || operation == "document-info"
                        {
                            self.documents[index].state = DocumentState::Error;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if self.documents[index].restoring_from_session {
                            let path = self.tabs.tabs()[index].path().to_path_buf();
                            failed_restored_paths.push(path);
                            break;
                        }
                        let failed_path = self.tabs.tabs()[index].path().to_path_buf();
                        let document_id = self.documents[index].document_id;
                        self.documents[index].mark_worker_disconnected();
                        if let Some(editor) = self
                            .annotation_editor
                            .as_mut()
                            .filter(|editor| editor.document_id == document_id)
                        {
                            editor.mutation_in_flight = false;
                            editor.notice = Some(
                                "文書処理が停止したため、注釈の変更を完了できませんでした。"
                                    .to_owned(),
                            );
                        }
                        if let Some(confirmation) = &mut self.close_confirmation
                            && confirmation.path == failed_path
                        {
                            // A dead worker cannot finish the queued save, so the
                            // dialog must become dismissible instead of waiting forever.
                            confirmation.save_in_flight = false;
                        }
                        break;
                    }
                }
            }
        }

        // Removing a tab while iterating its event queue would shift the vector
        // and could skip the next document. Deferred close runs after all queues.
        if let Some(path) = self.saved_tab_to_close.take() {
            self.close_tab_by_path(&path);
        }
        for path in failed_restored_paths {
            if let Some(index) = self.tabs.tabs().iter().position(|tab| tab.path() == path) {
                self.remove_tab_now(index);
            } else {
                // A concurrent close may have removed the tab after its worker
                // reported failure; account for that completion exactly once.
                self.finish_session_restore(false);
            }
        }
        if self.window_close_pending
            && self.close_confirmation.is_none()
            && self.session_close_failure.is_none()
            && !self
                .documents
                .iter()
                .any(|document| document.is_saving() || document.is_printing())
        {
            self.prompt_next_window_document(context);
        }
        if self.close_all_pending
            && self.close_confirmation.is_none()
            && !self
                .documents
                .iter()
                .any(|document| document.is_saving() || document.is_printing())
        {
            self.prompt_next_close_all_document();
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

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        let open_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::O));
        // Native dialogs must not start underneath an unresolved close flow;
        // that flow owns the document set until the user makes a decision.
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
            // Escape is consumed here before the PDF view reads raw input, so
            // autoscroll must stop in this branch rather than only in its frame update.
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
                // The first Escape leaves the multiline editor; it must not
                // also discard the buffer or act on the PDF behind the overlay.
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

        let zoom_delta = context.input(|input| input.zoom_delta());
        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.zoom_by(zoom_delta);
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
        let copy_pressed = !text_input_has_focus
            && context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::C));
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

    fn next_activity_sequence(&mut self) -> u64 {
        self.activity_sequence = self.activity_sequence.wrapping_add(1);
        self.activity_sequence
    }

    fn begin_search(&mut self, index: usize) {
        let query = self.documents[index].search.query.trim().to_owned();
        if query.is_empty() {
            self.cancel_search(index);
            return;
        }
        let Some(page_count) = self.documents[index]
            .info
            .as_ref()
            .map(|info| info.page_bounds.len())
        else {
            return;
        };
        let current_page = self.documents[index].view.current_page;
        let generation = self.documents[index].search.generation.wrapping_add(1);
        let query: Arc<str> = Arc::from(query);
        let tab = &mut self.documents[index];
        tab.search.generation = generation;
        tab.search.pages.clear();
        tab.search.selected = None;
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = true;
        if !tab.send(DocumentCommand::SetSearchGeneration(generation)) {
            tab.search.in_progress = false;
            tab.error = Some(
                "検索を開始できません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
            return;
        }
        for page_index in search_page_order(current_page, page_count) {
            let _queued = tab.send(DocumentCommand::SearchPage {
                page_index,
                query: Arc::clone(&query),
                generation,
            });
        }
        self.status = format!("Searching {page_count} pages…");
    }

    fn cancel_search(&mut self, index: usize) {
        let tab = &mut self.documents[index];
        tab.search.generation = tab.search.generation.wrapping_add(1);
        let _queued = tab.send(DocumentCommand::SetSearchGeneration(tab.search.generation));
        tab.search.pages.clear();
        tab.search.selected = None;
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = false;
    }

    fn navigate_search(&mut self, index: usize, forward: bool) {
        let current_page = self.documents[index].view.current_page;
        let cursor = next_search_match(
            &self.documents[index].search.pages,
            self.documents[index].search.selected,
            current_page,
            forward,
        );
        if let Some(cursor) = cursor {
            let anchor = search_match_anchor_for_cursor(&self.documents[index], cursor);
            let ordinal = search_match_ordinal(&self.documents[index].search.pages, cursor);
            let tab = &mut self.documents[index];
            tab.search.selected = Some(cursor);
            if let Some(anchor) = anchor {
                tab.jump_to_search_match(anchor);
            } else {
                tab.jump_to_page(cursor.page_index);
            }
            self.status = ordinal.map_or_else(
                || format!("Search result on page {}", cursor.page_index + 1),
                |ordinal| format!("Search result {ordinal} on page {}", cursor.page_index + 1),
            );
        }
    }

    fn activate_document(&mut self, index: usize, previous: Option<usize>) {
        if let Some(tab_to_reveal) = tab_reveal_for_selection_change(previous, index) {
            self.tab_to_reveal = Some(tab_to_reveal);
        }
        if let Some(previous) = previous.filter(|previous| *previous != index) {
            self.documents[previous].view.stop_autoscroll();
            // A tab switch changes request ownership, not tile identity. Cancel
            // queued work without discarding the generation or reusable textures.
            self.documents[previous].cancel_rendering_requests();
            self.documents[previous].invalidate_text_snapshots();
            self.documents[previous].invalidate_annotation_pages();
            self.documents[previous].search.generation =
                self.documents[previous].search.generation.wrapping_add(1);
            let generation = self.documents[previous].search.generation;
            let _queued =
                self.documents[previous].send(DocumentCommand::SetSearchGeneration(generation));
            self.documents[previous].search.in_progress = false;
        }

        let sequence = self.next_activity_sequence();
        self.documents[index].last_selected_sequence = sequence;
        if self.documents[index].state == DocumentState::Suspended {
            let path = self.tabs.tabs()[index].path().to_path_buf();
            self.documents[index].resume(path);
            self.status = "Reopening suspended PDF after external-change check…".to_owned();
        } else if !self.documents[index].search.query.trim().is_empty()
            && !self.documents[index].search.in_progress
        {
            self.begin_search(index);
        }
    }

    fn select_tab(&mut self, index: usize) {
        let previous = self.active_index();
        if !self.tabs.select(index) {
            return;
        }
        self.activate_document(index, previous);
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
        let Some(index) = oldest_suspendable_index(self.active_index(), &candidates) else {
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
        if self.documents.is_empty() {
            return;
        }
        let next = self
            .active_index()
            .map(|index| (index + 1) % self.documents.len())
            .unwrap_or(0);
        self.select_tab(next);
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
        let Some(document) = self.documents.get(index) else {
            return;
        };
        // A queued save precedes Shutdown on the worker command
        // queue, so Discard cannot honestly cancel it. Wait for completion.
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
        if was_restoring {
            // Closing an opening restore tab consumes its pending result; the
            // worker is dropped with the tab, so no event can complete it later.
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

    fn receive_search_page(&mut self, index: usize, result: SearchPageResult) {
        let tab = &mut self.documents[index];
        let current_revision = tab.info.as_ref().map(|info| info.revision);
        if !search_result_is_current(
            result.generation,
            tab.search.generation,
            result.revision,
            current_revision,
        ) {
            return;
        }
        tab.search.completed_pages = tab.search.completed_pages.saturating_add(1);
        tab.search.truncated |= result.truncated;
        if !result.matches.is_empty() {
            tab.search.pages.insert(result.page_index, result.matches);
        }
        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
        if tab.search.completed_pages >= page_count {
            tab.search.in_progress = false;
        }
    }

    fn close_tab_by_path(&mut self, path: &Path) {
        let index = self.tabs.tabs().iter().position(|tab| tab.path() == path);
        if let Some(index) = index {
            self.close_tab_now(index);
        }
    }

    fn handle_window_close(&mut self, context: &egui::Context) {
        let close_requested = context.input(|input| input.viewport().close_requested());
        if !close_requested || self.allow_window_close {
            return;
        }

        // eframe closes the native window unless cancellation is sent during
        // the same frame in which the OS close request is observed.
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
            // Session state must not be captured until every restored open has
            // reported success or failure; receive_document_events retries the
            // close flow when the last pending result is consumed.
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
            selected_tab: self.active_index(),
            sidebar_open: self.sidebar_open,
            sidebar_tab: match self.sidebar_tab {
                SidebarTab::Outline => SessionSidebarTab::Outline,
                SidebarTab::Thumbnails => SessionSidebarTab::Thumbnails,
                SidebarTab::Highlights => SessionSidebarTab::Highlights,
            },
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
                // This explicit choice is the only path that permits shutdown
                // without the atomic session update required by normal close.
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
                // Switching targets must never silently discard a long comment
                // or implicitly save it into another annotation.
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
                // Context-menu deletion of another annotation must not make an
                // unrelated edit buffer disappear or become attached to a new target.
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
            // The adapter reports a concrete restriction; the UI does not
            // invent a save fallback that could leave an unsavable dirty tab.
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
            // This local pending count closes the race before MuPDF reports its
            // dirty flag back from the document worker.
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
            // Keep the action in history until the backend confirms the exact
            // stable ID was removed; a failure must remain retryable.
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

    fn tab_bar(&mut self, root_ui: &mut egui::Ui) {
        let selected_index = self.active_index();
        let tab_to_reveal = self.tab_to_reveal.take();
        let editor_dirty_document = self
            .annotation_editor
            .as_ref()
            .filter(|editor| editor.is_dirty() || editor.mutation_in_flight)
            .map(|editor| editor.document_id);
        let mut select_request = None;
        let mut close_request = None;
        egui::Panel::top("tabs").show(root_ui, |ui| {
            let tab_count = self.tabs.tabs().len();
            let tab_width = tab_width_for_count(
                ui.available_width(),
                tab_count,
                TAB_ITEM_SPACING,
                TAB_MIN_WIDTH,
                TAB_MAX_WIDTH,
            );
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = TAB_ITEM_SPACING;
                    for (index, tab) in self.tabs.tabs().iter().enumerate() {
                        let dirty = self.documents[index].has_unsaved_changes()
                            || editor_dirty_document == Some(self.documents[index].document_id);
                        let marker = if dirty { "● " } else { "" };
                        let title = tab
                            .path()
                            .file_name()
                            .map(|name| name.to_string_lossy())
                            .unwrap_or_else(|| tab.path().as_os_str().to_string_lossy());
                        let full_title = format!("{marker}{title}");
                        let (tab_rect, tab_response) = ui
                            .allocate_exact_size(Vec2::new(tab_width, TAB_HEIGHT), Sense::hover());
                        let content = tab_content_rects(
                            tab_rect,
                            TAB_HORIZONTAL_PADDING,
                            TAB_CLOSE_WIDTH,
                            TAB_CONTENT_GAP,
                        );
                        let tab_id = ui.id().with(("document-tab", index));
                        let select_response = ui
                            .interact(content.selection, tab_id.with("select"), Sense::click())
                            .on_hover_text(tab.path().display().to_string());
                        let can_close = !self.documents[index].is_printing();
                        let close_sense = if can_close {
                            Sense::click()
                        } else {
                            Sense::hover()
                        };
                        let close_response =
                            ui.interact(content.close, tab_id.with("close"), close_sense);
                        let selected = selected_index == Some(index);
                        paint_document_tab(
                            ui,
                            tab_rect,
                            content,
                            &full_title,
                            TabPaintState {
                                selected,
                                can_close,
                                select_response: &select_response,
                                close_response: &close_response,
                            },
                        );
                        if selected && tab_to_reveal == Some(index) {
                            tab_response.scroll_to_me(None);
                        }
                        match tab_pointer_action(
                            select_response.clicked(),
                            select_response.clicked_by(PointerButton::Middle),
                        ) {
                            Some(TabPointerAction::Select) => select_request = Some(index),
                            Some(TabPointerAction::Close) if can_close => {
                                close_request = Some(index)
                            }
                            Some(TabPointerAction::Close) | None => {}
                        }
                        if can_close && close_response.clicked() {
                            close_request = Some(index);
                        }
                    }
                });
            });
        });
        if let Some(index) = select_request {
            self.select_tab(index);
        }
        if let Some(index) = close_request {
            self.close_tab(index);
        }
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

        egui::Panel::top("menu-bar").show(root_ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("ファイル", |ui| {
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
                    ui.separator();
                    if ui.button("終了").clicked() {
                        exit_requested = true;
                        ui.close();
                    }
                });
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
                            self.documents[index].set_display_mode(DisplayMode::Continuous);
                            ui.close();
                        }
                        if ui.button("単一ページ表示").clicked() {
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
    }

    fn toolbar(&mut self, root_ui: &mut egui::Ui) {
        let mut search_changed = false;
        let mut search_navigation = None;
        let mut submitted_page = None;
        let mut page_delta_requested = None;
        let mut open_requested = false;
        let mut print_requested = false;

        egui::Panel::top("toolbar").show(root_ui, |ui| {
            // Scrolling preserves one stable row when the window is too narrow;
            // wrapping would separate controls inside a functional group.
            egui::ScrollArea::horizontal()
                .id_salt("toolbar-scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Keep the sidebar slot stable while no PDF is open so the remaining
                        // toolbar groups do not shift when the first document is loaded.
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
                        let page_response = ui.add(
                            egui::TextEdit::singleline(&mut self.documents[index].page_input)
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
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut search.query)
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
        // The native dialog runs on the document worker, so opening, saving,
        // printing, shutdown confirmation, and a missing worker are exclusive.
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
            let tab = self
                .active_tab_mut()
                .expect("can_print requires an active tab");
            if tab.send(DocumentCommand::Print) {
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

    fn status_panel(&self, root_ui: &mut egui::Ui) {
        #[cfg(debug_assertions)]
        egui::Panel::bottom("debug-status").show(root_ui, |ui| {
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
                                // The explicit strategy distinguishes a slower full rewrite
                                // from a stalled save during development diagnostics.
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
        #[cfg(not(debug_assertions))]
        let _ = root_ui;
    }

    fn error_banner(&self, root_ui: &mut egui::Ui) {
        let app_error = self.error.as_deref();
        let document_error = self
            .active_index()
            .and_then(|index| self.documents[index].error.as_deref());
        let page_input_error = self
            .active_index()
            .and_then(|index| self.documents[index].page_input_error.as_deref());
        if app_error.is_none() && document_error.is_none() && page_input_error.is_none() {
            return;
        }
        egui::Panel::top("persistent-error-banner").show(root_ui, |ui| {
            if let Some(error) = app_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if let Some(error) = document_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            if let Some(error) = page_input_error {
                ui.colored_label(Color32::LIGHT_RED, error);
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
                                    // Persistent PDF errors must not trigger a retry on
                                    // every frame; only an explicit user action requeues it.
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

    fn central_panel(&mut self, root_ui: &mut egui::Ui) -> Rect {
        let response = egui::CentralPanel::default().show(root_ui, |ui| {
            let Some(index) = self.active_index() else {
                ui.centered_and_justified(|ui| {
                    ui.label("PDFをこのウィンドウへドロップしてください");
                });
                return;
            };
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
            let density_changed = self.documents[index]
                .view
                .update_render_density(pixels_per_point);
            if density_changed {
                self.documents[index].invalidate_rendering();
            }
            self.update_fit_zoom(index, ui.available_size());
            match self.documents[index].view.display_mode {
                DisplayMode::Continuous => self.continuous_view(ui, index),
                DisplayMode::SinglePage => self.single_page_view(ui, index),
            }
        });
        response.response.rect
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
            // A revision-bound xref must not be updated after another edit or
            // save/reopen cycle. The visible buffer can still be explicitly discarded.
            editor.notice = Some(
                "PDFが変更されたため、この編集内容は保存できません。変更を破棄してください。"
                    .to_owned(),
            );
        }
        let action = show_annotation_editor(context, bounds, editor);
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

    /// Calculates fit zoom from the current viewport so resize invalidation is testable.
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

    fn continuous_view(&mut self, ui: &mut egui::Ui, index: usize) {
        let document_id = self.documents[index].document_id;
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
        let viewport = &mut self.viewport;
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
        let mut page_screen_rects = Vec::new();
        let output = scroll_area.show_viewport(ui, |ui, visible_viewport| {
            ui.set_min_size(Vec2::new(content_width, layout.total_height()));
            let visible_text_pages =
                layout.visible_pages(visible_viewport.min.y..visible_viewport.max.y, 0.0);
            tab.prepare_text_snapshots(visible_text_pages.clone(), revision);
            tab.prepare_annotation_pages(visible_text_pages, revision);
            // One viewport of prefetch keeps ordinary wheel scrolling smooth
            // while the shared byte LRU supplies the hard memory bound.
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
        clear_selection |= background_interaction.clear_selection;
        if let Some(delta) = pan_delta {
            let requested_offset = output.state.offset - delta;
            tab.view.pan_requested_offset =
                Some(clamp_scroll_offset(requested_offset, maximum_offset));
        }
        if let Some(frame) = update_autoscroll(
            ui.ctx(),
            &mut tab.view,
            output.inner_rect,
            &excluded_rects,
            ui.layer_id(),
            output.state.offset,
            maximum_offset,
        ) {
            paint_autoscroll_marker(ui, output.inner_rect, frame.anchor);
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

    fn single_page_view(&mut self, ui: &mut egui::Ui, index: usize) {
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
        let viewport = &mut self.viewport;
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
            // Enter the next page at its top and the previous page at its
            // bottom so the wheel continues in the direction of travel.
            let y = if wheel_page_delta > 0 { 0.0 } else { 1.0 };
            tab.jump_to_single_page_edge(target, Vec2::new(x, y));
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

fn adjacent_page_index(current: usize, page_count: usize, delta: isize) -> Option<usize> {
    let last_page = page_count.checked_sub(1)?;
    let target = current.saturating_add_signed(delta).min(last_page);
    (target != current).then_some(target)
}

impl TileCacheKey {
    fn from_request(document_id: u64, request: &TileRequest) -> Self {
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

    fn from_tile(document_id: u64, tile: &RenderedTile) -> Self {
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
    fn for_page(document_id: u64, page_index: usize, revision: u64) -> Self {
        Self {
            document_id,
            page_index,
            max_pixel_width: THUMBNAIL_MAX_WIDTH,
            max_pixel_height: THUMBNAIL_MAX_HEIGHT,
            revision,
        }
    }

    fn from_thumbnail(document_id: u64, thumbnail: &RenderedThumbnail) -> Self {
        Self {
            document_id,
            page_index: thumbnail.page_index,
            max_pixel_width: thumbnail.max_pixel_width,
            max_pixel_height: thumbnail.max_pixel_height,
            revision: thumbnail.revision,
        }
    }

    fn from_request(document_id: u64, request: &ThumbnailRequest) -> Self {
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
    fn from_snapshot(snapshot: &TextPageSnapshot) -> Self {
        Self {
            page_index: snapshot.page_index,
            revision: snapshot.revision,
        }
    }

    fn from_request(request: &TextSnapshotRequest) -> Self {
        Self {
            page_index: request.page_index,
            revision: request.expected_revision,
        }
    }
}

/// Enumerates the raster tiles needed for one page and assigns view priority.
fn tile_requests_for_page(
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

/// Orders the current page and its two transition views without rasterizing an
/// enlarged adjacent page outside the range that appears after navigation.
fn single_page_tile_requests(
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
            // The current page's one-viewport margin must finish before either
            // adjacent transition range, regardless of scroll direction.
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

/// Requests only the page area visible immediately after an edge transition.
fn transition_tile_requests_for_page(
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

/// Limits raster work to the visible area and exactly one viewport around it.
fn prioritized_tile_specs(
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

/// Enumerates only tiles intersecting the requested logical page region.
fn tile_specs_intersecting_viewport(
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

/// Maps a logical page edge to a bounded page-local device pixel edge.
fn logical_edge_to_pixel(
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
    // Outward rounding includes every edge pixel touched by the logical
    // prefetch rectangle, including partial right and bottom tiles.
    let rounded = if round_up {
        scaled.ceil()
    } else {
        scaled.floor()
    };
    Some(rounded.clamp(0.0, pixel_extent as f32) as u32)
}

fn logical_tile_rect(page_rect: Rect, grid: TileGrid, spec: TileSpec) -> Rect {
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

fn tile_priority(tile_rect: Rect, visible_viewport: Rect) -> RenderPriority {
    if tile_rect.intersects(visible_viewport) {
        return RenderPriority::Visible;
    }
    // Right/below is the usual forward reading direction for both continuous
    // and zoomed single-page scrolling; left/above is retained as lower rank.
    if tile_rect.top() >= visible_viewport.bottom() || tile_rect.left() >= visible_viewport.right()
    {
        RenderPriority::NextViewport
    } else {
        RenderPriority::PreviousViewport
    }
}

/// Converts the viewport center into a page-relative coordinate for zoom restore.
fn normalized_page_point(page_rect: Rect, point: Pos2) -> Vec2 {
    Vec2::new(
        ((point.x - page_rect.left()) / page_rect.width()).clamp(0.0, 1.0),
        ((point.y - page_rect.top()) / page_rect.height()).clamp(0.0, 1.0),
    )
}

/// Converts raw wheel events into bounded single-page navigation steps.
///
/// The edge flags describe the scroll position before this frame's ScrollArea
/// processing. This prevents the event that merely reaches an edge from also
/// changing the page.
fn single_page_wheel_steps(
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

        // End and cancellation must release the latch even if the pointer left
        // the PDF area before the backend delivered the final phase.
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
            // Reversal is a deliberate new intent and must not inherit either
            // the accumulated distance or the previous page latch.
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
                // A raw discrete event represents one physical wheel action;
                // its platform-specific numeric magnitude must not multiply pages.
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
        // After one edge transition the adjacent enlarged page begins at the
        // opposite edge, which cannot be evaluated until the following frame.
        page_steps.signum()
    }
}

fn expire_wheel_gesture_after_idle(now: f64, state: &mut SinglePageWheelState) {
    let idle = state
        .last_input_time
        .is_some_and(|last| now - last >= WHEEL_GESTURE_IDLE_SECONDS);
    if idle {
        reset_wheel_gesture(state);
    }
}

fn reset_wheel_gesture(state: &mut SinglePageWheelState) {
    state.accumulated_points = 0.0;
    state.latched = false;
    state.direction = 0.0;
    state.last_input_time = None;
}

struct AutoscrollFrame {
    anchor: Pos2,
}

/// Starts, advances, or stops browser-style autoscroll for one PDF ScrollArea.
fn update_autoscroll(
    context: &egui::Context,
    view: &mut ViewState,
    view_rect: Rect,
    excluded_rects: &[Rect],
    view_layer: LayerId,
    current_offset: Vec2,
    maximum_offset: Vec2,
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
        && pointer.is_some_and(|position| {
            // Foreground windows and the annotation editor may overlap the
            // central rect. Only the central layer's unexcluded area owns the
            // middle-click start.
            view_rect.contains(position)
                && excluded_rects.iter().all(|rect| !rect.contains(position))
                && context.layer_id_at(position) == Some(view_layer)
        })
    {
        let anchor = pointer.expect("the start condition requires a pointer position");
        view.autoscroll = Some(AutoscrollState {
            anchor,
            requested_offset: Some(current_offset),
        });
    }

    let autoscroll = view.autoscroll.as_mut()?;
    let velocity = pointer.map_or(Vec2::ZERO, |position| {
        autoscroll_velocity(autoscroll.anchor, position)
    });
    let desired_offset = current_offset + velocity * dt;
    autoscroll.requested_offset = Some(clamp_scroll_offset(desired_offset, maximum_offset));
    context.set_cursor_icon(CursorIcon::AllScroll);
    context.request_repaint();
    Some(AutoscrollFrame {
        anchor: autoscroll.anchor,
    })
}

fn autoscroll_velocity(anchor: Pos2, pointer: Pos2) -> Vec2 {
    let displacement = pointer - anchor;
    let distance = displacement.length();
    if distance <= AUTOSCROLL_DEAD_ZONE_POINTS {
        return Vec2::ZERO;
    }

    // Radial scaling keeps diagonal movement directionally stable, unlike
    // independent per-axis clipping which would skew near the speed ceiling.
    let requested_speed = (distance - AUTOSCROLL_DEAD_ZONE_POINTS) * AUTOSCROLL_SPEED_PER_POINT;
    let speed = requested_speed.min(AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND);
    displacement / distance * speed
}

fn clamp_scroll_offset(offset: Vec2, maximum: Vec2) -> Vec2 {
    Vec2::new(
        offset.x.clamp(0.0, maximum.x.max(0.0)),
        offset.y.clamp(0.0, maximum.y.max(0.0)),
    )
}

/// Produces the exact persistent ID that `ScrollArea::id_salt` stores.
fn scroll_area_state_id(ui: &egui::Ui, id_salt: impl egui::AsIdSalt) -> Id {
    // ScrollArea hashes its caller salt into IdSalt before combining it with
    // the parent. Passing the raw tuple directly to make_persistent_id hashes
    // a different value and reads a permanently empty scroll state.
    ui.make_persistent_id(egui::IdSalt::new(id_salt))
}

fn paint_autoscroll_marker(ui: &egui::Ui, view_rect: Rect, anchor: Pos2) {
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

/// Builds the same centered ScrollArea coordinates for current and prefetched pages.
fn single_page_geometry(
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

fn single_page_centered_offset(
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

/// Selects one cached raster identity nearest to the current zoom while keeping
/// document, page, revision, and rotation exact.
fn closest_provisional_tile_keys(
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
            // Log ratios make half/double scales equally distant. Zoom is the
            // primary match; density breaks ties because logical mapping is safe.
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

fn paint_page_tiles(
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
            // PageViewport maps device pixels through normalized page space, so
            // an older DPI is safe here and is replaced by exact tiles below.
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
                // Once a prefetched tile has served its transition, a later
                // revisit is an ordinary cache hit rather than another prefetch use.
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
        select_after_restore: bool,
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
            select_after_restore,
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
        // No completion event can arrive after channel disconnection. Clear
        // every response-waiting flag so close and recovery actions remain usable.
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
        // Dropping a printing tab would also drop its completion receiver and
        // leave the UI permanently believing the print job is still active.
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
        // A resumed worker starts with generation zero; install the retained
        // tab token before any cached index continuation reaches its queue.
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
            // Display modes use different ScrollArea coordinate systems, so an
            // anchor from the old mode cannot be resumed safely.
            self.view.stop_autoscroll();
            self.invalidate_rendering();
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
                // Page-top scrolling would hide lower-page hits. Reuse the
                // pending center-anchor path so the complete hit stays visible.
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

    /// Applies bounds checking and shared page-input state before a caller sets
    /// the display-mode-specific restore position.
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

    /// Starts the tab-local Highlight scan only after its sidebar is first shown.
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

    /// Cancels all old batches and rebuilds the index after xrefs may have changed.
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

    /// Refreshes one edited page while preserving completed pages from the same document.
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

        // A failed visible page stays blocked until it leaves and re-enters the
        // wanted set. Pending stale pages are harmless because revision checks
        // reject their eventual worker responses.
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
            // A lower enum rank is a stronger priority. Requeue only when the
            // same tile has moved into a more urgent viewport class.
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
        // Advancing the generation prevents a worker result from an earlier
        // drag from restoring a selection after this explicit click-clear.
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection = None;
    }
}

impl ViewState {
    /// Records the effective display density and reports whether existing
    /// render requests use device-pixel coordinates from an older density.
    fn update_render_density(&mut self, pixels_per_point: f32) -> bool {
        let current_bits = pixels_per_point.to_bits();
        let changed = self
            .render_pixels_per_point_bits
            .is_some_and(|previous_bits| previous_bits != current_bits);
        self.render_pixels_per_point_bits = Some(current_bits);
        changed
    }

    fn stop_autoscroll(&mut self) {
        self.autoscroll = None;
    }

    fn new() -> Self {
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

    fn from_session(saved: SessionView) -> Self {
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

    fn to_session(&self) -> SessionView {
        let (page_index, page_x, page_y) = match self.display_mode {
            DisplayMode::Continuous => {
                // A queued page jump has not updated the scroll area's center
                // yet, so it must supersede the previous frame's anchor.
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

    fn clamp_to_page_count(&mut self, page_count: usize) {
        let last_page = page_count.saturating_sub(1);
        self.current_page = self.current_page.min(last_page);
        self.scroll_to_page = self.scroll_to_page.map(|page| page.min(last_page));
        for anchor in [&mut self.center_anchor, &mut self.restore_anchor]
            .into_iter()
            .flatten()
        {
            // Session positions are captured before the next open knows the
            // page count; a shorter replacement PDF must stay in bounds.
            anchor.page_index = anchor.page_index.min(last_page);
        }
    }

    /// Transfers the centered PDF coordinate between the two display modes.
    fn switch_display_mode(&mut self, mode: DisplayMode) -> bool {
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
        // Both pointer-scroll modes store offsets in the current ScrollArea's
        // coordinate system. A mode switch must discard the anchor and any
        // pending two-axis movement before the other layout is shown.
        self.autoscroll = None;
        self.pan_requested_offset = None;
        true
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
            // Requests for the stable old zoom are not "intermediate" work;
            // only a later input in this same gesture makes the prior target stale.
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
        let modal_open = self.close_confirmation.is_some() || self.session_close_failure.is_some();
        if modal_open {
            // A modal owns pointer intent until it closes; retaining a background
            // autoscroll anchor would move the document under the dialog.
            self.stop_active_autoscroll();
        }
        self.receive_document_events(context);
        self.maybe_suspend_inactive_document();
        self.handle_dropped_files(context);
        self.handle_shortcuts(context);
        self.handle_window_close(context);
        context.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.menu_bar(ui);
        self.tab_bar(ui);
        self.toolbar(ui);
        self.error_banner(ui);
        self.status_panel(ui);
        self.sidebar_panel(ui);
        let central_rect = self.central_panel(ui);
        self.annotation_candidate_picker(ui.ctx());
        self.annotation_editor_overlay(ui.ctx(), central_rect);
        self.close_confirmation_dialog(ui.ctx());
        self.session_close_failure_dialog(ui.ctx());
    }
}

/// Picks the longest-unused fully suspendable document and skips the active tab.
fn oldest_suspendable_index(
    active_index: Option<usize>,
    candidates: &[(bool, u64)],
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(index, (is_suspendable, _))| Some(*index) != active_index && *is_suspendable)
        .min_by_key(|(_, (_, last_selected))| *last_selected)
        .map(|(index, _)| index)
}

/// Converts internal worker operation tags into actionable release-UI messages.
fn document_failure_message(operation: &str, detail: &str) -> String {
    // Backend detail is retained for diagnosis, but the leading summary always
    // explains in Japanese what failed and what the user can do next.
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

/// A dirty Highlight event can precede the completion of an already queued Save.
fn state_after_document_info(current: DocumentState, dirty: bool) -> DocumentState {
    if dirty && current == DocumentState::Saving {
        DocumentState::Saving
    } else if dirty {
        DocumentState::ReadyDirty
    } else {
        DocumentState::ReadyClean
    }
}

fn search_page_order(current_page: usize, page_count: usize) -> Vec<usize> {
    if page_count == 0 {
        return Vec::new();
    }
    let current_page = current_page.min(page_count - 1);
    let mut pages = Vec::with_capacity(page_count);
    pages.push(current_page);
    for distance in 1..page_count {
        if let Some(page) = current_page.checked_add(distance)
            && page < page_count
        {
            pages.push(page);
        }
        if let Some(page) = current_page.checked_sub(distance) {
            pages.push(page);
        }
    }
    pages
}

fn next_search_match(
    pages: &BTreeMap<usize, Vec<SearchMatch>>,
    selected: Option<SearchCursor>,
    current_page: usize,
    forward: bool,
) -> Option<SearchCursor> {
    let matches = pages
        .iter()
        .flat_map(|(page_index, matches)| {
            (0..matches.len()).map(|match_index| SearchCursor {
                page_index: *page_index,
                match_index,
            })
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return None;
    }

    if let Some(position) =
        selected.and_then(|cursor| matches.iter().position(|candidate| *candidate == cursor))
    {
        // The selected logical hit is stable while later page results arrive;
        // wrapping is based on its new ordered position, not a stale flat index.
        let next = if forward {
            (position + 1) % matches.len()
        } else {
            position.checked_sub(1).unwrap_or(matches.len() - 1)
        };
        return matches.get(next).copied();
    }

    if forward {
        matches
            .iter()
            .copied()
            .find(|cursor| cursor.page_index >= current_page)
            .or_else(|| matches.first().copied())
    } else {
        matches
            .iter()
            .rev()
            .copied()
            .find(|cursor| cursor.page_index <= current_page)
            .or_else(|| matches.last().copied())
    }
}

fn search_match_ordinal(
    pages: &BTreeMap<usize, Vec<SearchMatch>>,
    selected: SearchCursor,
) -> Option<usize> {
    pages
        .iter()
        .flat_map(|(page_index, matches)| {
            (0..matches.len()).map(|match_index| SearchCursor {
                page_index: *page_index,
                match_index,
            })
        })
        .position(|candidate| candidate == selected)
        .map(|position| position + 1)
}

fn search_match_anchor_for_cursor(
    document: &DocumentTab,
    cursor: SearchCursor,
) -> Option<PageAnchor> {
    let page_bounds = document
        .info
        .as_ref()?
        .page_bounds
        .get(cursor.page_index)
        .copied()?;
    let search_match = document
        .search
        .pages
        .get(&cursor.page_index)?
        .get(cursor.match_index)?;
    search_match_anchor(cursor.page_index, search_match, page_bounds)
}

fn search_match_anchor(
    page_index: usize,
    search_match: &SearchMatch,
    page_bounds: crate::domain::document::PageRect,
) -> Option<PageAnchor> {
    let first = search_match.quads.first()?.bounds();
    let (x0, y0, x1, y1) =
        search_match
            .quads
            .iter()
            .skip(1)
            .fold(first, |(x0, y0, x1, y1), quad| {
                let bounds = quad.bounds();
                (
                    x0.min(bounds.0),
                    y0.min(bounds.1),
                    x1.max(bounds.2),
                    y1.max(bounds.3),
                )
            });
    // Center the union of all line Quads so a multi-line hit is navigated as
    // one result. Clamping contains minor PDF coordinate rounding at page edges.
    let x = (((x0 + x1) / 2.0 - page_bounds.x0) / page_bounds.width()).clamp(0.0, 1.0);
    let y = (((y0 + y1) / 2.0 - page_bounds.y0) / page_bounds.height()).clamp(0.0, 1.0);
    (x.is_finite() && y.is_finite()).then_some(PageAnchor {
        page_index,
        page_x_fraction: x,
        page_y_fraction: y,
    })
}

fn search_result_is_current(
    result_generation: u64,
    current_generation: u64,
    result_revision: u64,
    current_revision: Option<u64>,
) -> bool {
    result_generation == current_generation && current_revision == Some(result_revision)
}

fn text_snapshot_result_is_current(
    is_active: bool,
    key: TextSnapshotKey,
    current_revision: Option<u64>,
    page_count: usize,
    wanted: &HashSet<TextSnapshotKey>,
) -> bool {
    // An extraction may finish after a scroll, tab switch, or annotation
    // mutation. Only the exact visible document state may affect UI or errors.
    is_active
        && current_revision == Some(key.revision)
        && key.page_index < page_count
        && wanted.contains(&key)
}

fn annotation_page_result_is_current(
    is_active: bool,
    request: AnnotationPageRequest,
    current_revision: Option<u64>,
    wanted: &HashSet<AnnotationPageRequest>,
) -> bool {
    // Annotation xrefs are document-local mutable identities. A result from an
    // inactive tab, old revision, or no-longer-visible page must not become an
    // edit target in the current UI.
    is_active && current_revision == Some(request.expected_revision) && wanted.contains(&request)
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
        // A page-local extraction error must not remain attached to the tab
        // after that page leaves the visible selection scope.
        *error = None;
    }
}

fn mark_thumbnail_failed(
    pending: &mut HashSet<ThumbnailCacheKey>,
    failed: &mut HashSet<ThumbnailCacheKey>,
    key: ThumbnailCacheKey,
) {
    // A failed key stays blocked until the user explicitly retries it. Other
    // pending pages remain intact because their worker commands are still valid.
    pending.remove(&key);
    failed.insert(key);
}

/// Accepts a raster only when it still belongs to the visible document state.
///
/// The worker cannot cancel an in-progress MuPDF render, so all four identity
/// dimensions are checked before the result allocates a GPU texture.
fn tile_result_is_current(
    is_active: bool,
    key: TileCacheKey,
    result_generation: u64,
    current_generation: u64,
    current_revision: Option<u64>,
    wanted_tiles: &HashSet<TileCacheKey>,
) -> bool {
    is_active
        && result_generation == current_generation
        && current_revision == Some(key.revision)
        && wanted_tiles.contains(&key)
}

/// Copies validated RGBA pixels into egui and releases the worker transfer allocation.
fn take_rgba_image(pixels_rgba: &mut Vec<u8>, size: [usize; 2]) -> egui::ColorImage {
    // `Vec::clear` would retain up to the complete GPU/thumbnail cache budget
    // on the CPU. Moving the allocation out makes it drop after egui copies it.
    let transferred_pixels = std::mem::take(pixels_rgba);
    egui::ColorImage::from_rgba_unmultiplied(size, &transferred_pixels)
}

fn search_query_id(document_id: u64) -> Id {
    Id::new(("pdf-search-query", document_id))
}

/// Converts a one-based user page number to the existing zero-based page index.
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
        // A bare `h` belongs to the focused editor. Consuming it here
        // would both drop the character and create an unrelated annotation.
        return false;
    }
    context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::H))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::selection::PageQuad;
    use mupdf::Size;
    use mupdf::pdf::PdfDocument;

    fn write_blank_pdf(path: &Path) {
        let path_text = path.to_str().unwrap();
        let mut document = PdfDocument::new();
        let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
        document.save(path_text).unwrap();
    }

    fn saved_tab(path: PathBuf, page_index: usize) -> SessionTab {
        SessionTab {
            path,
            view: SessionView {
                page_index,
                page_x: 0.5,
                page_y: 0.5,
                display: SessionDisplayMode::Continuous,
                zoom_mode: SessionZoomMode::FitWidth,
                zoom: 1.0,
            },
        }
    }

    fn finish_async_session_restore(app: &mut PrototypeApp) {
        let context = egui::Context::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.session_restore_progress.is_some() && std::time::Instant::now() < deadline {
            app.receive_document_events(&context);
            if app.session_restore_progress.is_some() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(
            app.session_restore_progress.is_none(),
            "document workers did not finish session restoration before the test deadline"
        );
    }

    #[test]
    fn rgba_upload_releases_the_worker_transfer_allocation() {
        let mut pixels_rgba = Vec::with_capacity(64);
        pixels_rgba.extend_from_slice(&[255, 0, 0, 255, 0, 255, 0, 255]);

        let image = take_rgba_image(&mut pixels_rgba, [2, 1]);

        assert_eq!(image.pixels.len(), 2);
        assert!(pixels_rgba.is_empty());
        assert_eq!(pixels_rgba.capacity(), 0);
    }

    #[test]
    fn tab_width_handles_empty_and_single_tab_bars() {
        assert_eq!(tab_width_for_count(800.0, 0, 1.0, 96.0, 240.0), 0.0);
        assert_eq!(tab_width_for_count(800.0, 1, 1.0, 96.0, 240.0), 240.0);
    }

    #[test]
    fn tab_width_shrinks_monotonically_until_the_minimum() {
        let widths = (1..=20)
            .map(|count| tab_width_for_count(1_000.0, count, 1.0, 96.0, 240.0))
            .collect::<Vec<_>>();

        assert!(widths.windows(2).all(|pair| pair[1] <= pair[0]));
        assert_eq!(widths.last().copied(), Some(96.0));
    }

    #[test]
    fn tab_width_accounts_for_spacing_before_horizontal_scroll() {
        let available = 1_000.0;
        let count = 10;
        let spacing = 1.0;
        let width = tab_width_for_count(available, count, spacing, 96.0, 240.0);
        let total = width * count as f32 + spacing * count.saturating_sub(1) as f32;

        assert_eq!(total, available);

        let minimum_width = tab_width_for_count(available, 11, spacing, 96.0, 240.0);
        let minimum_total = minimum_width * 11.0 + spacing * 10.0;
        assert_eq!(minimum_width, 96.0);
        assert!(minimum_total > available);
    }

    #[test]
    fn tab_title_and_close_regions_do_not_overlap_at_minimum_width() {
        let tab_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(96.0, 24.0));
        let content = tab_content_rects(tab_rect, 8.0, 24.0, 4.0);

        assert!(content.title.is_positive());
        assert_eq!(content.close.width(), 24.0);
        assert!(content.selection.right() <= content.close.left());
        assert!(content.title.right() <= content.close.left());
    }

    #[test]
    fn tab_close_region_reaches_tab_right_edge() {
        let tab_rect = Rect::from_min_size(Pos2::new(10.0, 5.0), Vec2::new(96.0, 24.0));
        let content = tab_content_rects(tab_rect, 8.0, 24.0, 4.0);

        assert_eq!(content.close.right(), tab_rect.right());
    }

    #[test]
    fn tab_middle_click_closes_without_selecting_first() {
        assert_eq!(
            tab_pointer_action(false, true),
            Some(TabPointerAction::Close)
        );
        assert_eq!(
            tab_pointer_action(true, false),
            Some(TabPointerAction::Select)
        );
        assert_eq!(
            tab_pointer_action(true, true),
            Some(TabPointerAction::Close)
        );
    }

    #[test]
    fn tab_close_icon_uses_two_equal_vector_strokes() {
        let close_rect = Rect::from_min_size(Pos2::new(10.0, 5.0), Vec2::splat(24.0));
        let segments = close_icon_segments(close_rect);
        let first_length = segments[0][0].distance(segments[0][1]);
        let second_length = segments[1][0].distance(segments[1][1]);

        assert_eq!(first_length, second_length);
        assert_eq!(segments[0][0].x, segments[1][0].x);
        assert_eq!(segments[0][1].x, segments[1][1].x);
    }

    #[test]
    fn tab_reveal_requests_only_follow_selection_changes() {
        assert_eq!(tab_reveal_for_selection_change(Some(2), 2), None);
        assert_eq!(tab_reveal_for_selection_change(Some(2), 7), Some(7));
        assert_eq!(tab_reveal_for_selection_change(None, 0), Some(0));

        assert_eq!(tab_reveal_after_close(Some(2), 3, Some(2)), None);
        assert_eq!(tab_reveal_after_close(Some(2), 0, Some(1)), None);
        assert_eq!(tab_reveal_after_close(Some(2), 2, Some(2)), Some(2));
        assert_eq!(tab_reveal_after_close(Some(2), 2, Some(1)), Some(1));
    }

    #[test]
    fn focused_search_editor_keeps_h_for_text_input() {
        let focused_context = egui::Context::default();
        let query_id = search_query_id(7);
        focused_context.memory_mut(|memory| memory.request_focus(query_id));
        let mut consumed_by_shortcut = true;
        let mut remained_for_editor = false;
        let _output = focused_context.run_ui(h_key_input(), |ui| {
            consumed_by_shortcut = consume_highlight_shortcut(ui.ctx(), Some(query_id));
            remained_for_editor = ui.input(|input| input.key_pressed(Key::H));
        });
        assert!(!consumed_by_shortcut);
        assert!(remained_for_editor);

        let unfocused_context = egui::Context::default();
        let mut ordinary_shortcut = false;
        let _output = unfocused_context.run_ui(h_key_input(), |ui| {
            ordinary_shortcut = consume_highlight_shortcut(ui.ctx(), None);
        });
        assert!(ordinary_shortcut);
    }

    #[test]
    fn consumed_escape_still_stops_active_autoscroll() {
        let directory = tempfile::tempdir().unwrap();
        let pdf_path = directory.path().join("document.pdf");
        write_blank_pdf(&pdf_path);
        let session_path = directory.path().join("session.json");
        let mut app = PrototypeApp::from_startup(vec![pdf_path], SessionStore::new(session_path));
        app.documents[0].view.autoscroll = Some(AutoscrollState {
            anchor: Pos2::ZERO,
            requested_offset: Some(Vec2::ZERO),
        });
        let context = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };

        let _output = context.run_ui(input, |ui| app.handle_shortcuts(ui.ctx()));

        assert!(app.documents[0].view.autoscroll.is_none());
    }

    fn h_key_input() -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::H,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn render_result_requires_active_tab_and_current_document_state() {
        let key = TileCacheKey {
            document_id: 1,
            page_index: 3,
            zoom_bits: 1.0_f32.to_bits(),
            pixels_per_point_bits: 1.0_f32.to_bits(),
            rotation_quarter_turns: 0,
            spec: TileSpec {
                pixel_x: 0,
                pixel_y: 0,
                pixel_width: 512,
                pixel_height: 512,
            },
            revision: 4,
        };
        let wanted = HashSet::from([key]);

        assert!(tile_result_is_current(true, key, 8, 8, Some(4), &wanted));
        assert!(!tile_result_is_current(false, key, 8, 8, Some(4), &wanted));
        assert!(!tile_result_is_current(true, key, 7, 8, Some(4), &wanted));
    }

    #[test]
    fn tile_requests_and_cache_keys_separate_display_density() {
        let spec = TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        };
        let request_1x = TileRequest {
            page_index: 0,
            zoom: 1.0,
            pixels_per_point: 1.0,
            scale: 1.0,
            generation: 1,
            expected_revision: 0,
            spec,
            priority: RenderPriority::Visible,
        };
        let request_2x = TileRequest {
            pixels_per_point: 2.0,
            scale: 2.0,
            ..request_1x
        };

        assert_ne!(request_1x.scale, request_2x.scale);
        assert_ne!(
            TileCacheKey::from_request(1, &request_1x),
            TileCacheKey::from_request(1, &request_2x)
        );
    }

    #[test]
    fn display_density_invalidates_only_after_the_recorded_value_changes() {
        let mut view = ViewState::new();

        assert!(!view.update_render_density(1.0));
        assert!(!view.update_render_density(1.0));
        assert!(view.update_render_density(1.25));
        assert!(!view.update_render_density(1.25));
    }

    #[test]
    fn provisional_tiles_use_the_closest_zoom_from_the_current_revision() {
        let spec = TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 512,
        };
        let base = TileCacheKey {
            document_id: 1,
            page_index: 2,
            zoom_bits: 1.0_f32.to_bits(),
            pixels_per_point_bits: 1.0_f32.to_bits(),
            rotation_quarter_turns: 0,
            spec,
            revision: 4,
        };
        let keys = vec![
            base,
            TileCacheKey {
                zoom_bits: 1.4_f32.to_bits(),
                ..base
            },
            TileCacheKey {
                zoom_bits: 1.4_f32.to_bits(),
                spec: TileSpec {
                    pixel_x: 512,
                    ..spec
                },
                ..base
            },
            TileCacheKey {
                zoom_bits: 1.49_f32.to_bits(),
                revision: 3,
                ..base
            },
        ];

        let selected =
            closest_provisional_tile_keys(keys.into_iter(), 1, 2, 4, 0, 1.5, 1.0_f32.to_bits());

        assert_eq!(selected.len(), 2);
        assert!(
            selected
                .iter()
                .all(|key| key.zoom_bits == 1.4_f32.to_bits() && key.revision == 4)
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn performance_metrics_group_only_consecutive_zoom_inputs() {
        let started_at = Instant::now();
        let mut performance = RenderPerformance::default();
        performance.begin_zoom(1.1, 7, started_at);
        performance.begin_zoom(1.2, 3, started_at + Duration::from_millis(100));
        performance.note_paint(
            0,
            1.2,
            true,
            true,
            true,
            started_at + Duration::from_millis(125),
        );

        let measurement = performance.zoom.as_ref().unwrap();
        assert_eq!(measurement.discarded_intermediate_requests, 3);
        assert_eq!(
            measurement.provisional_display,
            Some(Duration::from_millis(25))
        );
        assert_eq!(
            measurement.first_exact_tile,
            Some(Duration::from_millis(25))
        );
        assert_eq!(
            measurement.full_exact_viewport,
            Some(Duration::from_millis(25))
        );

        performance.begin_zoom(1.3, 4, started_at + Duration::from_millis(500));
        assert_eq!(
            performance
                .zoom
                .as_ref()
                .unwrap()
                .discarded_intermediate_requests,
            0
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn page_metrics_record_cache_prefetch_and_visible_completion() {
        let started_at = Instant::now();
        let mut performance = RenderPerformance::default();
        performance.begin_page_transition(3, started_at);
        performance.note_page_cache_state(3, true, true);
        performance.note_paint(
            3,
            1.0,
            false,
            true,
            true,
            started_at + Duration::from_millis(12),
        );

        let measurement = performance.page_transition.as_ref().unwrap();
        assert_eq!(measurement.cache_hit, Some(true));
        assert_eq!(measurement.prefetch_used, Some(true));
        assert_eq!(
            measurement.first_exact_tile,
            Some(Duration::from_millis(12))
        );
        assert_eq!(
            measurement.full_exact_viewport,
            Some(Duration::from_millis(12))
        );
    }

    #[test]
    fn huge_page_requests_stay_bounded_to_three_viewports() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 10_000.0,
            y1: 10_000_000.0,
        };
        let grid = TileGrid::new(bounds, 16.0).unwrap();
        let page_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(10_000.0, 10_000_000.0));
        let visible = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 800.0));

        let requested = prioritized_tile_specs(grid, page_rect, visible).unwrap();

        assert_eq!(requested.len(), 63 * 50);
    }

    fn requested_right_edge(
        bounds: crate::domain::document::PageRect,
        zoom: f32,
        pixels_per_point: f32,
        page_rect: Rect,
        viewport: Rect,
    ) -> (TileGrid, Vec<TileSpec>) {
        let grid = TileGrid::new(bounds, zoom * pixels_per_point).unwrap();
        let specs = tile_specs_intersecting_viewport(grid, page_rect, viewport).unwrap();
        (grid, specs)
    }

    #[derive(Clone, Copy, Debug)]
    struct FitPageScrollMetrics {
        available_size: Vec2,
        visible_viewport: Rect,
        content_size: Vec2,
        page_content_rect: Rect,
        page_screen_rect: Rect,
        clip_rect: Rect,
    }

    fn measure_single_page_scroll_area(
        bounds: crate::domain::document::PageRect,
        screen_size: Vec2,
        pixels_per_point: f32,
    ) -> FitPageScrollMetrics {
        let context = egui::Context::default();
        let mut latest = None;
        for _ in 0..3 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen_size)),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(pixels_per_point);
            let _output = context.run_ui(input, |ui| {
                let available_size = ui.available_size();
                let zoom =
                    PrototypeApp::fit_zoom_for_page(bounds, available_size, ZoomMode::FitPage)
                        .unwrap();
                let geometry = single_page_geometry(bounds, zoom, available_size);
                let output = egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, visible_viewport| {
                        ui.set_min_size(geometry.content_size);
                        let page_screen_rect = Rect::from_min_size(
                            ui.max_rect().min + geometry.page_rect.min.to_vec2(),
                            geometry.page_rect.size(),
                        );
                        latest = Some(FitPageScrollMetrics {
                            available_size,
                            visible_viewport,
                            content_size: geometry.content_size,
                            page_content_rect: geometry.page_rect,
                            page_screen_rect,
                            clip_rect: ui.clip_rect(),
                        });
                    });
                assert_eq!(output.content_size, geometry.content_size);
            });
        }
        latest.unwrap()
    }

    #[test]
    fn landscape_fit_page_scroll_area_exposes_the_page_right_edge() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 1_280.0,
            y1: 720.0,
        };

        for pixels_per_point in [1.0, 1.25, 1.5, 2.0] {
            for screen_size in [
                Vec2::new(800.0, 600.0),
                Vec2::new(1_000.0, 600.0),
                Vec2::new(1_200.0, 700.0),
            ] {
                let metrics =
                    measure_single_page_scroll_area(bounds, screen_size, pixels_per_point);
                let grid = TileGrid::new(
                    bounds,
                    metrics.page_content_rect.width() / bounds.width() * pixels_per_point,
                )
                .unwrap();
                let specs = tile_specs_intersecting_viewport(
                    grid,
                    metrics.page_content_rect,
                    metrics.visible_viewport,
                )
                .unwrap();
                let rightmost = specs.iter().max_by_key(|spec| spec.pixel_x).unwrap();

                assert_eq!(metrics.content_size, metrics.available_size);
                assert!(
                    metrics
                        .visible_viewport
                        .contains(metrics.page_content_rect.right_top())
                );
                assert!(metrics.clip_rect.right() >= metrics.page_screen_rect.right());
                assert_eq!(
                    rightmost.pixel_x + rightmost.pixel_width,
                    grid.pixel_width()
                );
                assert_eq!(
                    logical_tile_rect(metrics.page_content_rect, grid, *rightmost).right(),
                    metrics.page_content_rect.right()
                );
            }
        }
    }

    #[test]
    fn landscape_fit_page_requests_the_rightmost_tile() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 1_280.0,
            y1: 720.0,
        };
        let page_rect = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(960.0, 540.0));
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 580.0));

        let (grid, specs) = requested_right_edge(bounds, 0.75, 1.25, page_rect, viewport);
        let rightmost = specs.iter().max_by_key(|spec| spec.pixel_x).unwrap();

        assert_eq!(
            rightmost.pixel_x + rightmost.pixel_width,
            grid.pixel_width()
        );
        assert_eq!(
            logical_tile_rect(page_rect, grid, *rightmost).right(),
            page_rect.right()
        );
    }

    #[test]
    fn fit_page_right_edge_handles_nonzero_page_and_layout_origins() {
        let bounds = crate::domain::document::PageRect {
            x0: 100.25,
            y0: 200.5,
            x1: 1_300.75,
            y1: 900.5,
        };
        let page_rect = Rect::from_min_size(Pos2::new(137.0, 41.0), Vec2::new(900.375, 525.0));
        let viewport = Rect::from_min_size(Pos2::new(100.0, 0.0), Vec2::new(980.0, 620.0));

        let (grid, specs) = requested_right_edge(bounds, 0.75, 1.5, page_rect, viewport);
        let rightmost = specs.iter().max_by_key(|spec| spec.pixel_x).unwrap();

        assert_eq!(
            rightmost.pixel_x + rightmost.pixel_width,
            grid.pixel_width()
        );
        assert_eq!(
            logical_tile_rect(page_rect, grid, *rightmost).right(),
            page_rect.right()
        );
    }

    #[test]
    fn square_and_portrait_fit_pages_keep_their_right_edges() {
        for (width, height) in [(700.0, 700.0), (600.0, 900.0)] {
            let bounds = crate::domain::document::PageRect {
                x0: 0.0,
                y0: 0.0,
                x1: width,
                y1: height,
            };
            let zoom = (760.0_f32 / width).min(560.0_f32 / height);
            let page_size = Vec2::new(width * zoom, height * zoom);
            let page_rect = Rect::from_center_size(Pos2::new(400.0, 300.0), page_size);
            let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

            let (grid, specs) = requested_right_edge(bounds, zoom, 2.0, page_rect, viewport);
            let rightmost = specs.iter().max_by_key(|spec| spec.pixel_x).unwrap();

            assert_eq!(
                rightmost.pixel_x + rightmost.pixel_width,
                grid.pixel_width()
            );
            assert_eq!(
                logical_tile_rect(page_rect, grid, *rightmost).right(),
                page_rect.right()
            );
        }
    }

    #[test]
    fn fit_page_zoom_recalculates_after_window_resize() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 1_280.0,
            y1: 720.0,
        };

        let wide =
            PrototypeApp::fit_zoom_for_page(bounds, Vec2::new(1_000.0, 600.0), ZoomMode::FitPage)
                .unwrap();
        let narrow =
            PrototypeApp::fit_zoom_for_page(bounds, Vec2::new(800.0, 600.0), ZoomMode::FitPage)
                .unwrap();

        assert_eq!(wide, (1_000.0 - PAGE_GAP * 2.0) / bounds.width());
        assert_eq!(narrow, (800.0 - PAGE_GAP * 2.0) / bounds.width());
        assert!(narrow < wide);
    }

    #[test]
    fn enlarged_landscape_page_does_not_request_every_tile() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 8_000.0,
            y1: 4_500.0,
        };
        let grid = TileGrid::new(bounds, 2.0).unwrap();
        let page_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(16_000.0, 9_000.0));
        let viewport = Rect::from_min_size(Pos2::new(4_000.0, 2_000.0), Vec2::new(1_000.0, 700.0));

        let requested = prioritized_tile_specs(grid, page_rect, viewport).unwrap();
        let full_grid_count = grid
            .specs_in_pixel_rect(0, 0, grid.pixel_width(), grid.pixel_height())
            .unwrap()
            .len();

        assert!(requested.len() < full_grid_count);
    }

    #[test]
    fn adjacent_enlarged_page_prefetches_only_the_transition_viewport() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 10_000.0,
        };
        let grid = TileGrid::new(bounds, 1.0).unwrap();
        let page_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 10_000.0));
        let transition_viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 400.0));

        let requested =
            tile_specs_intersecting_viewport(grid, page_rect, transition_viewport).unwrap();

        assert_eq!(requested.len(), 1);
        assert_eq!(requested[0].pixel_y, 0);
    }

    #[test]
    fn prefetched_tile_becomes_visible_when_viewport_reaches_it() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 2_000.0,
        };
        let grid = TileGrid::new(bounds, 1.0).unwrap();
        let page_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 2_000.0));
        let first_view = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 400.0));
        let later_view = Rect::from_min_size(Pos2::new(0.0, 500.0), Vec2::new(100.0, 400.0));

        let first = prioritized_tile_specs(grid, page_rect, first_view).unwrap();
        let later = prioritized_tile_specs(grid, page_rect, later_view).unwrap();
        let target = TileSpec {
            pixel_x: 0,
            pixel_y: 512,
            pixel_width: 100,
            pixel_height: 512,
        };

        assert_eq!(
            first.iter().find(|(spec, _)| *spec == target).unwrap().1,
            RenderPriority::NextViewport
        );
        assert_eq!(
            later.iter().find(|(spec, _)| *spec == target).unwrap().1,
            RenderPriority::Visible
        );
    }

    #[test]
    fn single_page_zoom_keeps_two_dimensional_page_anchor() {
        let viewport_size = Vec2::new(500.0, 400.0);
        let page_rect = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(2_000.0, 3_000.0));
        let content_size = page_rect.size() + Vec2::splat(40.0);
        let expected_anchor = Vec2::new(0.7, 0.6);

        let offset =
            single_page_centered_offset(page_rect, expected_anchor, viewport_size, content_size);
        let restored_anchor =
            normalized_page_point(page_rect, (offset + viewport_size / 2.0).to_pos2());

        assert!((restored_anchor.x - expected_anchor.x).abs() < f32::EPSILON);
        assert!((restored_anchor.y - expected_anchor.y).abs() < f32::EPSILON);
    }

    fn wheel_event(
        unit: MouseWheelUnit,
        delta: Vec2,
        phase: TouchPhase,
        control_held: bool,
    ) -> Event {
        Event::MouseWheel {
            unit,
            delta,
            phase,
            modifiers: Modifiers {
                ctrl: control_held,
                ..Modifiers::default()
            },
        }
    }

    #[test]
    fn discrete_single_page_wheel_uses_one_step_per_raw_event() {
        let events = vec![
            wheel_event(
                MouseWheelUnit::Line,
                Vec2::new(0.0, -3.0),
                TouchPhase::Move,
                false,
            ),
            wheel_event(
                MouseWheelUnit::Page,
                Vec2::new(0.0, -1.0),
                TouchPhase::Move,
                false,
            ),
        ];
        let mut state = SinglePageWheelState::default();

        assert_eq!(
            single_page_wheel_steps(&events, true, true, true, true, 1.0, &mut state),
            2
        );
    }

    #[test]
    fn point_wheel_accumulates_once_until_end_idle_or_reversal() {
        let small = [wheel_event(
            MouseWheelUnit::Point,
            Vec2::new(0.0, -12.0),
            TouchPhase::Move,
            false,
        )];
        let full = [wheel_event(
            MouseWheelUnit::Point,
            Vec2::new(0.0, -24.0),
            TouchPhase::Move,
            false,
        )];
        let end = [wheel_event(
            MouseWheelUnit::Point,
            Vec2::ZERO,
            TouchPhase::End,
            false,
        )];
        let reverse = [wheel_event(
            MouseWheelUnit::Point,
            Vec2::new(0.0, 24.0),
            TouchPhase::Move,
            false,
        )];
        let mut state = SinglePageWheelState::default();

        assert_eq!(
            single_page_wheel_steps(&small, true, true, true, true, 1.0, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&small, true, true, true, true, 1.01, &mut state),
            1
        );
        assert_eq!(
            single_page_wheel_steps(&full, true, true, true, true, 1.02, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&end, false, false, false, true, 1.03, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&full, true, true, true, true, 1.04, &mut state),
            1
        );
        assert_eq!(
            single_page_wheel_steps(&reverse, true, true, true, true, 1.05, &mut state),
            -1
        );
        assert_eq!(
            single_page_wheel_steps(&full, true, true, true, true, 1.30, &mut state),
            1
        );
    }

    #[test]
    fn single_page_wheel_ignores_control_horizontal_and_outside_input() {
        let control = [wheel_event(
            MouseWheelUnit::Line,
            Vec2::new(0.0, -1.0),
            TouchPhase::Move,
            true,
        )];
        let horizontal = [wheel_event(
            MouseWheelUnit::Line,
            Vec2::new(2.0, -1.0),
            TouchPhase::Move,
            false,
        )];
        let vertical = [wheel_event(
            MouseWheelUnit::Line,
            Vec2::new(0.0, -1.0),
            TouchPhase::Move,
            false,
        )];
        let mut state = SinglePageWheelState::default();

        assert_eq!(
            single_page_wheel_steps(&control, true, true, true, true, 1.0, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&horizontal, true, true, true, true, 1.0, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&vertical, false, true, true, true, 1.0, &mut state),
            0
        );
    }

    #[test]
    fn enlarged_page_changes_only_after_it_was_already_at_the_edge() {
        let events = vec![
            wheel_event(
                MouseWheelUnit::Line,
                Vec2::new(0.0, -1.0),
                TouchPhase::Move,
                false,
            ),
            wheel_event(
                MouseWheelUnit::Line,
                Vec2::new(0.0, -1.0),
                TouchPhase::Move,
                false,
            ),
        ];
        let mut state = SinglePageWheelState::default();

        assert_eq!(
            single_page_wheel_steps(&events, true, false, false, false, 1.0, &mut state),
            0
        );
        assert_eq!(
            single_page_wheel_steps(&events, true, false, true, false, 1.1, &mut state),
            1
        );
    }

    #[test]
    fn fit_width_scroll_area_uses_the_stored_bottom_for_wheel_transition() {
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 600.0,
            y1: 1_600.0,
        };
        let screen_size = Vec2::new(800.0, 600.0);
        let context = egui::Context::default();
        let mut reconstructed_bottom = false;
        let mut actual_bottom = false;

        for frame in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen_size)),
                ..Default::default()
            };
            let _output = context.run_ui(input, |ui| {
                let viewport_size = ui.available_size();
                let zoom =
                    PrototypeApp::fit_zoom_for_page(bounds, viewport_size, ZoomMode::FitWidth)
                        .unwrap();
                let geometry = single_page_geometry(bounds, zoom, viewport_size);
                let maximum_offset = (geometry.content_size - viewport_size).max(Vec2::ZERO);
                let id = scroll_area_state_id(ui, "fit-width-wheel-edge");
                let stored_offset = egui::scroll_area::State::load(ui.ctx(), id)
                    .map(|state| state.offset)
                    .unwrap_or(Vec2::ZERO);
                let starting_offset = clamp_scroll_offset(stored_offset, maximum_offset);
                reconstructed_bottom =
                    starting_offset.y >= maximum_offset.y - SINGLE_PAGE_EDGE_TOLERANCE_POINTS;

                let mut scroll_area = egui::ScrollArea::both()
                    .id_salt("fit-width-wheel-edge")
                    .auto_shrink([false, false]);
                if frame == 0 {
                    scroll_area = scroll_area.scroll_offset(Vec2::splat(f32::INFINITY));
                }
                let output = scroll_area.show_viewport(ui, |ui, _| {
                    ui.set_min_size(geometry.content_size);
                });
                let maximum_output_offset =
                    (output.content_size - output.inner_rect.size()).max(Vec2::ZERO);
                actual_bottom = output.state.offset.y
                    >= maximum_output_offset.y - SINGLE_PAGE_EDGE_TOLERANCE_POINTS;
            });
        }

        assert!(actual_bottom);
        assert_eq!(reconstructed_bottom, actual_bottom);
        let down = [wheel_event(
            MouseWheelUnit::Line,
            Vec2::new(0.0, -1.0),
            TouchPhase::Move,
            false,
        )];
        let mut wheel_state = SinglePageWheelState::default();
        assert_eq!(
            single_page_wheel_steps(
                &down,
                true,
                false,
                reconstructed_bottom,
                false,
                1.0,
                &mut wheel_state,
            ),
            1
        );
    }

    #[test]
    fn single_page_wheel_does_not_cross_document_boundaries() {
        assert_eq!(adjacent_page_index(0, 3, -1), None);
        assert_eq!(adjacent_page_index(0, 3, 1), Some(1));
        assert_eq!(adjacent_page_index(1, 3, -1), Some(0));
        assert_eq!(adjacent_page_index(2, 3, 1), None);
        assert_eq!(adjacent_page_index(0, 0, 1), None);
    }

    #[test]
    fn autoscroll_velocity_has_dead_zone_and_speed_ceiling() {
        let anchor = Pos2::new(100.0, 100.0);
        assert_eq!(AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND, 4_800.0);
        assert_eq!(
            autoscroll_velocity(anchor, Pos2::new(110.0, 100.0)),
            Vec2::ZERO
        );

        let moderate = autoscroll_velocity(anchor, Pos2::new(40.0, 100.0));
        let distant = autoscroll_velocity(anchor, Pos2::new(10_000.0, 100.0));
        assert!(moderate.x < 0.0);
        assert!(moderate.length() < distant.length());
        assert!(distant.length() <= AUTOSCROLL_MAX_SPEED_POINTS_PER_SECOND + f32::EPSILON);
    }

    #[test]
    fn single_page_mode_rejects_and_clears_autoscroll_state() {
        let mut view = ViewState::new();
        view.display_mode = DisplayMode::SinglePage;
        view.autoscroll = Some(AutoscrollState {
            anchor: Pos2::new(20.0, 30.0),
            requested_offset: Some(Vec2::new(40.0, 50.0)),
        });

        let frame = update_autoscroll(
            &egui::Context::default(),
            &mut view,
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0)),
            &[],
            LayerId::background(),
            Vec2::ZERO,
            Vec2::splat(1_000.0),
        );

        assert!(frame.is_none());
        assert!(view.autoscroll.is_none());
    }

    #[test]
    fn page_input_accepts_only_one_based_in_range_numbers() {
        assert_eq!(page_index_from_input(" 4 ", 5), Some(3));
        assert_eq!(page_index_from_input("", 5), None);
        assert_eq!(page_index_from_input("0", 5), None);
        assert_eq!(page_index_from_input("-1", 5), None);
        assert_eq!(page_index_from_input("abc", 5), None);
        assert_eq!(page_index_from_input("6", 5), None);
    }

    #[test]
    fn page_input_reserves_three_columns_and_expands_for_longer_documents() {
        for page_count in [1, 9, 10, 99, 100, 999] {
            assert_eq!(page_number_input_columns(page_count), 3);
        }
        assert_eq!(page_number_input_columns(1_000), 4);
        assert_eq!(page_number_input_columns(12_345), 5);
    }

    #[test]
    fn page_input_width_is_stable_through_three_digits() {
        let context = egui::Context::default();
        let mut widths = Vec::new();
        let _output = context.run_ui(Default::default(), |ui| {
            for page_count in [1, 9, 10, 99, 100, 999, 1_000] {
                widths.push(page_number_input_width(ui, page_count));
            }
        });

        assert!(widths[..6].windows(2).all(|pair| pair[0] == pair[1]));
        assert!(widths[6] >= widths[5]);
    }

    #[test]
    fn page_navigation_preserves_the_render_generation() {
        let path = PathBuf::from("missing.pdf");
        let mut tab = DocumentTab::new(1, path.clone(), 0, None, false);
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 200.0,
        };
        tab.info = Some(DocumentInfo {
            path,
            page_bounds: vec![bounds; 2],
            highlight_count: 0,
            can_save_incrementally: false,
            highlight_capability: crate::domain::document::HighlightCapability::Allowed,
            dirty: false,
            revision: 0,
            open_time: Duration::ZERO,
            physical_memory_bytes: None,
            version: crate::domain::document::DocumentVersion {
                identity_primary: 0,
                identity_secondary: 0,
                length: 0,
                modified: std::time::SystemTime::UNIX_EPOCH,
            },
        });
        let generation = tab.view.generation;

        tab.jump_to_page(1);

        assert_eq!(tab.view.current_page, 1);
        assert_eq!(tab.view.generation, generation);
    }

    #[test]
    fn display_mode_roundtrip_preserves_page_and_normalized_position() {
        let expected = PageAnchor {
            page_index: 4,
            page_x_fraction: 0.7,
            page_y_fraction: 0.75,
        };
        let mut view = ViewState {
            display_mode: DisplayMode::Continuous,
            zoom_mode: ZoomMode::Fixed,
            zoom: 2.0,
            current_page: 3,
            scroll_to_page: None,
            center_anchor: Some(expected),
            restore_anchor: None,
            single_center_anchor: None,
            restore_single_anchor: None,
            single_wheel: SinglePageWheelState::default(),
            autoscroll: None,
            pan_requested_offset: None,
            render_pixels_per_point_bits: None,
            generation: 1,
        };
        view.autoscroll = Some(AutoscrollState {
            anchor: Pos2::new(10.0, 20.0),
            requested_offset: Some(Vec2::new(30.0, 40.0)),
        });
        view.pan_requested_offset = Some(Vec2::new(50.0, 60.0));

        assert!(view.switch_display_mode(DisplayMode::SinglePage));
        assert!(view.autoscroll.is_none());
        assert!(view.pan_requested_offset.is_none());
        assert_eq!(view.current_page, expected.page_index);
        assert_eq!(
            view.restore_single_anchor,
            Some(Vec2::new(
                expected.page_x_fraction,
                expected.page_y_fraction
            ))
        );

        assert!(view.switch_display_mode(DisplayMode::Continuous));
        assert_eq!(view.restore_anchor, Some(expected));
    }

    #[test]
    fn suspension_chooses_oldest_inactive_fully_suspendable_document() {
        let candidates = [(true, 2), (false, 1), (true, 3), (false, 0)];

        assert_eq!(oldest_suspendable_index(Some(0), &candidates), Some(2));
        assert_eq!(oldest_suspendable_index(Some(2), &candidates), Some(0));
    }

    #[test]
    fn suspension_skips_oldest_document_while_it_is_printing() {
        let candidates = [(false, 0), (true, 1), (true, 2)];

        assert_eq!(oldest_suspendable_index(Some(2), &candidates), Some(1));
    }

    #[test]
    fn worker_disconnect_clears_every_close_blocking_operation() {
        let mut tab = DocumentTab::new(1, PathBuf::from("missing.pdf"), 0, None, false);
        tab.state = DocumentState::Saving;
        tab.pending_edits = 1;
        tab.pending_annotation_pages.insert(AnnotationPageRequest {
            page_index: 0,
            expected_revision: 0,
        });
        tab.undo_in_flight = true;
        tab.save_in_flight = true;
        tab.print_in_flight = true;

        tab.mark_worker_disconnected();

        assert_eq!(tab.pending_edits, 0);
        assert!(tab.pending_annotation_pages.is_empty());
        assert!(!tab.undo_in_flight);
        assert!(!tab.is_saving());
        assert!(!tab.is_printing());
        assert_eq!(tab.state, DocumentState::Error);
        assert!(tab.service.is_none());
    }

    #[test]
    fn worker_errors_have_japanese_guidance_and_keep_diagnostic_detail() {
        let message = document_failure_message("save", "permission denied");

        assert!(message.starts_with("PDFを保存できませんでした。"));
        assert!(message.contains("書き込み権限"));
        assert!(message.ends_with("詳細: permission denied"));
    }

    #[test]
    fn queued_save_blocks_close_until_document_returns_clean() {
        let after_highlight_event = state_after_document_info(DocumentState::Saving, true);
        let save_in_flight = true;
        assert_eq!(after_highlight_event, DocumentState::Saving);
        assert!(document_save_blocks_close(save_in_flight));

        let after_info_failure = DocumentState::Error;
        assert_eq!(after_info_failure, DocumentState::Error);
        assert!(document_save_blocks_close(save_in_flight));

        let after_save_event = state_after_document_info(after_highlight_event, false);
        assert_eq!(after_save_event, DocumentState::ReadyClean);
        assert!(!document_save_blocks_close(false));
    }

    #[test]
    fn search_starts_at_current_page_and_alternates_forward_and_backward() {
        assert_eq!(search_page_order(3, 7), [3, 4, 2, 5, 1, 6, 0]);
        assert!(search_page_order(0, 0).is_empty());
    }

    #[test]
    fn search_navigation_visits_each_logical_match_and_wraps() {
        let search_match = || SearchMatch { quads: Vec::new() };
        let pages = BTreeMap::from([
            (1, vec![search_match()]),
            (4, vec![search_match(), search_match()]),
            (8, vec![search_match()]),
        ]);
        let first_on_page_four = SearchCursor {
            page_index: 4,
            match_index: 0,
        };
        let second_on_page_four = SearchCursor {
            page_index: 4,
            match_index: 1,
        };

        assert_eq!(
            next_search_match(&pages, None, 4, true),
            Some(first_on_page_four)
        );
        assert_eq!(
            next_search_match(&pages, Some(first_on_page_four), 4, true),
            Some(second_on_page_four)
        );
        assert_eq!(
            next_search_match(&pages, Some(second_on_page_four), 4, false),
            Some(first_on_page_four)
        );
        assert_eq!(
            next_search_match(
                &pages,
                Some(SearchCursor {
                    page_index: 8,
                    match_index: 0,
                }),
                8,
                true,
            ),
            Some(SearchCursor {
                page_index: 1,
                match_index: 0,
            })
        );
        assert_eq!(search_match_ordinal(&pages, second_on_page_four), Some(3));
    }

    #[test]
    fn multi_quad_search_match_uses_the_union_center() {
        let search_match = SearchMatch {
            quads: vec![
                PageQuad {
                    upper_left: PagePoint::new(10.0, 20.0),
                    upper_right: PagePoint::new(30.0, 20.0),
                    lower_left: PagePoint::new(10.0, 30.0),
                    lower_right: PagePoint::new(30.0, 30.0),
                },
                PageQuad {
                    upper_left: PagePoint::new(50.0, 60.0),
                    upper_right: PagePoint::new(70.0, 60.0),
                    lower_left: PagePoint::new(50.0, 80.0),
                    lower_right: PagePoint::new(70.0, 80.0),
                },
            ],
        };
        let bounds = crate::domain::document::PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };

        let anchor = search_match_anchor(2, &search_match, bounds).unwrap();

        assert_eq!(anchor.page_index, 2);
        assert!((anchor.page_x_fraction - 0.4).abs() < f32::EPSILON);
        assert!((anchor.page_y_fraction - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn stale_search_result_is_rejected_by_generation_and_revision() {
        assert!(search_result_is_current(4, 4, 2, Some(2)));
        assert!(!search_result_is_current(3, 4, 2, Some(2)));
        assert!(!search_result_is_current(4, 4, 1, Some(2)));
    }

    #[test]
    fn thumbnail_failure_blocks_only_the_failed_request() {
        let failed_key = ThumbnailCacheKey::for_page(1, 2, 3);
        let other_key = ThumbnailCacheKey::for_page(1, 4, 3);
        let mut pending = HashSet::from([failed_key, other_key]);
        let mut failed = HashSet::new();

        mark_thumbnail_failed(&mut pending, &mut failed, failed_key);

        assert!(!pending.contains(&failed_key));
        assert!(pending.contains(&other_key));
        assert!(failed.contains(&failed_key));
    }

    #[test]
    fn text_snapshot_result_requires_active_visible_current_page() {
        let key = TextSnapshotKey {
            page_index: 2,
            revision: 4,
        };
        let wanted = HashSet::from([key]);

        assert!(text_snapshot_result_is_current(
            true,
            key,
            Some(4),
            3,
            &wanted
        ));
        assert!(!text_snapshot_result_is_current(
            false,
            key,
            Some(4),
            3,
            &wanted
        ));
        assert!(!text_snapshot_result_is_current(
            true,
            key,
            Some(3),
            3,
            &wanted
        ));
        assert!(!text_snapshot_result_is_current(
            true,
            key,
            Some(4),
            2,
            &wanted
        ));
        assert!(!text_snapshot_result_is_current(
            true,
            key,
            Some(4),
            3,
            &HashSet::new()
        ));
    }

    #[test]
    fn annotation_result_requires_active_visible_current_revision() {
        let request = AnnotationPageRequest {
            page_index: 2,
            expected_revision: 4,
        };
        let wanted = HashSet::from([request]);

        assert!(annotation_page_result_is_current(
            true,
            request,
            Some(4),
            &wanted
        ));
        assert!(!annotation_page_result_is_current(
            false,
            request,
            Some(4),
            &wanted
        ));
        assert!(!annotation_page_result_is_current(
            true,
            request,
            Some(3),
            &wanted
        ));
        assert!(!annotation_page_result_is_current(
            true,
            request,
            Some(4),
            &HashSet::new()
        ));
    }

    #[test]
    fn text_snapshot_failure_is_cleared_after_page_leaves_visible_scope() {
        let failed_key = TextSnapshotKey {
            page_index: 2,
            revision: 4,
        };
        let next_key = TextSnapshotKey {
            page_index: 3,
            revision: 4,
        };
        let mut failed = HashSet::from([failed_key]);
        let mut error = Some("text snapshot: extraction failed".to_owned());
        let wanted = HashSet::from([next_key]);

        retain_visible_text_failures(&mut failed, &mut error, &wanted);

        assert!(failed.is_empty());
        assert!(error.is_none());
    }

    #[test]
    fn clearing_text_snapshot_failures_preserves_unrelated_error() {
        let mut failed = HashSet::new();
        let mut error = Some("document save: permission denied".to_owned());

        retain_visible_text_failures(&mut failed, &mut error, &HashSet::new());

        assert_eq!(error.as_deref(), Some("document save: permission denied"));
    }

    #[test]
    fn continuous_session_view_restores_anchor_and_clamps_shorter_document() {
        let saved = SessionView {
            page_index: 9,
            page_x: 0.25,
            page_y: 0.75,
            display: SessionDisplayMode::Continuous,
            zoom_mode: SessionZoomMode::Fixed,
            zoom: 1.5,
        };
        let mut view = ViewState::from_session(saved);

        view.clamp_to_page_count(4);
        let restored = view.to_session();

        assert_eq!(restored.page_index, 3);
        assert_eq!(restored.page_x, 0.25);
        assert_eq!(restored.page_y, 0.75);
        assert_eq!(restored.display, SessionDisplayMode::Continuous);
        assert_eq!(restored.zoom_mode, SessionZoomMode::Fixed);
        assert_eq!(restored.zoom, 1.5);
    }

    #[test]
    fn single_page_session_view_preserves_fit_mode_and_two_axis_anchor() {
        let saved = SessionView {
            page_index: 6,
            page_x: 0.2,
            page_y: 0.8,
            display: SessionDisplayMode::SinglePage,
            zoom_mode: SessionZoomMode::FitPage,
            zoom: 0.75,
        };

        let restored = ViewState::from_session(saved).to_session();

        assert_eq!(restored.page_index, 6);
        assert_eq!(restored.page_x, 0.2);
        assert_eq!(restored.page_y, 0.8);
        assert_eq!(restored.display, SessionDisplayMode::SinglePage);
        assert_eq!(restored.zoom_mode, SessionZoomMode::FitPage);
        assert_eq!(restored.zoom, 0.75);
    }

    #[test]
    fn startup_restores_fifty_one_tabs_in_order_and_selects_saved_tab() {
        let directory = tempfile::tempdir().unwrap();
        let paths = (0..51)
            .map(|index| {
                let path = directory.path().join(format!("{index:02}.pdf"));
                write_blank_pdf(&path);
                std::fs::canonicalize(path).unwrap()
            })
            .collect::<Vec<_>>();
        let state = SessionState {
            selected_tab: Some(12),
            tabs: paths
                .iter()
                .cloned()
                .map(|path| saved_tab(path, 0))
                .collect(),
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let mut app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path));
        finish_async_session_restore(&mut app);

        let restored_paths = app
            .tabs
            .tabs()
            .iter()
            .map(|tab| tab.path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(restored_paths, paths);
        assert_eq!(app.active_index(), Some(12));
    }

    #[test]
    fn startup_does_not_restore_tabs_when_session_restore_is_disabled() {
        let directory = tempfile::tempdir().unwrap();
        let saved = directory.path().join("saved.pdf");
        write_blank_pdf(&saved);
        let state = SessionState {
            restore_enabled: false,
            selected_tab: Some(0),
            tabs: vec![saved_tab(std::fs::canonicalize(saved).unwrap(), 0)],
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path));

        assert!(!app.restore_enabled);
        assert!(app.documents.is_empty());
        assert!(app.tabs.tabs().is_empty());
        assert!(app.session_restore_progress.is_none());
    }

    #[test]
    fn failed_initial_restore_is_removed_and_remaining_tab_stays_selected() {
        let directory = tempfile::tempdir().unwrap();
        let inaccessible = directory.path().join("unreadable.pdf");
        std::fs::write(&inaccessible, b"not a PDF").unwrap();
        let valid = directory.path().join("valid.pdf");
        write_blank_pdf(&valid);
        let inaccessible = std::fs::canonicalize(inaccessible).unwrap();
        let valid = std::fs::canonicalize(valid).unwrap();
        let state = SessionState {
            selected_tab: Some(0),
            tabs: vec![saved_tab(inaccessible, 0), saved_tab(valid.clone(), 0)],
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let mut app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path));
        assert!(app.session_restore_progress.is_some());
        finish_async_session_restore(&mut app);

        assert_eq!(app.documents.len(), 1);
        assert_eq!(app.tabs.tabs()[0].path(), valid);
        assert_eq!(app.active_index(), Some(0));
    }

    #[test]
    fn closing_restored_tab_consumes_pending_restore_result() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.pdf");
        let second = directory.path().join("second.pdf");
        write_blank_pdf(&first);
        write_blank_pdf(&second);
        let state = SessionState {
            selected_tab: Some(0),
            tabs: vec![
                saved_tab(std::fs::canonicalize(first).unwrap(), 0),
                saved_tab(std::fs::canonicalize(second).unwrap(), 0),
            ],
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let mut app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path));
        assert_eq!(
            app.session_restore_progress.as_ref().map(|p| p.pending),
            Some(2)
        );
        app.close_tab(0);

        assert_eq!(app.documents.len(), 1);
        assert_eq!(
            app.session_restore_progress.as_ref().map(|p| p.pending),
            Some(1)
        );
        finish_async_session_restore(&mut app);
    }

    #[test]
    fn window_close_waits_for_pending_session_restore_before_saving() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("restore.pdf");
        write_blank_pdf(&path);
        let state = SessionState {
            restore_enabled: true,
            selected_tab: Some(0),
            tabs: vec![saved_tab(std::fs::canonicalize(path).unwrap(), 0)],
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let mut app =
            PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path.clone()));
        assert!(app.session_restore_progress.is_some());
        app.restore_enabled = false;
        app.window_close_pending = true;
        app.prompt_next_window_document(&egui::Context::default());

        let saved = SessionStore::new(session_path).load().unwrap().unwrap();
        assert!(saved.restore_enabled);
        assert!(app.window_close_pending);
        assert!(!app.allow_window_close);
        finish_async_session_restore(&mut app);
    }

    #[test]
    fn explicit_cli_pdf_takes_precedence_over_saved_session() {
        let directory = tempfile::tempdir().unwrap();
        let saved = directory.path().join("saved.pdf");
        let explicit = directory.path().join("explicit.pdf");
        write_blank_pdf(&saved);
        write_blank_pdf(&explicit);
        let saved = std::fs::canonicalize(saved).unwrap();
        let explicit = std::fs::canonicalize(explicit).unwrap();
        let state = SessionState {
            selected_tab: Some(0),
            tabs: vec![saved_tab(saved, 0)],
            ..SessionState::default()
        };
        let session_path = directory.path().join("session.json");
        SessionStore::new(session_path.clone())
            .save(&state)
            .unwrap();

        let app =
            PrototypeApp::from_startup(vec![explicit.clone()], SessionStore::new(session_path));

        assert_eq!(app.documents.len(), 1);
        assert_eq!(app.tabs.tabs()[0].path(), explicit);
        assert!(app.session_restore_progress.is_none());
    }

    #[test]
    fn highlight_index_batches_only_the_first_contiguous_missing_pages() {
        let mut state = HighlightIndexState {
            started: true,
            revision: Some(7),
            total_pages: 40,
            ..HighlightIndexState::default()
        };

        let first = next_highlight_index_request(&state).unwrap();
        assert_eq!(first.first_page, 0);
        assert_eq!(first.page_count, HIGHLIGHT_INDEX_BATCH_PAGES);

        for page_index in 0..HIGHLIGHT_INDEX_BATCH_PAGES {
            state.pages.insert(page_index, Vec::new());
        }
        state
            .pages
            .insert(HIGHLIGHT_INDEX_BATCH_PAGES + 2, Vec::new());
        let second = next_highlight_index_request(&state).unwrap();
        assert_eq!(second.first_page, HIGHLIGHT_INDEX_BATCH_PAGES);
        assert_eq!(second.page_count, 2);
    }

    #[test]
    fn edited_highlight_page_is_refreshed_as_one_page_batch() {
        let mut state = HighlightIndexState {
            generation: 4,
            revision: Some(8),
            total_pages: 100,
            refresh_page: Some(73),
            started: true,
            ..HighlightIndexState::default()
        };
        state.pages.insert(0, Vec::new());

        let request = next_highlight_index_request(&state).unwrap();

        assert_eq!(
            request,
            HighlightIndexRequest {
                generation: 4,
                expected_revision: 8,
                first_page: 73,
                page_count: 1,
            }
        );
    }

    #[test]
    fn highlight_index_replaces_repeated_pages_and_rejects_stale_batches() {
        let request = HighlightIndexRequest {
            generation: 3,
            expected_revision: 7,
            first_page: 0,
            page_count: 1,
        };
        let mut state = HighlightIndexState {
            generation: 3,
            revision: Some(7),
            total_pages: 2,
            in_flight: Some(request),
            started: true,
            ..HighlightIndexState::default()
        };
        let page = crate::domain::annotation::HighlightIndexPage {
            page_index: 0,
            highlights: Vec::new(),
            scan_time: Duration::ZERO,
        };

        assert!(apply_highlight_index_batch(
            &mut state,
            HighlightIndexBatch {
                generation: 3,
                revision: 7,
                total_pages: 2,
                pages: vec![page.clone()],
            }
        ));
        assert_eq!(state.pages.len(), 1);

        state.in_flight = Some(request);
        assert!(apply_highlight_index_batch(
            &mut state,
            HighlightIndexBatch {
                generation: 3,
                revision: 7,
                total_pages: 2,
                pages: vec![page],
            }
        ));
        assert_eq!(state.pages.len(), 1);

        state.in_flight = Some(HighlightIndexRequest {
            generation: 4,
            ..request
        });
        assert!(!apply_highlight_index_batch(
            &mut state,
            HighlightIndexBatch {
                generation: 3,
                revision: 7,
                total_pages: 2,
                pages: Vec::new(),
            }
        ));
        assert_eq!(state.pages.len(), 1);
    }
}
