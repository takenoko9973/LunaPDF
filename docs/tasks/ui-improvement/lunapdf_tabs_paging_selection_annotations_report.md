# LunaPDF タブ・ページ操作・テキスト選択・注釈編集 調査・作業報告

更新日：2026-07-24

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
