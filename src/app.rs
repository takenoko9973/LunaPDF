use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::TryRecvError;
use eframe::egui::{
    self, Color32, Id, Key, Modifiers, Pos2, Rect, TextureHandle, TextureOptions, UiBuilder, Vec2,
    ViewportCommand,
};

use crate::domain::document::{
    DocumentInfo, HighlightRequest, OutlineItem, RenderPriority, RenderedThumbnail, RenderedTile,
    SearchPageResult, ThumbnailRequest, TileRequest, TileSpec,
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
use crate::ui::sidebar::{SidebarTab, show_outline};
use crate::ui::viewport::PageViewport;

// These bounds cover detailed inspection and overview use without allowing an
// accidental wheel gesture to request an unbounded raster allocation.
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;

// Fit modes can change by sub-pixel rounding as panel sizes settle. Ignoring a
// difference below one tenth of a percent avoids invalidating every page on
// visually identical consecutive frames.
const ZOOM_CHANGE_EPSILON: f32 = 0.001;

// The design budget is shared across all tabs so the active document can use
// available GPU memory instead of receiving one twentieth of a fixed split.
const GPU_TILE_BUDGET_BYTES: usize = 192 * 1_024 * 1_024;

// Thumbnails have their own budget so a long sidebar cannot evict the active
// page's display tiles from the 192 MiB rendering cache.
const THUMBNAIL_BUDGET_BYTES: usize = 32 * 1_024 * 1_024;
const THUMBNAIL_MAX_WIDTH: u32 = 160;
const THUMBNAIL_MAX_HEIGHT: u32 = 220;
const THUMBNAIL_ROW_HEIGHT: f32 = 248.0;

// N-05 sets 512 MiB as the stable 20-tab process target. Suspension is only
// allowed after crossing that limit; ordinary tab switches retain documents.
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
    session_close_failure: Option<String>,
    saved_tab_to_close: Option<PathBuf>,
    session_store: SessionStore,
    restore_enabled: bool,
    session_restore_progress: Option<SessionRestoreProgress>,
    next_document_id: u64,
    activity_sequence: u64,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    gpu_lru: WeightedLruCache<TileCacheKey, ()>,
    thumbnail_lru: WeightedLruCache<ThumbnailCacheKey, ()>,
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
    text_snapshots: HashMap<TextSnapshotKey, TextPageSnapshot>,
    pending_text_snapshots: HashSet<TextSnapshotKey>,
    failed_text_snapshots: HashSet<TextSnapshotKey>,
    wanted_text_snapshots: HashSet<TextSnapshotKey>,
    selection: Option<SelectionSnapshot>,
    selection_generation: u64,
    pending_highlights: usize,
    save_in_flight: bool,
    thumbnails: HashMap<ThumbnailCacheKey, CachedThumbnail>,
    pending_thumbnails: HashSet<ThumbnailCacheKey>,
    failed_thumbnails: HashSet<ThumbnailCacheKey>,
    thumbnail_generation: u64,
    search: SearchState,
    view: ViewState,
    restoring_from_session: bool,
    select_after_restore: bool,
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

#[derive(Default)]
struct SearchState {
    open: bool,
    query: String,
    generation: u64,
    pages: BTreeMap<usize, Vec<crate::domain::selection::PageQuad>>,
    completed_pages: usize,
    truncated: bool,
    in_progress: bool,
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
    generation: u64,
}

impl PrototypeApp {
    /// Creates the application and opens each command-line PDF up to the cap.
    pub(crate) fn new(
        _creation_context: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        session_store: SessionStore,
    ) -> Self {
        Self::from_startup(paths, session_store)
    }

    fn from_startup(paths: Vec<PathBuf>, session_store: SessionStore) -> Self {
        let (saved_session, session_load_error) = match session_store.load() {
            Ok(session) => (session, None),
            Err(error) => (None, Some(format!("session restore skipped: {error}"))),
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
            session_close_failure: None,
            saved_tab_to_close: None,
            session_store,
            restore_enabled,
            session_restore_progress: None,
            next_document_id: 1,
            activity_sequence: 0,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Outline,
            gpu_lru: WeightedLruCache::new(GPU_TILE_BUDGET_BYTES),
            thumbnail_lru: WeightedLruCache::new(THUMBNAIL_BUDGET_BYTES),
        };
        if paths.is_empty() && restore_enabled {
            if let Some(session) = saved_session {
                app.restore_session(session);
            }
        } else {
            // Explicit command-line files take precedence so a full saved
            // session cannot consume the tab cap before the requested PDFs.
            for path in paths {
                app.open_document(path);
            }
        }
        app
    }

    fn open_document(&mut self, path: PathBuf) {
        let _opened_index = self.open_document_with_intent(path, OpenIntent::User);
    }

    fn restore_session(&mut self, session: SessionState) {
        self.sidebar_open = session.sidebar_open;
        self.sidebar_tab = match session.sidebar_tab {
            SessionSidebarTab::Outline => SidebarTab::Outline,
            SessionSidebarTab::Thumbnails => SidebarTab::Thumbnails,
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
                self.error = Some(format!("not a PDF file: {}", path.display()));
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
                    .expect("twenty-tab document IDs cannot exhaust u64");
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
            Ok(OpenTabResult::LimitReached) => {
                if report_to_user {
                    self.error =
                        Some("tab limit reached (20); no existing tab was closed".to_owned());
                }
                None
            }
            Err(error) => {
                if report_to_user {
                    self.error = Some(format!("open {}: {error}", path.display()));
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
                        if !self.documents[index].outline_requested
                            && self.documents[index].send(DocumentCommand::LoadOutline)
                        {
                            self.documents[index].outline_requested = true;
                        }
                        if self.active_index() == Some(index)
                            && self.documents[index].search.open
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
                        let saved_path = (!info.dirty).then(|| info.path.clone());
                        let restart_search = self.active_index() == Some(index)
                            && self.close_confirmation.is_none()
                            && !self.window_close_pending
                            && self.documents[index].search.open
                            && !self.documents[index].search.query.trim().is_empty();
                        let tab = &mut self.documents[index];
                        if info.dirty {
                            tab.pending_highlights = tab.pending_highlights.saturating_sub(1);
                            tab.state = state_after_document_info(tab.state, true);
                        } else {
                            tab.save_in_flight = false;
                            tab.state = state_after_document_info(tab.state, false);
                        }
                        tab.info = Some(info);
                        tab.invalidate_rendering();
                        tab.invalidate_text_snapshots();
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
                        tab.pending_tiles.remove(&key);
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
                                "render: document worker returned an invalid tile payload"
                                    .to_owned(),
                            );
                            continue;
                        }

                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [
                                tile.spec.pixel_width as usize,
                                tile.spec.pixel_height as usize,
                            ],
                            &tile.pixels_rgba,
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
                        // Texture storage owns the upload copy; retaining the
                        // transfer buffer would double-count the page cache.
                        tile.pixels_rgba.clear();
                        let weight = tile
                            .spec
                            .rgba_bytes()
                            .expect("validated tile dimensions fit in memory");
                        tab.tiles.insert(key, CachedTile { tile, texture });
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
                            tab.error = Some(format!("text snapshot: {message}"));
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

                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [
                                thumbnail.pixel_width as usize,
                                thumbnail.pixel_height as usize,
                            ],
                            &thumbnail.pixels_rgba,
                        );
                        let texture = context.load_texture(
                            format!(
                                "pdf-{}-thumbnail-{}-revision-{}",
                                tab.document_id, thumbnail.page_index, thumbnail.revision
                            ),
                            image,
                            TextureOptions::LINEAR,
                        );
                        thumbnail.pixels_rgba.clear();
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
                        tab.error = Some(format!("thumbnail: {message}"));
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
                        self.documents[index].error = Some(format!("{operation}: {message}"));
                        if operation == "highlight" {
                            let tab = &mut self.documents[index];
                            tab.pending_highlights = tab.pending_highlights.saturating_sub(1);
                            if !tab.has_unsaved_changes() && !tab.is_saving() {
                                tab.state = DocumentState::ReadyClean;
                            }
                        }
                        if operation == "highlight-state" {
                            let tab = &mut self.documents[index];
                            tab.pending_highlights = tab.pending_highlights.saturating_sub(1);
                            // MuPDF already created the annotation; only the
                            // follow-up snapshot failed, so remain dirty.
                            tab.state = DocumentState::ReadyDirty;
                        }
                        if operation == "search" {
                            // A page error normally repeats for every queued page.
                            // Advancing the generation stops the remaining work instead
                            // of flooding the UI with identical failures.
                            self.cancel_search(index, false);
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
                        let tab = &mut self.documents[index];
                        tab.save_in_flight = false;
                        if tab.state != DocumentState::Error {
                            tab.error = Some("document worker terminated".to_owned());
                            tab.state = DocumentState::Error;
                        }
                        tab.service = None;
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
            && !self.documents.iter().any(DocumentTab::is_saving)
        {
            self.prompt_next_window_document(context);
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
        let find_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::F));
        if find_pressed && let Some(index) = self.active_index() {
            self.documents[index].search.open = true;
            let document_id = self.documents[index].document_id;
            context.memory_mut(|memory| {
                memory.request_focus(Id::new(("pdf-search-query", document_id)));
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
            self.cancel_search(index, true);
        }

        let next_tab = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Tab));
        if next_tab {
            self.select_next_tab();
        }

        let page_up = context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::PageUp));
        if page_up {
            self.move_page(-1);
        }
        let page_down =
            context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::PageDown));
        if page_down {
            self.move_page(1);
        }

        let zoom_delta = context.input(|input| input.zoom_delta());
        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.zoom_by(zoom_delta);
        }

        let close_flow_active = self.window_close_pending || self.close_confirmation.is_some();
        let save_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::S));
        if save_pressed && !close_flow_active {
            self.save();
        }
        let highlight_pressed =
            context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::H));
        if highlight_pressed && !close_flow_active {
            self.create_highlight();
        }
        let copy_pressed = context.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::C));
        if copy_pressed {
            self.copy_selection(context);
        }
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
            self.cancel_search(index, false);
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
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = true;
        if !tab.send(DocumentCommand::SetSearchGeneration(generation)) {
            tab.search.in_progress = false;
            tab.error = Some("search: document worker is unavailable".to_owned());
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

    fn cancel_search(&mut self, index: usize, close_bar: bool) {
        let tab = &mut self.documents[index];
        tab.search.generation = tab.search.generation.wrapping_add(1);
        let _queued = tab.send(DocumentCommand::SetSearchGeneration(tab.search.generation));
        tab.search.pages.clear();
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = false;
        if close_bar {
            tab.search.open = false;
        }
    }

    fn navigate_search(&mut self, index: usize, forward: bool) {
        let current_page = self.documents[index].view.current_page;
        let page = next_search_page(
            self.documents[index].search.pages.keys().copied(),
            current_page,
            forward,
        );
        if let Some(page) = page {
            self.documents[index].jump_to_page(page);
            self.status = format!("Search result on page {}", page + 1);
        }
    }

    fn activate_document(&mut self, index: usize, previous: Option<usize>) {
        if let Some(previous) = previous.filter(|previous| *previous != index) {
            // Pending work is invalidated immediately. Completed textures remain
            // reusable only while the shared byte-budget LRU can retain them.
            self.documents[previous].invalidate_rendering();
            self.documents[previous].invalidate_text_snapshots();
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
        } else if self.documents[index].search.open
            && !self.documents[index].search.query.trim().is_empty()
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

        let states = self
            .documents
            .iter()
            .map(|document| (document.state, document.last_selected_sequence))
            .collect::<Vec<_>>();
        let Some(index) = oldest_suspendable_index(self.active_index(), &states) else {
            return;
        };
        if !self.documents[index].is_suspendable() {
            return;
        }

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

    fn close_tab(&mut self, index: usize) {
        let Some(document) = self.documents.get(index) else {
            return;
        };
        // A queued save precedes Shutdown on the worker command
        // queue, so Discard cannot honestly cancel it. Wait for completion.
        if document.is_saving() {
            self.status = "Waiting for the current save before closing…".to_owned();
            return;
        }
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

    fn close_tab_now(&mut self, index: usize) {
        if self.remove_tab_now(index) {
            self.status = "Tab closed".to_owned();
        }
    }

    fn remove_tab_now(&mut self, index: usize) -> bool {
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
        if !result.quads.is_empty() {
            tab.search.pages.insert(result.page_index, result.quads);
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
        self.window_close_pending = true;
        if self.close_confirmation.is_none() {
            self.prompt_next_window_document(context);
        }
    }

    fn prompt_next_window_document(&mut self, context: &egui::Context) {
        if self.session_close_failure.is_some() {
            return;
        }
        if self.documents.iter().any(DocumentTab::is_saving) {
            self.status = "Waiting for the current save before closing…".to_owned();
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
            self.session_close_failure = Some(format!("session save failed: {error}"));
            self.status = "Session could not be saved; choose how to continue".to_owned();
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
                    self.error = Some("save: document worker is unavailable".to_owned());
                }
            }
            CloseDecision::Discard => {
                self.close_confirmation = None;
                match scope {
                    CloseScope::Tab => self.close_tab_by_path(&path),
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
                self.status = "Close canceled".to_owned();
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
            ui.heading("Unsaved Highlight annotations");
            ui.label(format!("Save changes to {file_name}?"));
            ui.horizontal(|ui| {
                let save = ui
                    .add_enabled(!save_in_flight, egui::Button::new("Save"))
                    .clicked()
                    .then_some(CloseDecision::Save);
                let discard = ui
                    .add_enabled(!save_in_flight, egui::Button::new("Discard"))
                    .clicked()
                    .then_some(CloseDecision::Discard);
                let cancel = ui
                    .button(if save_in_flight {
                        "Keep open"
                    } else {
                        "Cancel"
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
            ui.heading("Session could not be saved");
            ui.label(&message);
            ui.label("The PDF documents were not changed by this failure.");
            ui.horizontal(|ui| {
                let retry = ui
                    .button("Retry")
                    .clicked()
                    .then_some(SessionCloseDecision::Retry);
                let exit = ui
                    .button("Exit without session")
                    .clicked()
                    .then_some(SessionCloseDecision::ExitWithoutSession);
                let cancel = ui
                    .button("Cancel")
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
        tab.view.current_page = target;
        tab.view.scroll_to_page = Some(target);
        tab.view.restore_anchor = None;
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

    fn create_highlight(&mut self) {
        if self.window_close_pending || self.close_confirmation.is_some() {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let highlight_capability = tab.info.as_ref().map(|info| info.highlight_capability);
        let Some(highlight_capability) = highlight_capability else {
            return;
        };
        if let Some(restriction) = highlight_capability.restriction() {
            // The adapter reports a concrete restriction; the UI does not
            // invent a save fallback that could leave an unsavable dirty tab.
            self.error = Some(format!("highlight: {restriction}; editing is disabled"));
            return;
        }
        let Some(selection) = &tab.selection else {
            self.error = Some("highlight: select text before creating a Highlight".to_owned());
            return;
        };
        if selection.quads.is_empty() {
            self.error = Some("highlight: MuPDF returned no selection Quads".to_owned());
            return;
        }

        let request = HighlightRequest {
            page_index: selection.page_index,
            quads: selection.quads.clone(),
        };
        if tab.send(DocumentCommand::CreateHighlight(request)) {
            // This local pending count closes the race before MuPDF reports its
            // dirty flag back from the document worker.
            tab.pending_highlights += 1;
            tab.state = DocumentState::ReadyDirty;
            self.status = "Creating PDF Highlight annotation…".to_owned();
        } else {
            self.error = Some("highlight: document worker is unavailable".to_owned());
        }
    }

    fn save(&mut self) {
        if self.window_close_pending || self.close_confirmation.is_some() {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.is_saving() {
            self.status = "A save is already in progress".to_owned();
            return;
        }
        let Some(info) = &tab.info else {
            return;
        };
        if !info.dirty && tab.pending_highlights == 0 {
            self.status = "No unsaved Highlight annotations".to_owned();
            return;
        }

        if tab.send(DocumentCommand::Save) {
            tab.save_in_flight = true;
            tab.state = DocumentState::Saving;
            self.status = "Saving PDF and reopening for verification…".to_owned();
        } else {
            self.error = Some("save: document worker is unavailable".to_owned());
        }
    }

    fn active_tab_mut(&mut self) -> Option<&mut DocumentTab> {
        let index = self.active_index()?;
        self.documents.get_mut(index)
    }

    fn tab_bar(&mut self, root_ui: &mut egui::Ui) {
        let selected_index = self.active_index();
        let mut select_request = None;
        let mut close_request = None;
        egui::Panel::top("tabs").show(root_ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, tab) in self.tabs.tabs().iter().enumerate() {
                        let dirty = self.documents[index].has_unsaved_changes();
                        let marker = if dirty { "● " } else { "" };
                        let title = tab
                            .path()
                            .file_name()
                            .map(|name| name.to_string_lossy())
                            .unwrap_or_else(|| tab.path().as_os_str().to_string_lossy());
                        if ui
                            .selectable_label(
                                selected_index == Some(index),
                                format!("{marker}{title}"),
                            )
                            .clicked()
                        {
                            select_request = Some(index);
                        }
                        if ui.small_button("×").clicked() {
                            close_request = Some(index);
                        }
                        ui.separator();
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

    fn toolbar(&mut self, root_ui: &mut egui::Ui) {
        let mut search_changed = false;
        let mut search_navigation = None;
        let mut close_search = false;
        egui::Panel::top("toolbar").show(root_ui, |ui| {
            ui.checkbox(&mut self.restore_enabled, "Restore previous session")
                .on_hover_text("Restore tabs only when LunaPDF starts without PDF arguments");
            ui.separator();
            let Some(index) = self.active_index() else {
                ui.label("Drop PDF files here (maximum 20 tabs)");
                return;
            };
            let page_count = self.documents[index]
                .info
                .as_ref()
                .map(|info| info.page_bounds.len())
                .unwrap_or(0);
            let current_page = self.documents[index].view.current_page;
            let highlight_restriction = self.documents[index]
                .info
                .as_ref()
                .and_then(|info| info.highlight_capability.restriction());
            let can_highlight = self.documents[index]
                .info
                .as_ref()
                .is_some_and(|info| info.highlight_capability.is_allowed())
                && self.documents[index]
                    .selection
                    .as_ref()
                    .is_some_and(|selection| !selection.quads.is_empty());

            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(self.sidebar_open, "Sidebar").clicked() {
                    self.sidebar_open = !self.sidebar_open;
                }
                if ui.button("◀").clicked() {
                    self.move_page(-1);
                }
                ui.label(format!("{} / {}", current_page + 1, page_count));
                if ui.button("▶").clicked() {
                    self.move_page(1);
                }
                ui.separator();
                if ui.button("−").clicked() {
                    self.zoom_by(1.0 / 1.1);
                }
                ui.label(format!("{:.0}%", self.documents[index].view.zoom * 100.0));
                if ui.button("+").clicked() {
                    self.zoom_by(1.1);
                }
                if ui.button("Fit width").clicked() {
                    self.documents[index].view.zoom_mode = ZoomMode::FitWidth;
                }
                if ui.button("Fit page").clicked() {
                    self.documents[index].view.zoom_mode = ZoomMode::FitPage;
                }
                ui.separator();
                let continuous = self.documents[index].view.display_mode == DisplayMode::Continuous;
                if ui.selectable_label(continuous, "Continuous").clicked() {
                    self.documents[index].set_display_mode(DisplayMode::Continuous);
                }
                if ui.selectable_label(!continuous, "Single page").clicked() {
                    self.documents[index].set_display_mode(DisplayMode::SinglePage);
                }
                ui.separator();
                let highlight_button = ui
                    .add_enabled(can_highlight, egui::Button::new("Highlight (H)"))
                    .on_disabled_hover_text(
                        highlight_restriction.unwrap_or("Select text before creating a Highlight"),
                    );
                if highlight_button.clicked() {
                    self.create_highlight();
                }
                if ui.button("Save (Ctrl+S)").clicked() {
                    self.save();
                }
                ui.separator();
                if ui
                    .selectable_label(self.documents[index].search.open, "Find (Ctrl+F)")
                    .clicked()
                {
                    self.documents[index].search.open = true;
                    let document_id = self.documents[index].document_id;
                    ui.memory_mut(|memory| {
                        memory.request_focus(Id::new(("pdf-search-query", document_id)));
                    });
                }
                if self.documents[index].search.open {
                    let document_id = self.documents[index].document_id;
                    let search = &mut self.documents[index].search;
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut search.query)
                            .id(Id::new(("pdf-search-query", document_id)))
                            .desired_width(180.0)
                            .hint_text("Search this PDF"),
                    );
                    search_changed = response.changed();
                    let enter = ui.input(|input| input.key_pressed(Key::Enter));
                    if response.has_focus() && enter && !search_changed {
                        let backwards = ui.input(|input| input.modifiers.shift);
                        search_navigation = Some(!backwards);
                    }
                    let area_count = search.pages.values().map(Vec::len).sum::<usize>();
                    let progress = if search.in_progress {
                        format!(
                            "{area_count} areas · {}/{}",
                            search.completed_pages, page_count
                        )
                    } else {
                        format!("{area_count} areas")
                    };
                    ui.label(progress);
                    if search.truncated {
                        ui.label("result limit reached");
                    }
                    close_search = ui.small_button("×").clicked();
                }
            });
        });
        if let Some(index) = self.active_index() {
            if close_search {
                self.cancel_search(index, true);
            } else if search_changed {
                self.begin_search(index);
            } else if let Some(forward) = search_navigation {
                self.navigate_search(index, forward);
            }
        }
    }

    fn status_panel(&self, root_ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .resizable(true)
            .default_size(105.0)
            .show(root_ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&self.status);
                    if let Some(index) = self.active_index() {
                        let tab = &self.documents[index];
                        if let Some(info) = &tab.info {
                            ui.separator();
                            ui.label(format!("open {:.1} ms", milliseconds(info.open_time)));
                            ui.label(format!("Highlights: {}", info.highlight_count));
                            // Exposing the chosen strategy makes a potentially slower full
                            // rewrite visible instead of making it look like a stalled save.
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
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                if let Some(error) = self
                    .active_index()
                    .and_then(|index| self.documents[index].error.as_deref())
                {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                ui.separator();
                ui.strong("Logical selection / Ctrl+C");
                let selection_text = self
                    .active_index()
                    .and_then(|index| self.documents[index].selection.as_ref())
                    .map(|selection| selection.text.as_str())
                    .unwrap_or("Drag across page text to inspect MuPDF selection Quads.");
                ui.label(selection_text);
            });
    }

    fn sidebar_panel(&mut self, root_ui: &mut egui::Ui) {
        if !self.sidebar_open {
            return;
        }
        let mut selected_page = None;
        egui::Panel::left("document-sidebar")
            .default_size(220.0)
            .size_range(180.0..=360.0)
            .resizable(true)
            .show(root_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Outline, "Outline");
                    ui.selectable_value(
                        &mut self.sidebar_tab,
                        SidebarTab::Thumbnails,
                        "Thumbnails",
                    );
                });
                ui.separator();
                let Some(index) = self.active_index() else {
                    return;
                };
                match self.sidebar_tab {
                    SidebarTab::Outline => {
                        if let Some(outline) = &self.documents[index].outline {
                            if outline.is_empty() {
                                ui.label("This PDF has no outline.");
                            } else {
                                selected_page = show_outline(ui, outline);
                            }
                        } else {
                            ui.spinner();
                            ui.label("Loading outline…");
                        }
                    }
                    SidebarTab::Thumbnails => {
                        selected_page = self.thumbnail_sidebar(ui, index);
                    }
                }
            });
        if let Some(index) = self.active_index()
            && let Some(page) = selected_page
        {
            self.documents[index].jump_to_page(page);
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
                                ui.label("Thumbnail unavailable");
                                if ui.button("Retry").clicked() {
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
                                    format!("Page {}", page_index + 1),
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

    fn central_panel(&mut self, root_ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(root_ui, |ui| {
            let Some(index) = self.active_index() else {
                ui.centered_and_justified(|ui| {
                    ui.label("Drop one or more PDF files into this window");
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
                            .unwrap_or("The PDF worker stopped"),
                    );
                });
                return;
            }
            if self.documents[index].info.is_none() {
                return;
            }

            self.update_fit_zoom(index, ui.available_size());
            match self.documents[index].view.display_mode {
                DisplayMode::Continuous => self.continuous_view(ui, index),
                DisplayMode::SinglePage => self.single_page_view(ui, index),
            }
        });
    }

    fn update_fit_zoom(&mut self, index: usize, available: Vec2) {
        let tab = &mut self.documents[index];
        let Some(info) = &tab.info else {
            return;
        };
        let Some(bounds) = info.page_bounds.get(tab.view.current_page) else {
            return;
        };
        let usable_width = (available.x - PAGE_GAP * 2.0).max(1.0);
        let usable_height = (available.y - PAGE_GAP * 2.0).max(1.0);
        let desired = match tab.view.zoom_mode {
            ZoomMode::Fixed => return,
            ZoomMode::FitWidth => usable_width / bounds.width(),
            ZoomMode::FitPage => {
                (usable_width / bounds.width()).min(usable_height / bounds.height())
            }
        }
        .clamp(MIN_ZOOM, MAX_ZOOM);

        if (tab.view.zoom - desired).abs() > ZOOM_CHANGE_EPSILON {
            let mode = tab.view.zoom_mode;
            tab.set_zoom(desired, mode);
        }
    }

    fn continuous_view(&mut self, ui: &mut egui::Ui, index: usize) {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport_size = ui.available_size();
        let viewport = &mut self.viewport;
        let gpu_lru = &mut self.gpu_lru;
        let tab = &mut self.documents[index];
        let info = tab.info.as_ref().expect("checked before drawing");
        let path = info.path.clone();
        let page_bounds = info.page_bounds.clone();
        let revision = info.revision;
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
        let mut scroll_area = egui::ScrollArea::both()
            .id_salt(("continuous-pdf", path))
            .auto_shrink([false, false]);
        if let Some(offset) = jump_offset {
            scroll_area = scroll_area.scroll_offset(offset);
        }

        let mut completed_drag = None;
        let output = scroll_area.show_viewport(ui, |ui, visible_viewport| {
            ui.set_min_size(Vec2::new(content_width, layout.total_height()));
            let visible_text_pages =
                layout.visible_pages(visible_viewport.min.y..visible_viewport.max.y, 0.0);
            tab.prepare_text_snapshots(visible_text_pages, revision);
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
                paint_page_tiles(ui, screen_rect, page_index, tab, gpu_lru);
                if let Some(quads) = tab.search.pages.get(&page_index) {
                    PageViewport::paint_search_quads(
                        ui,
                        screen_rect,
                        page_bounds[page_index],
                        quads,
                    );
                }
                let response = ui
                    .scope_builder(UiBuilder::new().max_rect(screen_rect), |page_ui| {
                        let text_key = TextSnapshotKey {
                            page_index,
                            revision,
                        };
                        viewport.interact_at(
                            page_ui,
                            screen_rect,
                            page_index,
                            page_bounds[page_index],
                            tab.text_snapshots.get(&text_key),
                            tab.selection.as_ref(),
                        )
                    })
                    .inner;
                if response.is_some() {
                    completed_drag = response;
                }
            }
        });

        if let Some(page) = layout.page_at_y(output.state.offset.y + 1.0) {
            tab.view.current_page = page;
        }
        let viewport_center = output.state.offset + output.inner_rect.size() / 2.0;
        tab.view.center_anchor = layout.anchor_at(viewport_center.x, viewport_center.y);
        if let Some((page_index, start, end)) = completed_drag {
            tab.request_selection(page_index, start, end);
            self.status = "Resolving selection on the document worker…".to_owned();
        }
    }

    fn single_page_view(&mut self, ui: &mut egui::Ui, index: usize) {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport = &mut self.viewport;
        let gpu_lru = &mut self.gpu_lru;
        let tab = &mut self.documents[index];
        let info = tab.info.as_ref().expect("checked before drawing");
        let path = info.path.clone();
        let revision = info.revision;
        let page_index = tab.view.current_page;
        let bounds = info.page_bounds[page_index];
        let display_size = Vec2::new(
            bounds.width() * tab.view.zoom,
            bounds.height() * tab.view.zoom,
        );
        let viewport_size = ui.available_size();
        let content_size = Vec2::new(
            viewport_size.x.max(display_size.x + PAGE_GAP * 2.0),
            viewport_size.y.max(display_size.y + PAGE_GAP * 2.0),
        );
        let page_x = ((content_size.x - display_size.x) / 2.0).max(PAGE_GAP);
        let page_y = ((content_size.y - display_size.y) / 2.0).max(PAGE_GAP);
        let page_content_rect = Rect::from_min_size(Pos2::new(page_x, page_y), display_size);
        let restored_offset = tab.view.restore_single_anchor.take().map(|anchor| {
            single_page_centered_offset(page_content_rect, anchor, viewport_size, content_size)
        });
        let mut completed_drag = None;
        let mut scroll_area = egui::ScrollArea::both()
            .id_salt(("single-pdf", path))
            .auto_shrink([false, false]);
        if let Some(offset) = restored_offset {
            scroll_area = scroll_area.scroll_offset(offset);
        }
        let output = scroll_area.show_viewport(ui, |ui, visible_viewport| {
            ui.set_min_size(content_size);
            tab.prepare_text_snapshots(std::iter::once(page_index), revision);
            let screen_rect = Rect::from_min_size(
                ui.max_rect().min + page_content_rect.min.to_vec2(),
                display_size,
            );
            let requests = tile_requests_for_page(
                tab,
                page_index,
                bounds,
                page_content_rect,
                visible_viewport,
                pixels_per_point,
            )
            .unwrap_or_default();
            tab.prepare_tiles(requests);
            paint_page_tiles(ui, screen_rect, page_index, tab, gpu_lru);
            if let Some(quads) = tab.search.pages.get(&page_index) {
                PageViewport::paint_search_quads(ui, screen_rect, bounds, quads);
            }
            completed_drag = ui
                .scope_builder(UiBuilder::new().max_rect(screen_rect), |page_ui| {
                    let text_key = TextSnapshotKey {
                        page_index,
                        revision,
                    };
                    viewport.interact_at(
                        page_ui,
                        screen_rect,
                        page_index,
                        bounds,
                        tab.text_snapshots.get(&text_key),
                        tab.selection.as_ref(),
                    )
                })
                .inner;
        });

        let viewport_center = output.state.offset + output.inner_rect.size() / 2.0;
        tab.view.single_center_anchor = Some(normalized_page_point(
            page_content_rect,
            viewport_center.to_pos2(),
        ));

        if let Some((page_index, start, end)) = completed_drag {
            tab.request_selection(page_index, start, end);
            self.status = "Resolving selection on the document worker…".to_owned();
        }
    }
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

/// Limits raster work to the visible area and exactly one viewport around it.
fn prioritized_tile_specs(
    grid: TileGrid,
    page_rect: Rect,
    visible_viewport: Rect,
) -> Option<Vec<(TileSpec, RenderPriority)>> {
    let margin = visible_viewport.size();
    let request_viewport =
        Rect::from_min_max(visible_viewport.min - margin, visible_viewport.max + margin);
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
    let mut prioritized = Vec::new();
    for spec in specs {
        let tile_rect = logical_tile_rect(page_rect, grid, spec);
        if tile_rect.intersects(request_viewport) {
            prioritized.push((spec, tile_priority(tile_rect, visible_viewport)));
        }
    }
    Some(prioritized)
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

/// Centers a normalized PDF page point while respecting both scroll extents.
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

fn paint_page_tiles(
    ui: &egui::Ui,
    screen_rect: Rect,
    page_index: usize,
    tab: &DocumentTab,
    gpu_lru: &mut WeightedLruCache<TileCacheKey, ()>,
) {
    ui.painter()
        .rect_filled(screen_rect, 2.0, Color32::from_gray(245));
    let mut keys = tab
        .wanted_tiles
        .iter()
        .filter(|key| key.page_index == page_index)
        .copied()
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| (key.spec.pixel_y, key.spec.pixel_x));

    let mut painted_any = false;
    for key in keys {
        let retained = gpu_lru.get(&key).is_some();
        if retained && let Some(cached) = tab.tiles.get(&key) {
            PageViewport::paint_tile(ui, screen_rect, &cached.texture, &cached.tile);
            painted_any = true;
        }
    }
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
            text_snapshots: HashMap::new(),
            pending_text_snapshots: HashSet::new(),
            failed_text_snapshots: HashSet::new(),
            wanted_text_snapshots: HashSet::new(),
            selection: None,
            selection_generation: 0,
            pending_highlights: 0,
            save_in_flight: false,
            thumbnails: HashMap::new(),
            pending_thumbnails: HashSet::new(),
            failed_thumbnails: HashSet::new(),
            thumbnail_generation: 1,
            search: SearchState::default(),
            view: restored_view.map_or_else(ViewState::new, ViewState::from_session),
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

    fn is_suspendable(&self) -> bool {
        self.state == DocumentState::ReadyClean && !self.has_unsaved_changes()
    }

    fn suspend(&mut self) {
        self.invalidate_rendering();
        self.invalidate_text_snapshots();
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
    }

    fn set_zoom(&mut self, zoom: f32, mode: ZoomMode) {
        match self.view.display_mode {
            DisplayMode::Continuous => self.view.restore_anchor = self.view.center_anchor,
            DisplayMode::SinglePage => {
                self.view.restore_single_anchor = self.view.single_center_anchor
            }
        }
        self.view.zoom = zoom;
        self.view.zoom_mode = mode;
        self.invalidate_rendering();
    }

    fn set_display_mode(&mut self, mode: DisplayMode) {
        if self.view.switch_display_mode(mode) {
            self.invalidate_rendering();
        }
    }

    fn jump_to_page(&mut self, page_index: usize) {
        let page_count = self.info.as_ref().map_or(0, |info| info.page_bounds.len());
        if page_index >= page_count {
            return;
        }
        self.view.current_page = page_index;
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
        self.invalidate_rendering();
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
            || self.pending_highlights > 0
            || self.info.as_ref().is_some_and(|info| info.dirty)
    }

    fn invalidate_rendering(&mut self) {
        self.view.generation = self.view.generation.wrapping_add(1);
        let pending = self
            .pending_tiles
            .drain()
            .map(|(_, request)| request)
            .collect::<Vec<_>>();
        for request in pending {
            self.cancel_render(&request);
        }
        self.wanted_tiles.clear();
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

    fn prepare_tiles(&mut self, requests: Vec<TileRequest>) {
        let wanted_tiles = requests
            .iter()
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
}

impl ViewState {
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
        true
    }
}

impl eframe::App for PrototypeApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_document_events(context);
        self.maybe_suspend_inactive_document();
        self.handle_dropped_files(context);
        self.handle_shortcuts(context);
        self.handle_window_close(context);
        context.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tab_bar(ui);
        self.toolbar(ui);
        self.status_panel(ui);
        self.sidebar_panel(ui);
        self.central_panel(ui);
        self.close_confirmation_dialog(ui.ctx());
        self.session_close_failure_dialog(ui.ctx());
    }
}

/// Picks the longest-unused clean document and never selects the active tab.
fn oldest_suspendable_index(
    active_index: Option<usize>,
    states: &[(DocumentState, u64)],
) -> Option<usize> {
    states
        .iter()
        .enumerate()
        .filter(|(index, (state, _))| {
            Some(*index) != active_index && *state == DocumentState::ReadyClean
        })
        .min_by_key(|(_, (_, last_selected))| *last_selected)
        .map(|(index, _)| index)
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

fn next_search_page(
    pages: impl Iterator<Item = usize>,
    current_page: usize,
    forward: bool,
) -> Option<usize> {
    let pages = pages.collect::<Vec<_>>();
    if forward {
        pages
            .iter()
            .copied()
            .find(|page| *page > current_page)
            .or_else(|| pages.first().copied())
    } else {
        pages
            .iter()
            .rev()
            .copied()
            .find(|page| *page < current_page)
            .or_else(|| pages.last().copied())
    }
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

fn is_pdf_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn format_memory(bytes: usize) -> String {
    format!("resident memory: {:.1} MiB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            generation: 1,
        };

        assert!(view.switch_display_mode(DisplayMode::SinglePage));
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
    fn suspension_chooses_oldest_inactive_clean_document() {
        let states = [
            (DocumentState::ReadyClean, 2),
            (DocumentState::ReadyDirty, 1),
            (DocumentState::ReadyClean, 3),
            (DocumentState::Saving, 0),
        ];

        assert_eq!(oldest_suspendable_index(Some(0), &states), Some(2));
        assert_eq!(oldest_suspendable_index(Some(2), &states), Some(0));
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
    fn search_navigation_wraps_across_matching_pages() {
        let pages = [1, 4, 8];

        assert_eq!(next_search_page(pages.into_iter(), 4, true), Some(8));
        assert_eq!(next_search_page(pages.into_iter(), 8, true), Some(1));
        assert_eq!(next_search_page(pages.into_iter(), 4, false), Some(1));
        assert_eq!(next_search_page(pages.into_iter(), 1, false), Some(8));
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
    fn startup_restores_twenty_tabs_in_order_and_selects_saved_tab() {
        let directory = tempfile::tempdir().unwrap();
        let paths = (0..20)
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
}
