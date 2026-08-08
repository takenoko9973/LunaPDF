use super::*;

impl PrototypeApp {
    pub(super) fn receive_document_events(&mut self, context: &egui::Context) {
        let mut failed_restored_paths = Vec::new();
        let mut saved_as_paths = Vec::new();
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
                        self.documents[index].restoring_from_session = false;
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
                        self.documents[index].external_candidate = None;
                        self.documents[index].external_conflict_reported = false;
                        self.documents[index].reload_in_flight = false;
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
                            self.finish_session_restore(true);
                        }
                    }
                    Ok(DocumentEvent::DocumentChanged {
                        info,
                        external_reload,
                    }) => {
                        if self.is_visible_index(index) {
                            self.documents[index].view.stop_autoscroll();
                            self.cancel_viewport_for_index(index);
                        }
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
                            // Save は PDF を再オープンして xref/revision の有効区間を
                            // 新しく始めるため、変更のない古いエディターも残してはならない。
                            self.annotation_editor = None;
                        }
                        let restart_search = self.active_index() == Some(index)
                            && self.close_confirmation.is_none()
                            && !self.window_close_pending
                            && !self.close_all_pending
                            && !self.documents[index].search.query.trim().is_empty();
                        let tab = &mut self.documents[index];
                        if external_reload {
                            tab.external_candidate = None;
                            tab.external_conflict_reported = false;
                            tab.reload_in_flight = false;
                            tab.failed_external_version = None;
                            tab.outline = None;
                            tab.outline_requested = false;
                            tab.clear_selection();
                            tab.search.generation = tab.search.generation.wrapping_add(1);
                            tab.search.pages.clear();
                            tab.search.selected = None;
                            tab.search.completed_pages = 0;
                            tab.search.truncated = false;
                            tab.search.in_progress = false;
                            tab.view.clamp_to_page_count(page_count);
                        }
                        tab.pending_rebind_path = None;
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
                                // アプリケーション編集を伴わない revision 変更では、
                                // xref を保持できるページの同一性が存在しない。
                                tab.reset_highlight_index(revision, page_count);
                            }
                        } else {
                            // Save は MuPDF を再オープンし、表示上の注釈がすべて同じでも
                            // 新しい xref を割り当てることがある。
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
                        if external_reload
                            && !self.documents[index].outline_requested
                            && self.documents[index].send(DocumentCommand::LoadOutline)
                        {
                            self.documents[index].outline_requested = true;
                        }
                    }
                    Ok(DocumentEvent::PathRebound { path, info }) => {
                        self.documents[index].info = Some(info);
                        self.documents[index].external_candidate = None;
                        self.documents[index].pending_rebind_path = None;
                        self.status = format!("PDF の名前変更を追跡しました: {}", path.display());
                    }
                    Ok(DocumentEvent::SavedAs(path)) => {
                        if !matches!(self.documents[index].save_as, SaveAsState::Saving(ref expected) if expected == &path)
                        {
                            continue;
                        }
                        self.documents[index].save_as = SaveAsState::Idle;
                        let original = self.tabs.tabs()[index].path().to_path_buf();
                        // 先に別名版を新規タブとして残す。外部版の reload が失敗しても、
                        // 成功した編集版の到達可能性を失ってはならない。
                        saved_as_paths.push(path.clone());
                        self.documents[index].reload_in_flight =
                            self.documents[index].send(DocumentCommand::Reload(original));
                        if self.documents[index].reload_in_flight {
                            self.status =
                                "編集版を保存しました。外部版を再読み込みしています…".to_owned();
                        }
                    }
                    Ok(DocumentEvent::TileRendered(mut tile)) => {
                        let is_visible = self.is_visible_index(index);
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
                            is_visible,
                            key,
                            tile.generation,
                            tab.view.generation,
                            current_revision,
                            &tab.wanted_tiles,
                        );
                        if !result_is_current {
                            continue;
                        }
                        // テクスチャ転送はバイト数とページ相対の寸法を信頼するため、
                        // 先にワーカーのスナップショットを検証する。
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
                        if let SaveAsState::WaitingEditorCommit(path) = &tab.save_as {
                            let path = path.clone();
                            if tab.send(DocumentCommand::SaveAs(path.clone())) {
                                tab.save_as = SaveAsState::Saving(path);
                                self.status =
                                    "未確定の注釈を反映し、編集版を別名保存しています…".to_owned();
                            } else {
                                tab.save_as = SaveAsState::Idle;
                            }
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
                            // ワーカー応答はこのタブの履歴先頭の操作に対応しなければならない。
                            // 並べ替えを黙って行うと、後続の取り消しが誤った注釈の同一性を対象にする。
                            tab.error = Some(
                                "編集を元に戻せませんでした。タブを開き直してください。詳細: 編集履歴の応答順序が一致しません。"
                                    .to_owned(),
                            );
                        }
                    }
                    Ok(DocumentEvent::TextSnapshotReady(snapshot)) => {
                        let key = TextSnapshotKey::from_snapshot(&snapshot);
                        let is_visible = self.is_visible_index(index);
                        let tab = &mut self.documents[index];
                        tab.pending_text_snapshots.remove(&key);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
                        let is_current = text_snapshot_result_is_current(
                            is_visible,
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
                        let is_visible = self.is_visible_index(index);
                        let tab = &mut self.documents[index];
                        tab.pending_text_snapshots.remove(&key);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
                        let is_current = text_snapshot_result_is_current(
                            is_visible,
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
                        let is_visible = self.is_visible_index(index);
                        let tab = &mut self.documents[index];
                        tab.pending_annotation_pages.remove(&request);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let is_current = annotation_page_result_is_current(
                            is_visible,
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
                        let is_visible = self.is_visible_index(index);
                        let tab = &mut self.documents[index];
                        tab.pending_annotation_pages.remove(&request);
                        let current_revision = tab.info.as_ref().map(|info| info.revision);
                        let is_current = annotation_page_result_is_current(
                            is_visible,
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
                        if self.is_visible_index(index) {
                            self.documents[index].view.stop_autoscroll();
                            self.cancel_viewport_for_index(index);
                        }
                        if operation == "open" && self.documents[index].restoring_from_session {
                            // イベントキューを走査中はタブ配列をずらせない。現在の全インデックスを
                            // 調べ終えてから、復元に失敗したタブを削除する。
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
                            // MuPDF はすでに注釈を作成しており、後続スナップショットだけが
                            // 失敗したため、`dirty` のままにする。
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
                            // ページエラーは通常、キューに入った全ページで繰り返される。
                            // generation を進めて残りの処理を止め、同じ失敗で UI を埋めない。
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
                        } else if operation == "reload" {
                            self.documents[index].reload_in_flight = false;
                            self.documents[index].failed_external_version = self.documents[index]
                                .external_candidate
                                .map(|(version, _)| version);
                        } else if operation == "save-as" {
                            self.documents[index].save_as = SaveAsState::Idle;
                        } else if operation == "rename" {
                            if let Some(previous) = self.documents[index].pending_rebind_path.take()
                            {
                                let _ = self.tabs.replace_path(index, previous);
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
                        if self.is_visible_index(index) {
                            self.documents[index].view.stop_autoscroll();
                            self.cancel_viewport_for_index(index);
                        }
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
                            // 停止したワーカーはキュー済みの保存を完了できないため、
                            // 無限に待たずダイアログを閉じられるようにする。
                            confirmation.save_in_flight = false;
                        }
                        break;
                    }
                }
            }
        }

        // イベントキューの走査中にタブを削除すると配列がずれ、次の文書を飛ばす
        // おそれがある。すべてのキューの後で遅延クローズを実行する。
        if let Some(path) = self.saved_tab_to_close.take() {
            self.close_tab_by_path(&path);
        }
        for path in saved_as_paths {
            self.open_document(path);
        }
        for path in failed_restored_paths {
            if let Some(index) = self.tabs.tabs().iter().position(|tab| tab.path() == path) {
                self.remove_tab_now(index);
            } else {
                // ワーカーの失敗報告後に並行クローズがタブを削除した可能性があるため、
                // その完了を正確に一度だけ計上する。
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
}
