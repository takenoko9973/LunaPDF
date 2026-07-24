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

