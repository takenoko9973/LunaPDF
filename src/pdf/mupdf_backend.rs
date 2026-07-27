use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle as _;

use anyhow::{Context, Result, anyhow, ensure};
use mupdf::color::AnnotationColor;
use mupdf::pdf::{
    AnnotationFlags, AnnotationQuadPoints, Encryption, PdfAnnotationType, PdfDocument,
    PdfWriteOptions, Permission, WidgetType,
};
use mupdf::text_page::SearchHitResponse;
use mupdf::{
    Colorspace, Device, DisplayList, IRect, Matrix, Outline, Pixmap, Point, Quad, Rect,
    TextBlockContent, TextPage, TextPageFlags,
};
use tempfile::{Builder as TempFileBuilder, TempPath};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, ReplaceFileW,
};

use crate::domain::annotation::{
    AnnotationDeleteRequest, AnnotationId, AnnotationKind, AnnotationPageRequest,
    AnnotationPageSnapshot, AnnotationSnapshot, AnnotationUpdateRequest, PdfAnnotationColor,
};
use crate::domain::document::{
    DocumentInfo, DocumentVersion, EditAction, HighlightCapability, OutlineItem, PageRect,
    RenderedThumbnail, RenderedTile, SearchMatch, SearchPageResult, TILE_EDGE_PIXELS,
    ThumbnailRequest, TileRequest, TileSpec,
};
use crate::domain::selection::{
    GlyphSnapshot, PagePoint, PageQuad, SelectionSnapshot, TextPageSnapshot, TextSnapshotRequest,
    selected_display_quads, selected_quads, selected_text,
};

// PDF numeric objects can round coordinates while serializing an incremental
// update. One hundredth of a PDF point is below display-pixel precision at the
// supported zoom range while still detecting a materially different Quad.
const PDF_COORDINATE_TOLERANCE: f32 = 0.01;

// PDF writers can round normalized color and opacity numbers. One thousandth
// is below an 8-bit channel step while still detecting a visible replacement.
const PDF_PROPERTY_TOLERANCE: f32 = 0.001;

// 16,384 Quads bound one page snapshot near 512 KiB while covering dense
// papers. Logical hit boundaries are preserved within that byte-oriented cap.
const SEARCH_QUAD_CAPACITY: usize = 16_384;

pub(super) struct MuPdfBackend {
    path: PathBuf,
    document: PdfDocument,
    page_bounds: Vec<PageRect>,
    version: DocumentVersion,
    open_time: Duration,
    revision: u64,
    highlight_capability: HighlightCapability,
    pending_edits: Vec<PendingEdit>,
    display_list: Option<CachedDisplayList>,
    // A recovery document opened from bytes has no safe association with the
    // original path, so retries must use the full-rewrite path until a fresh
    // path-backed document is installed after successful verification.
    incremental_association_lost: bool,
}

struct CachedDisplayList {
    page_index: usize,
    revision: u64,
    list: DisplayList,
}

#[derive(Clone, Debug)]
struct PendingEdit {
    action: EditAction,
    undo: UndoEdit,
    expected: ExpectedAnnotationMutation,
}

#[derive(Clone, Debug)]
enum UndoEdit {
    DeleteCreatedAnnotation(AnnotationId),
    RestoreDocument(Vec<u8>),
}

#[derive(Clone, Debug)]
enum ExpectedAnnotationMutation {
    Present(ExpectedAnnotationState),
    Absent(AnnotationId),
}

#[derive(Clone, Debug)]
struct ExpectedAnnotationState {
    id: AnnotationId,
    quads: Vec<PageQuad>,
    contents: String,
    color: Option<PdfAnnotationColor>,
    opacity: f32,
}

impl MuPdfBackend {
    /// Opens the PDF and reads lightweight page geometry on the owner worker.
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let version_before_open = read_document_version(&path)?;
        let mupdf_path = path
            .to_str()
            .context("MuPDF requires a Unicode path on Windows")?;
        let open_started = Instant::now();
        let document = PdfDocument::open(mupdf_path)
            .with_context(|| format!("failed to open PDF: {}", path.display()))?;
        ensure!(
            !document.needs_password()?,
            "PDF requires a password; password-protected documents are not supported"
        );
        let page_bounds = load_page_bounds(&document)?;
        ensure!(!page_bounds.is_empty(), "PDF contains no pages");
        let highlight_capability = determine_highlight_capability(&document, &path)?;
        let version_after_open = read_document_version(&path)?;
        let version = stable_open_version(version_before_open, version_after_open)?;
        let open_time = open_started.elapsed();

        Ok(Self {
            path,
            document,
            page_bounds,
            version,
            open_time,
            revision: 0,
            highlight_capability,
            pending_edits: Vec::new(),
            display_list: None,
            incremental_association_lost: false,
        })
    }

    pub(super) fn info(&self) -> Result<DocumentInfo> {
        Ok(DocumentInfo {
            path: self.path.clone(),
            page_bounds: self.page_bounds.clone(),
            highlight_count: highlight_count(&self.document)?,
            can_save_incrementally: self.should_save_incrementally(),
            highlight_capability: self.highlight_capability,
            // MuPDF keeps xref dirty bits after undo operations. Application
            // dirty state follows the still-live edit log so a complete LIFO
            // undo returns to a clean tab without trusting those stale bits.
            dirty: !self.pending_edits.is_empty(),
            revision: self.revision,
            open_time: self.open_time,
            physical_memory_bytes: physical_memory_bytes(),
            version: self.version,
        })
    }

    /// Returns a Rust-owned hierarchy with only validated internal page targets.
    pub(super) fn load_outline(&self) -> Result<Vec<OutlineItem>> {
        let outlines = self.document.outlines()?;
        Ok(outlines
            .into_iter()
            .map(|outline| outline_item(outline, self.page_bounds.len()))
            .collect())
    }

    /// Searches one page so foreground rendering can run between page commands.
    pub(super) fn search_page(
        &mut self,
        page_index: usize,
        query: &str,
        generation: u64,
    ) -> Result<SearchPageResult> {
        ensure!(!query.is_empty(), "cannot search for an empty string");
        page_number(page_index, self.page_bounds.len())?;
        let revision = self.revision;
        let text_page = self
            .display_list(page_index)?
            .to_text_page(TextPageFlags::empty())?;
        struct PageSearchMatches {
            matches: Vec<SearchMatch>,
            quad_count: usize,
            truncated: bool,
        }
        let mut result = PageSearchMatches {
            matches: Vec::new(),
            quad_count: 0,
            truncated: false,
        };
        text_page.search_cb(query, &mut result, |result, quads| {
            let Some(next_quad_count) = result.quad_count.checked_add(quads.len()) else {
                result.truncated = true;
                return SearchHitResponse::AbortSearch;
            };
            if next_quad_count > SEARCH_QUAD_CAPACITY {
                // Never split a multi-line hit at the memory boundary. A partial
                // hit would make navigation and the painted result disagree.
                result.truncated = true;
                return SearchHitResponse::AbortSearch;
            }
            result.quad_count = next_quad_count;
            result.matches.push(SearchMatch {
                quads: quads.iter().map(page_quad_from_mupdf).collect(),
            });
            SearchHitResponse::ContinueSearch
        })?;
        Ok(SearchPageResult {
            page_index,
            generation,
            revision,
            matches: result.matches,
            truncated: result.truncated,
        })
    }

    /// Renders a bounded whole-page image through the same annotated tile path.
    pub(super) fn render_thumbnail(
        &mut self,
        request: ThumbnailRequest,
    ) -> Result<Option<RenderedThumbnail>> {
        ensure!(
            request.max_pixel_width > 0 && request.max_pixel_height > 0,
            "thumbnail dimensions must be positive"
        );
        ensure!(
            request.max_pixel_width <= TILE_EDGE_PIXELS
                && request.max_pixel_height <= TILE_EDGE_PIXELS,
            "thumbnail dimensions exceed the single-tile transfer bound"
        );
        page_number(request.page_index, self.page_bounds.len())?;
        if request.expected_revision != self.revision {
            return Ok(None);
        }

        let bounds = self.page_bounds[request.page_index];
        let scale = (request.max_pixel_width as f32 / bounds.width())
            .min(request.max_pixel_height as f32 / bounds.height());
        ensure!(
            scale.is_finite() && scale > 0.0,
            "thumbnail scale must be finite and positive"
        );
        let pixel_bounds = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
            .transform(&Matrix::new_scale(scale, scale))
            .round();
        let pixel_width = u32::try_from(pixel_bounds.x1 - pixel_bounds.x0)?;
        let pixel_height = u32::try_from(pixel_bounds.y1 - pixel_bounds.y0)?;
        let tile = self.render_tile(TileRequest {
            page_index: request.page_index,
            zoom: scale,
            pixels_per_point: 1.0,
            scale,
            generation: request.generation,
            expected_revision: request.expected_revision,
            spec: TileSpec {
                pixel_x: 0,
                pixel_y: 0,
                pixel_width,
                pixel_height,
            },
            priority: crate::domain::document::RenderPriority::PreviousViewport,
        })?;
        Ok(tile.map(|tile| RenderedThumbnail {
            page_index: tile.page_index,
            max_pixel_width: request.max_pixel_width,
            max_pixel_height: request.max_pixel_height,
            generation: tile.generation,
            revision: tile.revision,
            pixel_width: tile.spec.pixel_width,
            pixel_height: tile.spec.pixel_height,
            pixels_rgba: tile.pixels_rgba,
        }))
    }

    pub(super) fn render_tile(&mut self, request: TileRequest) -> Result<Option<RenderedTile>> {
        // A non-finite or non-positive matrix produces meaningless dimensions
        // in MuPDF, so invalid zoom state is rejected at the adapter boundary.
        ensure!(
            request.scale.is_finite() && request.scale > 0.0,
            "render scale must be finite and positive"
        );
        ensure!(
            request.spec.pixel_width > 0 && request.spec.pixel_height > 0,
            "tile dimensions must be positive"
        );
        page_number(request.page_index, self.page_bounds.len())?;
        // A mutation can overtake queued prefetch work. Stale tiles are normal
        // cancellation, not a document error that should be shown to the user.
        if request.expected_revision != self.revision {
            return Ok(None);
        }

        let bounds = self.page_bounds[request.page_index];
        let render_started = Instant::now();
        let transform = Matrix::new_scale(request.scale, request.scale);
        let page_pixel_bounds = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
            .transform(&transform)
            .round();
        let page_pixel_width = u32::try_from(page_pixel_bounds.x1 - page_pixel_bounds.x0)
            .context("MuPDF returned a negative page pixel width")?;
        let page_pixel_height = u32::try_from(page_pixel_bounds.y1 - page_pixel_bounds.y0)
            .context("MuPDF returned a negative page pixel height")?;
        let clip = tile_clip(page_pixel_bounds, request.spec)?;
        let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), clip, false)?;
        // MuPDF does not initialize a newly allocated pixmap. Opaque white is
        // the PDF page background expected outside painted content.
        pixmap.clear_with(255)?;
        let device = Device::from_pixmap_with_clip(&pixmap, clip)?;
        let display_list = self.display_list(request.page_index)?;
        display_list.run(&device, &transform, Rect::from(clip))?;
        drop(device);

        let pixel_width = usize::try_from(pixmap.width())?;
        let pixel_height = usize::try_from(pixmap.height())?;
        let component_count = usize::from(pixmap.n());
        let stride =
            usize::try_from(pixmap.stride()).context("MuPDF returned a negative pixmap stride")?;
        let pixels_rgba = pixmap_samples_to_rgba(
            pixmap.samples(),
            pixel_width,
            pixel_height,
            stride,
            component_count,
        )?;

        let pixel_x = u32::try_from(pixmap.x() - page_pixel_bounds.x0)
            .context("MuPDF returned a tile origin before the page")?;
        let pixel_y = u32::try_from(pixmap.y() - page_pixel_bounds.y0)
            .context("MuPDF returned a tile origin before the page")?;
        Ok(Some(RenderedTile {
            page_index: request.page_index,
            zoom: request.zoom,
            pixels_per_point: request.pixels_per_point,
            scale: request.scale,
            generation: request.generation,
            revision: self.revision,
            spec: TileSpec {
                pixel_x,
                pixel_y,
                pixel_width: pixmap.width(),
                pixel_height: pixmap.height(),
            },
            page_pixel_width,
            page_pixel_height,
            pixels_rgba,
            bounds,
            render_time: render_started.elapsed(),
            physical_memory_bytes: physical_memory_bytes(),
        }))
    }

    fn display_list(&mut self, page_index: usize) -> Result<&DisplayList> {
        let cache_is_current = self.display_list.as_ref().is_some_and(|cached| {
            cached.page_index == page_index && cached.revision == self.revision
        });
        if !cache_is_current {
            let page_number = page_number(page_index, self.page_bounds.len())?;
            let page = self.document.load_pdf_page(page_number)?;
            let list = page.to_display_list(true)?;
            self.display_list = Some(CachedDisplayList {
                page_index,
                revision: self.revision,
                list,
            });
        }
        Ok(&self
            .display_list
            .as_ref()
            .expect("display list was populated above")
            .list)
    }

    /// Extracts a Rust-owned text snapshot for one currently visible page.
    pub(super) fn text_snapshot(
        &self,
        request: TextSnapshotRequest,
    ) -> Result<Option<TextPageSnapshot>> {
        // Annotation mutations can overtake queued extraction. The UI keys
        // snapshots by revision, so stale work is discarded before allocation.
        if request.expected_revision != self.revision {
            return Ok(None);
        }
        let (_text_page, glyphs, _extraction_time) =
            load_text_snapshot(&self.document, request.page_index, self.page_bounds.len())?;
        Ok(Some(TextPageSnapshot {
            page_index: request.page_index,
            revision: self.revision,
            glyphs,
        }))
    }

    /// Reads one revision-bound page of editable annotation metadata.
    pub(super) fn annotation_page(
        &self,
        request: AnnotationPageRequest,
    ) -> Result<Option<AnnotationPageSnapshot>> {
        if request.expected_revision != self.revision {
            return Ok(None);
        }
        let page = self
            .document
            .load_pdf_page(page_number(request.page_index, self.page_bounds.len())?)?;
        let document_allows_edits = self.highlight_capability.is_allowed();
        let mut annotations = Vec::new();
        for annotation in page.annotations() {
            if annotation.r#type()? != PdfAnnotationType::Highlight {
                continue;
            }
            let xref = annotation.xref()?;
            ensure!(
                xref > 0,
                "MuPDF returned an invalid xref for an existing Highlight"
            );
            let flags = annotation.flags()?;
            let properties_locked =
                flags.intersects(AnnotationFlags::IS_READ_ONLY | AnnotationFlags::IS_LOCKED);
            let contents_locked =
                properties_locked || flags.contains(AnnotationFlags::IS_LOCKED_CONTENTS);
            annotations.push(AnnotationSnapshot {
                id: AnnotationId {
                    page_index: request.page_index,
                    xref,
                },
                kind: AnnotationKind::Highlight,
                quads: annotation
                    .quad_points()?
                    .iter()
                    .map(page_quad_from_mupdf)
                    .collect(),
                contents: annotation.contents()?.unwrap_or_default().to_owned(),
                color: annotation.color()?.map(annotation_color_from_mupdf),
                opacity: annotation.opacity()?,
                can_edit_contents: document_allows_edits && !contents_locked,
                can_edit_color: document_allows_edits && !properties_locked,
                can_delete: document_allows_edits && !properties_locked,
            });
        }
        Ok(Some(AnnotationPageSnapshot {
            page_index: request.page_index,
            revision: self.revision,
            annotations,
        }))
    }

    pub(super) fn select(
        &self,
        page_index: usize,
        generation: u64,
        start: PagePoint,
        end: PagePoint,
    ) -> Result<SelectionSnapshot> {
        let (_text_page, glyphs, extraction_time) =
            load_text_snapshot(&self.document, page_index, self.page_bounds.len())?;
        // The drag preview already resolves pointer positions to snapshot glyphs.
        // Re-running MuPDF's point selection here excludes endpoint glyphs, so
        // confirmed display, copy and Highlight all derive from that same range.
        let selection_quads = selected_quads(&glyphs, start, end);

        Ok(SelectionSnapshot {
            page_index,
            generation,
            text: selected_text(&glyphs, start, end),
            display_quads: selected_display_quads(&glyphs, start, end),
            quads: selection_quads,
            extraction_time,
        })
    }

    /// Creates one in-memory Highlight and returns the exact MuPDF identity
    /// needed to undo this operation without inspecting coordinates or order.
    pub(super) fn create_highlight(
        &mut self,
        page_index: usize,
        quads: &[PageQuad],
    ) -> Result<EditAction> {
        ensure!(!quads.is_empty(), "cannot highlight an empty selection");
        ensure!(
            self.highlight_capability.is_allowed(),
            "Highlight editing is disabled: {}",
            self.highlight_capability
                .restriction()
                .expect("a disallowed capability has a reason")
        );
        let page_number = page_number(page_index, self.page_bounds.len())?;
        let annotation_quads = quads.iter().map(mupdf_quad_from_page).collect::<Vec<_>>();
        let mut page = self.document.load_pdf_page(page_number)?;
        let mut annotation =
            page.add_highlight_annotation(AnnotationQuadPoints::new(annotation_quads))?;
        annotation.set_color(AnnotationColor::Rgb {
            red: 1.0,
            green: 1.0,
            blue: 0.0,
        })?;
        let annotation_xref = annotation.xref()?;
        // PDF xrefs are positive indirect-object numbers; zero is not stable
        // enough to identify an application-owned annotation for undo.
        ensure!(
            annotation_xref > 0,
            "MuPDF returned an invalid xref for the created Highlight"
        );
        annotation.update()?;
        page.update()?;
        let annotation_id = AnnotationId {
            page_index,
            xref: annotation_xref,
        };
        let action = EditAction::CreateHighlight {
            page_index,
            annotation_xref,
        };
        let expected = ExpectedAnnotationState {
            id: annotation_id,
            quads: annotation
                .quad_points()?
                .iter()
                .map(page_quad_from_mupdf)
                .collect(),
            contents: annotation.contents()?.unwrap_or_default().to_owned(),
            color: annotation.color()?.map(annotation_color_from_mupdf),
            opacity: annotation.opacity()?,
        };
        self.pending_edits.push(PendingEdit {
            action: action.clone(),
            undo: UndoEdit::DeleteCreatedAnnotation(annotation_id),
            expected: ExpectedAnnotationMutation::Present(expected),
        });
        self.display_list = None;
        self.revision += 1;
        Ok(action)
    }

    /// Updates supported fields on one exact revision-bound annotation xref.
    pub(super) fn update_annotation(
        &mut self,
        request: AnnotationUpdateRequest,
    ) -> Result<EditAction> {
        ensure!(
            request.expected_revision == self.revision,
            "annotation update revision is stale"
        );
        ensure!(
            request.contents.is_some() || request.color.is_some(),
            "annotation update contains no changed fields"
        );
        ensure!(
            self.highlight_capability.is_allowed(),
            "annotation editing is disabled: {}",
            self.highlight_capability
                .restriction()
                .expect("a disallowed capability has a reason")
        );
        let flags = self.validate_highlight_target(request.id)?;
        let properties_locked =
            flags.intersects(AnnotationFlags::IS_READ_ONLY | AnnotationFlags::IS_LOCKED);
        let contents_locked =
            properties_locked || flags.contains(AnnotationFlags::IS_LOCKED_CONTENTS);
        ensure!(
            request.contents.is_none() || !contents_locked,
            "annotation Contents is locked"
        );
        ensure!(
            request.color.is_none() || !properties_locked,
            "annotation color is locked"
        );

        let undo_document = self.document_snapshot_for_undo()?;
        if let Err(error) = self.apply_annotation_update(&request) {
            return self.rollback_failed_mutation(undo_document, error);
        }
        let expected = match self.expected_annotation_state(request.id) {
            Ok(expected) => expected,
            Err(error) => return self.rollback_failed_mutation(undo_document, error),
        };
        self.display_list = None;
        self.revision += 1;
        let action = EditAction::UpdateAnnotation {
            annotation_id: request.id,
            revision_after: self.revision,
        };
        self.pending_edits.push(PendingEdit {
            action: action.clone(),
            undo: UndoEdit::RestoreDocument(undo_document),
            expected: ExpectedAnnotationMutation::Present(expected),
        });
        Ok(action)
    }

    /// Deletes one exact revision-bound annotation while retaining an exact undo snapshot.
    pub(super) fn delete_annotation(
        &mut self,
        request: AnnotationDeleteRequest,
    ) -> Result<EditAction> {
        ensure!(
            request.expected_revision == self.revision,
            "annotation delete revision is stale"
        );
        ensure!(
            self.highlight_capability.is_allowed(),
            "annotation deletion is disabled: {}",
            self.highlight_capability
                .restriction()
                .expect("a disallowed capability has a reason")
        );
        let flags = self.validate_highlight_target(request.id)?;
        ensure!(
            !flags.intersects(AnnotationFlags::IS_READ_ONLY | AnnotationFlags::IS_LOCKED),
            "annotation deletion is locked"
        );

        let undo_document = self.document_snapshot_for_undo()?;
        if let Err(error) = self.apply_annotation_delete(request.id) {
            return self.rollback_failed_mutation(undo_document, error);
        }
        self.display_list = None;
        self.revision += 1;
        let action = EditAction::DeleteAnnotation {
            annotation_id: request.id,
            revision_after: self.revision,
        };
        self.pending_edits.push(PendingEdit {
            action: action.clone(),
            undo: UndoEdit::RestoreDocument(undo_document),
            expected: ExpectedAnnotationMutation::Absent(request.id),
        });
        Ok(action)
    }

    fn validate_highlight_target(&self, id: AnnotationId) -> Result<AnnotationFlags> {
        ensure!(id.xref > 0, "annotation xref must be positive");
        let page_number = page_number(id.page_index, self.page_bounds.len())?;
        let page = self.document.load_pdf_page(page_number)?;
        for annotation in page.annotations() {
            if annotation.xref()? != id.xref {
                continue;
            }
            ensure!(
                annotation.r#type()? == PdfAnnotationType::Highlight,
                "annotation xref {} is not a Highlight",
                id.xref
            );
            return Ok(annotation.flags()?);
        }
        Err(anyhow!(
            "Highlight xref {} was not found on page {}",
            id.xref,
            id.page_index + 1
        ))
    }

    fn apply_annotation_update(&mut self, request: &AnnotationUpdateRequest) -> Result<()> {
        let page_number = page_number(request.id.page_index, self.page_bounds.len())?;
        let mut page = self.document.load_pdf_page(page_number)?;
        for mut annotation in page.annotations() {
            if annotation.xref()? != request.id.xref {
                continue;
            }
            if let Some(contents) = &request.contents {
                annotation.set_contents(contents)?;
            }
            if let Some(color) = request.color {
                annotation.set_color(annotation_color_to_mupdf(color))?;
            }
            annotation.update()?;
            page.update()?;
            return Ok(());
        }
        Err(anyhow!(
            "Highlight xref {} disappeared before update",
            request.id.xref
        ))
    }

    fn apply_annotation_delete(&mut self, id: AnnotationId) -> Result<()> {
        let page_number = page_number(id.page_index, self.page_bounds.len())?;
        let mut page = self.document.load_pdf_page(page_number)?;
        for annotation in page.annotations() {
            if annotation.xref()? != id.xref {
                continue;
            }
            page.delete_annotation(annotation)?;
            page.update()?;
            return Ok(());
        }
        Err(anyhow!(
            "Highlight xref {} disappeared before deletion",
            id.xref
        ))
    }

    fn expected_annotation_state(&self, id: AnnotationId) -> Result<ExpectedAnnotationState> {
        let page_number = page_number(id.page_index, self.page_bounds.len())?;
        let page = self.document.load_pdf_page(page_number)?;
        for annotation in page.annotations() {
            if annotation.xref()? != id.xref {
                continue;
            }
            ensure!(
                annotation.r#type()? == PdfAnnotationType::Highlight,
                "annotation xref {} changed type after mutation",
                id.xref
            );
            return Ok(ExpectedAnnotationState {
                id,
                quads: annotation
                    .quad_points()?
                    .iter()
                    .map(page_quad_from_mupdf)
                    .collect(),
                contents: annotation.contents()?.unwrap_or_default().to_owned(),
                color: annotation.color()?.map(annotation_color_from_mupdf),
                opacity: annotation.opacity()?,
            });
        }
        Err(anyhow!(
            "Highlight xref {} disappeared after mutation",
            id.xref
        ))
    }

    fn document_snapshot_for_undo(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut options = PdfWriteOptions::default();
        options
            .set_incremental(false)
            // Undo bytes must retain the current security settings; the
            // default writer option would remove encryption from the snapshot.
            .set_encryption(Encryption::Keep);
        self.document
            .write_to_with_options(&mut bytes, options)
            .context("failed to snapshot the PDF before annotation mutation")?;
        Ok(bytes)
    }

    fn restore_document_snapshot(&mut self, bytes: &[u8]) -> Result<()> {
        let document = PdfDocument::from_bytes(bytes)
            .context("failed to reopen the pre-mutation PDF snapshot")?;
        let page_bounds = load_page_bounds(&document)?;
        ensure!(
            page_bounds.len() == self.page_bounds.len(),
            "undo snapshot page count changed"
        );
        self.document = document;
        self.page_bounds = page_bounds;
        self.display_list = None;
        // A memory-backed restored document cannot use MuPDF's path-associated
        // incremental writer safely; the next save must use verified full rewrite.
        self.incremental_association_lost = true;
        Ok(())
    }

    fn rollback_failed_mutation(
        &mut self,
        undo_document: Vec<u8>,
        mutation_error: anyhow::Error,
    ) -> Result<EditAction> {
        match self.restore_document_snapshot(&undo_document) {
            Ok(()) => Err(mutation_error),
            Err(rollback_error) => Err(anyhow!(
                "{mutation_error:#}; additionally failed to restore the pre-mutation PDF: {rollback_error:#}"
            )),
        }
    }

    /// Undoes only the latest unsaved application edit.
    ///
    /// Create is reversed by its exact xref. Update and delete restore a
    /// pre-mutation PDF snapshot so external appearance streams and metadata
    /// are not reconstructed from an incomplete application model.
    pub(super) fn undo(&mut self, action: EditAction) -> Result<()> {
        let pending = self
            .pending_edits
            .last()
            .context("there is no unsaved application edit to undo")?;
        ensure!(
            pending.action == action,
            "edit action is not the latest unsaved application edit"
        );
        let undo = pending.undo.clone();
        match undo {
            UndoEdit::DeleteCreatedAnnotation(id) => {
                self.validate_highlight_target(id)?;
                self.apply_annotation_delete(id)?;
            }
            UndoEdit::RestoreDocument(bytes) => {
                self.restore_document_snapshot(&bytes)?;
            }
        }
        self.pending_edits.pop();
        self.display_list = None;
        self.revision += 1;
        Ok(())
    }

    /// Saves the in-memory PDF, choosing MuPDF's safe incremental path first.
    ///
    /// A document that needs a full rewrite is written to a same-directory
    /// temporary PDF, verified there, and atomically replaced only after the
    /// original version is checked again. This keeps a failed write from
    /// truncating the user's only copy.
    pub(super) fn save(&mut self) -> Result<usize> {
        ensure_current_version(&self.path, self.version)?;
        if self.should_save_incrementally() {
            self.save_incrementally_verified()
        } else {
            self.save_full_rewrite()
        }
    }

    fn should_save_incrementally(&self) -> bool {
        !self.incremental_association_lost && self.document.can_be_saved_incrementally()
    }

    fn save_incrementally_verified(&mut self) -> Result<usize> {
        let file_name = self
            .path
            .to_str()
            .context("MuPDF cannot save a path that is not valid Unicode")?;
        let expected_highlights = highlight_count(&self.document)?;
        let expected_page_count = self.page_bounds.len();
        let mut options = PdfWriteOptions::default();
        options
            .set_incremental(true)
            // Keep the original encryption settings instead of allowing the
            // writer default to silently change a protected document.
            .set_encryption(Encryption::Keep);
        self.document
            .save_with_options(file_name, options)
            .with_context(|| format!("failed to save PDF: {}", self.path.display()))?;

        let reopened = PdfDocument::open(file_name)
            .context("saved PDF could not be reopened for verification")?;
        let (verified_highlights, page_bounds) = verify_saved_document(
            &reopened,
            expected_page_count,
            expected_highlights,
            &self.pending_edits,
        )?;
        self.update_after_save(reopened, page_bounds)?;
        Ok(verified_highlights)
    }

    fn save_full_rewrite(&mut self) -> Result<usize> {
        let parent = self
            .path
            .parent()
            .context("PDF path has no parent directory for atomic replacement")?;
        let expected_highlights = highlight_count(&self.document)?;
        let expected_page_count = self.page_bounds.len();
        let named_temp = TempFileBuilder::new()
            .prefix(".lunapdf-")
            .suffix(".pdf")
            .tempfile_in(parent)
            .with_context(|| format!("failed to create temporary PDF in {}", parent.display()))?;
        let temp_path = named_temp.into_temp_path();

        if let Err(error) = preserve_temp_permissions(&self.path, &temp_path) {
            return Err(cleanup_temp_after_error(temp_path, error));
        }
        let temp_name = match temp_path.to_str() {
            Some(name) => name.to_owned(),
            None => {
                return Err(cleanup_temp_after_error(
                    temp_path,
                    anyhow!("MuPDF cannot save a temporary path that is not valid Unicode"),
                ));
            }
        };
        let mut options = PdfWriteOptions::default();
        options
            .set_incremental(false)
            // The temp path already carries the source file permissions. Keep
            // the PDF encryption too; the writer default would remove it.
            .set_encryption(Encryption::Keep);

        if let Err(error) = self
            .document
            .save_with_options(&temp_name, options)
            .with_context(|| format!("failed to write temporary PDF: {temp_name}"))
        {
            return Err(cleanup_temp_after_error(temp_path, error));
        }
        if let Err(error) = sync_file(&temp_path) {
            return Err(cleanup_temp_after_error(temp_path, error));
        }

        let temporary_document = match PdfDocument::open(&temp_name)
            .context("temporary PDF could not be reopened for verification")
        {
            Ok(document) => document,
            Err(error) => return Err(cleanup_temp_after_error(temp_path, error)),
        };
        if let Err(error) = verify_saved_document(
            &temporary_document,
            expected_page_count,
            expected_highlights,
            &self.pending_edits,
        ) {
            return Err(cleanup_temp_after_error(temp_path, error));
        }
        drop(temporary_document);

        // MuPDF's path-backed document may retain an ordinary FILE* handle.
        // On Windows that handle can deny the replacement even after Rust drops
        // its wrapper, so build a handle-free recovery document from the
        // verified bytes before releasing the original handle. The transient
        // whole-file copy is limited to this rare path and preserves the user's
        // in-memory edits if the atomic replacement itself fails.
        let temporary_bytes = match fs::read(&temp_path)
            .with_context(|| format!("failed to read verified temporary PDF: {temp_name}"))
        {
            Ok(bytes) => bytes,
            Err(error) => return Err(cleanup_temp_after_error(temp_path, error)),
        };
        let recovery_document = match PdfDocument::from_bytes(&temporary_bytes)
            .context("verified temporary PDF could not be opened from memory")
        {
            Ok(document) => document,
            Err(error) => return Err(cleanup_temp_after_error(temp_path, error)),
        };
        if let Err(error) = verify_saved_document(
            &recovery_document,
            expected_page_count,
            expected_highlights,
            &self.pending_edits,
        ) {
            return Err(cleanup_temp_after_error(temp_path, error));
        }

        let destination = self.path.clone();
        let expected_version = self.version;
        let mut recovery_document = Some(recovery_document);
        persist_temp_if_current(temp_path, &destination, expected_version, || {
            let replacement = recovery_document
                .take()
                .expect("recovery document callback executes at most once");
            let previous = std::mem::replace(&mut self.document, replacement);
            drop(previous);
            self.display_list = None;
            self.incremental_association_lost = true;
        })?;

        let file_name = self
            .path
            .to_str()
            .context("MuPDF cannot reopen a replaced path that is not valid Unicode")?;
        let reopened = PdfDocument::open(file_name).map_err(|error| {
            anyhow!(
                "replacement completed but verification failed: saved PDF could not be reopened: {error}"
            )
        })?;
        let (verified_highlights, page_bounds) = verify_saved_document(
            &reopened,
            expected_page_count,
            expected_highlights,
            &self.pending_edits,
        )
        .map_err(|error| anyhow!("replacement completed but verification failed: {error:#}"))?;
        self.update_after_save(reopened, page_bounds)
            .map_err(|error| anyhow!("replacement completed but verification failed: {error:#}"))?;
        Ok(verified_highlights)
    }

    fn update_after_save(
        &mut self,
        reopened: PdfDocument,
        page_bounds: Vec<PageRect>,
    ) -> Result<()> {
        let highlight_capability = determine_highlight_capability(&reopened, &self.path)?;
        let version = read_document_version(&self.path)?;
        self.document = reopened;
        self.page_bounds = page_bounds;
        self.highlight_capability = highlight_capability;
        self.version = version;
        self.pending_edits.clear();
        self.display_list = None;
        self.incremental_association_lost = false;
        self.revision += 1;
        Ok(())
    }
}

fn ensure_current_version(path: &Path, expected: DocumentVersion) -> Result<()> {
    let current_version = read_document_version(path)?;
    ensure!(
        current_version == expected,
        "the PDF changed outside LunaPDF; refusing to overwrite it"
    );
    Ok(())
}

/// Replaces the original only while it still matches the version opened by this worker.
///
/// The comparison cannot make the rename a filesystem compare-and-swap, but keeping it
/// adjacent to `persist` minimizes the unavoidable race and centralizes cleanup on every
/// pre-replacement failure.
fn persist_temp_if_current(
    temp_path: TempPath,
    destination: &Path,
    expected_version: DocumentVersion,
    release_original_handle: impl FnOnce(),
) -> Result<()> {
    if let Err(error) = ensure_current_version(destination, expected_version) {
        return Err(cleanup_temp_after_error(temp_path, error));
    }
    release_original_handle();
    replace_temp_file(temp_path, destination)
}

#[cfg(not(windows))]
fn preserve_temp_permissions(source: &Path, temp_path: &Path) -> Result<()> {
    let permissions = fs::metadata(source)
        .with_context(|| format!("failed to read PDF permissions: {}", source.display()))?
        .permissions();
    fs::set_permissions(temp_path, permissions)
        .with_context(|| format!("failed to preserve permissions on {}", temp_path.display()))?;
    Ok(())
}

#[cfg(windows)]
fn preserve_temp_permissions(_source: &Path, _temp_path: &Path) -> Result<()> {
    // ReplaceFileW merges the original file's DACL, attributes, encryption,
    // compression, and named streams. Pre-copying Rust's readonly bit would be
    // incomplete and tempfile::keep must normalize the replacement first.
    Ok(())
}

#[cfg(not(windows))]
fn replace_temp_file(temp_path: TempPath, destination: &Path) -> Result<()> {
    if let Err(error) = temp_path.persist(destination) {
        let tempfile::PathPersistError { error, path } = error;
        return Err(cleanup_temp_after_error(
            path,
            anyhow!(
                "failed to atomically replace {}: {error}",
                destination.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_temp_file(temp_path: TempPath, destination: &Path) -> Result<()> {
    let replacement = match temp_path.keep() {
        Ok(path) => path,
        Err(tempfile::PathPersistError { error, path }) => {
            return Err(cleanup_temp_after_error(
                path,
                anyhow!("failed to prepare temporary PDF for replacement: {error}"),
            ));
        }
    };
    let destination_wide = match wide_path(destination) {
        Ok(path) => path,
        Err(error) => return cleanup_kept_temp_after_error(&replacement, error),
    };
    let replacement_wide = match wide_path(&replacement) {
        Ok(path) => path,
        Err(error) => return cleanup_kept_temp_after_error(&replacement, error),
    };
    // Passing no IGNORE_* flag is deliberate: a merge failure must abort
    // instead of silently replacing the PDF with relaxed ACLs or attributes.
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced != 0 {
        return Ok(());
    }

    let error = anyhow!(
        "failed to atomically replace {} while preserving Windows metadata: {}",
        destination.display(),
        std::io::Error::last_os_error()
    );
    cleanup_kept_temp_after_error(&replacement, error)
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Result<Vec<u16>> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    ensure!(
        !wide.contains(&0),
        "Windows cannot replace a path containing an embedded NUL: {}",
        path.display()
    );
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn cleanup_kept_temp_after_error(path: &Path, error: anyhow::Error) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Err(error),
        // ReplaceFileW can consume the replacement while still reporting a
        // merge/move error. Missing temp cleanup must not hide that first error.
        Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => Err(error),
        Err(cleanup_error) => Err(anyhow!(
            "{error:#}; additionally failed to remove temporary PDF: {cleanup_error}"
        )),
    }
}

fn verify_saved_document(
    document: &PdfDocument,
    expected_page_count: usize,
    expected_highlights: usize,
    pending_edits: &[PendingEdit],
) -> Result<(usize, Vec<PageRect>)> {
    let verified_highlights = highlight_count(document)?;
    ensure!(
        verified_highlights == expected_highlights,
        "saved PDF Highlight count changed during verification"
    );
    let page_bounds = load_page_bounds(document)?;
    ensure!(
        page_bounds.len() == expected_page_count,
        "saved PDF page count changed during verification"
    );
    let mut verified_ids = HashSet::new();
    for pending in pending_edits.iter().rev() {
        let id = match &pending.expected {
            ExpectedAnnotationMutation::Present(state) => state.id,
            ExpectedAnnotationMutation::Absent(id) => *id,
        };
        // Several unsaved actions can target one annotation. Only its latest
        // expected state describes the final PDF; earlier entries remain solely
        // for LIFO Undo and must not be verified as simultaneous states.
        if verified_ids.insert(id) {
            verify_annotation_mutation(document, &pending.expected)?;
        }
    }
    Ok((verified_highlights, page_bounds))
}

fn sync_file(path: &Path) -> Result<()> {
    // Windows FlushFileBuffers requires a write-capable handle even though no
    // bytes are changed here; a read-only handle makes every full rewrite fail.
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| {
            format!(
                "failed to reopen temporary PDF for syncing: {}",
                path.display()
            )
        })?;
    file.sync_all()
        .with_context(|| format!("failed to sync temporary PDF: {}", path.display()))?;
    Ok(())
}

fn cleanup_temp_after_error(temp_path: TempPath, error: anyhow::Error) -> anyhow::Error {
    match temp_path.close() {
        Ok(()) => error,
        Err(cleanup_error) => {
            anyhow!("{error:#}; additionally failed to remove temporary PDF: {cleanup_error}")
        }
    }
}

fn read_document_version(path: &Path) -> Result<DocumentVersion> {
    // Handle-based metadata supplies Windows volume/file IDs; path-only
    // metadata may omit them and cannot detect same-size path replacement.
    let file = fs::File::open(path)
        .with_context(|| format!("failed to open PDF metadata: {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to read PDF metadata: {}", path.display()))?;
    let (identity_primary, identity_secondary) = file_identity(&file, &metadata)?;
    Ok(DocumentVersion {
        identity_primary,
        identity_secondary,
        length: metadata.len(),
        modified: metadata
            .modified()
            .with_context(|| format!("failed to read PDF modification time: {}", path.display()))?,
    })
}

/// Rejects a path replacement or in-place write racing MuPDF's open/read pass.
fn stable_open_version(before: DocumentVersion, after: DocumentVersion) -> Result<DocumentVersion> {
    ensure!(
        before == after,
        "the PDF changed while LunaPDF was opening it"
    );
    Ok(after)
}

/// Converts a page-local tile request into MuPDF device coordinates.
///
/// The final intersection tolerates a one-pixel edge difference between the
/// Rust layout calculation and MuPDF's rectangle rounding, but rejects a tile
/// that does not overlap the page at all.
fn tile_clip(page_bounds: IRect, spec: TileSpec) -> Result<IRect> {
    let local_x0 = i32::try_from(spec.pixel_x).context("tile x exceeds MuPDF's range")?;
    let local_y0 = i32::try_from(spec.pixel_y).context("tile y exceeds MuPDF's range")?;
    let width = i32::try_from(spec.pixel_width).context("tile width exceeds MuPDF's range")?;
    let height = i32::try_from(spec.pixel_height).context("tile height exceeds MuPDF's range")?;
    let local_x1 = local_x0
        .checked_add(width)
        .context("tile right edge overflowed")?;
    let local_y1 = local_y0
        .checked_add(height)
        .context("tile bottom edge overflowed")?;
    let requested = IRect::new(
        page_bounds
            .x0
            .checked_add(local_x0)
            .context("tile device x overflowed")?,
        page_bounds
            .y0
            .checked_add(local_y0)
            .context("tile device y overflowed")?,
        page_bounds
            .x0
            .checked_add(local_x1)
            .context("tile device right edge overflowed")?,
        page_bounds
            .y0
            .checked_add(local_y1)
            .context("tile device bottom edge overflowed")?,
    );
    let clip = requested.intersect(&page_bounds);
    ensure!(!clip.is_empty(), "tile does not intersect the PDF page");
    Ok(clip)
}

#[cfg(unix)]
fn file_identity(_file: &fs::File, metadata: &fs::Metadata) -> Result<(u64, u64)> {
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &fs::File, _metadata: &fs::Metadata) -> Result<(u64, u64)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a valid open handle and `information` remains writable
    // for the duration of this synchronous Windows API call.
    let succeeded = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    ensure!(
        succeeded != 0,
        "failed to query the Windows PDF file identity: {}",
        std::io::Error::last_os_error()
    );

    // Windows splits the persistent 64-bit file index into two DWORDs; keep
    // the complete value so a same-size path replacement is still detected.
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((u64::from(information.dwVolumeSerialNumber), file_index))
}

fn load_page_bounds(document: &PdfDocument) -> Result<Vec<PageRect>> {
    let page_count =
        usize::try_from(document.page_count()?).context("MuPDF returned a negative page count")?;
    let mut page_bounds = Vec::with_capacity(page_count);
    for page_index in 0..page_count {
        let page = document.load_pdf_page(page_number(page_index, page_count)?)?;
        let bounds = page.bounds()?;
        page_bounds.push(PageRect {
            x0: bounds.x0,
            y0: bounds.y0,
            x1: bounds.x1,
            y1: bounds.y1,
        });
    }
    Ok(page_bounds)
}

fn determine_highlight_capability(
    document: &PdfDocument,
    path: &Path,
) -> Result<HighlightCapability> {
    let file_is_read_only = fs::metadata(path)?.permissions().readonly();
    let annotation_allowed = document.permissions().contains(Permission::ANNOTATE);
    let has_signed_signature = document_has_signed_signature(document)?;
    Ok(highlight_capability_from_constraints(
        file_is_read_only,
        annotation_allowed,
        has_signed_signature,
    ))
}

fn highlight_capability_from_constraints(
    file_is_read_only: bool,
    annotation_allowed: bool,
    has_signed_signature: bool,
) -> HighlightCapability {
    // These checks are ordered by the restriction the user can act on most
    // directly. A non-incremental document remains editable because the save
    // strategy now verifies and atomically replaces a full-file temporary.
    if file_is_read_only {
        HighlightCapability::ReadOnlyFile
    } else if !annotation_allowed {
        HighlightCapability::AnnotationPermissionDenied
    } else if has_signed_signature {
        HighlightCapability::SignedDocument
    } else {
        HighlightCapability::Allowed
    }
}

fn document_has_signed_signature(document: &PdfDocument) -> Result<bool> {
    let page_count =
        usize::try_from(document.page_count()?).context("MuPDF returned a negative page count")?;
    for page_index in 0..page_count {
        let page = document.load_pdf_page(page_number(page_index, page_count)?)?;
        for widget in page.widgets() {
            if widget.r#type()? == WidgetType::Signature && widget.is_signed()? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn load_text_snapshot(
    document: &PdfDocument,
    page_index: usize,
    page_count: usize,
) -> Result<(TextPage, Vec<GlyphSnapshot>, Duration)> {
    let page = document.load_pdf_page(page_number(page_index, page_count)?)?;
    let extraction_started = Instant::now();
    // Empty flags record MuPDF's standard extraction baseline. Typst-specific
    // bounding-box adjustments require the documented comparison first.
    let text_page = page.to_text_page(TextPageFlags::empty())?;
    let structured = text_page.structured();
    let mut glyphs = Vec::new();
    let mut line_index = 0;

    for block in structured.blocks {
        let TextBlockContent::Text { lines } = block.content else {
            continue;
        };
        for line in lines {
            glyphs.extend(line.chars.into_iter().map(|character| GlyphSnapshot {
                character: character.ch,
                quad: page_quad_from_mupdf(&character.quad),
                line_index,
            }));
            line_index += 1;
        }
    }

    Ok((text_page, glyphs, extraction_started.elapsed()))
}

fn pixmap_samples_to_rgba(
    samples: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    component_count: usize,
) -> Result<Vec<u8>> {
    ensure!(
        component_count == 3 || component_count == 4,
        "expected an RGB or RGBA MuPDF pixmap, got {component_count} components"
    );
    let row_bytes = width
        .checked_mul(component_count)
        .context("pixmap row byte count overflowed")?;
    ensure!(
        stride >= row_bytes,
        "MuPDF pixmap stride is shorter than one row"
    );
    let required_bytes = stride
        .checked_mul(height)
        .context("pixmap byte count overflowed")?;
    ensure!(
        samples.len() >= required_bytes,
        "MuPDF pixmap sample buffer is shorter than its dimensions"
    );

    let output_capacity = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("RGBA output byte count overflowed")?;
    let mut rgba = Vec::with_capacity(output_capacity);
    for row in samples.chunks(stride).take(height) {
        for pixel in row[..row_bytes].chunks_exact(component_count) {
            rgba.extend_from_slice(&pixel[..3]);
            rgba.push(if component_count == 4 { pixel[3] } else { 255 });
        }
    }
    Ok(rgba)
}

fn highlight_count(document: &PdfDocument) -> Result<usize> {
    let page_count =
        usize::try_from(document.page_count()?).context("MuPDF returned a negative page count")?;
    let mut count = 0;
    for page_index in 0..page_count {
        let page = document.load_pdf_page(page_number(page_index, page_count)?)?;
        for annotation in page.annotations() {
            if annotation.r#type()? == PdfAnnotationType::Highlight {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn verify_annotation_mutation(
    document: &PdfDocument,
    expected: &ExpectedAnnotationMutation,
) -> Result<()> {
    let id = match expected {
        ExpectedAnnotationMutation::Present(state) => state.id,
        ExpectedAnnotationMutation::Absent(id) => *id,
    };
    let page_count =
        usize::try_from(document.page_count()?).context("MuPDF returned a negative page count")?;
    let page = document.load_pdf_page(page_number(id.page_index, page_count)?)?;
    for annotation in page.annotations() {
        if annotation.xref()? != id.xref {
            continue;
        }
        match expected {
            ExpectedAnnotationMutation::Absent(_) => {
                return Err(anyhow!(
                    "saved PDF still contains deleted annotation xref {} on page {}",
                    id.xref,
                    id.page_index + 1
                ));
            }
            ExpectedAnnotationMutation::Present(state) => {
                ensure!(
                    annotation.r#type()? == PdfAnnotationType::Highlight,
                    "saved annotation xref {} changed type",
                    id.xref
                );
                let actual_quads = annotation.quad_points()?;
                ensure!(
                    actual_quads.len() == state.quads.len()
                        && actual_quads
                            .iter()
                            .zip(&state.quads)
                            .all(|(actual, expected)| quad_matches(actual, expected)),
                    "saved annotation xref {} changed Quad geometry",
                    id.xref
                );
                ensure!(
                    annotation.contents()?.unwrap_or_default() == state.contents,
                    "saved annotation xref {} changed Contents",
                    id.xref
                );
                let color = annotation.color()?.map(annotation_color_from_mupdf);
                ensure!(
                    annotation_colors_match(color, state.color),
                    "saved annotation xref {} changed color",
                    id.xref
                );
                ensure!(
                    (annotation.opacity()? - state.opacity).abs() <= PDF_PROPERTY_TOLERANCE,
                    "saved annotation xref {} changed opacity",
                    id.xref
                );
                return Ok(());
            }
        }
    }
    match expected {
        ExpectedAnnotationMutation::Present(_) => Err(anyhow!(
            "saved PDF does not contain annotation xref {} on page {}",
            id.xref,
            id.page_index + 1
        )),
        ExpectedAnnotationMutation::Absent(_) => Ok(()),
    }
}

fn annotation_colors_match(
    actual: Option<PdfAnnotationColor>,
    expected: Option<PdfAnnotationColor>,
) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(PdfAnnotationColor::Gray(actual)), Some(PdfAnnotationColor::Gray(expected))) => {
            property_matches(actual, expected)
        }
        (
            Some(PdfAnnotationColor::Rgb {
                red: actual_red,
                green: actual_green,
                blue: actual_blue,
            }),
            Some(PdfAnnotationColor::Rgb {
                red: expected_red,
                green: expected_green,
                blue: expected_blue,
            }),
        ) => {
            property_matches(actual_red, expected_red)
                && property_matches(actual_green, expected_green)
                && property_matches(actual_blue, expected_blue)
        }
        (
            Some(PdfAnnotationColor::Cmyk {
                cyan: actual_cyan,
                magenta: actual_magenta,
                yellow: actual_yellow,
                key: actual_key,
            }),
            Some(PdfAnnotationColor::Cmyk {
                cyan: expected_cyan,
                magenta: expected_magenta,
                yellow: expected_yellow,
                key: expected_key,
            }),
        ) => {
            property_matches(actual_cyan, expected_cyan)
                && property_matches(actual_magenta, expected_magenta)
                && property_matches(actual_yellow, expected_yellow)
                && property_matches(actual_key, expected_key)
        }
        _ => false,
    }
}

fn property_matches(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= PDF_PROPERTY_TOLERANCE
}

fn quad_matches(actual: &Quad, expected: &PageQuad) -> bool {
    point_matches(&actual.ul, expected.upper_left)
        && point_matches(&actual.ur, expected.upper_right)
        && point_matches(&actual.ll, expected.lower_left)
        && point_matches(&actual.lr, expected.lower_right)
}

fn point_matches(actual: &Point, expected: PagePoint) -> bool {
    (actual.x - expected.x).abs() <= PDF_COORDINATE_TOLERANCE
        && (actual.y - expected.y).abs() <= PDF_COORDINATE_TOLERANCE
}

fn page_number(page_index: usize, page_count: usize) -> Result<i32> {
    ensure!(
        page_index < page_count,
        "page index {page_index} is outside the document's {page_count} pages"
    );
    i32::try_from(page_index).context("page index exceeds MuPDF's supported range")
}

fn outline_item(outline: Outline, page_count: usize) -> OutlineItem {
    let page_index = outline
        .dest
        .and_then(|destination| usize::try_from(destination.loc.page_number).ok())
        .filter(|page_index| *page_index < page_count);
    OutlineItem {
        title: outline.title,
        page_index,
        children: outline
            .down
            .into_iter()
            .map(|child| outline_item(child, page_count))
            .collect(),
    }
}

fn physical_memory_bytes() -> Option<usize> {
    memory_stats::memory_stats().map(|stats| stats.physical_mem)
}

fn page_quad_from_mupdf(quad: &Quad) -> PageQuad {
    PageQuad {
        upper_left: PagePoint::new(quad.ul.x, quad.ul.y),
        upper_right: PagePoint::new(quad.ur.x, quad.ur.y),
        lower_left: PagePoint::new(quad.ll.x, quad.ll.y),
        lower_right: PagePoint::new(quad.lr.x, quad.lr.y),
    }
}

fn annotation_color_from_mupdf(color: AnnotationColor) -> PdfAnnotationColor {
    match color {
        AnnotationColor::Gray(gray) => PdfAnnotationColor::Gray(gray),
        AnnotationColor::Rgb { red, green, blue } => PdfAnnotationColor::Rgb { red, green, blue },
        AnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        } => PdfAnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        },
    }
}

fn annotation_color_to_mupdf(color: PdfAnnotationColor) -> AnnotationColor {
    match color {
        PdfAnnotationColor::Gray(gray) => AnnotationColor::Gray(gray),
        PdfAnnotationColor::Rgb { red, green, blue } => AnnotationColor::Rgb { red, green, blue },
        PdfAnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        } => AnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        },
    }
}

fn mupdf_quad_from_page(quad: &PageQuad) -> Quad {
    Quad::new(
        Point::new(quad.upper_left.x, quad.upper_left.y),
        Point::new(quad.upper_right.x, quad.upper_right.y),
        Point::new(quad.lower_left.x, quad.lower_left.y),
        Point::new(quad.lower_right.x, quad.lower_right.y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(windows)]
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, SetFileAttributesW,
    };

    use mupdf::pdf::{Encryption, PdfObject};
    use mupdf::shape::{Shape, TextOptions};
    use mupdf::{DestinationKind, Size};
    use mupdf::{document::Location, link::LinkDestination};

    fn tile_request(page_index: usize, scale: f32, generation: u64, revision: u64) -> TileRequest {
        tile_request_with_spec(
            page_index,
            scale,
            generation,
            revision,
            TileSpec {
                pixel_x: 0,
                pixel_y: 0,
                pixel_width: 400,
                pixel_height: 300,
            },
        )
    }

    fn tile_request_with_spec(
        page_index: usize,
        scale: f32,
        generation: u64,
        revision: u64,
        spec: TileSpec,
    ) -> TileRequest {
        TileRequest {
            page_index,
            zoom: scale,
            pixels_per_point: 1.0,
            scale,
            generation,
            expected_revision: revision,
            spec,
            priority: crate::domain::document::RenderPriority::Visible,
        }
    }

    #[test]
    fn rgb_rows_with_padding_are_converted_to_rgba() {
        let samples = [10, 20, 30, 40, 50, 60, 0, 0];

        let rgba = pixmap_samples_to_rgba(&samples, 2, 1, 8, 3).unwrap();

        assert_eq!(rgba, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn unexpected_pixmap_components_fail_explicitly() {
        let error = pixmap_samples_to_rgba(&[128], 1, 1, 1, 1).unwrap_err();

        assert!(error.to_string().contains("got 1 components"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_identity_is_stable_across_file_handles() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.pdf");
        fs::write(&path, b"file identity fixture").unwrap();
        let first_identity = {
            let first = fs::File::open(&path).unwrap();
            let first_metadata = first.metadata().unwrap();
            file_identity(&first, &first_metadata).unwrap()
        };
        let second = fs::File::open(&path).unwrap();
        let second_metadata = second.metadata().unwrap();

        assert_eq!(
            first_identity,
            file_identity(&second, &second_metadata).unwrap()
        );
    }

    #[test]
    fn document_change_during_open_is_rejected() {
        let before = DocumentVersion {
            identity_primary: 1,
            identity_secondary: 2,
            length: 100,
            modified: SystemTime::UNIX_EPOCH,
        };
        let after = DocumentVersion {
            length: 101,
            ..before
        };

        assert!(stable_open_version(before, before).is_ok());
        assert!(stable_open_version(before, after).is_err());
    }

    #[test]
    fn password_protected_pdf_is_rejected_before_page_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("encrypted.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut options = PdfWriteOptions::default();
            options
                .set_encryption(Encryption::Aes256)
                .set_user_password("user-password")
                .set_owner_password("owner-password");
            document.save_with_options(path_text, options).unwrap();
        }

        let error = MuPdfBackend::open(path)
            .err()
            .expect("encrypted PDF must fail");

        assert!(
            error
                .to_string()
                .contains("password-protected documents are not supported")
        );
    }

    #[test]
    fn corrupt_pdf_is_rejected_without_changing_original_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.pdf");
        let bytes = b"not a PDF";
        fs::write(&path, bytes).unwrap();

        assert!(MuPdfBackend::open(path.clone()).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn readonly_file_reports_readonly_highlight_capability() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.pdf");
        write_blank_pdf_for_test(&path);

        let original_permissions = fs::metadata(&path).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        let backend = MuPdfBackend::open(path.clone()).unwrap();

        assert_eq!(
            backend.info().unwrap().highlight_capability,
            HighlightCapability::ReadOnlyFile
        );

        // Windows may deny deletion while the readonly attribute is set;
        // restoring the original mode/attribute keeps tempfile cleanup
        // portable without weakening the capability check above.
        fs::set_permissions(path, original_permissions).unwrap();
    }

    fn write_blank_pdf_for_test(path: &Path) {
        let mut document = PdfDocument::new();
        let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
        document.save(path.to_str().unwrap()).unwrap();
    }

    #[test]
    fn highlight_capability_reports_each_persistence_restriction() {
        assert_eq!(
            highlight_capability_from_constraints(true, true, false),
            HighlightCapability::ReadOnlyFile
        );
        assert_eq!(
            highlight_capability_from_constraints(false, false, false),
            HighlightCapability::AnnotationPermissionDenied
        );
        assert_eq!(
            highlight_capability_from_constraints(false, true, true),
            HighlightCapability::SignedDocument
        );
        assert_eq!(
            highlight_capability_from_constraints(false, true, false),
            HighlightCapability::Allowed
        );
    }

    #[test]
    fn reads_existing_highlight_identity_geometry_comment_color_and_opacity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing-highlight.pdf");
        let path_text = path.to_str().unwrap();
        let expected_xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut annotation = page
                .add_highlight_annotation(Quad::from(Rect::new(30.0, 40.0, 130.0, 60.0)))
                .unwrap();
            annotation
                .set_color(AnnotationColor::Rgb {
                    red: 0.1,
                    green: 0.2,
                    blue: 0.8,
                })
                .unwrap();
            annotation
                .set_contents("外部コメント\nsecond line")
                .unwrap();
            annotation.set_opacity(0.65).unwrap();
            expected_xref = annotation.xref().unwrap();
            annotation.update().unwrap();
            page.update().unwrap();
            drop(annotation);
            drop(page);
            document.save(path_text).unwrap();
        }

        let backend = MuPdfBackend::open(path).unwrap();
        let snapshot = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();

        assert_eq!(snapshot.annotations.len(), 1);
        let annotation = &snapshot.annotations[0];
        assert_eq!(annotation.id.xref, expected_xref);
        assert_eq!(annotation.contents, "外部コメント\nsecond line");
        assert!((annotation.opacity - 0.65).abs() < f32::EPSILON);
        assert_eq!(annotation.quads.len(), 1);
        assert!(annotation.can_edit_contents);
        assert!(annotation.can_edit_color);
        assert!(annotation.can_delete);
        let Some(PdfAnnotationColor::Rgb { red, green, blue }) = annotation.color else {
            panic!("expected the existing RGB annotation color");
        };
        assert!((red - 0.1).abs() < f32::EPSILON);
        assert!((green - 0.2).abs() < f32::EPSILON);
        assert!((blue - 0.8).abs() < f32::EPSILON);

        let repeated = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();
        assert_eq!(repeated.annotations[0].id.xref, expected_xref);
        assert!(
            backend
                .annotation_page(AnnotationPageRequest {
                    page_index: 0,
                    expected_revision: 1,
                })
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn annotation_flags_limit_only_the_supported_edits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("locked-highlights.pdf");
        let path_text = path.to_str().unwrap();
        let contents_locked_xref;
        let properties_locked_xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut contents_locked = page
                .add_highlight_annotation(Quad::from(Rect::new(30.0, 40.0, 130.0, 60.0)))
                .unwrap();
            contents_locked
                .set_flags(AnnotationFlags::IS_LOCKED_CONTENTS)
                .unwrap();
            contents_locked_xref = contents_locked.xref().unwrap();
            contents_locked.update().unwrap();
            drop(contents_locked);

            let mut properties_locked = page
                .add_highlight_annotation(Quad::from(Rect::new(30.0, 80.0, 130.0, 100.0)))
                .unwrap();
            properties_locked
                .set_flags(AnnotationFlags::IS_LOCKED)
                .unwrap();
            properties_locked_xref = properties_locked.xref().unwrap();
            properties_locked.update().unwrap();
            page.update().unwrap();
            drop(properties_locked);
            drop(page);
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let snapshot = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();
        let contents_locked = snapshot
            .annotations
            .iter()
            .find(|annotation| annotation.id.xref == contents_locked_xref)
            .unwrap();
        let properties_locked = snapshot
            .annotations
            .iter()
            .find(|annotation| annotation.id.xref == properties_locked_xref)
            .unwrap();

        assert!(!contents_locked.can_edit_contents);
        assert!(contents_locked.can_edit_color);
        assert!(contents_locked.can_delete);
        assert!(!properties_locked.can_edit_contents);
        assert!(!properties_locked.can_edit_color);
        assert!(!properties_locked.can_delete);
        assert!(
            backend
                .update_annotation(AnnotationUpdateRequest {
                    id: contents_locked.id,
                    expected_revision: 0,
                    contents: Some("拒否される更新".to_owned()),
                    color: None,
                })
                .is_err()
        );
        assert!(
            backend
                .update_annotation(AnnotationUpdateRequest {
                    id: properties_locked.id,
                    expected_revision: 0,
                    contents: None,
                    color: Some(PdfAnnotationColor::Rgb {
                        red: 1.0,
                        green: 0.0,
                        blue: 0.0,
                    }),
                })
                .is_err()
        );
        assert!(
            backend
                .delete_annotation(AnnotationDeleteRequest {
                    id: properties_locked.id,
                    expected_revision: 0,
                })
                .is_err()
        );
        assert_eq!(backend.info().unwrap().revision, 0);
        assert!(!backend.info().unwrap().dirty);
    }

    #[test]
    fn updates_existing_highlight_and_undo_restores_exact_properties() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-highlight.pdf");
        let path_text = path.to_str().unwrap();
        let expected_xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut annotation = page
                .add_highlight_annotation(Quad::from(Rect::new(30.0, 40.0, 130.0, 60.0)))
                .unwrap();
            annotation.set_contents("更新前").unwrap();
            annotation
                .set_color(AnnotationColor::Rgb {
                    red: 0.1,
                    green: 0.2,
                    blue: 0.8,
                })
                .unwrap();
            annotation.set_opacity(0.65).unwrap();
            expected_xref = annotation.xref().unwrap();
            annotation.update().unwrap();
            page.update().unwrap();
            drop(annotation);
            drop(page);
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let id = AnnotationId {
            page_index: 0,
            xref: expected_xref,
        };
        let action = backend
            .update_annotation(AnnotationUpdateRequest {
                id,
                expected_revision: 0,
                contents: Some("日本語\nupdated".to_owned()),
                color: Some(PdfAnnotationColor::Rgb {
                    red: 0.9,
                    green: 0.1,
                    blue: 0.2,
                }),
            })
            .unwrap();

        assert!(matches!(
            action,
            EditAction::UpdateAnnotation {
                annotation_id,
                revision_after: 1
            } if annotation_id == id
        ));
        assert!(backend.info().unwrap().dirty);
        let updated = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 1,
            })
            .unwrap()
            .unwrap();
        let updated = &updated.annotations[0];
        assert_eq!(updated.id, id);
        assert_eq!(updated.contents, "日本語\nupdated");
        assert!(annotation_colors_match(
            updated.color,
            Some(PdfAnnotationColor::Rgb {
                red: 0.9,
                green: 0.1,
                blue: 0.2,
            })
        ));
        assert!(property_matches(updated.opacity, 0.65));

        backend.undo(action).unwrap();

        assert!(!backend.info().unwrap().dirty);
        let restored = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 2,
            })
            .unwrap()
            .unwrap();
        let restored = &restored.annotations[0];
        assert_eq!(restored.id, id);
        assert_eq!(restored.contents, "更新前");
        assert!(annotation_colors_match(
            restored.color,
            Some(PdfAnnotationColor::Rgb {
                red: 0.1,
                green: 0.2,
                blue: 0.8,
            })
        ));
        assert!(property_matches(restored.opacity, 0.65));
    }

    #[test]
    fn update_and_delete_survive_save_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("persist-mutations.pdf");
        let path_text = path.to_str().unwrap();
        let update_xref;
        let delete_xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut update_target = page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 30.0, 120.0, 50.0)))
                .unwrap();
            update_xref = update_target.xref().unwrap();
            update_target.update().unwrap();
            drop(update_target);
            let mut delete_target = page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 70.0, 120.0, 90.0)))
                .unwrap();
            delete_target.set_contents("削除対象").unwrap();
            delete_xref = delete_target.xref().unwrap();
            delete_target.update().unwrap();
            page.update().unwrap();
            drop(delete_target);
            drop(page);
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        backend
            .update_annotation(AnnotationUpdateRequest {
                id: AnnotationId {
                    page_index: 0,
                    xref: update_xref,
                },
                expected_revision: 0,
                contents: Some("保存後コメント\nsecond line".to_owned()),
                color: Some(PdfAnnotationColor::Rgb {
                    red: 0.2,
                    green: 0.8,
                    blue: 0.3,
                }),
            })
            .unwrap();
        backend
            .delete_annotation(AnnotationDeleteRequest {
                id: AnnotationId {
                    page_index: 0,
                    xref: delete_xref,
                },
                expected_revision: 1,
            })
            .unwrap();
        backend
            .update_annotation(AnnotationUpdateRequest {
                id: AnnotationId {
                    page_index: 0,
                    xref: update_xref,
                },
                expected_revision: 2,
                contents: Some("最後のコメント\nsecond line".to_owned()),
                color: None,
            })
            .unwrap();

        assert_eq!(backend.save().unwrap(), 1);
        assert!(!backend.info().unwrap().dirty);
        let saved = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 4,
            })
            .unwrap()
            .unwrap();
        assert_eq!(saved.annotations.len(), 1);
        assert_eq!(saved.annotations[0].id.xref, update_xref);
        assert_eq!(saved.annotations[0].contents, "最後のコメント\nsecond line");
        assert!(annotation_colors_match(
            saved.annotations[0].color,
            Some(PdfAnnotationColor::Rgb {
                red: 0.2,
                green: 0.8,
                blue: 0.3,
            })
        ));
    }

    #[test]
    fn delete_undo_restores_target_and_leaves_other_annotation_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delete-undo.pdf");
        let path_text = path.to_str().unwrap();
        let target_xref;
        let other_xref;
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut target = page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 30.0, 120.0, 50.0)))
                .unwrap();
            target.set_contents("復元対象").unwrap();
            target.set_opacity(0.4).unwrap();
            target_xref = target.xref().unwrap();
            target.update().unwrap();
            drop(target);
            let mut other = page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 70.0, 120.0, 90.0)))
                .unwrap();
            other.set_contents("変更しない").unwrap();
            other_xref = other.xref().unwrap();
            other.update().unwrap();
            page.update().unwrap();
            drop(other);
            drop(page);
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let target_id = AnnotationId {
            page_index: 0,
            xref: target_xref,
        };
        let action = backend
            .delete_annotation(AnnotationDeleteRequest {
                id: target_id,
                expected_revision: 0,
            })
            .unwrap();
        let deleted = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 1,
            })
            .unwrap()
            .unwrap();
        assert_eq!(deleted.annotations.len(), 1);
        assert_eq!(deleted.annotations[0].id.xref, other_xref);

        backend.undo(action).unwrap();

        let restored = backend
            .annotation_page(AnnotationPageRequest {
                page_index: 0,
                expected_revision: 2,
            })
            .unwrap()
            .unwrap();
        assert_eq!(restored.annotations.len(), 2);
        let target = restored
            .annotations
            .iter()
            .find(|annotation| annotation.id == target_id)
            .unwrap();
        assert_eq!(target.contents, "復元対象");
        assert!(property_matches(target.opacity, 0.4));
        assert!(
            restored
                .annotations
                .iter()
                .any(|annotation| annotation.id.xref == other_xref
                    && annotation.contents == "変更しない")
        );
        assert!(!backend.info().unwrap().dirty);
    }

    #[test]
    fn stale_or_wrong_annotation_identity_is_rejected_without_dirtying_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reject-stale-annotation.pdf");
        write_blank_pdf_for_test(&path);
        let mut backend = MuPdfBackend::open(path).unwrap();

        let missing = AnnotationId {
            page_index: 0,
            xref: 999_999,
        };
        assert!(
            backend
                .update_annotation(AnnotationUpdateRequest {
                    id: missing,
                    expected_revision: 1,
                    contents: Some("stale".to_owned()),
                    color: None,
                })
                .is_err()
        );
        assert!(
            backend
                .delete_annotation(AnnotationDeleteRequest {
                    id: missing,
                    expected_revision: 0,
                })
                .is_err()
        );
        assert_eq!(backend.info().unwrap().revision, 0);
        assert!(!backend.info().unwrap().dirty);
    }

    #[test]
    fn second_page_highlight_is_incrementally_saved_and_verified() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("highlight-roundtrip.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _first_page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let _second_page = document.new_page(Size::new(400.0, 300.0)).unwrap();
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let initial = backend.info().unwrap();
        assert_eq!(initial.page_bounds.len(), 2);
        let second_page = backend
            .render_tile(tile_request(1, 1.0, 7, 0))
            .unwrap()
            .unwrap();
        assert_eq!(second_page.page_index, 1);
        assert_eq!(second_page.generation, 7);
        assert!(second_page.page_pixel_width > second_page.page_pixel_height);
        let unannotated_pixels = second_page.pixels_rgba;

        backend
            .create_highlight(
                1,
                &[PageQuad {
                    upper_left: PagePoint::new(40.0, 40.0),
                    upper_right: PagePoint::new(180.0, 40.0),
                    lower_left: PagePoint::new(40.0, 60.0),
                    lower_right: PagePoint::new(180.0, 60.0),
                }],
            )
            .unwrap();
        assert!(backend.info().unwrap().dirty);
        let annotated_page = backend
            .render_tile(tile_request(1, 1.0, 8, 1))
            .unwrap()
            .unwrap();
        assert_ne!(annotated_page.pixels_rgba, unannotated_pixels);

        let verified_highlights = backend.save().unwrap();
        let saved = backend.info().unwrap();
        assert_eq!(verified_highlights, 1);
        assert_eq!(saved.highlight_count, 1);
        assert!(!saved.dirty);
    }

    #[test]
    fn undo_deletes_only_the_exact_created_xref() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("highlight-undo.pdf");
        let path_text = path.to_str().unwrap();
        let existing_quad = Quad::from(Rect::new(20.0, 20.0, 80.0, 40.0));
        let existing_xref = {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut annotation = page.add_highlight_annotation(existing_quad).unwrap();
            annotation.update().unwrap();
            page.update().unwrap();
            drop(annotation);
            drop(page);
            document.save(path_text).unwrap();
            let reopened = PdfDocument::open(path_text).unwrap();
            let page = reopened.load_pdf_page(0).unwrap();
            page.annotations().next().unwrap().xref().unwrap()
        };

        let mut backend = MuPdfBackend::open(path).unwrap();
        let action = backend
            .create_highlight(
                0,
                &[PageQuad {
                    upper_left: PagePoint::new(120.0, 20.0),
                    upper_right: PagePoint::new(180.0, 20.0),
                    lower_left: PagePoint::new(120.0, 40.0),
                    lower_right: PagePoint::new(180.0, 40.0),
                }],
            )
            .unwrap();
        let EditAction::CreateHighlight {
            annotation_xref: created_xref,
            ..
        } = &action
        else {
            panic!("create_highlight must return a create action");
        };
        let created_xref = *created_xref;
        assert_ne!(created_xref, existing_xref);
        assert!(backend.info().unwrap().dirty);
        assert!(
            backend
                .undo(EditAction::CreateHighlight {
                    page_index: 0,
                    annotation_xref: existing_xref,
                })
                .is_err()
        );

        backend.undo(action).unwrap();

        let page = backend.document.load_pdf_page(0).unwrap();
        let remaining_xrefs = page
            .annotations()
            .map(|annotation| annotation.xref().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(remaining_xrefs, vec![existing_xref]);
        assert!(backend.pending_edits.is_empty());
        assert!(!backend.info().unwrap().dirty);
    }

    #[test]
    fn undo_rejects_non_latest_edit_and_preserves_history_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("undo-order.pdf");
        write_blank_pdf_for_test(&path);
        let mut backend = MuPdfBackend::open(path).unwrap();
        let first = backend
            .create_highlight(
                0,
                &[PageQuad {
                    upper_left: PagePoint::new(20.0, 20.0),
                    upper_right: PagePoint::new(80.0, 20.0),
                    lower_left: PagePoint::new(20.0, 40.0),
                    lower_right: PagePoint::new(80.0, 40.0),
                }],
            )
            .unwrap();
        let second = backend
            .create_highlight(
                0,
                &[PageQuad {
                    upper_left: PagePoint::new(20.0, 60.0),
                    upper_right: PagePoint::new(80.0, 60.0),
                    lower_left: PagePoint::new(20.0, 80.0),
                    lower_right: PagePoint::new(80.0, 80.0),
                }],
            )
            .unwrap();

        assert!(backend.undo(first.clone()).is_err());
        assert_eq!(backend.pending_edits.len(), 2);
        assert_eq!(highlight_count(&backend.document).unwrap(), 2);

        backend.undo(second).unwrap();
        backend.undo(first).unwrap();
        assert!(backend.pending_edits.is_empty());
        assert_eq!(highlight_count(&backend.document).unwrap(), 0);
        assert!(!backend.info().unwrap().dirty);
    }

    #[test]
    #[ignore = "writes a PDF to LUNAPDF_ACCEPTANCE_OUTPUT for external inspection"]
    fn exports_highlight_fixture_for_external_viewers() {
        let output = PathBuf::from(
            std::env::var_os("LUNAPDF_ACCEPTANCE_OUTPUT")
                .expect("LUNAPDF_ACCEPTANCE_OUTPUT must name the output PDF"),
        );
        // Requiring an absolute destination prevents an explicit acceptance run
        // from leaving an untracked artifact at a shell-dependent relative path.
        assert!(
            output.is_absolute(),
            "LUNAPDF_ACCEPTANCE_OUTPUT must be an absolute path"
        );
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("external-viewer-highlight.pdf");
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(400.0, 200.0)).unwrap();
            let mut shape = Shape::new(&mut page).unwrap();
            // Keeping the text away from page edges makes the annotation easy to
            // identify without depending on a viewer's page-margin presentation.
            shape
                .insert_text(
                    Point::new(40.0, 100.0),
                    "LunaPDF external viewer highlight",
                    &TextOptions::default(),
                )
                .unwrap()
                .commit(&mut document, true)
                .unwrap();
            document.save(source.to_str().unwrap()).unwrap();
        }

        let mut backend = MuPdfBackend::open(source.clone()).unwrap();
        let text_snapshot = backend
            .text_snapshot(TextSnapshotRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();
        let first = text_snapshot.glyphs.first().unwrap().quad.bounds();
        let last = text_snapshot.glyphs.last().unwrap().quad.bounds();
        let selection = backend
            .select(
                0,
                1,
                PagePoint::new((first.0 + first.2) / 2.0, (first.1 + first.3) / 2.0),
                PagePoint::new((last.0 + last.2) / 2.0, (last.1 + last.3) / 2.0),
            )
            .unwrap();
        assert!(!selection.quads.is_empty());
        backend.create_highlight(0, &selection.quads).unwrap();
        assert_eq!(backend.save().unwrap(), 1);
        drop(backend);

        let output_directory = output.parent().expect("absolute paths have a parent");
        fs::create_dir_all(output_directory).unwrap();
        // create_new is the non-overwrite guarantee: a separate process cannot
        // replace the existence check between validation and the final copy.
        let mut source_file = fs::File::open(source).unwrap();
        let mut output_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .unwrap();
        std::io::copy(&mut source_file, &mut output_file).unwrap();
        output_file.sync_all().unwrap();
    }

    #[test]
    fn redacted_pdf_accepts_highlight_and_is_saved_by_full_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("redacted.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            document.save(path_text).unwrap();
        }

        let original_mode = fs::metadata(&path).unwrap().permissions();
        let mut backend = MuPdfBackend::open(path.clone()).unwrap();
        let mut page = backend.document.load_pdf_page(0).unwrap();
        page.add_redact_annotation(Rect::new(0.0, 0.0, 10.0, 10.0))
            .unwrap();
        assert!(page.apply_redactions().unwrap());
        // Production commands never retain a page outside one backend call.
        // Drop this test-only handle before ReplaceFileW so the test exercises
        // the backend's actual ownership boundary on Windows.
        drop(page);
        assert!(!backend.document.can_be_saved_incrementally());
        assert_eq!(
            backend.info().unwrap().highlight_capability,
            HighlightCapability::Allowed
        );
        backend
            .create_highlight(
                0,
                &[PageQuad {
                    upper_left: PagePoint::new(20.0, 20.0),
                    upper_right: PagePoint::new(80.0, 20.0),
                    lower_left: PagePoint::new(20.0, 40.0),
                    lower_right: PagePoint::new(80.0, 40.0),
                }],
            )
            .unwrap();
        assert!(backend.info().unwrap().dirty);

        assert_eq!(backend.save().unwrap(), 1);
        let saved = backend.info().unwrap();
        assert_eq!(saved.page_bounds.len(), 1);
        assert_eq!(saved.highlight_count, 1);
        assert!(!saved.dirty);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().readonly(),
            original_mode.readonly()
        );
        #[cfg(unix)]
        assert_eq!(fs::metadata(&path).unwrap().mode(), original_mode.mode());
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lunapdf-")
        }));

        let reopened = PdfDocument::open(path_text).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 1);
        assert_eq!(highlight_count(&reopened).unwrap(), 1);
    }

    #[test]
    fn recovery_state_forces_full_rewrite_even_when_mupdf_reports_incremental() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory-recovery.pdf");
        write_blank_pdf_for_test(&path);
        let bytes = fs::read(&path).unwrap();
        let recovery = PdfDocument::from_bytes(&bytes).unwrap();
        let mut backend = MuPdfBackend::open(path).unwrap();

        // MuPDF may report a byte-backed document as incrementally writable;
        // the backend therefore tracks path association explicitly instead of
        // trusting this value for a recovery retry.
        assert!(recovery.can_be_saved_incrementally());
        backend.document = recovery;
        backend.incremental_association_lost = true;
        assert!(!backend.should_save_incrementally());
        assert!(!backend.info().unwrap().can_save_incrementally);
    }

    #[test]
    fn externally_replaced_pdf_is_not_overwritten_by_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("external-change.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            document.save(path_text).unwrap();
        }
        let mut backend = MuPdfBackend::open(path.clone()).unwrap();
        backend
            .create_highlight(
                0,
                &[PageQuad {
                    upper_left: PagePoint::new(20.0, 20.0),
                    upper_right: PagePoint::new(80.0, 20.0),
                    lower_left: PagePoint::new(20.0, 40.0),
                    lower_right: PagePoint::new(80.0, 40.0),
                }],
            )
            .unwrap();
        let external_bytes = b"%PDF-1.7\n% external replacement is intentionally distinct\n";
        fs::write(&path, external_bytes).unwrap();

        let error = backend.save().unwrap_err();

        assert!(error.to_string().contains("changed outside LunaPDF"));
        assert!(backend.info().unwrap().dirty);
        assert_eq!(fs::read(path).unwrap(), external_bytes);
    }

    #[test]
    fn version_check_before_replace_preserves_external_bytes_and_cleans_temp() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("external-before-replace.pdf");
        write_blank_pdf_for_test(&path);
        let backend = MuPdfBackend::open(path.clone()).unwrap();
        let expected_version = backend.version;
        let named_temp = TempFileBuilder::new()
            .prefix(".lunapdf-")
            .suffix(".pdf")
            .tempfile_in(directory.path())
            .unwrap();
        let temp_path = named_temp.into_temp_path();
        fs::write(&temp_path, b"candidate bytes").unwrap();
        let external_bytes = b"external replacement before atomic replace";
        fs::write(&path, external_bytes).unwrap();

        let mut callback_called = false;
        let error = persist_temp_if_current(temp_path, &path, expected_version, || {
            callback_called = true;
        })
        .unwrap_err();
        assert!(error.to_string().contains("changed outside LunaPDF"));
        assert!(!callback_called);
        assert_eq!(fs::read(&path).unwrap(), external_bytes);
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lunapdf-")
        }));
    }

    #[test]
    fn persist_callback_runs_only_after_current_version_check_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("callback.pdf");
        write_blank_pdf_for_test(&path);
        #[cfg(windows)]
        let original_attributes = {
            let path_wide = wide_path(&path).unwrap();
            let attributes = FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_HIDDEN;
            assert_ne!(
                unsafe { SetFileAttributesW(path_wide.as_ptr(), attributes) },
                0
            );
            unsafe { GetFileAttributesW(path_wide.as_ptr()) }
        };
        let expected_version = read_document_version(&path).unwrap();
        let named_temp = TempFileBuilder::new()
            .prefix(".lunapdf-")
            .suffix(".pdf")
            .tempfile_in(directory.path())
            .unwrap();
        let temp_path = named_temp.into_temp_path();
        write_blank_pdf_for_test(&temp_path);
        let mut callback_called = false;
        persist_temp_if_current(temp_path, &path, expected_version, || {
            callback_called = true;
        })
        .unwrap();
        assert!(callback_called);
        assert_eq!(
            PdfDocument::open(path.to_str().unwrap())
                .unwrap()
                .page_count()
                .unwrap(),
            1
        );
        #[cfg(windows)]
        assert_eq!(
            unsafe { GetFileAttributesW(wide_path(&path).unwrap().as_ptr()) },
            original_attributes
        );
    }

    #[test]
    fn pre_replace_verification_failure_keeps_original_and_cleans_temp() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("verification-failure.pdf");
        write_blank_pdf_for_test(&path);
        let original_bytes = fs::read(&path).unwrap();
        let named_temp = TempFileBuilder::new()
            .prefix(".lunapdf-")
            .suffix(".pdf")
            .tempfile_in(directory.path())
            .unwrap();
        let temp_path = named_temp.into_temp_path();
        write_blank_pdf_for_test(&temp_path);
        let temporary_document = PdfDocument::open(temp_path.to_str().unwrap()).unwrap();

        let error = verify_saved_document(&temporary_document, 1, 1, &[]).unwrap_err();
        assert!(error.to_string().contains("Highlight count changed"));
        drop(temporary_document);
        temp_path.close().unwrap();
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lunapdf-")
        }));
    }

    #[test]
    fn invalid_page_or_scale_is_rejected_at_adapter_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bounds.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            document.save(path_text).unwrap();
        }
        let mut backend = MuPdfBackend::open(path).unwrap();

        assert!(backend.render_tile(tile_request(1, 1.0, 0, 0)).is_err());
        assert!(backend.render_tile(tile_request(0, 0.0, 0, 0)).is_err());
        assert!(
            backend
                .render_tile(tile_request(0, 1.0, 0, 99))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn confirmed_selection_quads_match_the_preview_glyph_range() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("selection-range.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(400.0, 200.0)).unwrap();
            let mut shape = Shape::new(&mut page).unwrap();
            shape
                .insert_text(Point::new(40.0, 100.0), "ABCDE", &TextOptions::default())
                .unwrap()
                .commit(&mut document, true)
                .unwrap();
            document.save(path_text).unwrap();
        }

        let backend = MuPdfBackend::open(path).unwrap();
        let snapshot = backend
            .text_snapshot(TextSnapshotRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();
        let first = snapshot.glyphs.first().unwrap().quad.bounds();
        let last = snapshot.glyphs.last().unwrap().quad.bounds();
        let start = PagePoint::new((first.0 + first.2) / 2.0, (first.1 + first.3) / 2.0);
        let end = PagePoint::new((last.0 + last.2) / 2.0, (last.1 + last.3) / 2.0);
        let expected_quads = snapshot
            .glyphs
            .iter()
            .map(|glyph| glyph.quad)
            .collect::<Vec<_>>();

        let selection = backend.select(0, 1, start, end).unwrap();

        assert_eq!(selection.text, "ABCDE");
        assert_eq!(selection.quads, expected_quads);
        assert_eq!(selection.display_quads.len(), 1);
        assert_eq!(
            selection.display_quads[0].upper_left,
            snapshot.glyphs[0].quad.upper_left
        );
        assert_eq!(
            selection.display_quads[0].lower_right,
            snapshot.glyphs[4].quad.lower_right
        );
    }

    #[test]
    fn nonzero_pdf_boxes_render_internal_and_right_bottom_edge_tiles() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nonzero-boxes.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(1_400.0, 1_300.0)).unwrap();
            let mut media_box = document.new_array_with_capacity(4).unwrap();
            for coordinate in [100.0, 200.0, 1_300.0, 1_100.0] {
                media_box
                    .array_push(PdfObject::new_real(coordinate).unwrap())
                    .unwrap();
            }
            page.object().dict_put("MediaBox", media_box).unwrap();
            page.set_crop_box(Rect::new(100.0, 200.0, 1_300.0, 1_100.0))
                .unwrap();
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let bounds = backend.info().unwrap().page_bounds[0];
        assert_eq!((bounds.width(), bounds.height()), (1_200.0, 700.0));
        let specs = [
            TileSpec {
                pixel_x: 512,
                pixel_y: 0,
                pixel_width: 512,
                pixel_height: 512,
            },
            TileSpec {
                pixel_x: 1_024,
                pixel_y: 0,
                pixel_width: 176,
                pixel_height: 512,
            },
            TileSpec {
                pixel_x: 0,
                pixel_y: 512,
                pixel_width: 512,
                pixel_height: 188,
            },
        ];

        for spec in specs {
            let tile = backend
                .render_tile(tile_request_with_spec(0, 1.0, 1, 0, spec))
                .unwrap()
                .unwrap();
            assert_eq!(tile.spec, spec);
            assert_eq!(tile.pixels_rgba.len(), spec.rgba_bytes().unwrap());
        }
    }

    #[test]
    fn landscape_fractional_dpi_keeps_request_and_rendered_tile_edges_identical() {
        use crate::render::tiles::TileGrid;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("landscape-fractional-dpi.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(1_600.0, 1_100.0)).unwrap();
            let mut media_box = document.new_array_with_capacity(4).unwrap();
            for coordinate in [100.25, 200.5, 893.951, 795.776] {
                media_box
                    .array_push(PdfObject::new_real(coordinate).unwrap())
                    .unwrap();
            }
            page.object().dict_put("MediaBox", media_box).unwrap();
            page.set_crop_box(Rect::new(100.25, 200.5, 893.951, 795.776))
                .unwrap();
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let bounds = backend.info().unwrap().page_bounds[0];
        for scale in [0.942_420_66, 0.9375, 1.125, 1.5] {
            let grid = TileGrid::new(bounds, scale).unwrap();
            let specs = grid
                .specs_in_pixel_rect(0, 0, grid.pixel_width(), grid.pixel_height())
                .unwrap();

            for spec in specs {
                let tile = backend
                    .render_tile(tile_request_with_spec(0, scale, 1, 0, spec))
                    .unwrap()
                    .unwrap();
                assert_eq!(tile.spec, spec);
                assert_eq!(tile.page_pixel_width, grid.pixel_width());
                assert_eq!(tile.page_pixel_height, grid.pixel_height());
            }
        }
    }

    #[test]
    fn outline_search_and_thumbnail_return_owned_phase_three_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("phase-three.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut first_page = document.new_page(Size::new(400.0, 600.0)).unwrap();
            let mut shape = Shape::new(&mut first_page).unwrap();
            shape
                .insert_text(
                    Point::new(50.0, 100.0),
                    "Needle in the first page and another Needle",
                    &TextOptions::default(),
                )
                .unwrap()
                .commit(&mut document, true)
                .unwrap();
            let _second_page = document.new_page(Size::new(600.0, 400.0)).unwrap();
            document
                .set_outlines(&[Outline {
                    title: "Second page".to_owned(),
                    uri: None,
                    dest: Some(LinkDestination {
                        loc: Location {
                            chapter: 0,
                            page_in_chapter: 1,
                            page_number: 1,
                        },
                        kind: DestinationKind::Fit,
                    }),
                    down: vec![Outline {
                        title: "External entry".to_owned(),
                        uri: Some("https://example.invalid".to_owned()),
                        dest: None,
                        down: Vec::new(),
                    }],
                }])
                .unwrap();
            document.save(path_text).unwrap();
        }

        let mut backend = MuPdfBackend::open(path).unwrap();
        let outline = backend.load_outline().unwrap();
        let text_snapshot = backend
            .text_snapshot(TextSnapshotRequest {
                page_index: 0,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();
        let search = backend.search_page(0, "needle", 7).unwrap();
        let thumbnail = backend
            .render_thumbnail(ThumbnailRequest {
                page_index: 1,
                max_pixel_width: 160,
                max_pixel_height: 220,
                generation: 9,
                expected_revision: 0,
            })
            .unwrap()
            .unwrap();

        assert_eq!(outline[0].page_index, Some(1));
        assert_eq!(outline[0].children[0].page_index, None);
        let extracted = text_snapshot
            .glyphs
            .iter()
            .map(|glyph| glyph.character)
            .collect::<String>();
        assert!(extracted.contains("Needle in the first page and another Needle"));
        let first = text_snapshot.glyphs.first().unwrap().quad.bounds();
        let last = text_snapshot.glyphs.last().unwrap().quad.bounds();
        let selection = backend
            .select(
                0,
                11,
                PagePoint::new((first.0 + first.2) / 2.0, (first.1 + first.3) / 2.0),
                PagePoint::new((last.0 + last.2) / 2.0, (last.1 + last.3) / 2.0),
            )
            .unwrap();
        assert!(
            selection
                .text
                .contains("Needle in the first page and another Needle")
        );
        assert!(!selection.quads.is_empty());
        assert_eq!(search.generation, 7);
        assert_eq!(search.matches.len(), 2);
        assert!(search.matches.iter().all(|hit| !hit.quads.is_empty()));
        assert_eq!(thumbnail.generation, 9);
        assert!(thumbnail.pixel_width <= 160);
        assert!(thumbnail.pixel_height <= 220);
        assert_eq!(
            thumbnail.pixels_rgba.len(),
            thumbnail.pixel_width as usize * thumbnail.pixel_height as usize * 4
        );
    }
}
