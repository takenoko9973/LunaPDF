# LunaPDF タブ・ページ操作・テキスト選択・注釈編集 調査・作業報告

更新日：2026-07-27

この文書は、`lunapdf_codex_tabs_paging_selection_annotations_instructions.md` に基づく調査、実装、検証の結果を作業中から記録する。未完了の項目は成功扱いせず、確認済みの事実と未確認範囲を分けて記載する。

## 作業開始時点

- ブランチ：`main`
- 開始時HEAD：`374994070da19c814d2017502bd74568b69da8a5`
- 直近コミット：`3749940 docs: Windows GNUビルド手順を更新`
- 未コミット差分：
  - `assets/icons/fit-width.svg`：ユーザーの既存変更。本件では変更もステージも行わない。
  - `docs/tasks/lunapdf_codex_tabs_paging_selection_annotations_instructions.md`：未分類の作業指示書。作業開始前に`ui-improvement/`へ配置し、索引へ追加した。
- ホスト：Microsoft Windows NT 10.0.19045.0、x64
- Dev Container：WSL2 Linux x86_64、Rust 1.97.1、Cargo 1.97.1
- 基本のLinux build target：`x86_64-unknown-linux-gnu`
- DPI：開始時点では未計測
- 入力機器：Windows管理情報へのアクセスが拒否されたため、開始時点では未確認

今回の明示依頼に従い、実装を検証可能なフェーズへ分けて逐次コミットする。pushとPR作成は行わない。

## 開始時の自動検証

Dev Containerの`workspace`サービスを起動して次を実行した。

```text
docker compose -f .devcontainer/compose.base.yml exec workspace cargo fmt --check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo test
```

実結果：

- `cargo fmt --check`：成功
- `cargo check`：成功
- `cargo test`：成功（127 passed、0 failed、1 ignored）
- ignoredの1件は、環境変数で指定した場所へ外部閲覧用PDFを書き出す既存受入テスト

## 調査で確定した責務と契約

### 対象コンポーネント

- `src/app.rs`
  - タブ、ツールバー、単一ページScrollArea、文書イベント、編集状態のUI入口
- `src/ui/viewport.rs`
  - 画面座標とPDF座標の変換、ページ上のポインター入力、選択表示
- `src/domain/selection.rs`
  - glyph順序、コピー文字列、注釈保存用Quad、表示用geometry
- `src/domain/document.rs`
  - workerとUIの間で受け渡す文書・注釈・編集履歴のRust所有契約
- `src/pdf/worker.rs`
  - MuPDFを単一スレッドで所有するコマンド／イベント境界
- `src/pdf/mupdf_backend.rs`
  - 注釈列挙、xref指定更新・削除、保存検証

### 既存APIと利用可能なMuPDF API

- eframe／egui／egui_extras：0.35.0
- mupdf：0.8.0
- `PdfPage::annotations()`で既存注釈を列挙できる。
- `PdfAnnotation`からtype、xref、bounds、QuadPoints、color、contents、opacityを取得できる。
- `set_contents`、`set_color`、`update`、`PdfPage::delete_annotation`で更新・削除できる。
- xrefはPDFの間接オブジェクト番号で文書内では安定している。ページ内列挙順は識別に使わない。
- 保存処理はPDFを再オープンするため、保存後は古いrevisionの列挙結果と編集バッファを再利用しない。
- 注釈権限、読取専用、署名の既存`HighlightCapability`判定を作成・更新・削除へ共通利用する。更新失敗時に別注釈を作るfallbackは追加しない。

### 編集範囲

- crate外の公開API変更はない。
- 依存追加とegui／MuPDF更新は行わない。
- タイルキャッシュ、セッション形式、印刷、左サイドバーの契約は変更しない。
- ユーザー変更中の`assets/icons/fit-width.svg`を含む既存アイコン資産は変更しない。

## 実装フェーズ

1. タブ閉じる表示とページ番号欄
2. 単一ページ＋FitWidthホイール遷移
3. クリック／ドラッグ判定と行単位選択表示
4. 既存ハイライトの列挙・安定識別・読取・ヒットテスト
5. 右クリックメニュー、ダブルクリック入口、右端固定オーバーレイ
6. コメント／色更新、削除、Undo、dirty、revision、保存
7. 全体回帰検証と、実行可能な範囲の実機受入

各実装フェーズでは、修正前に対象の回帰テストを追加して失敗を確認し、原因となる処理だけを修正する。

## 1. タブ閉じる表示とページ番号欄

### 修正前の失敗

`tab_content_rects()`へ右端が10ptの最小幅タブを渡した結果、タブ右端106ptに対して閉じる領域右端は98ptだった。`TAB_HORIZONTAL_PADDING`の8ptが閉じる領域の外側にも適用され、見た目とクリック領域の両方が右端から離れていた。

```text
cargo test tab_close_region_reaches_tab_right_edge -- --nocapture
left: 98.0
right: 106.0
```

ページ番号欄は46pt固定であり、ページ数による表示桁数を表す契約がなかった。1・2桁でも3桁分を予約する回帰テストを追加し、修正前は1ページで1列を返して失敗することを確認した。

### 変更

- 閉じる24pt領域をタブ右端へ接するようにした。
- 文字グリフ`×`を廃止し、同じ長さ・同じ太さの2本のベクター線でXを描くようにした。フォントメトリクスによる上下の濃さの差を受けない。
- タイトル領域は閉じる領域より左で終わり、閉じる領域はタブ選択領域へ含めない既存契約を維持した。
- ページ番号欄は最低3桁を予約し、4桁以上は実際のページ数に応じて列数を増やす。
- 入力欄幅は現在のeguiフォントで必要な数字列を測り、スタイルのパディングと最小操作幅を加えて求める。固定のY補正や入力桁制限は追加していない。

### 自動検証

```text
cargo test tab_close -- --nocapture
cargo test page_input -- --nocapture
cargo test
```

実結果：

- タブ対象2件：成功
- ページ入力対象3件：成功
- 全体：131 passed、0 failed、1 ignored

100%・125%・150%・200%相当DPI、ホバー、無効状態、長い日本語名、多数タブの実画面確認は最終受入で実施する。

## 2. 単一ページ＋FitWidthホイール遷移

### 修正前の失敗

600 × 1600ptの縦長ページを800 × 600ptの表示領域へFitWidth表示し、eguiのScrollAreaを最下端へ移動した状態を再現した。

- `ScrollAreaOutput`が返す実際の下端判定：`true`
- 次フレーム用に事前読取した下端判定：`false`

```text
cargo test fit_width_scroll_area_reports_the_same_bottom_as_the_pre_frame_edge_check -- --nocapture
left: false
right: true
```

既存の純粋関数テストでは下端フラグを直接渡していたため、入力イベントの条件は検証できていたが、実際のScrollArea状態を取得できない問題を検出できていなかった。

### 原因

`ScrollArea::id_salt`は、呼び出し元のsaltを一度`egui::IdSalt`へハッシュしてから親UIのIDと結合する。一方、修正前の事前読取は生のタプルを`Ui::make_persistent_id`へ直接渡していたため、別のIDを読んでいた。結果として縦長ページでも保存済みoffsetは常に取得できず、開始offset 0として上端だけを判定していた。

FitPageなど縦方向に収まるページは最大offset自体が0なのでこの誤りが見えにくく、縦長になりやすいFitWidthで下端遷移ができなかった。

### 変更

- `ScrollArea`と同じ`IdSalt`変換を行う`scroll_area_state_id()`を追加し、事前offset読取を同一IDへ統一した。
- ページ境界処理を`adjacent_page_index()`へ分離し、先頭より前、最終より後、0ページ文書を返さないことをテストした。
- 既存の「端へ到達させる入力では遷移せず、その後の追加入力で遷移」「次ページは上端、前ページは下端」「Line／Pageは1イベント1段階、Pointは蓄積とラッチ」「Ctrl、横入力、表示領域外を除外」の処理は変更していない。

### 自動検証

```text
cargo test fit_width_scroll_area_uses_the_stored_bottom_for_wheel_transition -- --nocapture
cargo test single_page_wheel -- --nocapture
cargo test
```

実結果：

- FitWidth実ScrollArea下端と下方向遷移：成功
- 単一ページwheel対象3件：成功
- 全体：133 passed、0 failed、1 ignored

ノッチ式マウス、高精度wheel／trackpad、オーバーレイ上の入力除外は最終受入およびオーバーレイ実装後に確認する。

## 3. クリック／ドラッグ判定と行単位選択表示

### クリック／ドラッグの確認

egui 0.35の実入力状態を複数フレームで再現し、次を確認した。

- 同一位置の左押下・解放：選択要求なし
- egui既定click tolerance 6論理pt未満の移動：選択要求なし
- 1秒間静止した左押下：選択要求なし

この条件では「単純クリックで確定する」現象自体は再現しなかった。現行APIの`max_click_dist`を使うことを明示し、押下位置と現在位置がその距離を超えた場合だけ選択をactiveにする状態へ整理した。固定DPI補正は追加していない。押下点から8論理pt移動し、同じglyph内で解放した場合は1文字選択になることを確認した。

右クリックはPrimary押下条件へ入らない。注釈ダブルクリックとの競合は注釈入口の実装後に追加確認する。

### 行単位表示の修正前失敗

同一行の`ABC`を選択した表示geometryはglyphごとの3 Quadだった。

```text
cargo test display_quads_merge_adjacent_glyphs_on_the_same_line -- --nocapture
left: 3
right: 1
```

確定表示とドラッグ中プレビューの両方が各glyphを個別に塗り、さらに1.5ptの外枠を描いていたため、文字間に透明線と枠線が残っていた。

### 変更

- `SelectionSnapshot`で、コピー／注釈保存用のglyph Quadと、表示専用の行帯Quadを分離した。
- 表示用は同じ`line_index`の連続glyphだけを1本へ統合し、改行では別帯にする。
- 水平行は左右端glyphの外側edge、縦方向の行は上下端glyphのedgeを再利用する。外接水平矩形へ変換しないため、傾斜・縦書きの向きを維持する。
- ドラッグ中プレビューと確定表示は同じ`selected_display_quads()`を使用する。
- 選択表示は半透明塗りだけとし、glyphごとの外枠を廃止した。検索結果の既存外枠は維持した。
- コピー順、両端包含、Highlightへ渡す元のglyph Quadは変更していない。

### 自動検証

```text
cargo test domain::selection::tests -- --nocapture
cargo test ui::viewport::tests -- --nocapture
cargo test confirmed_selection_quads_match_the_preview_glyph_range -- --nocapture
cargo test
```

実結果：

- 論理選択／表示geometry 10件：成功
- 画面座標／ドラッグ入力7件：成功
- MuPDF選択統合1件：成功
- 全体：142 passed、0 failed、1 ignored

Typst PDF、逆方向ドラッグ、複数行の実画面表示、プレビュー・コピー・保存注釈の実機一致は最終受入で確認する。

## 4. 既存ハイライトの列挙・安定識別・読取・ヒットテスト

### 修正前の失敗

外部作成相当のHighlightへコメント、RGB色、透明度を設定して保存したfixtureを開き、ページ注釈を要求した。読取コマンドのstubは空一覧を返したため、注釈件数が0件となって失敗した。

```text
cargo test reads_existing_highlight_identity_geometry_comment_color_and_opacity -- --nocapture
left: 0
right: 1
```

### 変更

- ページ単位・revision指定の注釈読取コマンドをworker境界へ追加した。
- MuPDFのページ内配列順ではなく、文書内の間接オブジェクト番号であるxrefとページ番号の組を`AnnotationId`にした。IDの有効範囲は、その文書を開いているタブの同一revision内である。
- HighlightのQuadPoints、コメント（Contents）、Gray／RGB／CMYK色、透明度を所有データへコピーし、MuPDFオブジェクトをUIスレッドへ渡さない。
- 文書全体の既存`HighlightCapability`に加え、注釈の`ReadOnly`、`Locked`、`LockedContents`フラグからコメント・色・削除の可否を別々に判定する。更新不能時に新規注釈へ置き換えるfallbackは追加していない。
- 表示中ページだけを要求し、非表示化、タブ切替、文書revision更新でキャッシュを破棄する。非アクティブタブ、古いrevision、要求対象外ページから届いた結果は編集候補へ採用しない。
- 画面座標から変換済みのページ座標に対し、回転・傾斜Quadの実領域でヒット判定する。外接矩形内でもQuad外の点は候補にしない。退化Quadにもヒット領域を与えない。
- 候補列挙と0件／1件／複数件の決定方針を分離した。複数件は順序で自動決定せず、次フェーズのサブメニューへ全候補を渡す。

### 自動検証

```text
cargo test annotation -- --nocapture
cargo test reads_existing_highlight_identity_geometry_comment_color_and_opacity -- --nocapture
cargo test
cargo clippy --all-targets
```

実結果：

- 注釈読取、フラグ別編集可否、worker伝達、古い結果の拒否、Quadヒット、候補方針：成功
- 保存した外部作成相当Highlightのxref、geometry、コメント、色、透明度の再読込：成功
- 同一文書・同一revisionで再列挙したxrefの一致：成功
- 全体：150 passed、0 failed、1 ignored
- Clippy：終了コード0。次フェーズでUI入口へ接続するヒットテスト／候補決定の未使用警告4件のみ

読取専用ファイルの文書全体編集不可判定は既存テスト、注釈個別の`Locked`／`LockedContents`は追加テストで確認した。実在する署名・暗号化権限制限PDF、外部ソフト固有の外観ストリームはfixtureがないため、最終時点でも未確認ならその旨を残す。

## 5. 右クリックメニュー、ダブルクリック入口、右端固定オーバーレイ

### 入力経路の修正前失敗

注釈Quad上でSecondaryボタンを押下・解放するegui入力を再現したところ、ページの`Sense::click_and_drag()`から得た`Response::secondary_clicked()`だけでは候補が設定されなかった。

```text
cargo test secondary_click_targets_annotation_without_starting_selection -- --nocapture
left: None
right: Some(0)
```

ページ入力はPrimaryドラッグ選択も同じResponseで扱うため、Senseを別widgetへ分割せず、raw pointerのSecondary解放位置が当該ページ矩形内かを確認して同じPopup IDを開くようにした。右クリック位置が選択帯にも注釈Quadにも含まれない場合はPopupを閉じる。

### 変更

- ページの画面座標からページ座標への変換後、現在選択の表示Quadと注釈Quadを別々にヒット判定する。
- 選択上または注釈上だけで、「コピー」「ハイライト注釈を作成」「注釈を編集」「注釈を削除」を持つコンテキストメニューを開く。
- 作成不可、注釈個別の編集不可／削除不可は項目を無効化する。右クリックはPrimaryドラッグ状態を開始せず、既存選択を変更しない。
- 複数注釈はxref順で自動決定せず、色、コメント先頭、xrefを含むサブメニュー行を全件表示する。候補表示モデルはbackend snapshotと安定ID操作要求の間に分離した。
- 注釈ダブルクリックは同じ0件／1件／複数件方針を使い、1件は直接編集、複数件は非モーダルな候補一覧を開く。ダブルクリックをテキストドラッグとして確定しない。
- 編集状態は文書ID、revision、xref、読取時の値、編集バッファ、編集可否、dirty判定を保持する。選択本文はバッファへ入れない。
- コメントを上、色を下に置き、複数行入力、プリセット5色、任意色picker、保存、明示的破棄、削除の操作入口を配置した。読めない既存色は`None`のまま保持し、pickerを開いただけでは推測色を設定しない。
- 配置は`AnnotationOverlayPlacement::RightEdge`と純粋な矩形計算へ分離し、編集フォームや更新要求は右端座標を知らない。foreground `Area`として中央Panelの後に描くため、開閉はPanel layout、FitWidth／FitPage、タイル世代へ参加しない。
- 狭い中央領域では左右12論理pt内に縮め、縦方向は最大480論理ptの内部ScrollAreaにする。左サイドバーとは独立して同時表示できる。
- オーバーレイ上ではAreaが入力を所有する。単一ページのraw wheel端遷移にもオーバーレイ矩形を明示的な除外領域として渡した。
- 未保存バッファがある状態で別注釈を選んでも切り替えず、オーバーレイ内へ保存または破棄を促す。別revisionになったバッファはstaleとして入力・保存・削除を無効にし、明示的に破棄できる状態を維持する。
- コメント欄フォーカス中は`Ctrl+C/V/X/Z`などをTextEditへ渡す。Escapeは1回目でコメント欄のフォーカスを外し、未変更なら次のEscapeで閉じ、変更済みなら無言破棄せず案内する。

このフェーズではUI入口と編集バッファまでを接続した。保存・削除ボタンからMuPDFを変更する処理は次フェーズへ分離しており、このコミット単独では未反映であることをオーバーレイ内に明示する。最終成果ではこの暫定案内を残さず、stable ID更新・削除へ接続する。

### 自動検証

```text
cargo test annotation -- --nocapture
cargo test
cargo clippy --all-targets
```

実結果：

- 右クリックで注釈候補を設定し、ドラッグ選択を開始しない：成功
- 選択帯上の右クリックで選択操作だけを有効化する：成功
- 単一注釈のダブルクリックで安定IDを返し、選択を確定しない：成功
- 重複注釈のダブルクリックで2候補を保持する：成功
- 候補名、読取不能色の非推測、狭い領域内の配置：成功
- 全体：157 passed、0 failed、1 ignored
- Clippy：警告なし

実際のコンテキストメニュー操作、IME、foreground Areaによる背後入力遮断、開閉前後の倍率・スクロール位置は最終実機受入で確認する。

## 6. コメント／色更新、削除、Undo、dirty、revision、保存

### 修正前の状態と原因

フェーズ5時点では、オーバーレイの保存・削除ボタンは編集バッファを保持したまま「まだPDFへ反映されていない」と表示する入口だけだった。backendの編集履歴も`CreateHighlight`だけであり、dirty判定は未保存の新規Highlight件数、Undoは作成したxrefの削除だけを扱っていた。

このまま既存注釈を更新すると、次が欠ける状態だった。

- コメント／色のworkerコマンドとrevision指定。
- 削除対象のstable ID検証。
- 外部注釈を削除した後、外観ストリームや未モデル化メタデータを推測せずに戻すUndo情報。
- 更新・削除を含むdirtyと保存後検証。
- 同じ注釈へ複数回変更した場合の最終状態検証。

### 変更

- `AnnotationUpdateRequest`と`AnnotationDeleteRequest`へ文書内のページ＋xref、expected revision、実際に変更したフィールドだけを格納した。
- 更新・削除前に、文書全体の編集可否、ページ範囲、正のxref、Highlight種別、個別の`ReadOnly`／`Locked`／`LockedContents`を再検証する。UIキャッシュの可否だけを信用しない。
- コメントはPDF注釈のContents、色はGray／RGB／CMYKへ設定する。色変更を要求しない場合は既存値を読み書きせず、透明度も変更しない。
- コメントと色を一回の保存操作で変更した場合は一つの`EditAction::UpdateAnnotation`にする。
- backendの未保存履歴を作成・更新・削除共通のLIFOログへ変更し、dirtyをログの有無から判定する。
- 作成Undoは従来どおり正確な作成xrefだけを削除する。更新・削除Undoは変更直前のPDFをMuPDFでメモリへシリアライズし、そのsnapshotを復元する。外部注釈の外観ストリーム、作者・日時などアプリがモデル化していない情報を推測再作成しないためである。
- snapshot復元後の文書はpath associationを持たないため、次回保存は既存の検証付きfull rewriteへ送る。incremental saveへ黙って流さない。
- LIFO最上位でないUndo、保存で履歴を破棄した後のUndo、別ページ・不存在xref・古いrevisionを拒否する。
- 保存後はHighlight総数、ページ数に加え、最終xref、Quad、Contents、色、透明度、削除対象の不在を再オープンしたPDFで検証する。同じ注釈の履歴が複数ある場合は、IDごとの最後の期待状態だけを最終PDFと比較する。
- workerの更新・削除はforeground処理とし、成功したstable actionを既存のタブ別Undo履歴へ積んでから新しい`DocumentInfo`を返す。
- UIは送信直後からpending editとしてdirty表示し、成功時に編集オーバーレイを閉じる。失敗時はbufferを残して再操作可能にし、worker切断時もin-flight状態を解除する。
- コンテキストメニューの削除とフォーム下部の削除を同じ`DeleteAnnotation`コマンドへ接続した。
- `Ctrl+S`は開いている編集bufferがdirtyなら注釈更新を先に実行する。コメント欄フォーカス中の`Ctrl+Z`はTextEditを優先し、dirty bufferがある状態のPDF Undoは無効化して案内する。
- dirty bufferまたは注釈処理中に、別注釈選択、タブを閉じる、全タブを閉じる、ウィンドウを閉じる操作を行っても無言破棄しない。対象タブと右端オーバーレイを表示し、保存または明示的破棄を促す。タブのdirtyマーカーにも未送信bufferを含める。
- 保存による再オープンでは、cleanなものも含め旧revisionの編集UIを閉じる。文書変更後に古いxref bufferを適用しない。

Undo snapshotは未モデル化情報を保つ代わりに、更新・削除の未保存履歴1件ごとに現在のPDF全体と同程度のメモリを一時保持する。大きいPDFでの連続編集は最終性能確認の注意点として残す。件数制限や不完全な注釈再作成fallbackは、仕様にないため追加していない。

### 自動検証

```text
cargo test annotation -- --nocapture
cargo test update -- --nocapture
cargo test undo -- --nocapture
cargo test
cargo clippy --all-targets
```

実結果：

- 外部作成相当Highlightの日本語・改行コメントとRGB色更新：成功
- 更新前後でxref、Quad、透明度を維持：成功
- プリセット5色と任意RGBを明示的patchへ変換：成功
- コメント＋色更新を一つのUndoで元値へ復元：成功
- 2件中1件の削除、他注釈非変更、削除Undoで同じxref・コメント・透明度を復元：成功
- 更新と削除を保存し、再オープン後に最終コメント・色・削除不在を確認：成功
- 同一注釈の複数更新は最後の状態で保存検証：成功
- Locked Contents、Locked annotation、stale revision、不存在xrefを拒否し、revision 0・cleanを維持：成功
- LIFOでないUndoを拒否し、履歴と注釈件数を維持：成功
- worker更新コマンドからstable actionとdirty revision 1を返す：成功
- 全体：165 passed、0 failed、1 ignored
- Clippy：警告なし

外部ソフト固有の外観ストリームがUndo前後で視覚的に完全一致すること、非常に大きいPDFでのsnapshotメモリ量、外部ビューアーでの表示は最終受入の未確認候補として扱う。

## 7. 全体回帰検証と実機受入

### 最終自動検証

最終差分を含むDev Container上で、指示書指定のコマンドを再実行した。

```text
docker compose -f .devcontainer/compose.base.yml exec workspace cargo fmt --check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo test
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check --release
docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets
```

実結果：

- `cargo fmt --check`：終了コード0
- `cargo check`：終了コード0
- `cargo test`：165 passed、0 failed、1 ignored
- `cargo check --release`：終了コード0
- `cargo clippy --all-targets`：終了コード0、警告なし

Windows GNU向けは、`AGENTS.md`指定のdebug／releaseコマンドでcross buildし、次の出力を得た。

- `dist/lunapdf-debug.exe`：276,819,594 bytes
- `dist/lunapdf-release.exe`：24,157,696 bytes

### 保存互換性用fixture

既存のignored受入テストを、同一ページに重なるHighlightを2件作成し、一方を削除し、残る一方へ日本語・改行を含むコメントとRGB色を設定して保存する内容へ拡張した。最終注釈件数が1件であることも保存処理の再オープン検証で確認する。

```text
docker compose -f .devcontainer/compose.base.yml exec workspace sh -c \
  "LUNAPDF_ACCEPTANCE_OUTPUT=/workspace/dist/lunapdf-annotation-acceptance.pdf \
  cargo test exports_highlight_fixture_for_external_viewers -- --ignored --nocapture"
docker compose -f .devcontainer/compose.base.yml exec workspace sh -c \
  "qpdf --check /workspace/dist/lunapdf-annotation-acceptance.pdf"
```

実結果：

- ignored受入テスト：1 passed、0 failed
- 出力：`dist/lunapdf-annotation-acceptance.pdf`、9,786 bytes
- qpdf：PDF 1.7、非暗号化、構文・stream encoding errorなし

受入PDFは`dist/`へ別ファイルとして生成した。元PDF fixture、ユーザー変更中の`assets/icons/fit-width.svg`、既存PDFは変更していない。再実行時は、既存出力を上書きしないテスト契約により一度`AlreadyExists`となったため、前回このテストが生成した同じ受入PDFだけを削除して再生成した。

### Windows実機で確認できた範囲

Windowsホスト上で`dist/lunapdf-release.exe`を起動し、LunaPDFのメインウィンドウとネイティブなPDF選択ダイアログが開くことまでは確認した。

ただし、画面取得は次のWindows APIエラーで2回とも失敗した。

```text
SetIsBorderRequired failed: インターフェイスがサポートされていません (0x80004002)
```

アクセシビリティ経由ではLunaPDF本体がタイトルバー以外を公開せず、ネイティブファイル選択欄への入力も`element 52 is not available in cached app state`で停止した。このため、受入PDFをLunaPDFまたは起動済みのSumatraPDFへ読み込ませた目視確認は実施できていない。検証用に起動したLunaPDFプロセスは終了済みである。

以上から、次は自動テストまたは構造検査で確認済みである。

- タブ幅、閉じる領域、Xを構成する2線の長さ・太さ
- 3桁を最低幅とするページ番号欄
- raw wheelイベント単位のFitWidth端遷移、文書端、Ctrl、水平入力、領域外入力
- クリック許容距離とドラッグ開始、1 glyph選択、行単位表示geometry、コピー／保存Quadの論理範囲
- 注釈のstable ID、重複候補、右クリック／ダブルクリック方針
- オーバーレイ矩形が中央領域内に収まり、配置計算と編集状態が分離されていること
- コメント／色更新、削除、LIFO Undo、dirty、revision拒否、保存後再読込
- 受入PDFの構文

次は実機で未確認である。

- 100%／125%／150%／200% DPIでのX、長い日本語タブ、多数タブ、`999 / 999`表示
- ノッチ式マウス、高精度ホイール、トラックパッドによる操作感
- 単純クリック、短いドラッグ、複数行選択、コピー／作成対象の画面上の一致
- コンテキストメニュー、複数注釈サブメニュー、ダブルクリック、IME
- オーバーレイの右端表示、背後入力遮断、開閉前後の倍率・ページ・スクロール位置、白紙待ちや再描画の有無
- 外部PDFビューアーでのコメント、色、削除結果の表示
- 外部ソフト固有の外観ストリームを持つ注釈のUndo前後の視覚的一致
- 署名・暗号化権限制限PDF、非常に大きいPDFの連続編集時のsnapshotメモリ量

### 完了判定

ソース変更、自動回帰、debug／release build、保存後再読込、PDF構文検査は完了した。指示書の完了条件18「保存後も外部PDFビューアーで利用できる」と、画面・DPI・入力機器に依存する手動受入は、上記GUI検証環境の制約により完了と判定しない。
