use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, select_biased, unbounded};

use crate::domain::document::{
    DocumentInfo, DocumentVersion, HighlightRequest, RenderPriority, RenderedTile, TileRequest,
};
use crate::domain::selection::{PagePoint, SelectionSnapshot};
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
    CreateHighlight(HighlightRequest),
    Save,
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum DocumentEvent {
    Opened(DocumentInfo),
    DocumentChanged(DocumentInfo),
    TileRendered(RenderedTile),
    SelectionReady(SelectionSnapshot),
    Status(String),
    Failed {
        operation: &'static str,
        message: String,
    },
}

pub(crate) struct DocumentService {
    foreground_sender: Sender<DocumentCommand>,
    next_viewport_sender: Sender<DocumentCommand>,
    previous_viewport_sender: Sender<DocumentCommand>,
    scheduled_tiles: Arc<Mutex<HashMap<WorkerTileKey, RenderPriority>>>,
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
// cap a worker at 9 MiB; 20 workers therefore stay below the 192 MiB budget.
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
        let (next_viewport_sender, next_viewport_receiver) = unbounded();
        let (previous_viewport_sender, previous_viewport_receiver) = unbounded();
        let (event_sender, event_receiver) = bounded(BUFFERED_EVENT_CAPACITY);
        let scheduled_tiles = Arc::new(Mutex::new(HashMap::new()));
        let worker_scheduled_tiles = Arc::clone(&scheduled_tiles);
        thread::Builder::new()
            .name("lunapdf-document-worker".to_owned())
            .spawn(move || {
                run_worker(
                    path,
                    expected_version,
                    foreground_receiver,
                    next_viewport_receiver,
                    previous_viewport_receiver,
                    worker_scheduled_tiles,
                    event_sender,
                )
            })
            .expect("failed to start document worker");

        Self {
            foreground_sender,
            next_viewport_sender,
            previous_viewport_sender,
            scheduled_tiles,
            event_receiver,
        }
    }

    /// Queues a document operation and reports whether the owner still exists.
    pub(crate) fn send(&self, command: DocumentCommand) -> bool {
        match command {
            DocumentCommand::RenderTile(request) => self.queue_render(request),
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
    foreground_receiver: Receiver<DocumentCommand>,
    next_viewport_receiver: Receiver<DocumentCommand>,
    previous_viewport_receiver: Receiver<DocumentCommand>,
    scheduled_tiles: Arc<Mutex<HashMap<WorkerTileKey, RenderPriority>>>,
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

    while let Some(command) = next_command(
        &foreground_receiver,
        &next_viewport_receiver,
        &previous_viewport_receiver,
    ) {
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
            DocumentCommand::CreateHighlight(request) => {
                match backend.create_highlight(request.page_index, &request.quads) {
                    Ok(()) => {
                        let _ = event_sender.send(DocumentEvent::Status(
                            "Highlight annotation created in memory".to_owned(),
                        ));
                        send_info(&backend, &event_sender, "highlight-state");
                    }
                    Err(error) => send_failure(&event_sender, "highlight", error),
                }
            }
            DocumentCommand::Save => match backend.save_incrementally() {
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

fn next_command(
    foreground: &Receiver<DocumentCommand>,
    next_viewport: &Receiver<DocumentCommand>,
    previous_viewport: &Receiver<DocumentCommand>,
) -> Option<DocumentCommand> {
    // A ready visible/interactive command always wins before the worker starts
    // another prefetch tile. An in-progress MuPDF tile is not interrupted.
    if let Ok(command) = foreground.try_recv() {
        return Some(command);
    }
    select_biased! {
        recv(foreground) -> command => command.ok(),
        recv(next_viewport) -> command => command.ok(),
        recv(previous_viewport) -> command => command.ok(),
    }
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
    use crate::domain::document::TileSpec;

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

    #[test]
    fn foreground_command_precedes_queued_prefetch() {
        let (foreground_sender, foreground_receiver) = unbounded();
        let (next_sender, next_receiver) = unbounded();
        let (_previous_sender, previous_receiver) = unbounded();
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

        let command =
            next_command(&foreground_receiver, &next_receiver, &previous_receiver).unwrap();

        assert!(matches!(command, DocumentCommand::Select { .. }));
    }

    #[test]
    fn next_viewport_precedes_queued_previous_viewport() {
        let (_foreground_sender, foreground_receiver) = unbounded();
        let (next_sender, next_receiver) = unbounded();
        let (previous_sender, previous_receiver) = unbounded();
        previous_sender
            .send(prefetch_tile(RenderPriority::PreviousViewport))
            .unwrap();
        next_sender
            .send(prefetch_tile(RenderPriority::NextViewport))
            .unwrap();

        let command =
            next_command(&foreground_receiver, &next_receiver, &previous_receiver).unwrap();

        assert!(matches!(
            command,
            DocumentCommand::RenderTile(TileRequest {
                priority: RenderPriority::NextViewport,
                ..
            })
        ));
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
}
