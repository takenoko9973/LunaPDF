# LunaPDF アプリケーション仕様

この文書は、作業時点のソースコードと配布設定から利用者向けの現行機能を整理する。各項目の「実装確認」はコード上の経路を確認した意味であり、この文書の作成中に GUI、OS、実プリンターで操作を検証した意味ではない。記載したテスト名は既存テストへの案内で、今回実行した記録ではない。

## 1. 対応環境と基本方針

**状態：実装確認**

LunaPDF は PDF 文書をタブで開いて表示・編集するデスクトップアプリである。コードには Windows と Linux のセッション保存先があり、その他の OS ではセッション保存をサポートしない。Windows では単一起動の文書引き渡し、PDF の「プログラムから開く」候補、印刷に対応する。Linux のデスクトップファイルは PDF MIME type と複数ファイル引数を宣言する。これらのコード・配布設定の存在は、その OS での実機動作を保証しない。

Windows インストーラーは PDF の「プログラムから開く」候補を登録するが、既定アプリそのものは変更しない。ポータブル版は関連付けを登録しない。Linux の desktop entry は application/pdf を宣言する。OS 上で実際に既定アプリとして選択されているかは利用者の設定による。

確認箇所：[src/main.rs](../../src/main.rs) の main、pdf_paths_from_args、[src/platform/windows/single_instance.rs](../../src/platform/windows/single_instance.rs) の SingleInstanceListener、[src/app.rs](../../src/app.rs) の print、[src/persistence/session_store.rs](../../src/persistence/session_store.rs) の config_path_for_current_user、[packaging/linux/lunapdf.desktop](../../packaging/linux/lunapdf.desktop)、[packaging/windows/distribution-readme.txt](../../packaging/windows/distribution-readme.txt)。OS 実機・配布物の受入状態は要確認であり、今回その確認は行っていない。

## 2. PDF を開く

**状態：実装確認**

「ファイルを開く」では PDF ファイルを選べる。PDF のドラッグ＆ドロップ、起動引数での複数パス指定にも対応する。Windows では起動済みのプロセスへ後から渡されたパスを既存ウィンドウに送り、タブ追加と同じ経路で処理する。起動引数で指定した PDF は保存済みセッションの復元後に指定順で開く。セッション復元が無効なら、保存内容を先に開かず指定された PDF を開く。

開けるのは .pdf 拡張子のパスである。すでに同じパスのタブがあれば、新しい重複タブを作らずそのタブを選ぶ。ファイル選択や読み込みに失敗した場合はエラーを表示する。

確認箇所：[src/main.rs](../../src/main.rs) の pdf_paths_from_args、[src/app.rs](../../src/app.rs) の pick_pdf_and_open、open_document_with_intent、handle_dropped_files、receive_external_open_events、[src/platform/windows/single_instance.rs](../../src/platform/windows/single_instance.rs)。配布設定は上記 Windows/Linux の資料を参照。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の startup_opens_multiple_explicit_pdfs_after_restoring_saved_session_in_order、startup_opens_explicit_pdf_without_restoring_when_session_restore_is_disabled、external_open_request_uses_the_existing_tab_path_for_multiple_pdfs。

## 3. タブとワークスペース

**状態：実装確認**

複数の PDF をタブで管理し、タブをドラッグして順序を変更できる。タブを別の文書のPDF表示領域の端へドラッグすると、その二つを左右または上下に分割して表示する。左右分割では画面幅、上下分割では画面高さに沿ってペインを配置し、仕切りのドラッグで比率を変更する。分割した片方へフォーカスを移しても両方の文書を表示し続ける。分割ペアはタブ列上の一つの切り替え単位として扱う。

タブの閉じる操作では必要に応じて未保存編集の確認を行う。利用者が閉じたタブは直近に閉じたものから復元でき、保存済みの表示位置も引き継ぐ。

確認箇所：[src/domain/tabs.rs](../../src/domain/tabs.rs) の create_split、reorder_single、reorder_split、[src/app/workspace.rs](../../src/app/workspace.rs) の split_rects、[src/app.rs](../../src/app.rs) の tab_bar、close_tab、restore_closed_tab。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の split_keeps_both_selected_documents_visible_while_focus_changes、ctrl_tab_treats_a_split_set_as_one_display_entry、closed_tabs_reopen_in_lifo_order_with_saved_view_and_shortcut、startup_restores_shared_order_split_focus_direction_and_ratio。

## 4. ページ表示、移動、ズーム

**状態：実装確認**

表示方式は連続表示と単ページ表示を切り替えられる。新しい文書は連続表示・幅に合わせる倍率から始まる。ページ番号欄には1から始まる文書内の整数を入力でき、範囲外や整数でない入力はエラーとして扱う。表示方式を切り替えると、現在のページとページ内のおおよその表示位置を引き継ぐ。

倍率には固定倍率、幅に合わせる表示、ページ全体を合わせる表示がある。ページ全体に合わせる倍率はウィンドウ寸法に応じて再計算する。PageUp / PageDown は検索欄、ページ番号欄、注釈コメント欄などのテキスト入力にフォーカスがない場合に、前後のページへ移動する。文書の先頭・末尾を越えない。

単ページ表示の縦ホイールは、ポインターが PDF ビュー上にあり、Ctrl キーなしの縦入力で、表示ページの端にすでにいる場合にページを移動する。ページ端へ到達させるホイール入力自体は隣のページへ移動させない。ページ全体が縦に収まる場合は端で止まらず、上下のホイール入力で前後ページへ移動する。Ctrl 付き、横向き、PDF ビュー外のホイール入力はこのページ移動に使わない。

修飾キーなしの矢印キーは、連続表示と単ページ表示で同じ規則を使う。←／→ は現在ページの表示幅、↑／↓ は表示高さを実際のviewportと比較し、縦横を独立して判定する。操作軸で収まる場合は押下と所有したOSキーリピートごとに前後1ページへ移動する。大きい場合はページ内をスクロールし、長押しで端に到達したら停止する。押し始めから端であれば1ページだけ移動し、同じ長押しでは移動先のページ内だけをスクロールする。端を越えてページを移動するには、離して押し直す。遷移先は → なら左端、← なら右端、↓ なら上端、↑ なら下端から表示し、直交軸の位置は可能な限り維持する。先頭・末尾を越えない。入力欄、IME、メニュー、モーダルなどを優先し、分割表示ではフォーカス中のPDFだけを操作する。所有状態が切れた後のrepeatは操作に使わない。連続表示の矢印選択ページは通常の再描画で保持し、別の表示位置操作で既存のページ判定へ戻る。詳細と合成入力テストの記録は [keyboard-navigation.md](keyboard-navigation.md) を参照。実機のGUI／OS入力は未検証である。

確認箇所：[src/app/navigation.rs](../../src/app/navigation.rs) の DisplayMode、ZoomMode、ViewState、ArrowNavigationState、single_page_wheel_steps、[src/app.rs](../../src/app.rs) の handle_shortcuts、handle_arrow_navigation、move_page、update_fit_zoom、continuous_view、single_page_view、active_text_input_id。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の page_input_accepts_only_one_based_in_range_numbers、display_mode_roundtrip_preserves_page_and_normalized_position、fit_page_zoom_recalculates_after_window_resize、single_page_view_line_wheel_navigates_pages_and_respects_boundaries、single_page_wheel_ignores_control_horizontal_and_outside_input、single_page_wheel_does_not_cross_document_boundaries。

## 5. マウス操作、パン、自動スクロール

**状態：実装確認**

通常のホイールは PDF ビューをスクロールする。設定から行単位ホイールの速度を変えられ、Ctrl を伴うホイールのズーム量はこの速度設定の影響を受けない。タッチパッドのピンチ操作もズーム入力として扱う。

PDF の空白部分またはページ外の背景を左ドラッグすると、縦横両方向に表示位置をパンできる。ページ上の文字をドラッグした場合はテキスト選択が優先される。連続表示ではビュー上の中クリックで自動スクロールを開始し、ポインターの位置を動かして方向と速度を変える。別のマウスボタン、Escape、ウィンドウのフォーカス喪失で停止する。単ページ表示では自動スクロールを開始しない。

ホイール速度設定は初期値 100%、範囲 25% から 300% で、変更はセッションとは別の設定ファイルへ保存する。

確認箇所：[src/app/navigation.rs](../../src/app/navigation.rs) の update_autoscroll、autoscroll_velocity、[src/ui/viewport.rs](../../src/ui/viewport.rs) の PageViewport、[src/app.rs](../../src/app.rs) の set_wheel_scroll_speed、continuous_view、single_page_view、[src/domain/settings.rs](../../src/domain/settings.rs) の AppSettings。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の single_page_wheel_ignores_control_horizontal_and_outside_input、autoscroll_is_active_only_between_start_and_stop_clicks、focus_loss_stops_active_autoscroll、wheel_settings_store_defaults_round_trips_and_stays_separate_from_session、wheel_speed_changes_normal_line_scroll_without_changing_point_input、[src/ui/viewport.rs](../../src/ui/viewport.rs) の gray_background_click_clears_and_drag_pans_in_two_axes。

## 6. 検索と IME

**状態：実装確認**

検索欄に語を入力すると PDF 内をページ単位で検索し、見つかった一致箇所へ移動できる。検索が完了して一致箇所があるとき、Enter は次、Shift+Enter は前の一致箇所へ移動する。結果が0件のまま全ページの処理が完了した場合も検索完了として扱い、結果がない Enter は移動しない。検索欄内のクリア操作で語を消せる。

検索が未完了の間は語の変更などに応じて検索をやり直す。完了後はホイール、フォーカス移動、再描画だけでは検索をやり直さない。IME の変換中・確定イベントは検索結果への移動として扱わない。古い検索世代または文書リビジョンに属する結果は採用しない。

確認箇所：[src/app/search.rs](../../src/app/search.rs) の SearchState::needs_restart、begin_search、navigate_search、receive_search_page、[src/app.rs](../../src/app.rs) の検索欄描画と Event::Ime 処理。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の search_text_edit_enter_navigates_matches_without_restarting、search_text_edit_enter_ignores_zero_matches_and_ime_confirmation、completed_search_survives_wheel_focus_and_repaint_but_partial_search_restarts、search_clear_control_is_inside_field_and_rejects_late_results。

## 7. テキスト選択、ハイライト、Undo

**状態：実装確認**

PDF 上のテキスト範囲を選択してコピーし、選択範囲からハイライト注釈を作成できる。複数行の選択は行境界を保つ。ハイライト一覧から既存注釈を選び、対応するコメントや色などを編集し、削除できる。PDF の編集制限などで操作できない場合は、その文書に対する編集可能性に従う。

アプリが行ったハイライトの作成・更新・削除は Undo 履歴に記録され、最後の編集から戻せる。PDF 保存では変更を反映し、保存後に開き直して変更を検証する。保存・再読込を経ても編集したハイライトが残ることを確認するコードがある。

確認箇所：[src/domain/selection.rs](../../src/domain/selection.rs) の選択範囲処理、[src/ui/annotation_editor.rs](../../src/ui/annotation_editor.rs) の注釈エディター、[src/app.rs](../../src/app.rs) の copy_selection、create_highlight、undo、annotation_editor_overlay、[src/pdf/mupdf_backend.rs](../../src/pdf/mupdf_backend.rs) の create_highlight、update_annotation、delete_annotation、undo、save。

既存テスト宣言：[src/domain/selection.rs](../../src/domain/selection.rs) の logical_selection_preserves_line_boundaries、[src/pdf/mupdf_backend.rs](../../src/pdf/mupdf_backend.rs) の updates_existing_highlight_and_undo_restores_exact_properties、update_and_delete_survive_save_and_reopen、annotation_flags_limit_only_the_supported_edits。

## 8. 保存、外部変更、タブ終了

**状態：実装確認**

編集のない PDF は保存せず、変更がある PDF は保存できる。保存処理は PDF を保存したあと開き直して内容を検証する。PDF が外部で置き換わったことを検出した場合、未保存のローカル変更がある文書は競合状態になり、通常の保存で外部変更を上書きしない。競合文書を別名保存すると、元タブは外部側の版を読み直し、編集内容を保存したコピーは別タブで開く。明示確認して外部版を再読み込みする操作は、未保存の編集を破棄する。

未保存の文書を閉じるときは保存、破棄、キャンセルを選ぶ。保存中・印刷中は処理完了まで終了を待つ。注釈のコメント編集が未確定、または注釈処理中のときは、その状態を確定または破棄するまでタブを閉じられない。ウィンドウ終了では対象文書ごとに未保存編集を処理し、その後に設定とセッションを保存する。設定またはセッションの保存に失敗した場合は、再試行、セッションを保存せず終了、キャンセルを選ぶ。

確認箇所：[src/app.rs](../../src/app.rs) の save、save_conflicted_document_as、close_tab、apply_close_decision、handle_window_close、finalize_session_and_close、[src/pdf/mupdf_backend.rs](../../src/pdf/mupdf_backend.rs) の save、save_as。

既存テスト宣言：[src/pdf/mupdf_backend.rs](../../src/pdf/mupdf_backend.rs) の externally_replaced_pdf_is_not_overwritten_by_save、save_as_verifies_temp_copy_and_keeps_source_unchanged、[src/app/tests.rs](../../src/app/tests.rs) の saved_as_completion_keeps_edit_copy_and_external_source_reachable、queued_save_blocks_close_until_document_returns_clean、settings_save_failure_does_not_skip_writable_session_save。

## 9. セッションと設定

**状態：実装確認**

前回のセッション復元は利用者が有効・無効を選ぶ。セッションにはタブのパスと順序、選択中のタブ、分割の方向・比率・フォーカス、ページ位置、表示方式、ズーム、サイドバーの開閉と選択項目、最近使った注釈色を保存する。復元できない文書があっても、保存時の全タブを必ず開けることを保証しない。

Windows のセッションファイルは %APPDATA%\LunaPDF\session.json、Linux では絶対パスの XDG_CONFIG_HOME があればその lunapdf/session.json、なければ $HOME/.config/lunapdf/session.json に置く。設定ファイルはセッションと同じ場所の settings.json で、現在保存する設定はホイールのスクロール速度である。macOS 等の他 OS はセッション保存の実装対象外である。

確認箇所：[src/domain/session.rs](../../src/domain/session.rs) の SessionState、SessionLayout、SessionView、[src/persistence/session_store.rs](../../src/persistence/session_store.rs) の SessionStore、config_path_for_current_user、[src/persistence/settings_store.rs](../../src/persistence/settings_store.rs) の SettingsStore、[src/domain/settings.rs](../../src/domain/settings.rs) の AppSettings。

既存テスト宣言：[src/app/tests.rs](../../src/app/tests.rs) の startup_restores_shared_order_split_focus_direction_and_ratio、wheel_settings_store_defaults_round_trips_and_stays_separate_from_session、startup_applies_loaded_wheel_speed_and_ui_changes_apply_and_persist_immediately。

## 10. 印刷、エラー、バージョン表示

**状態：実装確認**

印刷は Windows のみ対応する。印刷時は Windows の印刷ダイアログで対象ページを指定し、プリンタードライバーへ出力する。ページごとの用紙向きを自動回転する設定がある。Windows 以外では印刷を開始せず、非対応のエラーを表示する。レイアウト計算のテストは、物理プリンターでの出力を確認するものではない。

アプリケーション、選択中の文書、ページ番号入力、外部変更の競合などに対するエラーは画面上部に表示する。エラー要約に操作案内を添え、必要に応じて詳細も表示する。閉じる操作は各エラー状態を個別に扱う。

「LunaPDF について」には、Cargo の package version を使ってバージョンを表示する。現在の確認対象では 0.2.1 である。

確認箇所：[src/app.rs](../../src/app.rs) の can_print、print、error_banner、document_failure_message、About 表示、[src/pdf/windows_print.rs](../../src/pdf/windows_print.rs) の show_print_dialog、print_selected_pages、[src/pdf/print_layout.rs](../../src/pdf/print_layout.rs)。配布物の version は Cargo package metadata から参照する。

既存テスト宣言：[src/pdf/print_layout.rs](../../src/pdf/print_layout.rs) の page_is_centered_and_preserves_aspect_ratio、mixed_pages_get_independent_orientation_and_content_plans、auto_disabled_keeps_current_orientation_and_zero_rotation、[src/app/tests.rs](../../src/app/tests.rs) の worker_errors_have_japanese_guidance_and_keep_diagnostic_detail、error_banner_dismisses_app_and_active_document_errors_independently。印刷ダイアログ、OS、実プリンターでの受入確認は今回行っていない。

## 確認範囲

ここに挙げたコードと既存テスト宣言は、関連する実装経路を特定するための根拠である。今回、テスト、ビルド、GUI操作、OS 実機、プリンター出力は確認していない。

- **要確認：** Windows/Linux の実機動作、配布物の OS 連携、印刷ダイアログ、プリンタードライバーを通した出力。
- **対象外：** あらゆる PDF 形式や注釈形式との互換性、コードで扱わない OS の動作保証。
