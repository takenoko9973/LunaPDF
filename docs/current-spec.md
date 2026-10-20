# LunaPDF 現行仕様

この文書は、利用者から確認できる現行動作を実装に照らして整理する。
作業依頼や過去の検証記録は、その時点の目的と結果を示す履歴資料であり、現行動作の根拠としては扱わない。
履歴資料との使い分けは [作業資料索引](tasks/README.md) を参照する。

仕様を確認したコードの基準は `main` の `8155214` である。
以下のテスト名は関連する既存テストへの案内であり、この文書の作成時にテスト、ビルド、GUI、OS実機、印刷を実行した記録ではない。

## PDFを開く

起動時に指定したPDFは、保存済みセッションを復元した後、指定順で開く。
ファイル選択とファイルのドロップも文書を開く操作に使える。
すでに開いているパスが再度渡された場合は同じタブを選択する。
Windowsでは、起動中のアプリに後から渡されたPDFも同じタブ追加経路で処理する。

実装は [`main.rs`](../src/main.rs) の `pdf_paths_from_args`、[`app.rs`](../src/app.rs) の `pick_pdf_and_open`、`open_document_with_intent`、`handle_dropped_files` を参照。
関連テストは [`app/tests.rs`](../src/app/tests.rs) の `startup_opens_multiple_explicit_pdfs_after_restoring_saved_session_in_order` と `external_open_request_uses_the_existing_tab_path_for_multiple_pdfs`。

## ページ移動と表示

連続表示と単ページ表示を切り替えられる。
倍率には固定倍率、幅に合わせる表示、ページ全体を合わせる表示がある。
表示方式を切り替えると、現在ページとページ内のおおよその表示位置を引き継ぐ。
ページ番号入力は1から始まり、文書のページ範囲内の整数を受け付ける。
PageUp / PageDown はテキスト入力欄にフォーカスがないときにページを移動する。

単ページ表示のホイールによるページ移動は、ポインターがPDFビュー上にあり、Ctrlキーなしの縦入力で、表示中ページの上下端に達したときに限る。
文書の先頭・末尾を越えず、端に達する前のホイール入力はページを移動しない。
Ctrl付き、横向き、PDFビュー外の入力はページ移動として扱わない。
ページ全体に合わせる倍率はウィンドウ寸法の変更に応じて再計算される。

実装は [`app/navigation.rs`](../src/app/navigation.rs) の `DisplayMode`、`ZoomMode`、`ViewState` と [`app.rs`](../src/app.rs) の `handle_shortcuts`、`move_page`、ページ番号入力処理を参照。
関連テストは [`app/tests.rs`](../src/app/tests.rs) の `page_input_accepts_only_one_based_in_range_numbers`、`display_mode_roundtrip_preserves_page_and_normalized_position`、`fit_page_zoom_recalculates_after_window_resize`、`single_page_view_line_wheel_navigates_pages_and_respects_boundaries`、`single_page_wheel_ignores_control_horizontal_and_outside_input`、`single_page_wheel_does_not_cross_document_boundaries`。

## 文書内検索

検索語を入れるとページ単位で検索し、見つかった結果へ移動できる。
検索が完了して既存の結果があるとき、Enter は次、Shift+Enter は前の結果へ移動し、同じ検索を再実行しない。
検索結果が0件のまま全ページの処理が終わった場合も検索完了として扱い、結果がない状態のEnterは移動しない。
IME変換中の確定入力は結果移動に使わない。
検索欄内のクリア操作で語を消せる。

検索が未完了の間は、語の変更などに応じて検索をやり直す。
完了済み検索は、ホイール操作、フォーカス移動、再描画だけではやり直さない。
古い検索世代や文書リビジョンから遅れて届いた結果は採用しない。
Enter操作の説明は、検索完了済みの結果移動と0件の状態に限り、初回検索や未完了の全入力経路に一般化しない。

実装は [`app/search.rs`](../src/app/search.rs) の `SearchState::needs_restart`、`begin_search`、`navigate_search`、`receive_search_page` を参照。
関連テストは [`app/tests.rs`](../src/app/tests.rs) の `search_text_edit_enter_navigates_matches_without_restarting`、`search_text_edit_enter_ignores_zero_matches_and_ime_confirmation`、`completed_search_survives_wheel_focus_and_repaint_but_partial_search_restarts`、`search_clear_control_is_inside_field_and_rejects_late_results`。

## タブと分割表示

文書ごとにタブを持ち、2文書を水平または垂直に分割して同時に表示できる。
分割中にフォーカスを移しても両方の文書を表示したままにできる。
Ctrl+Tab の切り替えでは、分割された2文書を1つの表示単位として扱う。
閉じたタブは直近に閉じた順に復元し、保存されていた表示位置も引き継ぐ。

セッション復元では、保存されたタブ順、選択状態、分割方向、比率とフォーカスを復元する。
復元に失敗したパスなどがある場合に、保存内容どおりの全タブが必ず開くという意味ではない。

実装は [`domain/tabs.rs`](../src/domain/tabs.rs)、[`app/workspace.rs`](../src/app/workspace.rs)、[`app.rs`](../src/app.rs) の `restore_closed_tab` を参照。
関連テストは [`app/tests.rs`](../src/app/tests.rs) の `split_keeps_both_selected_documents_visible_while_focus_changes`、`ctrl_tab_treats_a_split_set_as_one_display_entry`、`closed_tabs_reopen_in_lifo_order_with_saved_view_and_shortcut`、`startup_restores_shared_order_split_focus_direction_and_ratio`。

## テキスト選択とハイライト

PDF上のテキストを選択してコピーし、選択範囲からハイライト注釈を作成できる。
複数行の選択は行境界を保つ。
既存のハイライトは、PDF側で許可される編集だけを行える。
対象のハイライトを更新または削除した変更は、PDFを保存して開き直した後も残る。

実装は [`domain/selection.rs`](../src/domain/selection.rs)、[`ui/annotation_editor.rs`](../src/ui/annotation_editor.rs)、[`pdf/mupdf_backend.rs`](../src/pdf/mupdf_backend.rs) を参照。
関連テストは [`domain/selection.rs`](../src/domain/selection.rs) の `logical_selection_preserves_line_boundaries` と `saved_and_preview_selection_geometry_are_identical`、および [`pdf/mupdf_backend.rs`](../src/pdf/mupdf_backend.rs) の `annotation_flags_limit_only_the_supported_edits`、`updates_existing_highlight_and_undo_restores_exact_properties`、`update_and_delete_survive_save_and_reopen`。

## PDFの保存

変更のないPDFには保存操作を行わない。
編集済みのPDFを保存すると、保存後に開き直して検証する。
開いているPDFが外部で置き換えられたことを検出した場合、通常の保存でその変更を上書きしない。

競合した文書に Save As を行うと元タブのファイル関連付けを維持する。
処理完了時に元タブは外部側の版を読み直し、編集内容を保存したコピーは別タブとして開く。
元ファイルへ編集内容を書き戻さない。

実装は [`app.rs`](../src/app.rs) の `save` と `save_conflicted_document_as`、[`pdf/mupdf_backend.rs`](../src/pdf/mupdf_backend.rs) の `save` と `save_as` を参照。
関連テストは [`pdf/mupdf_backend.rs`](../src/pdf/mupdf_backend.rs) の `update_and_delete_survive_save_and_reopen`、`externally_replaced_pdf_is_not_overwritten_by_save`、`save_as_verifies_temp_copy_and_keeps_source_unchanged` と [`app/tests.rs`](../src/app/tests.rs) の `saved_as_completion_keeps_edit_copy_and_external_source_reachable`。

## セッションと設定

タブと表示状態はユーザー単位のセッションとして保存され、起動時に復元される。
セッションファイルは Windows では `%APPDATA%\LunaPDF\session.json`、Linux では `$XDG_CONFIG_HOME/lunapdf/session.json` に保存する。
`XDG_CONFIG_HOME` が絶対パスでないか未設定の場合は `$HOME/.config/lunapdf/session.json` を使う。
セッション保存は Windows と Linux で実装されており、その他のOSでは非対応である。

設定はセッションと別の `settings.json` に保存する。
現在の設定項目はマウスホイールのスクロール速度で、初期値は100%、指定範囲は25%から300%である。

実装は [`persistence/session_store.rs`](../src/persistence/session_store.rs)、[`persistence/settings_store.rs`](../src/persistence/settings_store.rs)、[`domain/settings.rs`](../src/domain/settings.rs) を参照。
関連テストは [`app/tests.rs`](../src/app/tests.rs) の `startup_restores_shared_order_split_focus_direction_and_ratio`、`wheel_settings_store_defaults_round_trips_and_stays_separate_from_session`、`startup_applies_loaded_wheel_speed_and_ui_changes_apply_and_persist_immediately`。

## 印刷とOS依存

印刷は Windows 専用で、Windowsの印刷ダイアログで対象ページを選び、プリンタードライバーへページを出力する。
印刷機能が使えるのも Windows の場合だけである。
自動回転設定を有効にすると、ページごとの用紙向きを選ぶ処理を行う。

実装は [`app.rs`](../src/app.rs) と [`pdf/windows_print.rs`](../src/pdf/windows_print.rs)、レイアウト計画は [`pdf/print_layout.rs`](../src/pdf/print_layout.rs) を参照。
レイアウト計画のテスト `page_is_centered_and_preserves_aspect_ratio`、`mixed_pages_get_independent_orientation_and_content_plans`、`auto_disabled_keeps_current_orientation_and_zero_rotation` はページ配置計算を確認するもので、Windowsの印刷ダイアログ、各ドライバー、物理プリンターでの出力結果を確認するものではない。

## 仕様と検証の境界

この文書はコードと既存テストから利用者向けの動作を整理したものだが、全PDFとの互換性、すべての注釈形式の編集、他OSでの印刷・セッション保存、または実プリンター出力を保証しない。
コード上の条件やOS制限が明記された場合は、その条件の範囲内で読む。

現行動作の変更では、変更対象の章を確認したうえで [検証マトリクス](tasks/development-environment/verification-matrix.md) から必要な検証を選ぶ。
過去の計画・作業結果を参照する場合は [作業資料索引](tasks/README.md) から該当資料を探し、その資料だけを現行仕様として扱わない。
