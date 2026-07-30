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
    AnnotationFlags, AnnotationQuadPoints, Encryption, PdfAnnotation, PdfAnnotationType,
    PdfDocument, PdfWriteOptions, Permission, WidgetType,
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
    AnnotationPageSnapshot, AnnotationSnapshot, AnnotationSummary, AnnotationUpdateRequest,
    HighlightIndexBatch, HighlightIndexPage, HighlightIndexRequest, PdfAnnotationColor,
};
use crate::domain::document::{
    DocumentInfo, DocumentVersion, EditAction, HighlightCapability, OutlineItem, PageRect,
    RenderedThumbnail, RenderedTile, SearchMatch, SearchPageResult, TILE_EDGE_PIXELS,
    ThumbnailRequest, TileRequest, TileSpec,
};
use crate::domain::selection::{
    GlyphSnapshot, NonTextTarget, NonTextTargetKind, PagePoint, PageQuad, SelectionSnapshot,
    TextPageSnapshot, TextSnapshotRequest, selected_display_quads, selected_quads, selected_text,
};

// PDF の数値オブジェクトは増分更新のシリアライズ時に座標を丸めることがある。
// PDF ポイントの 100 分の 1 は対応するズーム範囲の表示ピクセル精度を下回りつつ、
// 実質的に異なる Quad は検出できる。
const PDF_COORDINATE_TOLERANCE: f32 = 0.01;

// PDF ライターは正規化した色と不透明度の数値を丸めることがある。1000 分の 1 は
// 8 ビットチャネルの 1 段階を下回りつつ、目に見える置換は検出できる。
const PDF_PROPERTY_TOLERANCE: f32 = 0.001;

// 16,384 個の Quad で 1 ページのスナップショットを約 512 KiB に制限しながら、
// 高密度な文書を網羅する。このバイト指向の上限内では論理的なヒット境界を保持する。
const SEARCH_QUAD_CAPACITY: usize = 16_384;

pub(super) struct MuPdfBackend {
    path: PathBuf,
    document: PdfDocument,
    page_bounds: Vec<PageRect>,
    version: DocumentVersion,
    #[cfg(debug_assertions)]
    open_time: Duration,
    revision: u64,
    highlight_capability: HighlightCapability,
    pending_edits: Vec<PendingEdit>,
    display_list: Option<CachedDisplayList>,
    // バイト列から開いた復旧用ドキュメントは元のパスと安全に関連付けられないため、
    // 検証に成功して新しいパス関連ドキュメントを設定するまでは、再試行にも全書き換えの
    // 経路を使う。
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
    /// PDF を開き、所有ワーカー上で軽量なページ形状を読み取る。
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let version_before_open = read_document_version(&path)?;
        let mupdf_path = path
            .to_str()
            .context("MuPDF requires a Unicode path on Windows")?;
        // 起動時間はデバッグ表示専用なので、リリースでは計測自体を省く。
        #[cfg(debug_assertions)]
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
        #[cfg(debug_assertions)]
        let open_time = open_started.elapsed();

        Ok(Self {
            path,
            document,
            page_bounds,
            version,
            #[cfg(debug_assertions)]
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
            #[cfg(any(debug_assertions, test))]
            highlight_count: highlight_count(&self.document)?,
            #[cfg(any(debug_assertions, test))]
            can_save_incrementally: self.should_save_incrementally(),
            highlight_capability: self.highlight_capability,
            // MuPDF は Undo 操作後も xref の `dirty` ビットを保持する。アプリケーションの
            // `dirty` 状態は有効な編集ログに従わせ、LIFO で全て Undo したときに古いビットを
            // 信頼せずクリーンなタブへ戻せるようにする。
            dirty: !self.pending_edits.is_empty(),
            revision: self.revision,
            #[cfg(debug_assertions)]
            open_time: self.open_time,
            #[cfg(debug_assertions)]
            physical_memory_bytes: physical_memory_bytes(),
            version: self.version,
        })
    }

    /// 検証済みの内部ページ対象だけを含む Rust 所有の階層を返す。
    pub(super) fn load_outline(&self) -> Result<Vec<OutlineItem>> {
        let outlines = self.document.outlines()?;
        Ok(outlines
            .into_iter()
            .map(|outline| outline_item(outline, self.page_bounds.len()))
            .collect())
    }

    /// ページ単位で検索し、ページ処理の間にフォアグラウンド描画を実行できるようにする。
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
                // メモリ境界で複数行のヒットを分割しない。部分的なヒットにすると、ナビゲーションと
                // 描画結果が一致しなくなる。
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

    /// 同じ注釈付きタイル経路を使って、上限付きのページ全体画像を描画する。
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
        // 有限でない、または正でない行列は MuPDF で意味のない寸法を生むため、
        // 無効なズーム状態はアダプター境界で拒否する。
        ensure!(
            request.scale.is_finite() && request.scale > 0.0,
            "render scale must be finite and positive"
        );
        ensure!(
            request.spec.pixel_width > 0 && request.spec.pixel_height > 0,
            "tile dimensions must be positive"
        );
        page_number(request.page_index, self.page_bounds.len())?;
        // 変更がキュー済みの先読み処理を追い越すことがある。古いタイルは通常のキャンセルであり、
        // ユーザーに表示すべきドキュメントエラーではない。
        if request.expected_revision != self.revision {
            return Ok(None);
        }

        let bounds = self.page_bounds[request.page_index];
        // レンダー診断値はリリースのタイル転送契約に含めないため計測しない。
        #[cfg(debug_assertions)]
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
        // MuPDF は新しく割り当てた Pixmap を初期化しない。塗りつぶされた内容の外側で
        // 想定される PDF ページ背景は不透明な白である。
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
            #[cfg(debug_assertions)]
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
            #[cfg(debug_assertions)]
            render_time: render_started.elapsed(),
            #[cfg(debug_assertions)]
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

    /// 現在表示中の 1 ページについて Rust 所有のテキストスナップショットを抽出する。
    pub(super) fn text_snapshot(
        &self,
        request: TextSnapshotRequest,
    ) -> Result<Option<TextPageSnapshot>> {
        // 注釈の変更がキュー済みの抽出処理を追い越すことがある。UI はリビジョンで
        // スナップショットを識別するため、古い処理は割り当て前に破棄する。
        if request.expected_revision != self.revision {
            return Ok(None);
        }
        let (_text_page, glyphs, non_text_targets, _extraction_time) =
            load_text_snapshot(&self.document, request.page_index, self.page_bounds.len())?;
        Ok(Some(TextPageSnapshot {
            page_index: request.page_index,
            revision: self.revision,
            glyphs,
            non_text_targets,
        }))
    }

    /// リビジョンに束縛された 1 ページ分の編集可能な注釈メタデータを読み取る。
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
            let Some(summary) =
                annotation_summary(request.page_index, &annotation, document_allows_edits)?
            else {
                continue;
            };
            annotations.push(AnnotationSnapshot {
                id: summary.id,
                kind: summary.kind,
                quads: annotation
                    .quad_points()?
                    .iter()
                    .map(page_quad_from_mupdf)
                    .collect(),
                contents: summary.contents,
                color: summary.color,
                opacity: annotation.opacity()?,
                can_edit_contents: summary.can_edit_contents,
                can_edit_color: summary.can_edit_color,
                can_delete: summary.can_delete,
            });
        }
        Ok(Some(AnnotationPageSnapshot {
            page_index: request.page_index,
            revision: self.revision,
            annotations,
        }))
    }

    /// 永続的な Highlight リスト用に、上限付きでリビジョンに束縛されたバッチを読み取る。
    ///
    /// サイドバーの行は PDF 注釈の Quad を描画しないため、形状は意図的に省略する。
    /// バッチをページ単位に制限してキャンセル遅延を抑える。
    pub(super) fn highlight_index_batch(
        &self,
        request: HighlightIndexRequest,
    ) -> Result<Option<HighlightIndexBatch>> {
        if request.expected_revision != self.revision {
            return Ok(None);
        }
        ensure!(
            request.page_count > 0,
            "Highlight index batch must contain at least one page"
        );
        let last_page = request
            .first_page
            .checked_add(request.page_count)
            .context("Highlight index batch page range overflowed")?;
        ensure!(
            last_page <= self.page_bounds.len(),
            "Highlight index batch exceeds the document page count"
        );

        let document_allows_edits = self.highlight_capability.is_allowed();
        let mut pages = Vec::with_capacity(request.page_count);
        for page_index in request.first_page..last_page {
            let started_at = Instant::now();
            let page = self
                .document
                .load_pdf_page(page_number(page_index, self.page_bounds.len())?)?;
            let mut highlights = Vec::new();
            for annotation in page.annotations() {
                if let Some(summary) =
                    annotation_summary(page_index, &annotation, document_allows_edits)?
                {
                    highlights.push(summary);
                }
            }
            pages.push(HighlightIndexPage {
                page_index,
                highlights,
                scan_time: started_at.elapsed(),
            });
        }

        Ok(Some(HighlightIndexBatch {
            generation: request.generation,
            revision: self.revision,
            total_pages: self.page_bounds.len(),
            pages,
        }))
    }

    pub(super) fn select(
        &self,
        page_index: usize,
        generation: u64,
        start: PagePoint,
        end: PagePoint,
    ) -> Result<SelectionSnapshot> {
        let (_text_page, glyphs, _non_text_targets, extraction_time) =
            load_text_snapshot(&self.document, page_index, self.page_bounds.len())?;
        // ドラッグプレビューは既にポインター位置をスナップショットの字形へ解決している。
        // ここで MuPDF の点選択を再実行すると端点の字形が除外されるため、確定表示、コピー、
        // Highlight は全て同じ範囲から導出する。
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

    /// メモリ上に 1 つの Highlight を作成し、座標や順序を調べずにこの操作を Undo するために
    /// 必要な MuPDF の正確な識別子を返す。
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
        // PDF の xref は正の間接オブジェクト番号であり、0 ではアプリケーション所有の注釈を
        // Undo 用に安定して識別できない。
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
        let expected = expected_annotation_state_from_annotation(annotation_id, &annotation)?;
        self.pending_edits.push(PendingEdit {
            action: action.clone(),
            undo: UndoEdit::DeleteCreatedAnnotation(annotation_id),
            expected: ExpectedAnnotationMutation::Present(expected),
        });
        self.display_list = None;
        self.revision += 1;
        Ok(action)
    }

    /// 正確なリビジョンに束縛された 1 つの注釈 xref の対応フィールドを更新する。
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

    /// 正確なリビジョンに束縛された 1 つの注釈を削除し、正確な Undo スナップショットを保持する。
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
            return expected_annotation_state_from_annotation(id, &annotation);
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
            // Undo 用バイト列では現在のセキュリティ設定を保持する必要がある。ライターの
            // デフォルトオプションではスナップショットから暗号化が除去される。
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
        // メモリを基盤とする復元済みドキュメントでは MuPDF のパス関連増分ライターを安全に
        // 使用できないため、次の保存では検証済みの全書き換えを使う必要がある。
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

    /// 保存されていないアプリケーション編集のうち、最新のものだけを Undo する。
    ///
    /// Create は正確な xref で反転する。Update と delete は変更前の PDF スナップショットを
    /// 復元し、不完全なアプリケーションモデルから外部の外観ストリームやメタデータを再構築
    /// しない。
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

    /// メモリ上の PDF を保存し、まず MuPDF の安全な増分経路を選択する。
    ///
    /// 全書き換えが必要なドキュメントは同じディレクトリの一時 PDF に書き込み、そこで検証する。
    /// 元のバージョンを再確認した後にだけアトミック置換することで、書き込み失敗時にユーザーの
    /// 唯一のコピーを切り詰めないようにする。
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
            // ライターのデフォルトで保護されたドキュメントが黙って変更されないよう、
            // 元の暗号化設定を保持する。
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
            // 一時パスには既に元ファイルの権限が設定されている。ライターのデフォルトでは
            // PDF の暗号化が除去されるため、これも保持する。
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

        // MuPDF のパス基盤ドキュメントは通常の FILE* ハンドルを保持することがある。
        // Windows では Rust がラッパーを破棄した後もそのハンドルが置換を拒否する可能性があるため、
        // 元のハンドルを解放する前に検証済みバイト列からハンドルを持たない復旧用ドキュメントを
        // 作成する。この一時的な全ファイルコピーはまれな経路に限定され、アトミック置換自体が
        // 失敗した場合もユーザーのメモリ上の編集を保持する。
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

/// このワーカーが開いたバージョンと元ファイルが一致している間だけ置換する。
///
/// 比較によって `rename` をファイルシステムの比較交換にはできないが、`persist` の
/// 直前に置くことで避けられない競合を最小化し、置換前の全ての失敗でクリーンアップを一元化する。
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
    // ReplaceFileW は元ファイルの DACL、属性、暗号化、圧縮、名前付きストリームをマージする。
    // Rust の読み取り専用ビットを事前コピーするだけでは不完全であり、まず tempfile::keep で
    // 置換ファイルを正規化する必要がある。
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
    // IGNORE_* フラグを渡さないのは意図的である。マージ失敗時は ACL や属性を緩めて PDF を
    // 黙って置換せず、中断しなければならない。
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
        // ReplaceFileW はマージまたは移動のエラーを報告しながら置換ファイルを消費することがある。
        // 一時ファイルが見つからないというクリーンアップ結果で最初のエラーを隠してはならない。
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
        // 保存されていない複数の操作が 1 つの注釈を対象にできる。最新の期待状態だけが最終 PDF を
        // 表し、以前のエントリは LIFO Undo のためだけに残るため、同時状態として検証してはならない。
        if verified_ids.insert(id) {
            verify_annotation_mutation(document, &pending.expected)?;
        }
    }
    Ok((verified_highlights, page_bounds))
}

fn sync_file(path: &Path) -> Result<()> {
    // Windows の FlushFileBuffers はここでバイトを変更しなくても書き込み可能なハンドルを要求する。
    // 読み取り専用ハンドルでは全ての全書き換えが失敗する。
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
    // ハンドルベースのメタデータは Windows のボリューム ID とファイル ID を提供する。
    // パスのみのメタデータではこれらが欠落し、同じサイズのパス置換を検出できない場合がある。
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

/// MuPDF の `open`・`read` 処理と競合するパス置換またはインプレース書き込みを拒否する。
fn stable_open_version(before: DocumentVersion, after: DocumentVersion) -> Result<DocumentVersion> {
    ensure!(
        before == after,
        "the PDF changed while LunaPDF was opening it"
    );
    Ok(after)
}

/// ページローカルのタイル要求を MuPDF のデバイス座標へ変換する。
///
/// 最終的な交差判定では Rust のレイアウト計算と MuPDF の矩形丸めによる 1 ピクセルの端差を
/// 許容するが、ページと全く重ならないタイルは拒否する。
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
    // 安全性: `file` は有効なオープンハンドルを所有し、`information` はこの同期 Windows API
    // 呼び出しの期間中、書き込み可能なままである。
    let succeeded = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    ensure!(
        succeeded != 0,
        "failed to query the Windows PDF file identity: {}",
        std::io::Error::last_os_error()
    );

    // Windows は永続的な 64 ビットファイルインデックスを 2 つの DWORD に分割する。
    // 同じサイズのパス置換も検出できるよう、完全な値を保持する。
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
    // これらのチェックは、ユーザーが最も直接対処できる制限の順に並べている。
    // 保存戦略が全ファイル一時コピーを検証してアトミック置換するため、増分非対応のドキュメントも
    // 編集可能なままである。
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
) -> Result<(TextPage, Vec<GlyphSnapshot>, Vec<NonTextTarget>, Duration)> {
    let page = document.load_pdf_page(page_number(page_index, page_count)?)?;
    let extraction_started = Instant::now();
    // 空のフラグで MuPDF 標準の抽出ベースラインを記録する。Typst 固有のバウンディングボックス
    // 調整には、まず文書化された比較が必要である。
    let text_page = page.to_text_page(TextPageFlags::empty())?;
    let structured = text_page.structured();
    let mut glyphs = Vec::new();
    let mut non_text_targets = Vec::new();
    let mut line_index = 0;

    for block in structured.blocks {
        match block.content {
            TextBlockContent::Text { lines } => {
                for line in lines {
                    glyphs.extend(line.chars.into_iter().map(|character| GlyphSnapshot {
                        character: character.ch,
                        quad: page_quad_from_mupdf(&character.quad),
                        line_index,
                    }));
                    line_index += 1;
                }
            }
            TextBlockContent::Image { .. } => {
                non_text_targets.push(NonTextTarget {
                    kind: NonTextTargetKind::Image,
                    quad: page_quad_from_mupdf(&Quad::from(block.bounds)),
                });
            }
            TextBlockContent::Other => {}
        }
    }

    for link in page.resolved_links()? {
        non_text_targets.push(NonTextTarget {
            kind: NonTextTargetKind::Link,
            quad: page_quad_from_mupdf(&Quad::from(link?.bounds)),
        });
    }
    for widget in page.widgets() {
        non_text_targets.push(NonTextTarget {
            kind: NonTextTargetKind::Form,
            quad: page_quad_from_mupdf(&Quad::from(widget.annotation().bounds()?)),
        });
    }

    Ok((
        text_page,
        glyphs,
        non_text_targets,
        extraction_started.elapsed(),
    ))
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
                    annotation_contents(&annotation)? == state.contents,
                    "saved annotation xref {} changed Contents",
                    id.xref
                );
                let color = annotation_color(&annotation)?;
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

#[cfg(any(debug_assertions, test))]
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

fn annotation_contents(annotation: &PdfAnnotation) -> Result<&str> {
    // PDF では Contents を省略できるが、アプリケーションの注釈境界は所有 String を使う。
    // 欠損と空文字をここで一度だけ正規化し、一覧・編集・保存後検証の解釈を一致させる。
    Ok(annotation.contents()?.unwrap_or_default())
}

fn annotation_color(annotation: &PdfAnnotation) -> Result<Option<PdfAnnotationColor>> {
    Ok(annotation.color()?.map(annotation_color_from_mupdf))
}

fn expected_annotation_state_from_annotation(
    id: AnnotationId,
    annotation: &PdfAnnotation,
) -> Result<ExpectedAnnotationState> {
    Ok(ExpectedAnnotationState {
        id,
        quads: annotation
            .quad_points()?
            .iter()
            .map(page_quad_from_mupdf)
            .collect(),
        contents: annotation_contents(annotation)?.to_owned(),
        color: annotation_color(annotation)?,
        opacity: annotation.opacity()?,
    })
}

fn annotation_summary(
    page_index: usize,
    annotation: &PdfAnnotation,
    document_allows_edits: bool,
) -> Result<Option<AnnotationSummary>> {
    if annotation.r#type()? != PdfAnnotationType::Highlight {
        return Ok(None);
    }
    let xref = annotation.xref()?;
    ensure!(
        xref > 0,
        "MuPDF returned an invalid xref for an existing Highlight"
    );
    let flags = annotation.flags()?;
    let properties_locked =
        flags.intersects(AnnotationFlags::IS_READ_ONLY | AnnotationFlags::IS_LOCKED);
    let contents_locked = properties_locked || flags.contains(AnnotationFlags::IS_LOCKED_CONTENTS);
    Ok(Some(AnnotationSummary {
        id: AnnotationId { page_index, xref },
        kind: AnnotationKind::Highlight,
        contents: annotation_contents(annotation)?.to_owned(),
        color: annotation_color(annotation)?,
        can_edit_contents: document_allows_edits && !contents_locked,
        can_edit_color: document_allows_edits && !properties_locked,
        can_delete: document_allows_edits && !properties_locked,
    }))
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

    #[test]
    fn annotation_colors_match_gray_and_cmyk_within_component_tolerance() {
        let gray = PdfAnnotationColor::Gray(0.4);
        let gray_nearby = PdfAnnotationColor::Gray(0.4 + PDF_PROPERTY_TOLERANCE * 0.5);
        let gray_far = PdfAnnotationColor::Gray(0.4 + PDF_PROPERTY_TOLERANCE * 2.0);
        assert!(annotation_colors_match(Some(gray), Some(gray_nearby)));
        assert!(!annotation_colors_match(Some(gray), Some(gray_far)));
        assert!(!annotation_colors_match(
            Some(gray),
            Some(PdfAnnotationColor::Rgb {
                red: 0.4,
                green: 0.4,
                blue: 0.4,
            })
        ));

        let cmyk = PdfAnnotationColor::Cmyk {
            cyan: 0.1,
            magenta: 0.2,
            yellow: 0.3,
            key: 0.4,
        };
        let cmyk_nearby = PdfAnnotationColor::Cmyk {
            cyan: 0.1 + PDF_PROPERTY_TOLERANCE * 0.5,
            magenta: 0.2 - PDF_PROPERTY_TOLERANCE * 0.5,
            yellow: 0.3 + PDF_PROPERTY_TOLERANCE * 0.5,
            key: 0.4 + PDF_PROPERTY_TOLERANCE * 0.5,
        };
        let distant_cmyk_colors = [
            (
                "cyan",
                PdfAnnotationColor::Cmyk {
                    cyan: 0.1 + PDF_PROPERTY_TOLERANCE * 2.0,
                    magenta: 0.2,
                    yellow: 0.3,
                    key: 0.4,
                },
            ),
            (
                "magenta",
                PdfAnnotationColor::Cmyk {
                    cyan: 0.1,
                    magenta: 0.2 + PDF_PROPERTY_TOLERANCE * 2.0,
                    yellow: 0.3,
                    key: 0.4,
                },
            ),
            (
                "yellow",
                PdfAnnotationColor::Cmyk {
                    cyan: 0.1,
                    magenta: 0.2,
                    yellow: 0.3 + PDF_PROPERTY_TOLERANCE * 2.0,
                    key: 0.4,
                },
            ),
            (
                "key",
                PdfAnnotationColor::Cmyk {
                    cyan: 0.1,
                    magenta: 0.2,
                    yellow: 0.3,
                    key: 0.4 + PDF_PROPERTY_TOLERANCE * 2.0,
                },
            ),
        ];
        assert!(annotation_colors_match(Some(cmyk), Some(cmyk_nearby)));
        for (component, distant) in distant_cmyk_colors {
            assert!(
                !annotation_colors_match(Some(cmyk), Some(distant)),
                "{component} outside the tolerance must not match"
            );
        }
        assert!(!annotation_colors_match(Some(cmyk), Some(gray)));
    }

    #[test]
    fn property_matches_accepts_nearby_values_and_rejects_distant_values() {
        let expected = 0.5;
        assert!(property_matches(
            expected + PDF_PROPERTY_TOLERANCE * 0.5,
            expected
        ));
        assert!(!property_matches(
            expected + PDF_PROPERTY_TOLERANCE * 2.0,
            expected
        ));
    }

    #[test]
    fn quad_matches_respects_coordinate_tolerance_for_each_corner() {
        let expected = PageQuad {
            upper_left: PagePoint::new(10.0, 20.0),
            upper_right: PagePoint::new(30.0, 20.0),
            lower_left: PagePoint::new(10.0, 40.0),
            lower_right: PagePoint::new(30.0, 40.0),
        };

        let mut nearby = mupdf_quad_from_page(&expected);
        nearby.ul.x += PDF_COORDINATE_TOLERANCE * 0.5;
        assert!(quad_matches(&nearby, &expected));

        type DistantCoordinateChange = (&'static str, fn(&mut Quad));
        let distant_coordinate_changes: [DistantCoordinateChange; 8] = [
            ("ul.x", |quad| {
                quad.ul.x += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("ul.y", |quad| {
                quad.ul.y += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("ur.x", |quad| {
                quad.ur.x += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("ur.y", |quad| {
                quad.ur.y += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("ll.x", |quad| {
                quad.ll.x += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("ll.y", |quad| {
                quad.ll.y += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("lr.x", |quad| {
                quad.lr.x += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
            ("lr.y", |quad| {
                quad.lr.y += PDF_COORDINATE_TOLERANCE * 2.0;
            }),
        ];
        for (coordinate, change) in distant_coordinate_changes {
            let mut distant = mupdf_quad_from_page(&expected);
            change(&mut distant);
            assert!(
                !quad_matches(&distant, &expected),
                "{coordinate} outside the tolerance must not match"
            );
        }
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

        // 読み取り専用属性が設定されている間は Windows が削除を拒否することがある。
        // 元のモードまたは属性を復元することで、上記の権限チェックを弱めずに一時ファイルの
        // クリーンアップを移植可能に保つ。
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
    fn missing_highlight_contents_is_normalized_to_empty_string() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing-highlight-contents.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut annotation = page
                .add_highlight_annotation(Quad::from(Rect::new(30.0, 40.0, 130.0, 60.0)))
                .unwrap();
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
        assert_eq!(snapshot.annotations[0].contents, "");

        let batch = backend
            .highlight_index_batch(HighlightIndexRequest {
                generation: 1,
                expected_revision: 0,
                first_page: 0,
                page_count: 1,
            })
            .unwrap()
            .unwrap();
        assert_eq!(batch.pages.len(), 1);
        assert_eq!(batch.pages[0].highlights.len(), 1);
        assert_eq!(batch.pages[0].highlights[0].contents, "");
    }

    #[test]
    fn highlight_index_batch_keeps_page_and_pdf_annotation_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("highlight-index.pdf");
        let path_text = path.to_str().unwrap();
        {
            let mut document = PdfDocument::new();
            let mut first_page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut first = first_page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 30.0, 120.0, 50.0)))
                .unwrap();
            first.set_contents("first").unwrap();
            first.update().unwrap();
            drop(first);
            let mut ignored = first_page
                .add_underline_annotation(Quad::from(Rect::new(20.0, 60.0, 120.0, 80.0)))
                .unwrap();
            ignored.set_contents("not a Highlight").unwrap();
            ignored.update().unwrap();
            drop(ignored);
            let mut second = first_page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 90.0, 120.0, 110.0)))
                .unwrap();
            second.set_contents("second").unwrap();
            second.update().unwrap();
            first_page.update().unwrap();
            drop(second);
            drop(first_page);

            let _empty_page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut third_page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            let mut third = third_page
                .add_highlight_annotation(Quad::from(Rect::new(20.0, 30.0, 120.0, 50.0)))
                .unwrap();
            third.set_contents("third").unwrap();
            third.update().unwrap();
            third_page.update().unwrap();
            drop(third);
            drop(third_page);
            document.save(path_text).unwrap();
        }

        let backend = MuPdfBackend::open(path).unwrap();
        let batch = backend
            .highlight_index_batch(HighlightIndexRequest {
                generation: 9,
                expected_revision: 0,
                first_page: 0,
                page_count: 3,
            })
            .unwrap()
            .unwrap();

        assert_eq!(batch.generation, 9);
        assert_eq!(batch.total_pages, 3);
        assert_eq!(
            batch
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            batch.pages[0]
                .highlights
                .iter()
                .map(|summary| summary.contents.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(batch.pages[1].highlights.is_empty());
        assert_eq!(batch.pages[2].highlights[0].contents, "third");

        let partial = backend
            .highlight_index_batch(HighlightIndexRequest {
                generation: 10,
                expected_revision: 0,
                first_page: 1,
                page_count: 1,
            })
            .unwrap()
            .unwrap();
        assert_eq!(partial.pages.len(), 1);
        assert_eq!(partial.pages[0].page_index, 1);
        assert!(
            backend
                .highlight_index_batch(HighlightIndexRequest {
                    generation: 10,
                    expected_revision: 1,
                    first_page: 0,
                    page_count: 1,
                })
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[ignore = "generates the explicit Highlight index performance matrix"]
    fn measure_highlight_index_performance_matrix() {
        let directory = tempfile::tempdir().unwrap();
        for page_count in [100_usize, 500, 1_000] {
            for highlights_per_page in [0_usize, 1, 10] {
                let path = directory
                    .path()
                    .join(format!("index-{page_count}-{highlights_per_page}.pdf"));
                write_highlight_index_fixture(&path, page_count, highlights_per_page);
                for batch_size in [8_usize, 16, 32] {
                    measure_highlight_index_case(
                        &path,
                        page_count,
                        highlights_per_page,
                        batch_size,
                    );
                }
            }
        }
    }

    fn write_highlight_index_fixture(path: &Path, page_count: usize, highlights_per_page: usize) {
        let mut document = PdfDocument::new();
        for page_index in 0..page_count {
            let mut page = document.new_page(Size::new(300.0, 400.0)).unwrap();
            for highlight_index in 0..highlights_per_page {
                let y = 20.0 + highlight_index as f32 * 20.0;
                let mut annotation = page
                    .add_highlight_annotation(Quad::from(Rect::new(20.0, y, 120.0, y + 12.0)))
                    .unwrap();
                annotation
                    .set_contents(&format!("{page_index}-{highlight_index}"))
                    .unwrap();
                annotation.update().unwrap();
            }
            page.update().unwrap();
        }
        document.save(path.to_str().unwrap()).unwrap();
    }

    fn measure_highlight_index_case(
        path: &Path,
        page_count: usize,
        highlights_per_page: usize,
        batch_size: usize,
    ) {
        let backend = MuPdfBackend::open(path.to_path_buf()).unwrap();
        let memory_before = physical_memory_bytes();
        let started_at = Instant::now();
        let mut first_item_time = None;
        let mut page_times = Vec::with_capacity(page_count);
        let mut batch_times = Vec::new();
        let mut batch_wait_times = Vec::new();
        let mut previous_batch_finished = None;
        let mut indexed_highlights = 0;
        let mut retained_pages = Vec::with_capacity(page_count);

        for first_page in (0..page_count).step_by(batch_size) {
            if let Some(finished_at) = previous_batch_finished {
                batch_wait_times.push(Instant::now().duration_since(finished_at));
            }
            let pages_in_batch = batch_size.min(page_count - first_page);
            let batch_started_at = Instant::now();
            let batch = backend
                .highlight_index_batch(HighlightIndexRequest {
                    generation: 1,
                    expected_revision: 0,
                    first_page,
                    page_count: pages_in_batch,
                })
                .unwrap()
                .unwrap();
            batch_times.push(batch_started_at.elapsed());
            for page in batch.pages {
                indexed_highlights += page.highlights.len();
                if first_item_time.is_none() && !page.highlights.is_empty() {
                    first_item_time = Some(started_at.elapsed());
                }
                page_times.push(page.scan_time);
                retained_pages.push(page.highlights);
            }
            previous_batch_finished = Some(Instant::now());
        }

        let total_time = started_at.elapsed();
        let memory_after = physical_memory_bytes();
        let (mean_page, median_page, p95_page) = duration_statistics(&page_times);
        let mean_batch_wait = duration_mean(&batch_wait_times);
        let first_batch = batch_times.first().copied().unwrap_or_default();
        let longest_batch = batch_times.iter().copied().max().unwrap_or_default();
        assert_eq!(indexed_highlights, page_count * highlights_per_page);
        assert_eq!(retained_pages.len(), page_count);
        eprintln!(
            concat!(
                "HIGHLIGHT_INDEX_METRIC pages={} highlights_per_page={} batch={} ",
                "first_batch_ms={:.3} first_item_ms={} total_ms={:.3} page_mean_us={:.3} ",
                "page_median_us={:.3} page_p95_us={:.3} batch_wait_mean_us={:.3} ",
                "memory_before={} memory_after={} memory_delta={} longest_batch_ms={:.3}"
            ),
            page_count,
            highlights_per_page,
            batch_size,
            first_batch.as_secs_f64() * 1_000.0,
            first_item_time
                .map(|duration| format!("{:.3}", duration.as_secs_f64() * 1_000.0))
                .unwrap_or_else(|| "none".to_owned()),
            total_time.as_secs_f64() * 1_000.0,
            mean_page.as_secs_f64() * 1_000_000.0,
            median_page.as_secs_f64() * 1_000_000.0,
            p95_page.as_secs_f64() * 1_000_000.0,
            mean_batch_wait.as_secs_f64() * 1_000_000.0,
            memory_before
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "unavailable".to_owned()),
            memory_after
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "unavailable".to_owned()),
            memory_delta(memory_before, memory_after)
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "unavailable".to_owned()),
            longest_batch.as_secs_f64() * 1_000.0,
        );
    }

    fn duration_statistics(durations: &[Duration]) -> (Duration, Duration, Duration) {
        let mut sorted = durations.to_vec();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        let p95_index = (sorted.len() * 95).div_ceil(100).saturating_sub(1);
        (duration_mean(&sorted), median, sorted[p95_index])
    }

    fn duration_mean(durations: &[Duration]) -> Duration {
        if durations.is_empty() {
            return Duration::ZERO;
        }
        let total_seconds = durations.iter().map(Duration::as_secs_f64).sum::<f64>();
        Duration::from_secs_f64(total_seconds / durations.len() as f64)
    }

    fn memory_delta(before: Option<usize>, after: Option<usize>) -> Option<i128> {
        let before = i128::try_from(before?).ok()?;
        let after = i128::try_from(after?).ok()?;
        Some(after - before)
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
        // 絶対パスの宛先を要求し、明示的な受入れ実行でシェル依存の相対パスに追跡不能な
        // 成果物を残さないようにする。
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
            // テキストをページ端から離すことで、ビューアーのページ余白表示に依存せず注釈を
            // 識別しやすくする。
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
        let first_action = backend.create_highlight(0, &selection.quads).unwrap();
        let EditAction::CreateHighlight {
            annotation_xref: first_xref,
            ..
        } = first_action
        else {
            panic!("create_highlight must return a create action");
        };
        let second_action = backend
            .create_highlight(0, std::slice::from_ref(&selection.quads[0]))
            .unwrap();
        let EditAction::CreateHighlight {
            annotation_xref: second_xref,
            ..
        } = second_action
        else {
            panic!("create_highlight must return a create action");
        };
        backend
            .delete_annotation(AnnotationDeleteRequest {
                id: AnnotationId {
                    page_index: 0,
                    xref: second_xref,
                },
                expected_revision: 2,
            })
            .unwrap();
        backend
            .update_annotation(AnnotationUpdateRequest {
                id: AnnotationId {
                    page_index: 0,
                    xref: first_xref,
                },
                expected_revision: 3,
                contents: Some("LunaPDF 日本語コメント\nexternal viewer check".to_owned()),
                color: Some(PdfAnnotationColor::Rgb {
                    red: 0.2,
                    green: 0.45,
                    blue: 0.95,
                }),
            })
            .unwrap();
        assert_eq!(backend.save().unwrap(), 1);
        drop(backend);

        let output_directory = output.parent().expect("absolute paths have a parent");
        fs::create_dir_all(output_directory).unwrap();
        // create_new は上書きしないことを保証する。別プロセスが検証と最終コピーの間に
        // 存在チェックの結果を置き換えることはできない。
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
        // 本番コマンドが 1 回のバックエンド呼び出しの外でページを保持することはない。
        // このテスト専用ハンドルを ReplaceFileW の前に破棄し、Windows でバックエンドの
        // 実際の所有権境界をテストする。
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

        // MuPDF はバイトを基盤とするドキュメントを増分書き込み可能と報告することがある。
        // そのためバックエンドは、復旧再試行でこの値を信頼せず、パスとの関連付けを明示的に追跡する。
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
    fn confirmed_selection_quads_match_preview_geometry() {
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
        let selection = backend.select(0, 1, start, end).unwrap();

        assert_eq!(selection.text, "ABCDE");
        assert_eq!(selection.display_quads.len(), 1);
        assert_eq!(
            selection.display_quads[0].upper_left,
            snapshot.glyphs[0].quad.upper_left
        );
        assert_eq!(
            selection.display_quads[0].lower_right,
            snapshot.glyphs[4].quad.lower_right
        );
        assert_eq!(selection.quads, selection.display_quads);
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
