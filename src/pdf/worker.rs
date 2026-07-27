use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{
    Receiver, Sender, TryRecvError, TrySendError, bounded, select_biased, unbounded,
};

use crate::domain::annotation::{
    AnnotationDeleteRequest, AnnotationPageRequest, AnnotationPageSnapshot,
    AnnotationUpdateRequest, HighlightIndexBatch, HighlightIndexRequest,
};
use crate::domain::document::{
    DocumentInfo, DocumentVersion, EditAction, HighlightRequest, OutlineItem, RenderPriority,
    RenderedThumbnail, RenderedTile, SearchPageResult, ThumbnailRequest, TileRequest,
};
use crate::domain::selection::{
    PagePoint, SelectionSnapshot, TextPageSnapshot, TextSnapshotRequest,
};
use crate::pdf::mupdf_backend::MuPdfBackend;

#[derive(Debug)]
pub(crate) enum DocumentCommand {
    RenderTile(TileRequest),
    Select {
        page_index: usize,
        generation: u64,
        start: PagePoint,
        end: PagePoint,
    },
    LoadTextSnapshot(TextSnapshotRequest),
    LoadAnnotations(AnnotationPageRequest),
    SetHighlightIndexGeneration(u64),
    LoadHighlightIndexBatch(HighlightIndexRequest),
    CreateHighlight(HighlightRequest),
    UpdateAnnotation(AnnotationUpdateRequest),
    DeleteAnnotation(AnnotationDeleteRequest),
    /// Removes the exact application-owned edit identified by the backend.
    Undo(EditAction),
    LoadOutline,
    SetSearchGeneration(u64),
    SearchPage {
        page_index: usize,
        query: Arc<str>,
        generation: u64,
    },
    LoadThumbnail(ThumbnailRequest),
    #[cfg(windows)]
    Print,
    Save,
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum DocumentEvent {
    Opened(DocumentInfo),
    DocumentChanged(DocumentInfo),
    TileRendered(RenderedTile),
    SelectionReady(SelectionSnapshot),
    TextSnapshotReady(TextPageSnapshot),
    TextSnapshotSkipped(TextSnapshotRequest),
    TextSnapshotFailed {
        request: TextSnapshotRequest,
        message: String,
    },
    AnnotationsReady(AnnotationPageSnapshot),
    AnnotationsSkipped(AnnotationPageRequest),
    AnnotationsFailed {
        request: AnnotationPageRequest,
        message: String,
    },
    HighlightIndexReady(HighlightIndexBatch),
    HighlightIndexSkipped(HighlightIndexRequest),
    HighlightIndexFailed {
        request: HighlightIndexRequest,
        message: String,
    },
    /// Returns the stable identity produced when a document edit completes.
    EditActionCreated(EditAction),
    /// Confirms that the requested edit was removed from the in-memory PDF.
    EditActionUndone(EditAction),
    OutlineReady(Vec<OutlineItem>),
    SearchPageReady(SearchPageResult),
    ThumbnailReady(RenderedThumbnail),
    ThumbnailSkipped(ThumbnailRequest),
    ThumbnailFailed {
        request: ThumbnailRequest,
        message: String,
    },
    #[cfg(windows)]
    PrintCompleted,
    #[cfg(windows)]
    PrintCancelled,
    Status(String),
    Failed {
        operation: &'static str,
        message: String,
    },
}

pub(crate) struct DocumentService {
    foreground_sender: Sender<DocumentCommand>,
    current_viewport_sender: Sender<DocumentCommand>,
    next_viewport_sender: Sender<DocumentCommand>,
    previous_viewport_sender: Sender<DocumentCommand>,
    background_sender: Sender<DocumentCommand>,
    text_snapshot_wake_sender: Sender<()>,
    scheduled_tiles: Arc<Mutex<HashMap<WorkerTileKey, RenderPriority>>>,
    scheduled_text_snapshots: Arc<Mutex<HashMap<WorkerTextKey, TextSnapshotRequest>>>,
    active_highlight_index_generation: Arc<AtomicU64>,
    event_receiver: Receiver<DocumentEvent>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WorkerTileKey {
    page_index: usize,
    zoom_bits: u32,
    pixels_per_point_bits: u32,
    scale_bits: u32,
    generation: u64,
    expected_revision: u64,
    spec: crate::domain::document::TileSpec,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WorkerTextKey {
    page_index: usize,
    expected_revision: u64,
}

struct WorkerChannels {
    foreground: Receiver<DocumentCommand>,
    current_viewport: Receiver<DocumentCommand>,
    next_viewport: Receiver<DocumentCommand>,
    previous_viewport: Receiver<DocumentCommand>,
    background: Receiver<DocumentCommand>,
    text_snapshot_wake: Receiver<()>,
    text_snapshot_wake_sender: Sender<()>,
    scheduled_text_snapshots: Arc<Mutex<HashMap<WorkerTextKey, TextSnapshotRequest>>>,
}

impl WorkerTextKey {
    fn from_request(request: &TextSnapshotRequest) -> Self {
        Self {
            page_index: request.page_index,
            expected_revision: request.expected_revision,
        }
    }
}

impl WorkerTileKey {
    fn from_request(request: &TileRequest) -> Self {
        Self {
            page_index: request.page_index,
            zoom_bits: request.zoom.to_bits(),
            pixels_per_point_bits: request.pixels_per_point.to_bits(),
            scale_bits: request.scale.to_bits(),
            generation: request.generation,
            expected_revision: request.expected_revision,
            spec: request.spec,
        }
    }
}

// Each 512 px RGBA result is at most 1 MiB. Seven queued results plus the
// transient Pixmap/RGBA pair, or eight blocked results without a new raster,
// cap one worker's transfer memory at 9 MiB. The application separately
// suspends inactive clean documents after crossing its process-memory limit.
const BUFFERED_EVENT_CAPACITY: usize = 8;

impl DocumentService {
    /// Starts the single-owner MuPDF worker required by the document contract.
    ///
    /// All MuPDF values are constructed and dropped on this worker. The caller
    /// exchanges only application-owned commands and snapshots.
    pub(crate) fn spawn(path: PathBuf) -> Self {
        Self::spawn_with_version(path, None)
    }

    /// Reopens a suspended PDF only if its file identity and metadata match.
    pub(crate) fn resume(path: PathBuf, expected_version: DocumentVersion) -> Self {
        Self::spawn_with_version(path, Some(expected_version))
    }

    fn spawn_with_version(path: PathBuf, expected_version: Option<DocumentVersion>) -> Self {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (current_viewport_sender, current_viewport_receiver) = unbounded();
        let (next_viewport_sender, next_viewport_receiver) = unbounded();
        let (previous_viewport_sender, previous_viewport_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        let (text_snapshot_wake_sender, text_snapshot_wake_receiver) = bounded(1);
        let (event_sender, event_receiver) = bounded(BUFFERED_EVENT_CAPACITY);
        let scheduled_tiles = Arc::new(Mutex::new(HashMap::new()));
        let scheduled_text_snapshots = Arc::new(Mutex::new(HashMap::new()));
        let active_highlight_index_generation = Arc::new(AtomicU64::new(0));
        let worker_scheduled_tiles = Arc::clone(&scheduled_tiles);
        let worker_scheduled_text_snapshots = Arc::clone(&scheduled_text_snapshots);
        let worker_highlight_index_generation = Arc::clone(&active_highlight_index_generation);
        let worker_text_snapshot_wake_sender = text_snapshot_wake_sender.clone();
        let worker_channels = WorkerChannels {
            foreground: foreground_receiver,
            current_viewport: current_viewport_receiver,
            next_viewport: next_viewport_receiver,
            previous_viewport: previous_viewport_receiver,
            background: background_receiver,
            text_snapshot_wake: text_snapshot_wake_receiver,
            text_snapshot_wake_sender: worker_text_snapshot_wake_sender,
            scheduled_text_snapshots: worker_scheduled_text_snapshots,
        };
        thread::Builder::new()
            .name("lunapdf-document-worker".to_owned())
            .spawn(move || {
                run_worker(
                    path,
                    expected_version,
                    worker_channels,
                    worker_scheduled_tiles,
                    worker_highlight_index_generation,
                    event_sender,
                )
            })
            .expect("failed to start document worker");

        Self {
            foreground_sender,
            current_viewport_sender,
            next_viewport_sender,
            previous_viewport_sender,
            background_sender,
            text_snapshot_wake_sender,
            scheduled_tiles,
            scheduled_text_snapshots,
            active_highlight_index_generation,
            event_receiver,
        }
    }

    /// Queues a document operation and reports whether the owner still exists.
    pub(crate) fn send(&self, command: DocumentCommand) -> bool {
        match command {
            DocumentCommand::RenderTile(request) => self.queue_render(request),
            DocumentCommand::LoadTextSnapshot(request) => self.queue_text_snapshot(request),
            DocumentCommand::SearchPage { .. }
            | DocumentCommand::LoadThumbnail(_)
            | DocumentCommand::LoadHighlightIndexBatch(_) => {
                self.background_sender.send(command).is_ok()
            }
            DocumentCommand::SetHighlightIndexGeneration(generation) => {
                // The atomic token is visible during a non-preemptible MuPDF
                // batch, so a completed old batch is suppressed before send.
                self.active_highlight_index_generation
                    .store(generation, Ordering::Release);
                self.foreground_sender.send(command).is_ok()
            }
            command => self.foreground_sender.send(command).is_ok(),
        }
    }

    /// Removes a queued tile from the worker scheduler.
    ///
    /// The channel message remains but is discarded before it can call MuPDF.
    pub(crate) fn cancel_render(&self, request: &TileRequest) {
        let key = WorkerTileKey::from_request(request);
        self.scheduled_tiles
            .lock()
            .expect("render scheduler mutex poisoned")
            .remove(&key);
    }

    /// Removes a text extraction request before the worker can enter MuPDF.
    pub(crate) fn cancel_text_snapshot(&self, request: &TextSnapshotRequest) {
        cancel_scheduled_text_snapshot(&self.scheduled_text_snapshots, request);
    }

    fn queue_text_snapshot(&self, request: TextSnapshotRequest) -> bool {
        let key = WorkerTextKey::from_request(&request);
        self.scheduled_text_snapshots
            .lock()
            .expect("text snapshot scheduler mutex poisoned")
            .insert(key, request);
        match self.text_snapshot_wake_sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => true,
            Err(TrySendError::Disconnected(())) => {
                self.scheduled_text_snapshots
                    .lock()
                    .expect("text snapshot scheduler mutex poisoned")
                    .remove(&key);
                false
            }
        }
    }

    fn queue_render(&self, request: TileRequest) -> bool {
        let key = WorkerTileKey::from_request(&request);
        {
            let mut scheduled = self
                .scheduled_tiles
                .lock()
                .expect("render scheduler mutex poisoned");
            match scheduled.get_mut(&key) {
                Some(priority) if request.priority < *priority => *priority = request.priority,
                Some(_) => return true,
                None => {
                    scheduled.insert(key, request.priority);
                }
            }
        }

        let queued = match request.priority {
            RenderPriority::Visible => self
                .foreground_sender
                .send(DocumentCommand::RenderTile(request))
                .is_ok(),
            RenderPriority::CurrentViewport => self
                .current_viewport_sender
                .send(DocumentCommand::RenderTile(request))
                .is_ok(),
            RenderPriority::NextViewport => self
                .next_viewport_sender
                .send(DocumentCommand::RenderTile(request))
                .is_ok(),
            RenderPriority::PreviousViewport => self
                .previous_viewport_sender
                .send(DocumentCommand::RenderTile(request))
                .is_ok(),
        };
        if !queued {
            self.scheduled_tiles
                .lock()
                .expect("render scheduler mutex poisoned")
                .remove(&key);
        }
        queued
    }

    pub(crate) fn try_recv(&self) -> Result<DocumentEvent, TryRecvError> {
        self.event_receiver.try_recv()
    }
}

impl Drop for DocumentService {
    fn drop(&mut self) {
        let _ = self.foreground_sender.send(DocumentCommand::Shutdown);
    }
}

fn run_worker(
    path: PathBuf,
    expected_version: Option<DocumentVersion>,
    channels: WorkerChannels,
    scheduled_tiles: Arc<Mutex<HashMap<WorkerTileKey, RenderPriority>>>,
    active_highlight_index_generation: Arc<AtomicU64>,
    event_sender: Sender<DocumentEvent>,
) {
    let mut backend = match MuPdfBackend::open(path) {
        Ok(backend) => backend,
        Err(error) => {
            send_failure(&event_sender, "open", error);
            return;
        }
    };
    if !send_opened_info(&backend, expected_version, &event_sender) {
        return;
    }
    let mut active_search_generation = 0;
    while let Some(command) = next_worker_command(&channels) {
        match command {
            DocumentCommand::RenderTile(request) => {
                if !take_scheduled_render(&scheduled_tiles, &request) {
                    continue;
                }
                match backend.render_tile(request) {
                    Ok(Some(tile)) => {
                        let _ = event_sender.send(DocumentEvent::TileRendered(tile));
                    }
                    Ok(None) => {}
                    Err(error) => send_failure(&event_sender, "render", error),
                }
            }
            DocumentCommand::Select {
                page_index,
                generation,
                start,
                end,
            } => match backend.select(page_index, generation, start, end) {
                Ok(selection) => {
                    let _ = event_sender.send(DocumentEvent::SelectionReady(selection));
                }
                Err(error) => send_failure(&event_sender, "selection", error),
            },
            DocumentCommand::LoadTextSnapshot(request) => match backend.text_snapshot(request) {
                Ok(Some(snapshot)) => {
                    let _ = event_sender.send(DocumentEvent::TextSnapshotReady(snapshot));
                }
                Ok(None) => {
                    let _ = event_sender.send(DocumentEvent::TextSnapshotSkipped(request));
                }
                Err(error) => {
                    let _ = event_sender.send(DocumentEvent::TextSnapshotFailed {
                        request,
                        message: format!("{error:#}"),
                    });
                }
            },
            DocumentCommand::LoadAnnotations(request) => match backend.annotation_page(request) {
                Ok(Some(snapshot)) => {
                    let _ = event_sender.send(DocumentEvent::AnnotationsReady(snapshot));
                }
                Ok(None) => {
                    let _ = event_sender.send(DocumentEvent::AnnotationsSkipped(request));
                }
                Err(error) => {
                    let _ = event_sender.send(DocumentEvent::AnnotationsFailed {
                        request,
                        message: format!("{error:#}"),
                    });
                }
            },
            DocumentCommand::SetHighlightIndexGeneration(_) => {}
            DocumentCommand::LoadHighlightIndexBatch(request) => {
                if !highlight_index_generation_is_current(
                    active_highlight_index_generation.load(Ordering::Acquire),
                    request.generation,
                ) {
                    let _ = event_sender.send(DocumentEvent::HighlightIndexSkipped(request));
                    continue;
                }
                match backend.highlight_index_batch(request) {
                    Ok(Some(batch))
                        if highlight_index_generation_is_current(
                            active_highlight_index_generation.load(Ordering::Acquire),
                            request.generation,
                        ) =>
                    {
                        let _ = event_sender.send(DocumentEvent::HighlightIndexReady(batch));
                    }
                    Ok(Some(_)) => {
                        let _ = event_sender.send(DocumentEvent::HighlightIndexSkipped(request));
                    }
                    Ok(None) => {
                        let _ = event_sender.send(DocumentEvent::HighlightIndexSkipped(request));
                    }
                    Err(error) => {
                        let _ = event_sender.send(DocumentEvent::HighlightIndexFailed {
                            request,
                            message: format!("{error:#}"),
                        });
                    }
                }
            }
            DocumentCommand::CreateHighlight(request) => {
                match backend.create_highlight(request.page_index, &request.quads) {
                    Ok(action) => {
                        let _ = event_sender.send(DocumentEvent::EditActionCreated(action));
                        let _ = event_sender.send(DocumentEvent::Status(
                            "Highlight annotation created in memory".to_owned(),
                        ));
                        send_info(&backend, &event_sender, "highlight-state");
                    }
                    Err(error) => send_failure(&event_sender, "highlight", error),
                }
            }
            DocumentCommand::UpdateAnnotation(request) => {
                match backend.update_annotation(request) {
                    Ok(action) => {
                        let _ = event_sender.send(DocumentEvent::EditActionCreated(action));
                        let _ = event_sender.send(DocumentEvent::Status(
                            "注釈のコメントと色を更新しました".to_owned(),
                        ));
                        send_info(&backend, &event_sender, "annotation-update-state");
                    }
                    Err(error) => send_failure(&event_sender, "annotation-update", error),
                }
            }
            DocumentCommand::DeleteAnnotation(request) => {
                match backend.delete_annotation(request) {
                    Ok(action) => {
                        let _ = event_sender.send(DocumentEvent::EditActionCreated(action));
                        let _ = event_sender
                            .send(DocumentEvent::Status("注釈を削除しました".to_owned()));
                        send_info(&backend, &event_sender, "annotation-delete-state");
                    }
                    Err(error) => send_failure(&event_sender, "annotation-delete", error),
                }
            }
            DocumentCommand::Undo(action) => match backend.undo(action.clone()) {
                Ok(()) => {
                    let _ = event_sender.send(DocumentEvent::EditActionUndone(action));
                    send_info(&backend, &event_sender, "undo-state");
                }
                Err(error) => send_failure(&event_sender, "undo", error),
            },
            DocumentCommand::LoadOutline => match backend.load_outline() {
                Ok(outline) => {
                    let _ = event_sender.send(DocumentEvent::OutlineReady(outline));
                }
                Err(error) => send_failure(&event_sender, "outline", error),
            },
            DocumentCommand::SetSearchGeneration(generation) => {
                active_search_generation = generation;
            }
            DocumentCommand::SearchPage {
                page_index,
                query,
                generation,
            } => {
                if !search_generation_is_current(active_search_generation, generation) {
                    continue;
                }
                match backend.search_page(page_index, &query, generation) {
                    Ok(result) => {
                        let _ = event_sender.send(DocumentEvent::SearchPageReady(result));
                    }
                    Err(error) => send_failure(&event_sender, "search", error),
                }
            }
            DocumentCommand::LoadThumbnail(request) => match backend.render_thumbnail(request) {
                Ok(Some(thumbnail)) => {
                    let _ = event_sender.send(DocumentEvent::ThumbnailReady(thumbnail));
                }
                Ok(None) => {
                    let _ = event_sender.send(DocumentEvent::ThumbnailSkipped(request));
                }
                Err(error) => {
                    let _ = event_sender.send(DocumentEvent::ThumbnailFailed {
                        request,
                        message: format!("{error:#}"),
                    });
                }
            },
            #[cfg(windows)]
            DocumentCommand::Print => {
                match crate::pdf::windows_print::print_document(&mut backend) {
                    Ok(crate::pdf::windows_print::PrintOutcome::Completed) => {
                        let _ = event_sender.send(DocumentEvent::PrintCompleted);
                    }
                    Ok(crate::pdf::windows_print::PrintOutcome::Cancelled) => {
                        let _ = event_sender.send(DocumentEvent::PrintCancelled);
                    }
                    Err(error) => send_failure(&event_sender, "print", error),
                }
            }
            DocumentCommand::Save => match backend.save() {
                Ok(highlight_count) => {
                    let _ = event_sender.send(DocumentEvent::Status(format!(
                        "Saved and reopened successfully ({highlight_count} Highlight annotations)"
                    )));
                    send_info(&backend, &event_sender, "save");
                }
                Err(error) => send_failure(&event_sender, "save", error),
            },
            DocumentCommand::Shutdown => return,
        }
    }
}

/// Claims only the latest priority entry for a tile request.
///
/// Lower-priority channel messages remain as cheap tombstones after a tile is
/// promoted, so they must not reach MuPDF after the promoted request runs.
fn take_scheduled_render(
    scheduled_tiles: &Mutex<HashMap<WorkerTileKey, RenderPriority>>,
    request: &TileRequest,
) -> bool {
    let key = WorkerTileKey::from_request(request);
    let mut scheduled = scheduled_tiles
        .lock()
        .expect("render scheduler mutex poisoned");
    let request_is_current = scheduled.get(&key) == Some(&request.priority);
    if request_is_current {
        scheduled.remove(&key);
    }
    request_is_current
}

fn next_worker_command(channels: &WorkerChannels) -> Option<DocumentCommand> {
    loop {
        // Each non-blocking probe preserves the documented render tiers even
        // when several queues become ready between two MuPDF operations.
        if let Ok(command) = channels.foreground.try_recv() {
            return Some(command);
        }
        if let Ok(command) = channels.current_viewport.try_recv() {
            return Some(command);
        }
        if let Ok(command) = channels.next_viewport.try_recv() {
            return Some(command);
        }
        if let Ok(command) = channels.previous_viewport.try_recv() {
            return Some(command);
        }
        if channels.text_snapshot_wake.try_recv().is_ok() {
            if let Some(request) = take_scheduled_text_snapshot(
                &channels.scheduled_text_snapshots,
                &channels.text_snapshot_wake_sender,
            ) {
                return Some(DocumentCommand::LoadTextSnapshot(request));
            }
            continue;
        }
        if let Ok(command) = channels.background.try_recv() {
            return Some(command);
        }

        select_biased! {
            recv(channels.foreground) -> command => return command.ok(),
            recv(channels.current_viewport) -> command => return command.ok(),
            recv(channels.next_viewport) -> command => return command.ok(),
            recv(channels.previous_viewport) -> command => return command.ok(),
            recv(channels.text_snapshot_wake) -> signal => {
                signal.ok()?;
                if let Some(request) = take_scheduled_text_snapshot(
                    &channels.scheduled_text_snapshots,
                    &channels.text_snapshot_wake_sender,
                ) {
                    return Some(DocumentCommand::LoadTextSnapshot(request));
                }
            },
            recv(channels.background) -> command => return command.ok(),
        }
    }
}

fn take_scheduled_text_snapshot(
    scheduled: &Mutex<HashMap<WorkerTextKey, TextSnapshotRequest>>,
    wake_sender: &Sender<()>,
) -> Option<TextSnapshotRequest> {
    let (request, has_more) = {
        let mut scheduled = scheduled
            .lock()
            .expect("text snapshot scheduler mutex poisoned");
        let key = scheduled.keys().next().copied()?;
        let request = scheduled
            .remove(&key)
            .expect("text snapshot key came from the same scheduler map");
        (request, !scheduled.is_empty())
    };
    if has_more {
        // At most one wake token exists; the remaining wanted page is claimed
        // on the next scheduling cycle without producing one message per page.
        let _ = wake_sender.try_send(());
    }
    Some(request)
}

fn cancel_scheduled_text_snapshot(
    scheduled: &Mutex<HashMap<WorkerTextKey, TextSnapshotRequest>>,
    request: &TextSnapshotRequest,
) {
    let key = WorkerTextKey::from_request(request);
    scheduled
        .lock()
        .expect("text snapshot scheduler mutex poisoned")
        .remove(&key);
}

#[cfg(test)]
fn next_command(
    foreground: &Receiver<DocumentCommand>,
    current_viewport: &Receiver<DocumentCommand>,
    next_viewport: &Receiver<DocumentCommand>,
    previous_viewport: &Receiver<DocumentCommand>,
    background: &Receiver<DocumentCommand>,
) -> Option<DocumentCommand> {
    // A ready visible/interactive command always wins before the worker starts
    // another prefetch tile. An in-progress MuPDF tile is not interrupted.
    if let Ok(command) = foreground.try_recv() {
        return Some(command);
    }
    select_biased! {
        recv(foreground) -> command => command.ok(),
        recv(current_viewport) -> command => command.ok(),
        recv(next_viewport) -> command => command.ok(),
        recv(previous_viewport) -> command => command.ok(),
        recv(background) -> command => command.ok(),
    }
}

fn search_generation_is_current(active_generation: u64, request_generation: u64) -> bool {
    active_generation == request_generation
}

fn highlight_index_generation_is_current(active_generation: u64, request_generation: u64) -> bool {
    active_generation == request_generation
}

fn send_opened_info(
    backend: &MuPdfBackend,
    expected_version: Option<DocumentVersion>,
    event_sender: &Sender<DocumentEvent>,
) -> bool {
    match backend.info() {
        Ok(info) if expected_version.is_some_and(|expected| expected != info.version) => {
            let _ = event_sender.send(DocumentEvent::Failed {
                operation: "resume",
                message: "the PDF changed outside LunaPDF while its tab was suspended".to_owned(),
            });
            false
        }
        Ok(info) => {
            let _ = event_sender.send(DocumentEvent::Opened(info));
            true
        }
        Err(error) => {
            send_failure(event_sender, "document-info", error);
            false
        }
    }
}

fn send_info(
    backend: &MuPdfBackend,
    event_sender: &Sender<DocumentEvent>,
    failure_operation: &'static str,
) {
    match backend.info() {
        Ok(info) => {
            let _ = event_sender.send(DocumentEvent::DocumentChanged(info));
        }
        Err(error) => send_failure(event_sender, failure_operation, error),
    }
}

fn send_failure(
    event_sender: &Sender<DocumentEvent>,
    operation: &'static str,
    error: anyhow::Error,
) {
    let _ = event_sender.send(DocumentEvent::Failed {
        operation,
        message: format!("{error:#}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::annotation::{AnnotationId, PdfAnnotationColor};
    use crate::domain::document::TileSpec;
    use mupdf::color::AnnotationColor;
    use mupdf::pdf::PdfDocument;
    use mupdf::{Quad, Rect, Size};
    use std::time::{Duration, Instant};

    fn tile_request(priority: RenderPriority) -> TileRequest {
        TileRequest {
            page_index: 0,
            zoom: 1.0,
            pixels_per_point: 1.0,
            scale: 1.0,
            generation: 1,
            expected_revision: 0,
            spec: TileSpec {
                pixel_x: 0,
                pixel_y: 0,
                pixel_width: 512,
                pixel_height: 512,
            },
            priority,
        }
    }

    fn prefetch_tile(priority: RenderPriority) -> DocumentCommand {
        DocumentCommand::RenderTile(tile_request(priority))
    }

    fn highlight_index_request(generation: u64) -> HighlightIndexRequest {
        HighlightIndexRequest {
            generation,
            expected_revision: 0,
            first_page: 0,
            page_count: 1,
        }
    }

    #[test]
    fn foreground_command_precedes_queued_prefetch() {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
        let (_background_sender, background_receiver) = unbounded();
        next_sender
            .send(prefetch_tile(RenderPriority::NextViewport))
            .unwrap();
        foreground_sender
            .send(DocumentCommand::Select {
                page_index: 0,
                generation: 1,
                start: PagePoint::new(0.0, 0.0),
                end: PagePoint::new(1.0, 1.0),
            })
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(command, DocumentCommand::Select { .. }));
    }

    #[test]
    fn next_viewport_precedes_queued_previous_viewport() {
        let (_foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (next_sender, next_receiver) = unbounded();
        let (previous_sender, previous_receiver) = unbounded();
        let (_background_sender, background_receiver) = unbounded();
        previous_sender
            .send(prefetch_tile(RenderPriority::PreviousViewport))
            .unwrap();
        next_sender
            .send(prefetch_tile(RenderPriority::NextViewport))
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(
            command,
            DocumentCommand::RenderTile(TileRequest {
                priority: RenderPriority::NextViewport,
                ..
            })
        ));
    }

    #[test]
    fn current_viewport_precedes_adjacent_and_background_work() {
        let (_foreground_sender, foreground_receiver) = unbounded();
        let (current_sender, current_receiver) = unbounded();
        let (next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        background_sender
            .send(DocumentCommand::SearchPage {
                page_index: 0,
                query: Arc::from("needle"),
                generation: 1,
            })
            .unwrap();
        next_sender
            .send(prefetch_tile(RenderPriority::NextViewport))
            .unwrap();
        current_sender
            .send(prefetch_tile(RenderPriority::CurrentViewport))
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(
            command,
            DocumentCommand::RenderTile(TileRequest {
                priority: RenderPriority::CurrentViewport,
                ..
            })
        ));
    }

    #[test]
    fn previous_viewport_precedes_background_search() {
        let (_foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (_next_sender, next_receiver) = unbounded();
        let (previous_sender, previous_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        background_sender
            .send(DocumentCommand::SearchPage {
                page_index: 0,
                query: Arc::from("needle"),
                generation: 1,
            })
            .unwrap();
        previous_sender
            .send(prefetch_tile(RenderPriority::PreviousViewport))
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(command, DocumentCommand::RenderTile(_)));
    }

    #[test]
    fn visible_render_precedes_background_highlight_index() {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (_next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        background_sender
            .send(DocumentCommand::LoadHighlightIndexBatch(
                highlight_index_request(2),
            ))
            .unwrap();
        foreground_sender
            .send(DocumentCommand::RenderTile(tile_request(
                RenderPriority::Visible,
            )))
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(command, DocumentCommand::RenderTile(_)));
    }

    #[test]
    fn promoted_visible_tile_invalidates_older_prefetch_message() {
        let prefetch = tile_request(RenderPriority::NextViewport);
        let visible = tile_request(RenderPriority::Visible);
        let key = WorkerTileKey::from_request(&visible);
        let scheduled = Mutex::new(HashMap::from([(key, RenderPriority::Visible)]));

        assert!(!take_scheduled_render(&scheduled, &prefetch));
        assert!(take_scheduled_render(&scheduled, &visible));
        assert!(scheduled.lock().unwrap().is_empty());
    }

    #[test]
    fn visible_render_precedes_queued_search_page() {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (_next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        background_sender
            .send(DocumentCommand::SearchPage {
                page_index: 0,
                query: Arc::from("needle"),
                generation: 2,
            })
            .unwrap();
        foreground_sender
            .send(DocumentCommand::RenderTile(tile_request(
                RenderPriority::Visible,
            )))
            .unwrap();

        let command = next_command(
            &foreground_receiver,
            &current_receiver,
            &next_receiver,
            &previous_receiver,
            &background_receiver,
        )
        .unwrap();

        assert!(matches!(command, DocumentCommand::RenderTile(_)));
    }

    #[test]
    fn visible_text_snapshot_is_below_render_and_above_search() {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (_current_sender, current_receiver) = unbounded();
        let (text_wake_sender, text_wake_receiver) = bounded(1);
        let (_next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
        let (background_sender, background_receiver) = unbounded();
        let request = TextSnapshotRequest {
            page_index: 0,
            expected_revision: 0,
        };
        let key = WorkerTextKey::from_request(&request);
        let scheduled_text = Arc::new(Mutex::new(HashMap::from([(key, request)])));
        background_sender
            .send(DocumentCommand::SearchPage {
                page_index: 0,
                query: Arc::from("needle"),
                generation: 1,
            })
            .unwrap();
        text_wake_sender.send(()).unwrap();
        foreground_sender
            .send(DocumentCommand::RenderTile(tile_request(
                RenderPriority::Visible,
            )))
            .unwrap();

        let channels = WorkerChannels {
            foreground: foreground_receiver,
            current_viewport: current_receiver,
            next_viewport: next_receiver,
            previous_viewport: previous_receiver,
            background: background_receiver,
            text_snapshot_wake: text_wake_receiver,
            text_snapshot_wake_sender: text_wake_sender,
            scheduled_text_snapshots: scheduled_text,
        };
        let first = next_worker_command(&channels).unwrap();
        let second = next_worker_command(&channels).unwrap();

        assert!(matches!(first, DocumentCommand::RenderTile(_)));
        assert!(matches!(second, DocumentCommand::LoadTextSnapshot(_)));
    }

    #[test]
    fn offscreen_text_snapshot_is_removed_before_worker_claim() {
        let (wake_sender, _wake_receiver) = bounded(1);
        let offscreen = TextSnapshotRequest {
            page_index: 2,
            expected_revision: 0,
        };
        let visible = TextSnapshotRequest {
            page_index: 8,
            expected_revision: 0,
        };
        let scheduled = Mutex::new(HashMap::from([
            (WorkerTextKey::from_request(&offscreen), offscreen),
            (WorkerTextKey::from_request(&visible), visible),
        ]));

        cancel_scheduled_text_snapshot(&scheduled, &offscreen);
        let claimed = take_scheduled_text_snapshot(&scheduled, &wake_sender).unwrap();

        assert_eq!(claimed.page_index, visible.page_index);
        assert!(scheduled.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_search_generation_is_rejected_before_backend_work() {
        assert!(search_generation_is_current(3, 3));
        assert!(!search_generation_is_current(4, 3));
    }

    #[test]
    fn stale_highlight_index_generation_is_rejected_before_backend_work() {
        assert!(highlight_index_generation_is_current(3, 3));
        assert!(!highlight_index_generation_is_current(4, 3));
    }

    #[test]
    fn corrupt_pdf_open_failure_reaches_event_queue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.pdf");
        std::fs::write(&path, b"not a PDF").unwrap();
        let service = DocumentService::spawn(path);
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            match service.try_recv() {
                Ok(DocumentEvent::Failed { operation, .. }) => {
                    assert_eq!(operation, "open");
                    break;
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("worker did not report open failure: {error:?}"),
            }
        }
    }

    #[test]
    fn existing_annotation_snapshot_reaches_event_queue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("annotations.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            document.save(path_text).unwrap();
        }
        let service = DocumentService::spawn(path);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut request_sent = false;

        loop {
            match service.try_recv() {
                Ok(DocumentEvent::Opened(_)) if !request_sent => {
                    assert!(service.send(DocumentCommand::LoadAnnotations(
                        AnnotationPageRequest {
                            page_index: 0,
                            expected_revision: 0,
                        },
                    )));
                    request_sent = true;
                }
                Ok(DocumentEvent::AnnotationsReady(snapshot)) => {
                    assert_eq!(snapshot.page_index, 0);
                    assert_eq!(snapshot.revision, 0);
                    assert!(snapshot.annotations.is_empty());
                    break;
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("worker did not return annotations: {error:?}"),
            }
        }
    }

    #[test]
    fn highlight_index_worker_suppresses_canceled_generation_then_returns_current_batch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("highlight-index.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            document.save(path_text).unwrap();
        }
        let service = DocumentService::spawn(path);
        let deadline = Instant::now() + Duration::from_secs(5);
        let canceled = highlight_index_request(1);
        let current = highlight_index_request(2);
        let mut canceled_skipped = false;

        loop {
            match service.try_recv() {
                Ok(DocumentEvent::Opened(_)) => {
                    assert!(service.send(DocumentCommand::SetHighlightIndexGeneration(1)));
                    assert!(service.send(DocumentCommand::LoadHighlightIndexBatch(canceled)));
                    assert!(service.send(DocumentCommand::SetHighlightIndexGeneration(2)));
                    assert!(service.send(DocumentCommand::LoadHighlightIndexBatch(current)));
                }
                Ok(DocumentEvent::HighlightIndexSkipped(request)) if request == canceled => {
                    canceled_skipped = true;
                }
                Ok(DocumentEvent::HighlightIndexReady(batch)) if batch.generation == 2 => {
                    assert!(canceled_skipped);
                    assert_eq!(batch.pages.len(), 1);
                    assert_eq!(batch.pages[0].page_index, 0);
                    break;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("worker did not return the Highlight index batch: {error:?}"),
            }
        }
    }

    #[test]
    fn annotation_update_command_returns_history_action_and_revision() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("annotation-update.pdf");
        let path_text = path.to_str().unwrap();
        let xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut annotation = page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 30.0, 120.0, 50.0)))
                .unwrap();
            annotation
                .set_color(AnnotationColor::Rgb {
                    red: 1.0,
                    green: 1.0,
                    blue: 0.0,
                })
                .unwrap();
            xref = annotation.xref().unwrap();
            annotation.update().unwrap();
            page.update().unwrap();
            drop(annotation);
            drop(page);
            document.save(path_text).unwrap();
        }
        let service = DocumentService::spawn(path);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut action_received = false;
        let mut revision_received = false;

        loop {
            match service.try_recv() {
                Ok(DocumentEvent::Opened(_)) => {
                    assert!(service.send(DocumentCommand::UpdateAnnotation(
                        AnnotationUpdateRequest {
                            id: AnnotationId {
                                page_index: 0,
                                xref,
                            },
                            expected_revision: 0,
                            contents: Some("worker update".to_owned()),
                            color: Some(PdfAnnotationColor::Rgb {
                                red: 0.2,
                                green: 0.3,
                                blue: 0.9,
                            }),
                        },
                    )));
                }
                Ok(DocumentEvent::EditActionCreated(EditAction::UpdateAnnotation {
                    annotation_id,
                    revision_after,
                })) => {
                    assert_eq!(annotation_id.xref, xref);
                    assert_eq!(revision_after, 1);
                    action_received = true;
                }
                Ok(DocumentEvent::DocumentChanged(info)) => {
                    if info.revision == 1 {
                        assert!(info.dirty);
                        revision_received = true;
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) if Instant::now() < deadline => {
                    if action_received && revision_received {
                        break;
                    }
                    std::thread::yield_now();
                }
                Err(error) => panic!("worker did not complete annotation update: {error:?}"),
            }
            if action_received && revision_received {
                break;
            }
        }
    }
}
