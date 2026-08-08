use super::*;
use crate::domain::annotation::{AnnotationId, AnnotationKind, AnnotationSummary};
use crate::domain::selection::PageQuad;
use mupdf::Size;
use mupdf::pdf::PdfDocument;

fn write_blank_pdf(path: &Path) {
    let path_text = path.to_str().unwrap();
    let mut document = PdfDocument::new();
    let _page = document.new_page(Size::new(300.0, 400.0)).unwrap();
    document.save(path_text).unwrap();
}

fn replace_with_blank_pdf(path: &Path, replacement: &Path) {
    write_blank_pdf(replacement);
    std::fs::remove_file(path).unwrap();
    std::fs::rename(replacement, path).unwrap();
}

fn overwrite_with_blank_pdf(path: &Path, replacement: &Path) {
    write_blank_pdf(replacement);
    let mut bytes = std::fs::read(replacement).unwrap();
    bytes.extend_from_slice(b"\n% external replacement\n");
    std::fs::write(path, bytes).unwrap();
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

fn single_pane_layout(tab_count: usize, selected_tab: usize) -> SessionLayout {
    SessionLayout {
        entries: (0..tab_count)
            .map(|tab_index| SessionEntry::Single { tab_index })
            .collect(),
        active_tab: Some(selected_tab),
    }
}

fn runtime_tab_ids(count: usize) -> (tempfile::TempDir, Vec<TabId>) {
    let directory = tempfile::tempdir().unwrap();
    let mut tabs = TabState::new();
    for index in 0..count {
        let path = directory.path().join(format!("runtime-{index}.pdf"));
        std::fs::File::create(path.clone()).unwrap();
        tabs.open(path).unwrap();
    }
    let ids = tabs.tabs().iter().map(|tab| tab.id()).collect();
    (directory, ids)
}

#[test]
fn rename_tracking_finds_the_original_identity_when_old_path_is_recreated() {
    let directory = tempfile::tempdir().unwrap();
    let old_path = directory.path().join("source.pdf");
    let renamed_path = directory.path().join("renamed.pdf");
    write_blank_pdf(&old_path);
    let expected = read_document_version(&old_path).unwrap();
    std::fs::rename(&old_path, &renamed_path).unwrap();
    write_blank_pdf(&old_path);

    assert_ne!(read_document_version(&old_path).unwrap(), expected);
    assert_eq!(
        find_same_folder_rename(&old_path, expected),
        Some(renamed_path)
    );
}

#[test]
fn failed_external_version_is_not_retried_until_a_new_stable_version_arrives() {
    let failed = DocumentVersion {
        identity_primary: 1,
        identity_secondary: 2,
        length: 3,
        modified: std::time::SystemTime::UNIX_EPOCH,
    };
    let next = DocumentVersion {
        length: 4,
        ..failed
    };
    assert!(!external_reload_is_ready(
        Some((failed, 2)),
        Some(failed),
        false
    ));
    assert!(!external_reload_is_ready(
        Some((next, 1)),
        Some(failed),
        false
    ));
    assert!(!external_reload_is_ready(
        Some((next, 2)),
        Some(failed),
        true
    ));
    assert!(external_reload_is_ready(
        Some((next, 2)),
        Some(failed),
        false
    ));
}

#[test]
fn direct_overwrite_keeps_identity_but_requires_two_stable_observations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("direct-overwrite.pdf");
    write_blank_pdf(&path);
    let expected = read_document_version(&path).unwrap();
    std::fs::write(&path, b"%PDF-1.7\n% overwritten\n").unwrap();
    let current = read_document_version(&path).unwrap();

    assert!(same_file_identity(current, expected));
    assert_ne!(current, expected);
    assert!(!external_reload_is_ready(Some((current, 1)), None, false));
    assert!(external_reload_is_ready(Some((current, 2)), None, false));
}

#[test]
fn atomic_replace_changes_identity_and_is_the_only_case_that_needs_rename_scan() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("atomic-source.pdf");
    let replacement = directory.path().join("atomic-replacement.pdf");
    write_blank_pdf(&path);
    let expected = read_document_version(&path).unwrap();
    write_blank_pdf(&replacement);
    std::fs::rename(&replacement, &path).unwrap();
    let current = read_document_version(&path).unwrap();

    assert!(!same_file_identity(current, expected));
    assert_eq!(find_same_folder_rename(&path, expected), None);
}

fn finish_async_document_open(app: &mut PrototypeApp) {
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.documents[0].state == DocumentState::Opening && std::time::Instant::now() < deadline {
        app.receive_document_events(&context);
        if app.documents[0].state == DocumentState::Opening {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_ne!(app.documents[0].state, DocumentState::Opening);
    assert!(app.documents[0].info.is_some());
}

fn finish_async_resume_failure(app: &mut PrototypeApp) {
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.documents[0].state == DocumentState::Opening && std::time::Instant::now() < deadline {
        app.receive_document_events(&context);
        if app.documents[0].state == DocumentState::Opening {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(app.documents[0].state, DocumentState::Suspended);
    assert!(app.documents[0].service.is_none());
}

fn finish_async_document_opens(app: &mut PrototypeApp) {
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app
        .documents
        .iter()
        .any(|document| document.state == DocumentState::Opening)
        && std::time::Instant::now() < deadline
    {
        app.receive_document_events(&context);
        if app
            .documents
            .iter()
            .any(|document| document.state == DocumentState::Opening)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(
        app.documents
            .iter()
            .all(|document| document.state != DocumentState::Opening)
    );
    assert!(app.documents.iter().all(|document| document.info.is_some()));
}

fn create_dirty_external_conflict(app: &mut PrototypeApp, path: &Path) -> DocumentVersion {
    let quad = PageQuad {
        upper_left: PagePoint::new(20.0, 20.0),
        upper_right: PagePoint::new(80.0, 20.0),
        lower_left: PagePoint::new(20.0, 40.0),
        lower_right: PagePoint::new(80.0, 40.0),
    };
    let selection_generation = app.documents[0].selection_generation;
    app.documents[0].selection = Some(SelectionSnapshot {
        page_index: 0,
        text: "selected text".to_owned(),
        display_quads: vec![quad],
        quads: vec![quad],
        generation: selection_generation,
        #[cfg(debug_assertions)]
        extraction_time: Duration::ZERO,
    });
    app.create_highlight();

    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (app.documents[0].pending_edits > 0 || app.documents[0].edit_history.is_empty())
        && std::time::Instant::now() < deadline
    {
        app.receive_document_events(&context);
        if app.documents[0].pending_edits > 0 || app.documents[0].edit_history.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(app.documents[0].pending_edits, 0);
    assert!(!app.documents[0].edit_history.is_empty());
    assert_eq!(app.documents[0].state, DocumentState::ReadyDirty);

    let expected = app.documents[0].info.as_ref().unwrap().version;
    let replacement = path.with_file_name("external-conflict-replacement.pdf");
    overwrite_with_blank_pdf(path, &replacement);
    let current = read_document_version(path).unwrap();
    assert!(same_file_identity(current, expected));
    assert_ne!(current, expected);

    for _ in 0..2 {
        app.last_external_check = Instant::now() - Duration::from_secs(1);
        app.check_external_changes();
    }
    assert_eq!(
        app.documents[0].external_conflict,
        Some(ExternalConflict { version: current })
    );
    current
}

fn receive_rename_scan_result(
    app: &mut PrototypeApp,
    path: &Path,
    expected: DocumentVersion,
    observed_current: Option<DocumentVersion>,
    candidate: Option<(PathBuf, DocumentVersion)>,
) -> HashSet<u64> {
    let document_id = app.documents[0].document_id;
    app.documents[0].rename_scan_in_flight = true;
    app.rename_scan_result_sender
        .send(RenameScanResult {
            document_id,
            path: path.to_path_buf(),
            expected,
            observed_current,
            candidate,
        })
        .unwrap();
    app.receive_rename_scan_results()
}

#[test]
fn rename_scan_miss_adopts_atomic_replacement_after_two_stable_results() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("atomic-source.pdf");
    let replacement = directory.path().join("atomic-replacement.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    replace_with_blank_pdf(&path, &replacement);
    let current = read_document_version(&path).unwrap();

    assert!(
        receive_rename_scan_result(&mut app, &path, expected, Some(current), None)
            .contains(&app.documents[0].document_id)
    );
    assert_eq!(app.documents[0].external_candidate, Some((current, 1)));
    assert!(!app.documents[0].reload_in_flight);

    receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    assert_eq!(app.documents[0].external_candidate, Some((current, 2)));
    assert!(app.documents[0].reload_in_flight);
}

#[test]
fn rename_scan_waits_through_delete_and_recreate_before_reload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("delete-recreate.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    let _expected_handle = std::fs::File::open(&path).unwrap();

    std::fs::remove_file(&path).unwrap();
    receive_rename_scan_result(&mut app, &path, expected, None, None);
    assert!(app.documents[0].external_candidate.is_none());
    assert!(!app.documents[0].reload_in_flight);

    write_blank_pdf(&path);
    let recreated = read_document_version(&path).unwrap();
    receive_rename_scan_result(&mut app, &path, expected, Some(recreated), None);
    assert_eq!(app.documents[0].external_candidate, Some((recreated, 1)));
    receive_rename_scan_result(&mut app, &path, expected, Some(recreated), None);
    assert!(app.documents[0].reload_in_flight);
}

#[test]
fn stale_rename_scan_result_is_discarded_and_new_version_starts_over() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stale-scan.pdf");
    let first_replacement = directory.path().join("stale-first.pdf");
    let second_replacement = directory.path().join("stale-second.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    let _expected_handle = std::fs::File::open(&path).unwrap();
    replace_with_blank_pdf(&path, &first_replacement);
    let stale = read_document_version(&path).unwrap();
    replace_with_blank_pdf(&path, &second_replacement);
    let current = read_document_version(&path).unwrap();
    assert_ne!(stale, current);
    assert!(!same_file_identity(current, expected));

    let accepted = receive_rename_scan_result(&mut app, &path, expected, Some(stale), None);
    assert!(accepted.is_empty());
    assert!(app.documents[0].external_candidate.is_none());

    let accepted = receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    assert!(accepted.contains(&app.documents[0].document_id));
    assert_eq!(app.documents[0].external_candidate, Some((current, 1)));
}

#[test]
fn rename_candidate_wins_over_replacement_version_in_scan_result() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rename-source.pdf");
    let renamed = directory.path().join("rename-target.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    std::fs::rename(&path, &renamed).unwrap();
    write_blank_pdf(&path);
    let replacement = read_document_version(&path).unwrap();

    receive_rename_scan_result(
        &mut app,
        &path,
        expected,
        Some(replacement),
        Some((renamed.clone(), expected)),
    );
    assert_eq!(app.tabs.tabs()[0].path(), renamed.as_path());
    assert!(app.documents[0].external_candidate.is_none());
}

#[test]
fn rename_scan_result_routes_dirty_conflict_and_recovers_failed_visible_suspended_tab() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("conflict-resume.pdf");
    let first_replacement = directory.path().join("conflict-replacement.pdf");
    let second_replacement = directory.path().join("resume-replacement.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    let _expected_handle = std::fs::File::open(&path).unwrap();
    replace_with_blank_pdf(&path, &first_replacement);
    let current = read_document_version(&path).unwrap();

    app.documents[0].state = DocumentState::ReadyDirty;
    receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    assert_eq!(
        app.documents[0].external_conflict,
        Some(ExternalConflict { version: current })
    );

    app.documents[0].state = DocumentState::Suspended;
    app.documents[0].service = None;
    app.documents[0].external_candidate = None;
    app.documents[0].external_conflict = None;
    app.documents[0].resume_expected_version = Some(expected);
    app.documents[0].resume(path.clone(), expected);
    finish_async_resume_failure(&mut app);
    assert_eq!(app.documents[0].failed_external_version, Some(expected));

    replace_with_blank_pdf(&path, &second_replacement);
    let current = read_document_version(&path).unwrap();
    receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    receive_rename_scan_result(&mut app, &path, expected, Some(current), None);
    assert_eq!(app.documents[0].state, DocumentState::Opening);
    assert_eq!(app.documents[0].resume_expected_version, Some(current));
    assert!(app.documents[0].service.is_some());
}

#[test]
fn external_resume_opened_event_rebuilds_suspended_document_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("external-resume-state.pdf");
    let replacement = directory.path().join("external-resume-replacement.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let (expected, expected_revision, expected_page_count) = {
        let info = app.documents[0].info.as_ref().unwrap();
        (info.version, info.revision, info.page_bounds.len())
    };
    let _expected_handle = std::fs::File::open(&path).unwrap();
    replace_with_blank_pdf(&path, &replacement);
    let current = read_document_version(&path).unwrap();
    assert!(!same_file_identity(current, expected));

    let tab = &mut app.documents[0];
    tab.outline = Some(Vec::new());
    tab.outline_requested = true;
    tab.selection = Some(SelectionSnapshot {
        page_index: 0,
        text: "old selection".to_owned(),
        display_quads: Vec::new(),
        quads: Vec::new(),
        generation: tab.selection_generation,
        #[cfg(debug_assertions)]
        extraction_time: Duration::ZERO,
    });
    tab.annotation_pages.insert(
        AnnotationPageRequest {
            page_index: 0,
            expected_revision,
        },
        AnnotationPageSnapshot {
            page_index: 0,
            revision: expected_revision,
            annotations: Vec::new(),
        },
    );
    tab.search.query = "old query".to_owned();
    tab.search.pages.insert(0, Vec::new());
    tab.view.current_page = 4;
    tab.view.scroll_to_page = Some(4);
    tab.highlight_index.started = true;
    tab.highlight_index.revision = Some(expected_revision);
    tab.highlight_index.total_pages = expected_page_count;
    tab.highlight_index.pages.insert(
        0,
        vec![AnnotationSummary {
            id: AnnotationId {
                page_index: 0,
                xref: 99,
            },
            kind: AnnotationKind::Highlight,
            contents: "old highlight".to_owned(),
            color: None,
            can_edit_contents: true,
            can_edit_color: true,
            can_delete: true,
        }],
    );
    let old_search_generation = tab.search.generation;
    let old_view_generation = tab.view.generation;
    let old_thumbnail_generation = tab.thumbnail_generation;
    let old_highlight_generation = tab.highlight_index.generation;
    tab.suspend();
    tab.external_candidate = Some((current, 2));
    tab.resume_expected_version = Some(current);

    app.apply_external_poll_action(0, path.clone(), ExternalPollAction::Resume((current, 2)));
    assert_eq!(app.documents[0].state, DocumentState::Opening);
    finish_async_document_open(&mut app);

    let tab = &app.documents[0];
    assert_eq!(tab.state, DocumentState::ReadyClean);
    assert_eq!(tab.info.as_ref().unwrap().version, current);
    assert_eq!(tab.view.current_page, 0);
    assert_eq!(tab.view.scroll_to_page, Some(0));
    assert!(tab.outline.is_none());
    assert!(tab.selection.is_none());
    assert!(tab.annotation_pages.is_empty());
    assert!(tab.search.pages.is_empty());
    assert_ne!(tab.search.generation, old_search_generation);
    assert_ne!(tab.view.generation, old_view_generation);
    assert_ne!(tab.thumbnail_generation, old_thumbnail_generation);
    assert_ne!(tab.highlight_index.generation, old_highlight_generation);
    assert!(
        tab.highlight_index
            .pages
            .values()
            .flatten()
            .all(|annotation| annotation.contents != "old highlight")
    );
    assert!(!tab.external_resume_in_flight);
    assert!(tab.external_candidate.is_none());
}

#[test]
fn saved_as_completion_keeps_edit_copy_and_external_source_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("saved-as-source.pdf");
    let copy = directory.path().join("saved-as-copy.pdf");
    write_blank_pdf(&source);
    let mut app = PrototypeApp::from_startup(
        vec![source.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let external_version = create_dirty_external_conflict(&mut app, &source);
    app.begin_conflicted_document_save_as(0, Some(copy.clone()));
    assert!(matches!(
        app.documents[0].save_as,
        SaveAsState::Saving(SaveAsRequest { .. })
    ));

    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (app.documents.len() < 2
        || app.documents[0].reload_in_flight
        || app
            .documents
            .iter()
            .any(|document| document.state == DocumentState::Opening))
        && std::time::Instant::now() < deadline
    {
        app.receive_document_events(&context);
        if app.documents.len() < 2
            || app.documents[0].reload_in_flight
            || app
                .documents
                .iter()
                .any(|document| document.state == DocumentState::Opening)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.tabs.tabs()[0].path(), source.as_path());
    assert_eq!(app.tabs.tabs()[1].path(), copy.as_path());
    assert_eq!(app.documents[0].state, DocumentState::ReadyClean);
    assert_eq!(
        app.documents[0].info.as_ref().unwrap().version,
        external_version
    );
    assert_eq!(app.documents[0].info.as_ref().unwrap().highlight_count, 0);
    assert_eq!(app.documents[1].state, DocumentState::ReadyClean);
    assert_eq!(app.documents[1].info.as_ref().unwrap().highlight_count, 1);
    assert!(app.documents[0].external_conflict.is_none());
    assert!(matches!(app.documents[0].save_as, SaveAsState::Idle));
}

#[test]
fn cancelled_conflicted_save_as_leaves_state_untouched_without_a_command() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("cancel-save-as.pdf");
    write_blank_pdf(&source);
    let mut app = PrototypeApp::from_startup(
        vec![source.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let external_version = create_dirty_external_conflict(&mut app, &source);
    let document_id = app.documents[0].document_id;
    let annotation = AnnotationSummary {
        id: AnnotationId {
            page_index: 0,
            xref: 1,
        },
        kind: AnnotationKind::Highlight,
        contents: "original editor text".to_owned(),
        color: None,
        can_edit_contents: true,
        can_edit_color: true,
        can_delete: true,
    };
    let mut editor = AnnotationEditorState::from_summary(
        document_id,
        app.documents[0].info.as_ref().unwrap().revision,
        &annotation,
    );
    editor.buffer.contents = "editor draft".to_owned();
    app.annotation_editor = Some(editor);
    let conflict = app.documents[0].external_conflict;
    let edit_count = app.documents[0].edit_history.len();
    let source_info_version = app.documents[0].info.as_ref().unwrap().version;

    app.begin_conflicted_document_save_as(0, None);

    assert!(matches!(app.documents[0].save_as, SaveAsState::Idle));
    assert!(!app.documents[0].save_in_flight);
    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.documents[0].state, DocumentState::ReadyDirty);
    assert_eq!(app.documents[0].edit_history.len(), edit_count);
    assert_eq!(
        app.documents[0].info.as_ref().unwrap().version,
        source_info_version
    );
    assert_eq!(app.documents[0].external_conflict, conflict);
    assert_eq!(
        conflict,
        Some(ExternalConflict {
            version: external_version
        })
    );
    assert_eq!(
        app.annotation_editor
            .as_ref()
            .map(|editor| editor.buffer.contents.as_str()),
        Some("editor draft")
    );
    assert!(app.documents[0].error.is_none());
}

#[test]
fn reload_in_flight_close_does_not_mix_delayed_event_into_remaining_tab() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("reload-close-first.pdf");
    let second = directory.path().join("reload-close-second.pdf");
    write_blank_pdf(&first);
    write_blank_pdf(&second);
    let mut app = PrototypeApp::from_startup(
        vec![first.clone(), second.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_opens(&mut app);
    let remaining_id = app.documents[1].document_id;
    let remaining_version = app.documents[1].info.as_ref().unwrap().version;
    overwrite_with_blank_pdf(
        &first,
        &directory.path().join("reload-close-replacement.pdf"),
    );

    for _ in 0..2 {
        app.last_external_check = Instant::now() - Duration::from_secs(1);
        app.check_external_changes();
    }
    assert!(app.documents[0].reload_in_flight);
    app.close_tab(0);
    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.documents[0].document_id, remaining_id);
    assert_eq!(app.tabs.tabs()[0].path(), second.as_path());

    let context = egui::Context::default();
    for _ in 0..3 {
        app.receive_document_events(&context);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.documents[0].document_id, remaining_id);
    assert_eq!(
        app.documents[0].info.as_ref().unwrap().version,
        remaining_version
    );
    assert!(!app.documents[0].reload_in_flight);
}

#[test]
fn reload_in_flight_disables_annotation_editor_mutation_input() {
    let annotation = AnnotationSummary {
        id: AnnotationId {
            page_index: 0,
            xref: 1,
        },
        kind: AnnotationKind::Highlight,
        contents: "before".to_owned(),
        color: None,
        can_edit_contents: true,
        can_edit_color: true,
        can_delete: true,
    };
    let mut editor = AnnotationEditorState::from_summary(1, 1, &annotation);
    let context = egui::Context::default();
    let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let mut colors = Vec::new();
    let _ = context.run_ui(Default::default(), |ui| {
        show_annotation_editor(ui.ctx(), bounds, &mut editor, &mut colors, true);
    });
    context.memory_mut(|memory| {
        memory.request_focus(annotation_comment_id(1, annotation.id));
    });
    let _ = context.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Text("changed".to_owned())],
            ..Default::default()
        },
        |ui| {
            show_annotation_editor(ui.ctx(), bounds, &mut editor, &mut colors, false);
        },
    );
    assert_eq!(editor.buffer.contents, "before");
}

#[test]
fn reload_send_disables_annotation_editor_in_the_production_overlay_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reload-editor-production.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let document_id = app.documents[0].document_id;
    let revision = app.documents[0].info.as_ref().unwrap().revision;
    let annotation = AnnotationSummary {
        id: AnnotationId {
            page_index: 0,
            xref: 1,
        },
        kind: AnnotationKind::Highlight,
        contents: "before".to_owned(),
        color: None,
        can_edit_contents: true,
        can_edit_color: true,
        can_delete: true,
    };
    app.annotation_editor = Some(AnnotationEditorState::from_summary(
        document_id,
        revision,
        &annotation,
    ));
    overwrite_with_blank_pdf(
        &path,
        &directory.path().join("reload-editor-replacement.pdf"),
    );

    for _ in 0..2 {
        app.last_external_check = Instant::now() - Duration::from_secs(1);
        app.check_external_changes();
    }
    assert!(app.documents[0].reload_in_flight);

    let context = egui::Context::default();
    let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    context.memory_mut(|memory| {
        memory.request_focus(annotation_comment_id(document_id, annotation.id));
    });
    let _ = context.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Text("changed".to_owned())],
            ..Default::default()
        },
        |ui| app.annotation_editor_overlay(ui.ctx(), bounds),
    );

    assert_eq!(
        app.annotation_editor
            .as_ref()
            .map(|editor| editor.buffer.contents.as_str()),
        Some("before")
    );
    assert!(app.documents[0].reload_in_flight);
}

#[test]
fn custom_color_draft_blocks_reload_and_enters_external_conflict() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-color-conflict.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    finish_async_document_open(&mut app);
    let expected = app.documents[0].info.as_ref().unwrap().version;
    std::fs::write(&path, b"%PDF-1.7\n% external overwrite\n").unwrap();
    let current = read_document_version(&path).unwrap();
    assert!(same_file_identity(current, expected));
    assert_ne!(current, expected);

    let annotation = AnnotationSummary {
        id: AnnotationId {
            page_index: 0,
            xref: 1,
        },
        kind: AnnotationKind::Highlight,
        contents: String::new(),
        color: None,
        can_edit_contents: true,
        can_edit_color: true,
        can_delete: true,
    };
    let document_id = app.documents[0].document_id;
    let mut editor = AnnotationEditorState::from_summary(document_id, 0, &annotation);
    editor.custom_color_draft = Some([10, 20, 30]);
    assert!(editor.is_dirty());
    app.annotation_editor = Some(editor);

    app.last_external_check = Instant::now() - Duration::from_secs(1);
    app.check_external_changes();
    app.last_external_check = Instant::now() - Duration::from_secs(1);
    app.check_external_changes();

    assert!(!app.documents[0].reload_in_flight);
    assert_eq!(
        app.documents[0].external_conflict,
        Some(ExternalConflict { version: current })
    );
    assert_eq!(app.documents[0].external_candidate, Some((current, 2)));
    assert_eq!(
        app.annotation_editor
            .as_ref()
            .and_then(|editor| editor.custom_color_draft),
        Some([10, 20, 30])
    );
}

#[test]
fn consecutive_versions_reset_stability_to_the_latest_candidate() {
    let first = DocumentVersion {
        identity_primary: 1,
        identity_secondary: 2,
        length: 10,
        modified: std::time::SystemTime::UNIX_EPOCH,
    };
    let second = DocumentVersion {
        length: 11,
        ..first
    };

    assert_eq!(next_external_candidate(None, first), (first, 1));
    assert_eq!(next_external_candidate(Some((first, 1)), first), (first, 2));
    assert_eq!(
        next_external_candidate(Some((first, 2)), second),
        (second, 1)
    );
    assert!(external_reload_is_ready(Some((second, 2)), None, false));
}

#[test]
fn external_poll_reducer_separates_reload_conflict_and_rename_actions() {
    let expected = DocumentVersion {
        identity_primary: 7,
        identity_secondary: 8,
        length: 10,
        modified: std::time::SystemTime::UNIX_EPOCH,
    };
    let direct_overwrite = DocumentVersion {
        length: 11,
        ..expected
    };
    let input = |current,
                 previous_candidate,
                 has_unsaved_changes,
                 editor_dirty,
                 conflict_pending,
                 reload_in_flight| ExternalPollInput {
        expected,
        current,
        previous_candidate,
        failed_version: None,
        reload_in_flight,
        suspended: false,
        has_unsaved_changes,
        editor_dirty,
        conflict_pending,
    };

    assert_eq!(
        external_poll_action(input(
            Some(direct_overwrite),
            None,
            false,
            false,
            false,
            false,
        )),
        ExternalPollAction::Wait((direct_overwrite, 1))
    );
    assert_eq!(
        external_poll_action(input(
            Some(direct_overwrite),
            Some((direct_overwrite, 1)),
            false,
            false,
            false,
            false,
        )),
        ExternalPollAction::Reload((direct_overwrite, 2))
    );
    assert_eq!(
        external_poll_action(input(
            Some(direct_overwrite),
            Some((direct_overwrite, 1)),
            false,
            true,
            false,
            false,
        )),
        ExternalPollAction::Conflict((direct_overwrite, 2))
    );
    assert_eq!(
        external_poll_action(input(
            Some(direct_overwrite),
            Some((direct_overwrite, 1)),
            false,
            false,
            true,
            false,
        )),
        ExternalPollAction::Conflict((direct_overwrite, 2))
    );
    assert_eq!(
        external_poll_action(input(
            Some(direct_overwrite),
            Some((direct_overwrite, 1)),
            false,
            false,
            false,
            true,
        )),
        ExternalPollAction::Wait((direct_overwrite, 2))
    );

    let replacement = DocumentVersion {
        identity_primary: 9,
        ..direct_overwrite
    };
    assert_eq!(
        external_poll_action(input(Some(replacement), None, false, false, false, false,)),
        ExternalPollAction::ScanRename
    );
    assert_eq!(
        external_poll_action(input(None, None, false, false, false, false,)),
        ExternalPollAction::ScanRename
    );
}

#[test]
fn suspended_external_version_requires_stable_valid_resume_before_adoption() {
    let expected = DocumentVersion {
        identity_primary: 11,
        identity_secondary: 12,
        length: 20,
        modified: std::time::SystemTime::UNIX_EPOCH,
    };
    let current = DocumentVersion {
        length: 21,
        ..expected
    };
    let input = |previous_candidate, failed_version| ExternalPollInput {
        expected,
        current: Some(current),
        previous_candidate,
        failed_version,
        reload_in_flight: false,
        suspended: true,
        has_unsaved_changes: false,
        editor_dirty: false,
        conflict_pending: false,
    };

    assert_eq!(
        external_poll_action(input(None, None)),
        ExternalPollAction::Wait((current, 1))
    );
    assert_eq!(
        external_poll_action(input(Some((current, 1)), None)),
        ExternalPollAction::Resume((current, 2))
    );
    assert_eq!(
        external_poll_action(input(Some((current, 1)), Some(current))),
        ExternalPollAction::Wait((current, 2))
    );
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
fn external_open_request_uses_the_existing_tab_path_for_multiple_pdfs() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first paper.pdf");
    let second = directory.path().join("日本語.pdf");
    write_blank_pdf(&first);
    write_blank_pdf(&second);
    let mut app = PrototypeApp::from_startup(
        Vec::new(),
        SessionStore::new(directory.path().join("session.json")),
    );
    let (sender, receiver) = crossbeam_channel::unbounded();
    app.external_open_events = receiver;

    sender
        .send(Ok(vec![first.clone(), second.clone(), first.clone()]))
        .unwrap();
    let request_received = app.receive_external_open_events(&egui::Context::default());

    assert!(request_received);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(
        app.tabs.tabs()[app.tabs.selected_index().unwrap()].path(),
        std::fs::canonicalize(first).unwrap()
    );
}

#[test]
fn external_open_receive_error_is_reported_without_changing_tabs() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = PrototypeApp::from_startup(
        Vec::new(),
        SessionStore::new(directory.path().join("session.json")),
    );
    let (sender, receiver) = crossbeam_channel::unbounded();
    app.external_open_events = receiver;

    sender.send(Err("IPC error".to_owned())).unwrap();
    let request_received = app.receive_external_open_events(&egui::Context::default());

    assert!(!request_received);
    assert!(app.documents.is_empty());
    assert_eq!(app.error.as_deref(), Some("IPC error"));
}

#[test]
fn external_open_request_cancels_window_close_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("unsaved.pdf");
    let second = directory.path().join("追加.pdf");
    write_blank_pdf(&first);
    write_blank_pdf(&second);
    let mut app = PrototypeApp::from_startup(
        vec![first.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    let (sender, receiver) = crossbeam_channel::unbounded();
    app.external_open_events = receiver;
    app.window_close_pending = true;
    app.close_confirmation = Some(CloseConfirmation {
        scope: CloseScope::Window,
        path: first,
        save_in_flight: true,
    });

    sender.send(Ok(vec![second])).unwrap();
    let request_received = app.receive_external_open_events(&egui::Context::default());

    assert!(request_received);
    assert!(!app.window_close_pending);
    assert!(app.close_confirmation.is_none());
    assert_eq!(app.documents.len(), 2);
}

#[test]
fn external_open_request_cancels_a_sent_window_close_in_the_same_frame() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = PrototypeApp::from_startup(
        Vec::new(),
        SessionStore::new(directory.path().join("session.json")),
    );
    let (sender, receiver) = crossbeam_channel::unbounded();
    app.external_open_events = receiver;
    app.allow_window_close = true;
    sender.send(Ok(Vec::new())).unwrap();
    let context = egui::Context::default();
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);

    let output = context.run_ui(input, |ui| {
        let request_received = app.receive_external_open_events(ui.ctx());
        app.handle_window_close(ui.ctx(), request_received);
    });

    assert!(!app.allow_window_close);
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, ViewportCommand::CancelClose))
    );
}

#[test]
fn external_open_request_preserves_a_single_tab_close_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unsaved.pdf");
    write_blank_pdf(&path);
    let mut app = PrototypeApp::from_startup(
        vec![path.clone()],
        SessionStore::new(directory.path().join("session.json")),
    );
    let (sender, receiver) = crossbeam_channel::unbounded();
    app.external_open_events = receiver;
    app.window_close_pending = true;
    app.close_confirmation = Some(CloseConfirmation {
        scope: CloseScope::Tab,
        path: path.clone(),
        save_in_flight: false,
    });

    sender.send(Ok(Vec::new())).unwrap();
    app.receive_external_open_events(&egui::Context::default());

    assert!(!app.window_close_pending);
    assert!(matches!(
        app.close_confirmation.as_ref(),
        Some(confirmation)
            if confirmation.scope == CloseScope::Tab && confirmation.path == path
    ));
}

fn run_autoscroll_frame(
    context: &egui::Context,
    view: &mut ViewState,
    events: Vec<egui::Event>,
    focused: bool,
    primary_interaction_in_progress: bool,
) -> bool {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0))),
        events,
        focused,
        ..Default::default()
    };
    let mut active = false;
    let _output = context.run_ui(input, |ui| {
        let view_rect = ui.max_rect();
        let _response = ui.allocate_rect(view_rect, Sense::hover());
        active = update_autoscroll(
            ui.ctx(),
            view,
            view_rect,
            &[],
            ui.layer_id(),
            AutoscrollOffsets {
                current: Vec2::ZERO,
                maximum: Vec2::splat(1_000.0),
            },
            primary_interaction_in_progress,
        )
        .is_some();
    });
    active
}

#[test]
fn excluded_editor_rect_keeps_pdf_from_owning_the_cursor() {
    let context = egui::Context::default();
    let input = egui::RawInput {
        events: vec![egui::Event::PointerMoved(Pos2::new(50.0, 50.0))],
        ..Default::default()
    };
    let mut editor_owns_cursor = false;
    let mut distant_rect_does_not = false;
    let _output = context.run_ui(input, |ui| {
        editor_owns_cursor = pointer_over_any_rect(
            ui.ctx(),
            &[Rect::from_min_size(
                Pos2::new(40.0, 40.0),
                Vec2::splat(20.0),
            )],
        );
        distant_rect_does_not = !pointer_over_any_rect(
            ui.ctx(),
            &[Rect::from_min_size(
                Pos2::new(100.0, 100.0),
                Vec2::splat(20.0),
            )],
        );
    });

    assert!(editor_owns_cursor);
    assert!(distant_rect_does_not);
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
fn platform_copy_event_reaches_the_existing_pdf_copy_path_once() {
    let directory = tempfile::tempdir().unwrap();
    let pdf_path = directory.path().join("document.pdf");
    write_blank_pdf(&pdf_path);
    let session_path = directory.path().join("session.json");
    let mut app = PrototypeApp::from_startup(vec![pdf_path], SessionStore::new(session_path));
    app.documents[0].selection = Some(SelectionSnapshot {
        page_index: 0,
        generation: 1,
        text: "日本語\nPDF text".to_owned(),
        display_quads: Vec::new(),
        quads: Vec::new(),
        extraction_time: Duration::ZERO,
    });
    let context = egui::Context::default();

    let output = context.run_ui(copy_event_input(), |ui| app.handle_shortcuts(ui.ctx()));

    let copied_texts = output
        .platform_output
        .commands
        .iter()
        .filter_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.as_str()),
            egui::OutputCommand::CopyImage(_) | egui::OutputCommand::OpenUrl(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(copied_texts, vec!["日本語\nPDF text"]);
    assert_eq!(app.status, "Selected text copied");

    let repeated = context.run_ui(copy_event_input(), |ui| app.handle_shortcuts(ui.ctx()));
    assert!(
        repeated
            .platform_output
            .commands
            .iter()
            .all(|command| !matches!(command, egui::OutputCommand::CopyText(_)))
    );
}

#[test]
fn selected_text_inputs_keep_the_copy_event_ahead_of_pdf_selection() {
    let input_ids = [
        search_query_id(7),
        page_number_id(7),
        annotation_comment_id(
            7,
            AnnotationId {
                page_index: 0,
                xref: 11,
            },
        ),
    ];

    for input_id in input_ids {
        let context = egui::Context::default();
        let mut text = "search text".to_owned();
        let _initial = context.run_ui(egui::RawInput::default(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(input_id));
        });
        let mut state = egui::TextEdit::load_state(&context, input_id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(6),
            )));
        egui::TextEdit::store_state(&context, input_id, state);
        context.memory_mut(|memory| memory.request_focus(input_id));

        let mut copied_pdf = true;
        let mut copy_event_remained = false;
        let mut shortcut_active = false;
        let output = context.run_ui(copy_event_input(), |ui| {
            let text_input_owns_copy = text_edit_has_selection(ui.ctx(), input_id);
            copied_pdf =
                consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, text_input_owns_copy, true);
            copy_event_remained = ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Copy))
            });
            ui.add(egui::TextEdit::singleline(&mut text).id(input_id));
        });

        assert!(!copied_pdf);
        assert!(copy_event_remained);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == "search")
        }));
    }
}

#[test]
fn copy_event_without_any_selection_is_consumed_without_action() {
    let context = egui::Context::default();
    let mut shortcut_active = false;
    let mut copied_pdf = true;
    let mut copy_event_remained = true;

    let _output = context.run_ui(copy_event_input(), |ui| {
        copied_pdf = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, false);
        copy_event_remained = ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, Event::Copy))
        });
    });

    assert!(!copied_pdf);
    assert!(!copy_event_remained);
}

#[test]
fn copy_key_release_rearms_pdf_copy_after_repeat_is_ignored() {
    let context = egui::Context::default();
    let mut shortcut_active = false;
    let mut copied = false;
    let _first = context.run_ui(copy_event_input(), |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(copied);

    let _repeat = context.run_ui(copy_event_input(), |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(!copied);

    let release_input = egui::RawInput {
        events: vec![copy_key_release_event(Key::C)],
        ..Default::default()
    };
    let _release = context.run_ui(release_input, |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(!copied);
    assert!(!shortcut_active);

    let _second_press = context.run_ui(copy_event_input(), |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(copied);
}

#[test]
fn copy_release_in_the_same_frame_rearms_the_next_pdf_copy() {
    let context = egui::Context::default();
    let mut shortcut_active = false;
    let mut copied = false;
    let copy_then_release = egui::RawInput {
        events: vec![Event::Copy, copy_key_release_event(Key::C)],
        ..Default::default()
    };

    let _first = context.run_ui(copy_then_release, |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(copied);
    assert!(!shortcut_active);

    let _second = context.run_ui(copy_event_input(), |ui| {
        copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
    });
    assert!(copied);
}

#[test]
fn alternative_copy_key_releases_rearm_the_pdf_copy_latch() {
    for release_key in [Key::Insert, Key::Copy] {
        let context = egui::Context::default();
        let mut shortcut_active = false;
        let mut copied = false;
        let _first = context.run_ui(copy_event_input(), |ui| {
            copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
        });
        assert!(copied);

        let release_input = egui::RawInput {
            events: vec![copy_key_release_event(release_key)],
            ..Default::default()
        };
        let _release = context.run_ui(release_input, |ui| {
            copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
        });
        assert!(!copied);
        assert!(!shortcut_active);

        let _second = context.run_ui(copy_event_input(), |ui| {
            copied = consume_pdf_copy_event(ui.ctx(), &mut shortcut_active, false, true);
        });
        assert!(copied);
    }
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

#[test]
fn tab_activation_cancels_navigation_owned_by_previous_document() {
    let directory = tempfile::tempdir().unwrap();
    let first_pdf = directory.path().join("first.pdf");
    let second_pdf = directory.path().join("second.pdf");
    write_blank_pdf(&first_pdf);
    write_blank_pdf(&second_pdf);
    let session_path = directory.path().join("session.json");
    let mut app =
        PrototypeApp::from_startup(vec![first_pdf, second_pdf], SessionStore::new(session_path));
    app.select_tab(0);
    let side = SplitSide::First;
    app.documents[0].view.autoscroll = Some(AutoscrollState {
        anchor: Pos2::ZERO,
        requested_offset: Some(Vec2::ZERO),
    });
    let context = egui::Context::default();
    for events in [
        vec![
            egui::Event::PointerMoved(Pos2::new(50.0, 50.0)),
            egui::Event::PointerButton {
                pos: Pos2::new(50.0, 50.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        vec![egui::Event::PointerMoved(Pos2::new(70.0, 70.0))],
    ] {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0))),
            events,
            ..Default::default()
        };
        let _output = context.run_ui(input, |ui| {
            app.viewports.get_mut(&side).unwrap().interact_background(
                ui,
                ui.max_rect(),
                &[],
                &[],
                false,
            );
        });
    }
    assert!(app.viewports[&side].blank_pan_in_progress());

    app.select_tab(1);

    assert!(app.documents[0].view.autoscroll.is_none());
    assert!(!app.viewports[&side].primary_interaction_in_progress());
    assert!(!app.viewports[&side].blank_pan_in_progress());
}

#[test]
fn split_keeps_both_selected_documents_visible_while_focus_changes() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let session_path = directory.path().join("session.json");
    let mut app = PrototypeApp::from_startup(paths, SessionStore::new(session_path));
    let first_id = app.tabs.tabs()[0].id();
    let third_id = app.tabs.tabs()[2].id();

    assert!(app.tabs.create_split(
        first_id,
        third_id,
        crate::domain::tabs::SplitPlacement::Right
    ));
    app.sync_pane_viewports();

    assert_eq!(app.visible_indices(), vec![2, 0]);
    assert!(app.is_visible_index(0));
    assert!(!app.is_visible_index(1));
    assert!(app.is_visible_index(2));
    assert_eq!(app.active_index(), Some(0));
    assert_eq!(app.viewports.len(), 2);

    app.focus_side(SplitSide::First);

    assert_eq!(app.active_index(), Some(2));
    assert_eq!(app.visible_indices(), vec![2, 0]);
    assert_eq!(app.focused_side(), SplitSide::First);
}

#[test]
fn ctrl_tab_treats_a_split_set_as_one_display_entry() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("ctrl-tab-{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let mut app = PrototypeApp::from_startup(
        paths,
        SessionStore::new(directory.path().join("session.json")),
    );
    let ids = app
        .tabs
        .tabs()
        .iter()
        .map(|tab| tab.id())
        .collect::<Vec<_>>();
    assert!(app.tabs.reorder_single(ids[2], 0));
    assert_eq!(app.active_index(), Some(2));

    app.select_next_tab();

    assert_eq!(app.active_index(), Some(0));
}

#[test]
fn split_tabs_share_one_top_row_above_both_pdf_panes() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("top-tabs-{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let mut app = PrototypeApp::from_startup(
        paths,
        SessionStore::new(directory.path().join("session.json")),
    );
    let first_id = app.tabs.tabs()[0].id();
    let second_id = app.tabs.tabs()[1].id();
    assert!(app.tabs.create_split(
        first_id,
        second_id,
        crate::domain::tabs::SplitPlacement::Right
    ));

    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
        ..Default::default()
    };
    let mut tab_bar_rect = Rect::NOTHING;
    let mut entry_count = 0;
    let mut entry_widths = Vec::new();
    let mut tab_rects = Vec::new();
    let mut tab_ids = Vec::new();
    let mut selected_fill = Color32::TRANSPARENT;
    let mut focused_pdf_rect = Rect::NOTHING;
    let output = context.run_ui(input, |ui| {
        let tab_bar = app.tab_bar(ui);
        let tab_bar = tab_bar.as_ref().unwrap();
        tab_bar_rect = tab_bar.bar_rect;
        entry_count = tab_bar.entry_rects.len();
        entry_widths = tab_bar
            .entry_rects
            .iter()
            .map(Rect::width)
            .collect::<Vec<_>>();
        tab_rects = tab_bar.tab_rects.clone();
        tab_ids = tab_bar.tab_ids.clone();
        selected_fill = ui.visuals().selection.bg_fill;
        app.toolbar(ui);
        focused_pdf_rect = app.central_panel(ui, Some(tab_bar));
    });

    assert_eq!(entry_count, 2);
    assert!((entry_widths[0] - entry_widths[1]).abs() < f32::EPSILON);
    let group = app.tabs.split_for_tab(first_id).unwrap();
    let first_member = tab_ids
        .iter()
        .position(|tab_id| *tab_id == group.tab(SplitSide::First))
        .unwrap();
    let second_member = tab_ids
        .iter()
        .position(|tab_id| *tab_id == group.tab(SplitSide::Second))
        .unwrap();
    assert_eq!(
        tab_rects[first_member].right(),
        tab_rects[second_member].left()
    );

    let active_tab = app.tabs.active_tab_id().unwrap();
    for (tab_id, tab_rect) in tab_ids.iter().zip(&tab_rects) {
        if !group.tabs().contains(tab_id) {
            continue;
        }
        let uses_selected_fill = output.shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Rect(shape)
                    if shape.rect == *tab_rect && shape.fill == selected_fill
            )
        });
        assert_eq!(uses_selected_fill, *tab_id == active_tab);
    }
    assert!(focused_pdf_rect.top() > tab_bar_rect.bottom());
}

#[test]
fn tab_drag_preview_identifies_a_single_tab_and_both_split_members() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..2)
        .map(|index| {
            let path = directory.path().join(format!("drag-preview-{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let mut app = PrototypeApp::from_startup(
        paths,
        SessionStore::new(directory.path().join("session.json")),
    );
    let ids = app
        .tabs
        .tabs()
        .iter()
        .map(|tab| tab.id())
        .collect::<Vec<_>>();
    assert_eq!(
        app.tab_drag_preview_label(TabDragSource::Tab(ids[0])),
        Some("drag-preview-0.pdf".to_owned())
    );
    assert!(
        app.tabs
            .create_split(ids[0], ids[1], crate::domain::tabs::SplitPlacement::Right)
    );
    let group = app.tabs.active_split().unwrap();
    let expected = format!(
        "drag-preview-{}.pdf ｜ drag-preview-{}.pdf",
        app.tabs
            .tab_registry_index(group.tab(SplitSide::First))
            .unwrap(),
        app.tabs
            .tab_registry_index(group.tab(SplitSide::Second))
            .unwrap()
    );

    assert_eq!(
        app.tab_drag_preview_label(TabDragSource::Group(group.id())),
        Some(expected)
    );
}

#[test]
fn pointer_zoom_focuses_and_updates_the_pane_under_the_pointer() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..2)
        .map(|index| {
            let path = directory.path().join(format!("pointer-zoom-{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let mut app = PrototypeApp::from_startup(
        paths,
        SessionStore::new(directory.path().join("session.json")),
    );
    let first_id = app.tabs.tabs()[0].id();
    let second_id = app.tabs.tabs()[1].id();
    assert!(app.tabs.create_split(
        first_id,
        second_id,
        crate::domain::tabs::SplitPlacement::Right
    ));
    app.focus_side(SplitSide::First);
    assert_eq!(app.active_index(), Some(1));

    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
        events: vec![
            Event::PointerMoved(Pos2::new(650.0, 300.0)),
            Event::Zoom(1.2),
        ],
        ..Default::default()
    };
    let _output = context.run_ui(input, |ui| {
        let tab_bar = app.tab_bar(ui);
        app.central_panel(ui, tab_bar.as_ref());
    });

    assert_eq!(app.focused_side(), SplitSide::Second);
    assert_eq!(app.active_index(), Some(0));
    assert_eq!(app.documents[0].view.zoom, 1.2);
    assert_eq!(app.documents[1].view.zoom, 1.0);

    app.focus_side(SplitSide::First);
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
        events: vec![
            Event::PointerMoved(Pos2::new(650.0, 300.0)),
            wheel_event(
                MouseWheelUnit::Line,
                Vec2::new(0.0, -1.0),
                TouchPhase::Move,
                false,
            ),
        ],
        ..Default::default()
    };
    let _output = context.run_ui(input, |ui| {
        let tab_bar = app.tab_bar(ui);
        app.central_panel(ui, tab_bar.as_ref());
    });
    assert_eq!(app.focused_side(), SplitSide::Second);
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

fn copy_event_input() -> egui::RawInput {
    egui::RawInput {
        events: vec![egui::Event::Copy],
        ..Default::default()
    }
}

fn copy_key_release_event(key: Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    }
}

#[test]
fn render_result_requires_visible_tab_and_current_document_state() {
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
                PrototypeApp::fit_zoom_for_page(bounds, available_size, ZoomMode::FitPage).unwrap();
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
            let metrics = measure_single_page_scroll_area(bounds, screen_size, pixels_per_point);
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

    let requested = tile_specs_intersecting_viewport(grid, page_rect, transition_viewport).unwrap();

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

fn wheel_event(unit: MouseWheelUnit, delta: Vec2, phase: TouchPhase, control_held: bool) -> Event {
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
                PrototypeApp::fit_zoom_for_page(bounds, viewport_size, ZoomMode::FitWidth).unwrap();
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
        AutoscrollOffsets {
            current: Vec2::ZERO,
            maximum: Vec2::splat(1_000.0),
        },
        false,
    );

    assert!(frame.is_none());
    assert!(view.autoscroll.is_none());
}

#[test]
fn autoscroll_start_is_rejected_during_primary_page_interaction() {
    let context = egui::Context::default();
    let mut view = ViewState::new();
    let pointer = Pos2::new(100.0, 100.0);
    let events = vec![
        egui::Event::PointerMoved(pointer),
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Middle,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Middle,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    ];

    let active = run_autoscroll_frame(&context, &mut view, events, true, true);

    assert!(!active);
    assert!(view.autoscroll.is_none());
}

#[test]
fn autoscroll_is_active_only_between_start_and_stop_clicks() {
    let context = egui::Context::default();
    let mut view = ViewState::new();
    let pointer = Pos2::new(100.0, 100.0);
    let middle_click = vec![
        egui::Event::PointerMoved(pointer),
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Middle,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Middle,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    ];
    assert!(run_autoscroll_frame(
        &context,
        &mut view,
        middle_click,
        true,
        false,
    ));
    assert!(view.autoscroll.is_some());

    let primary_click = vec![
        egui::Event::PointerMoved(pointer),
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: pointer,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    ];
    assert!(!run_autoscroll_frame(
        &context,
        &mut view,
        primary_click,
        true,
        false,
    ));
    assert!(view.autoscroll.is_none());
}

#[test]
fn focus_loss_stops_active_autoscroll() {
    let context = egui::Context::default();
    let mut view = ViewState::new();
    view.autoscroll = Some(AutoscrollState {
        anchor: Pos2::new(20.0, 30.0),
        requested_offset: Some(Vec2::new(40.0, 50.0)),
    });

    let active = run_autoscroll_frame(&context, &mut view, Vec::new(), false, false);

    assert!(!active);
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
fn toolbar_singleline_text_is_centered_inside_its_clip_rect() {
    let context = egui::Context::default();
    let _output = context.run_ui(Default::default(), |ui| {
        let mut text = "日本語ABC123".to_owned();
        let output = toolbar_singleline_text_edit(&mut text)
            .min_size(Vec2::new(180.0, TOOLBAR_CONTROL_HEIGHT))
            .show(ui);
        let galley_center = output.galley_pos.y + output.galley.size().y / 2.0;

        assert!((galley_center - output.text_clip_rect.center().y).abs() < 0.01);
    });
}

#[test]
fn page_navigation_preserves_the_render_generation() {
    let path = PathBuf::from("missing.pdf");
    let mut tab = DocumentTab::new(1, path.clone(), 0, None);
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
        #[cfg(debug_assertions)]
        open_time: Duration::ZERO,
        #[cfg(debug_assertions)]
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
fn reload_success_clamps_page_clears_outline_selection_search_and_advances_generations() {
    let mut tab = DocumentTab::new(1, PathBuf::from("missing.pdf"), 0, None);
    tab.view.current_page = 4;
    tab.view.scroll_to_page = Some(4);
    tab.page_input = "5".to_owned();
    tab.outline = Some(Vec::new());
    tab.outline_requested = true;
    tab.search.query = "needle".to_owned();
    tab.search.pages.insert(4, Vec::new());
    tab.selection_generation = 7;
    tab.search.generation = 9;
    tab.selection = Some(SelectionSnapshot {
        page_index: 4,
        text: "selected".to_owned(),
        display_quads: Vec::new(),
        quads: Vec::new(),
        generation: 7,
        #[cfg(debug_assertions)]
        extraction_time: Duration::ZERO,
    });
    let search_generation = tab.search.generation;
    let selection_generation = tab.selection_generation;
    let render_generation = tab.view.generation;
    let thumbnail_generation = tab.thumbnail_generation;

    tab.prepare_for_external_reload(2);
    tab.invalidate_rendering();
    let _ = tab.invalidate_thumbnails();

    assert_eq!(tab.view.current_page, 1);
    assert_eq!(tab.view.scroll_to_page, Some(1));
    assert_eq!(tab.page_input, "2");
    assert!(tab.outline.is_none());
    assert!(!tab.outline_requested);
    assert!(tab.selection.is_none());
    assert!(tab.search.pages.is_empty());
    assert_eq!(tab.search.query, "needle");
    assert_ne!(tab.search.generation, search_generation);
    assert_ne!(tab.selection_generation, selection_generation);
    assert_ne!(tab.view.generation, render_generation);
    assert_ne!(tab.thumbnail_generation, thumbnail_generation);
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
fn pane_transition_restores_the_mode_specific_center_anchor() {
    let mut tab = DocumentTab::new(1, PathBuf::from("missing.pdf"), 0, None);
    let continuous_anchor = PageAnchor {
        page_index: 4,
        page_x_fraction: 0.3,
        page_y_fraction: 0.7,
    };
    tab.view.center_anchor = Some(continuous_anchor);
    tab.view.autoscroll = Some(AutoscrollState {
        anchor: Pos2::ZERO,
        requested_offset: None,
    });

    tab.prepare_for_pane_transition();

    assert_eq!(tab.view.restore_anchor, Some(continuous_anchor));
    assert!(tab.view.autoscroll.is_none());

    let single_anchor = Vec2::new(0.2, 0.8);
    tab.view.display_mode = DisplayMode::SinglePage;
    tab.view.single_center_anchor = Some(single_anchor);
    tab.prepare_for_pane_transition();

    assert_eq!(tab.view.restore_single_anchor, Some(single_anchor));
}

#[test]
fn closing_an_inactive_first_split_member_preserves_the_second_member_anchor() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("inactive-close-{index}.pdf"));
            write_blank_pdf(&path);
            path
        })
        .collect::<Vec<_>>();
    let mut app = PrototypeApp::from_startup(
        paths,
        SessionStore::new(directory.path().join("session.json")),
    );
    let ids = app
        .tabs
        .tabs()
        .iter()
        .map(|tab| tab.id())
        .collect::<Vec<_>>();
    assert!(
        app.tabs
            .create_split(ids[0], ids[1], crate::domain::tabs::SplitPlacement::Left)
    );
    assert!(app.tabs.select_tab(ids[2]));
    let anchor = PageAnchor {
        page_index: 0,
        page_x_fraction: 0.3,
        page_y_fraction: 0.4,
    };
    let second_index = app.tabs.tab_registry_index(ids[1]).unwrap();
    app.documents[second_index].view.center_anchor = Some(anchor);
    app.documents[second_index].view.restore_anchor = None;

    let first_index = app.tabs.tab_registry_index(ids[0]).unwrap();
    assert!(app.remove_tab_now(first_index));

    let remaining_index = app.tabs.tab_registry_index(ids[1]).unwrap();
    assert_eq!(
        app.documents[remaining_index].view.restore_anchor,
        Some(anchor)
    );
}

#[test]
fn runtime_layout_drops_unavailable_tabs_and_keeps_valid_focus() {
    let (_directory, ids) = runtime_tab_ids(3);
    let saved = SessionLayout {
        entries: vec![
            SessionEntry::Split {
                tab_indices: [0, 1],
                direction: SessionSplitDirection::Horizontal,
                ratio: 0.4,
                focused_tab: 1,
            },
            SessionEntry::Single { tab_index: 2 },
        ],
        active_tab: Some(2),
    };

    let (entries, active) = restored_runtime_layout(&saved, &[Some(ids[0]), None, Some(ids[2])]);

    assert_eq!(
        entries,
        vec![
            RestoredTabEntry::Single(ids[0]),
            RestoredTabEntry::Single(ids[2])
        ]
    );
    assert_eq!(active, Some(ids[2]));

    let (entries, active) = restored_runtime_layout(&saved, &[Some(ids[0]), None, None]);
    assert_eq!(entries, vec![RestoredTabEntry::Single(ids[0])]);
    assert_eq!(active, Some(ids[0]));
}

#[test]
fn floating_windows_and_popups_block_pane_input_but_tooltips_do_not() {
    let id = Id::new("layer-test");

    assert!(!foreground_layer_blocks_pane_input(Some(LayerId::new(
        egui::Order::Background,
        id,
    ))));
    assert!(foreground_layer_blocks_pane_input(Some(LayerId::new(
        egui::Order::Middle,
        id,
    ))));
    assert!(foreground_layer_blocks_pane_input(Some(LayerId::new(
        egui::Order::Foreground,
        id,
    ))));
    assert!(!foreground_layer_blocks_pane_input(Some(LayerId::new(
        egui::Order::Tooltip,
        id,
    ))));
}

#[test]
fn suspension_chooses_oldest_inactive_fully_suspendable_document() {
    let candidates = [(true, 2), (false, 1), (true, 3), (false, 0)];

    assert_eq!(oldest_suspendable_index(&[0], &candidates), Some(2));
    assert_eq!(oldest_suspendable_index(&[2], &candidates), Some(0));
}

#[test]
fn suspension_skips_oldest_document_while_it_is_printing() {
    let candidates = [(false, 0), (true, 1), (true, 2)];

    assert_eq!(oldest_suspendable_index(&[2], &candidates), Some(1));
}

#[test]
fn worker_disconnect_clears_every_close_blocking_operation() {
    let mut tab = DocumentTab::new(1, PathBuf::from("missing.pdf"), 0, None);
    tab.state = DocumentState::Saving;
    tab.pending_edits = 1;
    tab.pending_annotation_pages.insert(AnnotationPageRequest {
        page_index: 0,
        expected_revision: 0,
    });
    tab.undo_in_flight = true;
    tab.save_in_flight = true;
    tab.print_in_flight = true;
    tab.edit_history.push(EditAction::CreateHighlight {
        page_index: 0,
        annotation_xref: 1,
    });
    tab.save_as = SaveAsState::Saving(SaveAsRequest {
        path: PathBuf::from("saved-copy.pdf"),
        expected_destination: None,
    });

    tab.mark_worker_disconnected();

    assert_eq!(tab.pending_edits, 0);
    assert!(tab.pending_annotation_pages.is_empty());
    assert!(!tab.undo_in_flight);
    assert!(!tab.is_saving());
    assert!(matches!(tab.save_as, SaveAsState::Idle));
    assert!(!tab.is_printing());
    assert!(tab.has_unsaved_changes());
    assert_eq!(tab.state, DocumentState::Error);
    assert!(tab.service.is_none());
}

#[test]
fn conflict_state_is_dedicated_and_survives_status_like_local_changes() {
    let version = DocumentVersion {
        identity_primary: 1,
        identity_secondary: 2,
        length: 3,
        modified: std::time::SystemTime::UNIX_EPOCH,
    };
    let mut tab = DocumentTab::new(1, PathBuf::from("conflict.pdf"), 0, None);
    tab.external_conflict = Some(ExternalConflict { version });
    tab.state = DocumentState::ReadyDirty;
    tab.edit_history.clear();

    assert!(tab.error.is_none());
    assert_eq!(tab.external_conflict, Some(ExternalConflict { version }));
    assert!(tab.has_unsaved_changes());
}

#[test]
fn save_as_single_flight_and_cancel_keep_dirty_state_and_worker_disconnect_resets_state() {
    let mut tab = DocumentTab::new(1, PathBuf::from("source.pdf"), 0, None);
    tab.state = DocumentState::ReadyDirty;
    tab.save_as = SaveAsState::Saving(SaveAsRequest {
        path: PathBuf::from("copy.pdf"),
        expected_destination: None,
    });
    assert!(tab.is_saving());
    assert!(tab.has_unsaved_changes());

    tab.save_as = SaveAsState::Idle;
    assert!(!tab.is_saving());
    assert!(tab.has_unsaved_changes());

    tab.save_as = SaveAsState::Saving(SaveAsRequest {
        path: PathBuf::from("copy.pdf"),
        expected_destination: None,
    });
    tab.mark_worker_disconnected();
    assert!(matches!(tab.save_as, SaveAsState::Idle));
    assert!(tab.error.is_some());
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
fn text_snapshot_result_requires_visible_current_page() {
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
fn annotation_result_requires_visible_current_revision() {
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
        tabs: paths
            .iter()
            .cloned()
            .map(|path| saved_tab(path, 0))
            .collect(),
        layout: single_pane_layout(51, 12),
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
fn startup_restores_shared_order_split_focus_direction_and_ratio() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..4)
        .map(|index| {
            let path = directory.path().join(format!("split-{index}.pdf"));
            write_blank_pdf(&path);
            std::fs::canonicalize(path).unwrap()
        })
        .collect::<Vec<_>>();
    let layout = SessionLayout {
        entries: vec![
            SessionEntry::Single { tab_index: 2 },
            SessionEntry::Split {
                tab_indices: [0, 3],
                direction: SessionSplitDirection::Vertical,
                ratio: 0.35,
                focused_tab: 3,
            },
            SessionEntry::Single { tab_index: 1 },
        ],
        active_tab: Some(3),
    };
    let state = SessionState {
        tabs: paths
            .iter()
            .cloned()
            .map(|path| saved_tab(path, 0))
            .collect(),
        layout: layout.clone(),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path));
    finish_async_session_restore(&mut app);

    assert_eq!(app.tabs.entries().len(), 3);
    assert_eq!(app.active_index(), Some(3));
    let group = app.tabs.active_split().unwrap();
    assert_eq!(group.direction(), SplitDirection::Vertical);
    assert_eq!(group.ratio(), 0.35);
    assert_eq!(app.current_session().layout, layout);
}

#[test]
fn startup_opens_explicit_pdf_without_restoring_when_session_restore_is_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let saved = directory.path().join("saved.pdf");
    let explicit = directory.path().join("explicit.pdf");
    write_blank_pdf(&saved);
    write_blank_pdf(&explicit);
    let explicit = std::fs::canonicalize(explicit).unwrap();
    let state = SessionState {
        restore_enabled: false,
        tabs: vec![saved_tab(std::fs::canonicalize(saved).unwrap(), 0)],
        layout: single_pane_layout(1, 0),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let app = PrototypeApp::from_startup(vec![explicit.clone()], SessionStore::new(session_path));

    assert!(!app.restore_enabled);
    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.tabs.tabs()[0].path(), explicit);
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
        tabs: vec![saved_tab(inaccessible, 0), saved_tab(valid.clone(), 0)],
        layout: single_pane_layout(2, 0),
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
        tabs: vec![
            saved_tab(std::fs::canonicalize(first).unwrap(), 0),
            saved_tab(std::fs::canonicalize(second).unwrap(), 0),
        ],
        layout: single_pane_layout(2, 0),
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
        tabs: vec![saved_tab(std::fs::canonicalize(path).unwrap(), 0)],
        layout: single_pane_layout(1, 0),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app = PrototypeApp::from_startup(Vec::new(), SessionStore::new(session_path.clone()));
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
fn startup_restores_saved_session_before_opening_an_explicit_pdf() {
    let directory = tempfile::tempdir().unwrap();
    let saved = directory.path().join("saved.pdf");
    let explicit = directory.path().join("explicit.pdf");
    write_blank_pdf(&saved);
    write_blank_pdf(&explicit);
    let saved = std::fs::canonicalize(saved).unwrap();
    let explicit = std::fs::canonicalize(explicit).unwrap();
    let state = SessionState {
        tabs: vec![saved_tab(saved.clone(), 0)],
        layout: single_pane_layout(1, 0),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app =
        PrototypeApp::from_startup(vec![explicit.clone()], SessionStore::new(session_path));

    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.tabs.tabs()[0].path(), saved);
    assert_eq!(app.tabs.tabs()[1].path(), explicit);
    assert_eq!(app.active_index(), Some(1));
    assert!(app.session_restore_progress.is_some());
    finish_async_session_restore(&mut app);
}

#[test]
fn startup_selects_explicit_pdf_already_restored_without_duplicate_tab() {
    let directory = tempfile::tempdir().unwrap();
    let saved = directory.path().join("saved.pdf");
    write_blank_pdf(&saved);
    let saved = std::fs::canonicalize(saved).unwrap();
    let state = SessionState {
        tabs: vec![saved_tab(saved.clone(), 0)],
        layout: single_pane_layout(1, 0),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app = PrototypeApp::from_startup(vec![saved], SessionStore::new(session_path));

    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.tabs.tabs().len(), 1);
    assert_eq!(app.active_index(), Some(0));
    assert!(app.session_restore_progress.is_some());
    finish_async_session_restore(&mut app);
}

#[test]
fn startup_opens_multiple_explicit_pdfs_after_restoring_saved_session_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let saved = directory.path().join("saved.pdf");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");
    write_blank_pdf(&saved);
    write_blank_pdf(&first);
    write_blank_pdf(&second);
    let saved = std::fs::canonicalize(saved).unwrap();
    let first = std::fs::canonicalize(first).unwrap();
    let second = std::fs::canonicalize(second).unwrap();
    let state = SessionState {
        tabs: vec![saved_tab(saved.clone(), 0)],
        layout: single_pane_layout(1, 0),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app = PrototypeApp::from_startup(
        vec![first.clone(), second.clone()],
        SessionStore::new(session_path),
    );

    let paths = app
        .tabs
        .tabs()
        .iter()
        .map(|tab| tab.path().to_path_buf())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec![saved, first, second]);
    assert_eq!(app.active_index(), Some(2));
    finish_async_session_restore(&mut app);
}

#[test]
fn startup_pdf_is_added_without_changing_restored_split_layout() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..4)
        .map(|index| {
            let path = directory.path().join(format!("split-startup-{index}.pdf"));
            write_blank_pdf(&path);
            std::fs::canonicalize(path).unwrap()
        })
        .collect::<Vec<_>>();
    let explicit = directory.path().join("explicit.pdf");
    write_blank_pdf(&explicit);
    let explicit = std::fs::canonicalize(explicit).unwrap();
    let layout = SessionLayout {
        entries: vec![
            SessionEntry::Single { tab_index: 2 },
            SessionEntry::Split {
                tab_indices: [0, 3],
                direction: SessionSplitDirection::Vertical,
                ratio: 0.35,
                focused_tab: 3,
            },
            SessionEntry::Single { tab_index: 1 },
        ],
        active_tab: Some(3),
    };
    let state = SessionState {
        tabs: paths
            .iter()
            .cloned()
            .map(|path| saved_tab(path, 0))
            .collect(),
        layout: layout.clone(),
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let mut app = PrototypeApp::from_startup(vec![explicit], SessionStore::new(session_path));

    let mut expected_layout = layout;
    expected_layout
        .entries
        .insert(2, SessionEntry::Single { tab_index: 4 });
    expected_layout.active_tab = Some(4);
    assert_eq!(app.current_session().layout, expected_layout);
    finish_async_session_restore(&mut app);
}

#[test]
fn explicit_cli_pdf_restores_and_captures_recent_annotation_colors() {
    let directory = tempfile::tempdir().unwrap();
    let explicit = directory.path().join("explicit.pdf");
    write_blank_pdf(&explicit);
    let explicit = std::fs::canonicalize(explicit).unwrap();
    let state = SessionState {
        recent_annotation_colors: vec![[12, 34, 56], [90, 80, 70]],
        ..SessionState::default()
    };
    let session_path = directory.path().join("session.json");
    SessionStore::new(session_path.clone())
        .save(&state)
        .unwrap();

    let app = PrototypeApp::from_startup(vec![explicit], SessionStore::new(session_path));

    assert_eq!(app.recent_annotation_colors, state.recent_annotation_colors);
    assert_eq!(
        app.current_session().recent_annotation_colors,
        state.recent_annotation_colors
    );
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
