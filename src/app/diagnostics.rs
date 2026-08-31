use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::pdf::WorkerQueueSnapshot;
use crate::persistence::session_store::SessionStore;

pub(super) const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
pub(super) const WARMUP_DURATION: Duration = Duration::from_secs(180);
pub(super) const RSS_DETAIL_THRESHOLD_BYTES: usize = 100 * 1_024 * 1_024;

const DIAGNOSTICS_DIRECTORY_NAME: &str = "diagnostics";

const HEADER_COLUMNS: &[&str] = &[
    "row_kind",
    "phase",
    "event",
    "elapsed_s",
    "physical_mem_bytes",
    "gpu_tile_lru_count",
    "gpu_tile_lru_current_weight_bytes",
    "gpu_tile_lru_budget_bytes",
    "thumbnail_lru_count",
    "thumbnail_lru_current_weight_bytes",
    "thumbnail_lru_budget_bytes",
    "tab_count",
    "visible_tab_count",
    "active_document_id",
    "tiles",
    "pending_tiles",
    "wanted_tiles",
    "visible_tiles",
    "thumbnails",
    "pending_thumbnails",
    "text_snapshots",
    "pending_text_snapshots",
    "annotation_pages",
    "pending_annotation_pages",
    "highlight_index_pages",
    "highlight_index_items",
    "search_pages",
    "search_matches",
    "foreground_queue",
    "current_viewport_queue",
    "next_viewport_queue",
    "previous_viewport_queue",
    "background_queue",
    "event_queue",
    "scheduled_tiles",
    "scheduled_text_snapshots",
    "document_id",
    "document_state",
    "document_active",
    "document_visible",
    "document_tiles",
    "document_pending_tiles",
    "document_wanted_tiles",
    "document_visible_tiles",
    "document_thumbnails",
    "document_pending_thumbnails",
    "document_text_snapshots",
    "document_pending_text_snapshots",
    "document_annotation_pages",
    "document_pending_annotation_pages",
    "document_highlight_index_pages",
    "document_highlight_index_items",
    "document_search_pages",
    "document_search_matches",
    "document_foreground_queue",
    "document_current_viewport_queue",
    "document_next_viewport_queue",
    "document_previous_viewport_queue",
    "document_background_queue",
    "document_event_queue",
    "document_scheduled_tiles",
    "document_scheduled_text_snapshots",
];

const PHYSICAL_MEM_COLUMN: usize = 4;
const GPU_LRU_COUNT_COLUMN: usize = 5;
const GPU_LRU_WEIGHT_COLUMN: usize = 6;
const GPU_LRU_BUDGET_COLUMN: usize = 7;
const THUMBNAIL_LRU_COUNT_COLUMN: usize = 8;
const THUMBNAIL_LRU_WEIGHT_COLUMN: usize = 9;
const THUMBNAIL_LRU_BUDGET_COLUMN: usize = 10;
const TAB_COUNT_COLUMN: usize = 11;
const VISIBLE_TAB_COUNT_COLUMN: usize = 12;
const ACTIVE_DOCUMENT_ID_COLUMN: usize = 13;
const TILES_COLUMN: usize = 14;
const FOREGROUND_QUEUE_COLUMN: usize = 28;
const DOCUMENT_ID_COLUMN: usize = 36;
const DOCUMENT_STATE_COLUMN: usize = 37;
const DOCUMENT_ACTIVE_COLUMN: usize = 38;
const DOCUMENT_VISIBLE_COLUMN: usize = 39;
const DOCUMENT_TILES_COLUMN: usize = 40;
const DOCUMENT_FOREGROUND_QUEUE_COLUMN: usize = 54;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct LruSnapshot {
    pub(super) count: usize,
    pub(super) current_weight_bytes: usize,
    pub(super) budget_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ContainerCounts {
    pub(super) tiles: usize,
    pub(super) pending_tiles: usize,
    pub(super) wanted_tiles: usize,
    pub(super) visible_tiles: usize,
    pub(super) thumbnails: usize,
    pub(super) pending_thumbnails: usize,
    pub(super) text_snapshots: usize,
    pub(super) pending_text_snapshots: usize,
    pub(super) annotation_pages: usize,
    pub(super) pending_annotation_pages: usize,
    pub(super) highlight_index_pages: usize,
    pub(super) highlight_index_items: usize,
    pub(super) search_pages: usize,
    pub(super) search_matches: usize,
}

impl ContainerCounts {
    fn add_assign(&mut self, other: Self) {
        self.tiles = self.tiles.saturating_add(other.tiles);
        self.pending_tiles = self.pending_tiles.saturating_add(other.pending_tiles);
        self.wanted_tiles = self.wanted_tiles.saturating_add(other.wanted_tiles);
        self.visible_tiles = self.visible_tiles.saturating_add(other.visible_tiles);
        self.thumbnails = self.thumbnails.saturating_add(other.thumbnails);
        self.pending_thumbnails = self
            .pending_thumbnails
            .saturating_add(other.pending_thumbnails);
        self.text_snapshots = self.text_snapshots.saturating_add(other.text_snapshots);
        self.pending_text_snapshots = self
            .pending_text_snapshots
            .saturating_add(other.pending_text_snapshots);
        self.annotation_pages = self.annotation_pages.saturating_add(other.annotation_pages);
        self.pending_annotation_pages = self
            .pending_annotation_pages
            .saturating_add(other.pending_annotation_pages);
        self.highlight_index_pages = self
            .highlight_index_pages
            .saturating_add(other.highlight_index_pages);
        self.highlight_index_items = self
            .highlight_index_items
            .saturating_add(other.highlight_index_items);
        self.search_pages = self.search_pages.saturating_add(other.search_pages);
        self.search_matches = self.search_matches.saturating_add(other.search_matches);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct DocumentDiagnosticsSnapshot {
    pub(super) document_id: u64,
    pub(super) state: &'static str,
    pub(super) active: bool,
    pub(super) visible: bool,
    pub(super) containers: ContainerCounts,
    pub(super) worker: WorkerQueueSnapshot,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct DiagnosticsSnapshot {
    pub(super) physical_mem_bytes: Option<usize>,
    pub(super) gpu_tile_lru: LruSnapshot,
    pub(super) thumbnail_lru: LruSnapshot,
    pub(super) tab_count: usize,
    pub(super) visible_tab_count: usize,
    pub(super) active_document_id: Option<u64>,
    pub(super) containers: ContainerCounts,
    pub(super) worker: WorkerQueueSnapshot,
    pub(super) documents: Vec<DocumentDiagnosticsSnapshot>,
}

impl DiagnosticsSnapshot {
    pub(super) fn from_documents(
        physical_mem_bytes: Option<usize>,
        gpu_tile_lru: LruSnapshot,
        thumbnail_lru: LruSnapshot,
        visible_tab_count: usize,
        active_document_id: Option<u64>,
        documents: Vec<DocumentDiagnosticsSnapshot>,
    ) -> Self {
        let mut containers = ContainerCounts::default();
        let mut worker = WorkerQueueSnapshot::default();
        for document in &documents {
            containers.add_assign(document.containers);
            worker.add_assign(document.worker);
        }
        Self {
            physical_mem_bytes,
            gpu_tile_lru,
            thumbnail_lru,
            tab_count: documents.len(),
            visible_tab_count,
            active_document_id,
            containers,
            worker,
            documents,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiagnosticsPhase {
    Startup,
    Restore,
    DisplayPending,
    Warmup,
    Sampling,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DisplayRenderState {
    #[default]
    Pending,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct DisplayReadiness {
    pub(super) unavailable: bool,
    pub(super) render: DisplayRenderState,
    pub(super) has_visible_request: bool,
    pub(super) has_pending_visible_request: bool,
    pub(super) has_visible_result: bool,
}

/// 初期表示の要求と結果が揃ったかを、既存のDocumentTab状態から判定する。
///
/// 復元なしの空起動や表示不能文書を、表示安定として扱わない。
pub(super) fn display_is_stable(documents: &[DisplayReadiness]) -> bool {
    !documents.is_empty()
        && documents.iter().all(|document| {
            !document.unavailable
                && document.render == DisplayRenderState::Succeeded
                && document.has_visible_request
                && !document.has_pending_visible_request
                && document.has_visible_result
        })
}

/// 初期表示が正常に完了しないことを、表示可能な文書について確認する。
///
/// 描画失敗、空ページ、休止・エラー状態は表示安定とは別の終端である。正常な文書が
/// 同時に表示される場合は、その可視要求が完了してから一つのイベントへまとめる。
pub(super) fn display_is_unavailable(documents: &[DisplayReadiness], allow_empty: bool) -> bool {
    if documents.is_empty() {
        return allow_empty;
    }
    let unavailable = |document: &DisplayReadiness| {
        document.unavailable || document.render == DisplayRenderState::Failed
    };
    let stable = |document: &DisplayReadiness| {
        !unavailable(document)
            && document.render == DisplayRenderState::Succeeded
            && document.has_visible_request
            && !document.has_pending_visible_request
            && document.has_visible_result
    };
    documents.iter().any(unavailable)
        && documents
            .iter()
            .all(|document| unavailable(document) || stable(document))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SamplingTick {
    pub(super) phase: DiagnosticsPhase,
    pub(super) sampling_started: bool,
    pub(super) detail_due: bool,
}

pub(super) struct SamplingState {
    started_at: Instant,
    phase: DiagnosticsPhase,
    warmup_until: Option<Instant>,
    next_sample_at: Option<Instant>,
    rss_high_water: Option<usize>,
}

impl SamplingState {
    pub(super) fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            phase: DiagnosticsPhase::Startup,
            warmup_until: None,
            next_sample_at: None,
            rss_high_water: None,
        }
    }

    pub(super) fn phase(&self) -> DiagnosticsPhase {
        self.phase
    }

    pub(super) fn begin_restore(&mut self, _now: Instant) -> bool {
        if self.phase != DiagnosticsPhase::Startup {
            return false;
        }
        self.phase = DiagnosticsPhase::Restore;
        true
    }

    pub(super) fn restore_complete(&mut self, _now: Instant) -> bool {
        if self.phase != DiagnosticsPhase::Restore {
            return false;
        }
        self.phase = DiagnosticsPhase::DisplayPending;
        true
    }

    pub(super) fn display_stable(&mut self, now: Instant) -> bool {
        if !matches!(
            self.phase,
            DiagnosticsPhase::Startup | DiagnosticsPhase::DisplayPending
        ) {
            return false;
        }
        self.begin_warmup(now);
        true
    }

    pub(super) fn display_unavailable(&mut self, now: Instant) -> bool {
        if !matches!(
            self.phase,
            DiagnosticsPhase::Startup | DiagnosticsPhase::DisplayPending
        ) {
            return false;
        }
        self.begin_warmup(now);
        true
    }

    fn begin_warmup(&mut self, now: Instant) {
        self.phase = DiagnosticsPhase::Warmup;
        self.warmup_until = Some(now + WARMUP_DURATION);
        self.next_sample_at = Some(now);
    }

    pub(super) fn sample_due(&self, now: Instant) -> bool {
        self.next_sample_at.is_some_and(|next| now >= next)
    }

    pub(super) fn tick(&mut self, now: Instant, rss: Option<usize>) -> Option<SamplingTick> {
        if !self.sample_due(now) {
            return None;
        }

        let sampling_started = self.phase == DiagnosticsPhase::Warmup
            && self.warmup_until.is_some_and(|until| now >= until);
        if sampling_started {
            self.phase = DiagnosticsPhase::Sampling;
        }
        let detail_due = self.rss_crossed_detail_threshold(rss);
        self.next_sample_at = Some(now + SAMPLE_INTERVAL);
        Some(SamplingTick {
            phase: self.phase,
            sampling_started,
            detail_due,
        })
    }

    fn rss_crossed_detail_threshold(&mut self, rss: Option<usize>) -> bool {
        let Some(rss) = rss else {
            return false;
        };
        let Some(high_water) = self.rss_high_water else {
            self.rss_high_water = Some(rss);
            return false;
        };
        if rss < high_water.saturating_add(RSS_DETAIL_THRESHOLD_BYTES) {
            return false;
        }
        self.rss_high_water = Some(rss);
        true
    }

    fn elapsed_s(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.started_at).as_secs_f64()
    }
}

pub(super) struct Diagnostics {
    file: Option<File>,
    state: SamplingState,
    pending_error: Option<String>,
}

impl Diagnostics {
    pub(super) fn start(session_store: &SessionStore) -> Self {
        let started_at = Instant::now();
        let state = SamplingState::new(started_at);
        let file_result = open_diagnostics_file(session_store).and_then(|mut file| {
            file.write_all(header_line().as_bytes())?;
            file.write_all(b"\n")?;
            file.write_all(render_event_line("startup", "process_start", 0.0).as_bytes())?;
            file.write_all(b"\n")?;
            Ok(file)
        });
        match file_result {
            Ok(file) => Self {
                file: Some(file),
                state,
                pending_error: None,
            },
            Err(error) => Self {
                file: None,
                state,
                pending_error: Some(format!(
                    "デバッグ診断ログを開始できませんでした。詳細: {error}"
                )),
            },
        }
    }

    pub(super) fn begin_restore(&mut self, now: Instant) {
        if self.state.begin_restore(now) {
            self.write_event(now, "restore", "restore_start");
        }
    }

    pub(super) fn restore_complete(&mut self, now: Instant) {
        if !self.state.restore_complete(now) {
            return;
        }
        self.write_event(now, "restore", "restore_complete");
    }

    pub(super) fn display_stable(&mut self, now: Instant) {
        if !self.state.display_stable(now) {
            return;
        }
        self.write_event(now, "display", "display_stable");
        self.write_event(now, "warmup", "warmup_start");
    }

    pub(super) fn display_unavailable(&mut self, now: Instant) {
        if !self.state.display_unavailable(now) {
            return;
        }
        self.write_event(now, "display", "display_unavailable");
        self.write_event(now, "warmup", "warmup_start");
    }

    pub(super) fn sample_due(&self, now: Instant) -> bool {
        self.state.sample_due(now)
    }

    pub(super) fn sample(&mut self, now: Instant, snapshot: &DiagnosticsSnapshot) {
        let Some(tick) = self.state.tick(now, snapshot.physical_mem_bytes) else {
            return;
        };
        let phase = phase_name(tick.phase);
        if tick.sampling_started {
            self.write_event(now, phase, "sampling_start");
        }
        self.write_line(render_sample_line(
            phase,
            "",
            self.state.elapsed_s(now),
            snapshot,
        ));
        if tick.detail_due {
            if snapshot.documents.is_empty() {
                self.write_line(render_global_detail_line(
                    phase,
                    "rss_threshold",
                    self.state.elapsed_s(now),
                    snapshot,
                ));
            } else {
                for document in &snapshot.documents {
                    self.write_line(render_detail_line(
                        phase,
                        "rss_threshold",
                        self.state.elapsed_s(now),
                        snapshot,
                        document,
                    ));
                }
            }
        }
    }

    pub(super) fn phase(&self) -> DiagnosticsPhase {
        self.state.phase()
    }

    pub(super) fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    fn write_event(&mut self, now: Instant, phase: &str, event: &str) {
        self.write_line(render_event_line(phase, event, self.state.elapsed_s(now)));
    }

    fn write_line(&mut self, line: String) {
        let result = self.file.as_mut().map(|file| {
            file.write_all(line.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
        });
        if let Some(Err(error)) = result {
            self.file = None;
            self.pending_error = Some(format!(
                "デバッグ診断ログを書き込めませんでした。詳細: {error}"
            ));
        }
    }
}

fn open_diagnostics_file(session_store: &SessionStore) -> std::io::Result<File> {
    let directory = session_store.sibling_path(DIAGNOSTICS_DIRECTORY_NAME);
    std::fs::create_dir_all(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?;
    let path = directory.join(format!(
        "memory-diagnostics-{}-{:09}-{}.tsv",
        timestamp.as_secs(),
        timestamp.subsec_nanos(),
        std::process::id()
    ));
    OpenOptions::new().create_new(true).write(true).open(path)
}

pub(super) fn header_line() -> String {
    HEADER_COLUMNS.join("\t")
}

pub(super) fn render_event_line(phase: &str, event: &str, elapsed_s: f64) -> String {
    render_line("event", phase, event, elapsed_s, None, None)
}

pub(super) fn render_sample_line(
    phase: &str,
    event: &str,
    elapsed_s: f64,
    snapshot: &DiagnosticsSnapshot,
) -> String {
    render_line("sample", phase, event, elapsed_s, Some(snapshot), None)
}

pub(super) fn render_detail_line(
    phase: &str,
    event: &str,
    elapsed_s: f64,
    snapshot: &DiagnosticsSnapshot,
    document: &DocumentDiagnosticsSnapshot,
) -> String {
    render_line(
        "detail",
        phase,
        event,
        elapsed_s,
        Some(snapshot),
        Some(document),
    )
}

fn render_global_detail_line(
    phase: &str,
    event: &str,
    elapsed_s: f64,
    snapshot: &DiagnosticsSnapshot,
) -> String {
    render_line("detail", phase, event, elapsed_s, Some(snapshot), None)
}

fn render_line(
    row_kind: &str,
    phase: &str,
    event: &str,
    elapsed_s: f64,
    snapshot: Option<&DiagnosticsSnapshot>,
    document: Option<&DocumentDiagnosticsSnapshot>,
) -> String {
    let mut fields = vec![String::new(); HEADER_COLUMNS.len()];
    fields[0] = row_kind.to_owned();
    fields[1] = phase.to_owned();
    fields[2] = event.to_owned();
    fields[3] = format!("{elapsed_s:.3}");

    if let Some(snapshot) = snapshot {
        set_display(
            &mut fields,
            PHYSICAL_MEM_COLUMN,
            snapshot.physical_mem_bytes,
        );
        set_display(
            &mut fields,
            GPU_LRU_COUNT_COLUMN,
            Some(snapshot.gpu_tile_lru.count),
        );
        set_display(
            &mut fields,
            GPU_LRU_WEIGHT_COLUMN,
            Some(snapshot.gpu_tile_lru.current_weight_bytes),
        );
        set_display(
            &mut fields,
            GPU_LRU_BUDGET_COLUMN,
            Some(snapshot.gpu_tile_lru.budget_bytes),
        );
        set_display(
            &mut fields,
            THUMBNAIL_LRU_COUNT_COLUMN,
            Some(snapshot.thumbnail_lru.count),
        );
        set_display(
            &mut fields,
            THUMBNAIL_LRU_WEIGHT_COLUMN,
            Some(snapshot.thumbnail_lru.current_weight_bytes),
        );
        set_display(
            &mut fields,
            THUMBNAIL_LRU_BUDGET_COLUMN,
            Some(snapshot.thumbnail_lru.budget_bytes),
        );
        set_display(&mut fields, TAB_COUNT_COLUMN, Some(snapshot.tab_count));
        set_display(
            &mut fields,
            VISIBLE_TAB_COUNT_COLUMN,
            Some(snapshot.visible_tab_count),
        );
        set_display(
            &mut fields,
            ACTIVE_DOCUMENT_ID_COLUMN,
            snapshot.active_document_id,
        );
        set_container_fields(&mut fields, &snapshot.containers, false);
        set_worker_fields(&mut fields, snapshot.worker, false);
    }

    if let Some(document) = document {
        set_display(&mut fields, DOCUMENT_ID_COLUMN, Some(document.document_id));
        fields[DOCUMENT_STATE_COLUMN] = document.state.to_owned();
        fields[DOCUMENT_ACTIVE_COLUMN] = document.active.to_string();
        fields[DOCUMENT_VISIBLE_COLUMN] = document.visible.to_string();
        set_container_fields(&mut fields, &document.containers, true);
        set_worker_fields(&mut fields, document.worker, true);
    }

    debug_assert_eq!(fields.len(), HEADER_COLUMNS.len());
    fields.join("\t")
}

fn set_container_fields(fields: &mut [String], counts: &ContainerCounts, document: bool) {
    let offset = if document {
        DOCUMENT_TILES_COLUMN
    } else {
        TILES_COLUMN
    };
    set_display(fields, offset, Some(counts.tiles));
    set_display(fields, offset + 1, Some(counts.pending_tiles));
    set_display(fields, offset + 2, Some(counts.wanted_tiles));
    set_display(fields, offset + 3, Some(counts.visible_tiles));
    set_display(fields, offset + 4, Some(counts.thumbnails));
    set_display(fields, offset + 5, Some(counts.pending_thumbnails));
    set_display(fields, offset + 6, Some(counts.text_snapshots));
    set_display(fields, offset + 7, Some(counts.pending_text_snapshots));
    set_display(fields, offset + 8, Some(counts.annotation_pages));
    set_display(fields, offset + 9, Some(counts.pending_annotation_pages));
    set_display(fields, offset + 10, Some(counts.highlight_index_pages));
    set_display(fields, offset + 11, Some(counts.highlight_index_items));
    set_display(fields, offset + 12, Some(counts.search_pages));
    set_display(fields, offset + 13, Some(counts.search_matches));
}

fn set_worker_fields(fields: &mut [String], worker: WorkerQueueSnapshot, document: bool) {
    let offset = if document {
        DOCUMENT_FOREGROUND_QUEUE_COLUMN
    } else {
        FOREGROUND_QUEUE_COLUMN
    };
    set_display(fields, offset, Some(worker.foreground));
    set_display(fields, offset + 1, Some(worker.current_viewport));
    set_display(fields, offset + 2, Some(worker.next_viewport));
    set_display(fields, offset + 3, Some(worker.previous_viewport));
    set_display(fields, offset + 4, Some(worker.background));
    set_display(fields, offset + 5, Some(worker.event));
    set_display(fields, offset + 6, Some(worker.scheduled_tiles));
    set_display(fields, offset + 7, Some(worker.scheduled_text_snapshots));
}

fn set_display<T: std::fmt::Display>(fields: &mut [String], index: usize, value: Option<T>) {
    if let Some(value) = value {
        fields[index] = value.to_string();
    }
}

fn phase_name(phase: DiagnosticsPhase) -> &'static str {
    match phase {
        DiagnosticsPhase::Startup => "startup",
        DiagnosticsPhase::Restore => "restore",
        DiagnosticsPhase::DisplayPending => "display_pending",
        DiagnosticsPhase::Warmup => "warmup",
        DiagnosticsPhase::Sampling => "sampling",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_rendered_rows_have_the_same_number_of_columns() {
        let snapshot = DiagnosticsSnapshot::default();
        let document = DocumentDiagnosticsSnapshot::default();

        let expected = HEADER_COLUMNS.len();
        assert_eq!(header_line().split('\t').count(), expected);
        assert_eq!(
            render_event_line("startup", "process_start", 0.0)
                .split('\t')
                .count(),
            expected
        );
        assert_eq!(
            render_sample_line("warmup", "", 1.0, &snapshot)
                .split('\t')
                .count(),
            expected
        );
        assert_eq!(
            render_detail_line("warmup", "rss_threshold", 1.0, &snapshot, &document)
                .split('\t')
                .count(),
            expected
        );
    }
}
