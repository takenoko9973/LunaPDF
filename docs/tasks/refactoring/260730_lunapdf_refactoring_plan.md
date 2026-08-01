# LunaPDF 大規模リファクタリング計画

## 1. 目的

LunaPDF の公開動作、PDF 保存の安全性、セッション形式、非同期処理の整合性を維持しながら、次を達成する。

- Rust ソース内の英語コメントと doc comment を日本語化する。
- `src/app.rs` に集中した責務を、状態所有関係が分かる private module へ分割する。
- 重複している注釈属性の抽出・検証処理を一箇所へ集約する。
- Clippy 抑止や release ビルド専用の dead-code 許可を、構造上不要にする。
- フォールバック、黙示的スキップ、エラー無視を用途別に監査し、無意味なものだけを除去する。
- 各段階を独立して検証・コミットできる変更単位にする。

## 2. 調査結果

### 2.1 構成

- 起動入口は `src/main.rs` の `main` で、`PrototypeApp` と `SessionStore` を生成する。
- `src/app.rs` は約 8,500 行あり、アプリ状態、タブ、非同期イベント配送、UI 描画、検索、注釈、描画要求、終了処理、約 2,200 行のテストを同居させている。
- `src/pdf/worker.rs` は MuPDF を単一ワーカースレッドへ隔離し、優先度付き command/event 契約を提供する。
- `src/pdf/mupdf_backend.rs` は PDF 操作、注釈、保存、原子的置換、保存後検証を担う。
- `src/domain/session.rs` と `src/persistence/session_store.rs` が schema 1 の `session.json` 契約を担う。
- unit test は 223 件あり、基準実行は 221 件成功、2 件 ignored、失敗 0 件だった。
- 基準 Clippy は `cargo clippy --all-targets -- -D warnings` で警告 0 件だった。

### 2.2 守るべき契約

1. 明示された CLI の PDF パスは、保存セッションの復元より優先する。
2. MuPDF の値は PDF ワーカーだけが所有し、UI スレッドへ移動しない。
3. 非同期結果は document ID、generation、revision、ページ、要求キーが一致する場合だけ反映する。
4. 描画優先度は foreground、current viewport、next viewport、previous viewport、text snapshot、background の順序を維持する。
5. PDF 保存は外部変更を検出し、同一ディレクトリの一時 PDF を検証してから原子的に置換する。
6. 増分保存不能時の full rewrite は安全契約であり、不要なフォールバックとして削除しない。
7. annotation Contents の欠損は、現在のアプリ所有 `String` 境界で空文字へ正規化する。
8. セッション schema、絶対・字句正規化済みパス、選択タブ、zoom、アンカー、最近色の検証を変えない。
9. MuPDF と一致する座標丸め、viewport 限定 tile 列挙、GPU 192 MiB と thumbnail 32 MiB の上限を変えない。
10. Windows 印刷では `StartDoc` 後の失敗時に `AbortDoc` を行い、ページ範囲、revision、8 MiB strip 上限を維持する。

### 2.3 フォールバック監査

検出した既存処理のうち、現時点で無意味と断定できるフォールバックはなかった。したがって、構文だけを根拠に一律削除しない。

| 処理 | 判定 | 理由 |
|---|---|---|
| `annotation.contents()?.unwrap_or_default()` | 保持して集約 | PDF の Contents 欠損をアプリの `String` 契約へ正規化する。 |
| tile 計算の `.ok()?` と空要求 | 保持 | 座標・サイズが表現不能な要求を MuPDF へ送らない。 |
| event 送信の `let _ = ...send(...)` | 保持 | タブを閉じて receiver が消滅した後は、追加エラーを生成せずワーカーを終了可能にする。 |
| worker の切断時 `.ok()?` | 保持 | 全 sender 切断時に worker loop を終了する。 |
| full rewrite 保存 | 保持 | 増分保存不能、undo snapshot 復元後、redaction を安全に保存する正規経路である。 |
| 非同期初期状態の page count 0、中央 anchor 0.5 | 保持 | `DocumentInfo` 到着前と復元値欠損時の状態契約である。 |

削除対象は、用途のない互換分岐や安全性に寄与しない既定値が実装中の call site 確認で見つかった場合に限る。新しい fallback、互換 wrapper、自動推測ロジックは追加しない。

## 3. 変更計画

### フェーズ1: コメントの日本語化

対象:

- `src/**/*.rs` の `//!`、`///`、`//`

実施内容:

- 英語の説明文を日本語化する。
- MuPDF、egui、Windows API、xref、generation、revision などの識別語は保持する。
- 数値、単位、測定由来の根拠、保存・座標・非同期処理の「なぜ」を省略しない。
- UI 文言、エラー文字列、テスト関数名、型・関数・変数名はコメントではないため変更しない。

検証:

- 英語コメントの静的再検索
- `cargo fmt --check`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`

コミット:

- `docs: ソースコメントを日本語化`

### フェーズ2: `app` の private module 分割

予定構成:

- `src/app.rs`: `PrototypeApp` の状態所有と eframe 入口
- `src/app/events.rs`: `DocumentEvent` の受信と状態遷移
- `src/app/rendering.rs`: tile/cache key、要求列挙、provisional tile、描画補助
- `src/app/navigation.rs`: 表示モード、表示位置、wheel、autoscroll、座標復元
- `src/app/search.rs`: 検索順序、cursor、結果整合性、移動先計算
- `src/app/tests.rs`: app 層の回帰テスト

制約:

- まずコード移動と可視性の最小変更だけを行い、同じコミットでロジックを書き換えない。
- `pub(crate)` を増やさず、`pub(super)` も親 module から必要な項目だけに限定する。
- generation/revision 判定、イベント処理順、ScrollArea 座標系、キャッシュキーを変更しない。
- 連続表示と単一ページ表示は座標系と遷移規則が違うため、見かけが似た処理を無理に統合しない。

検証:

- `cargo fmt --check`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`

コミット:

- `refactor: アプリ責務を内部モジュールへ分割`

### フェーズ3: PDF 注釈境界の重複除去

対象:

- `src/pdf/mupdf_backend.rs`

実施内容:

- Quad、Contents、Color、opacity から `ExpectedAnnotationState` を構築する処理を helper へ集約する。
- 欠損 Contents を空文字へ正規化する処理を、一つの名前付き境界関数へ集約する。
- 作成直後、更新直後、保存後検証、sidebar summary が同じ変換規則を使うようにする。
- missing Contents、Gray/CMYK、Quad・opacity 許容誤差の回帰テストを補う。

制約:

- xref、ページ、annotation type の検証順を変えない。
- `PDF_COORDINATE_TOLERANCE` と `PDF_PROPERTY_TOLERANCE` を変えない。
- 保存方式、rollback、temporary file cleanup、原子的置換へ触れない。

検証:

- 注釈・保存関連 unit test
- 全 `cargo test`
- Clippy

コミット:

- `refactor: 注釈属性の抽出と検証を集約`

### フェーズ4: 抑止属性と印刷引数群の構造化

対象:

- `src/pdf/windows_print.rs`
- `src/render/cache.rs`
- `src/domain/document.rs`
- 必要な構築・参照 call site

実施内容:

- `print_selected_pages` の引数を印刷ジョブ文脈へ束ね、`#[allow(clippy::too_many_arguments)]` を削除する。
- debug 表示だけに必要なメトリクスと accessor は、release で dead code を許可する代わりに条件コンパイル境界を明示する。
- release で収集・転送する必要がない診断値は、構築側も同じ条件で除外する。

制約:

- Windows GDI の page/document lifecycle と `AbortDoc` 条件を変えない。
- 印刷 bitmap の top-down DIB、RGBA→BGRA、strip 上限を変えない。
- debug/test の測定項目と既存テストを維持する。

検証:

- Linux の debug test と Clippy
- Windows GNU debug cross build
- release `cargo check` または Windows GNU release cross build

コミット:

- `refactor: 診断状態と印刷文脈を明示化`

### フェーズ5: 最終監査

確認項目:

- 要求外 fallback、互換 wrapper、自動推測の追加がない。
- `allow(dead_code)`、`allow(clippy::too_many_arguments)` が残っていない。
- 英語の説明コメントが残っていない。
- 長い条件式と重複変換が、契約を隠す形で残っていない。
- session JSON、CLI、PDF 保存、worker command/event の公開動作に差分がない。
- `target/`、`dist/`、`assets/icons/`、`packaging/`、依存定義を意図せず変更していない。

検証:

```powershell
docker compose -f .devcontainer/compose.base.yml exec workspace cargo fmt --check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo check
docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets -- -D warnings
docker compose -f .devcontainer/compose.base.yml exec workspace cargo test
docker compose -f .devcontainer/compose.base.yml exec workspace sh -c "cargo build --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/debug/lunapdf.exe /workspace/dist/lunapdf-debug.exe"
```

GUI または性能受入が必要になった場合は、実行前に `.codex/validation-lessons.md` を読む。

## 4. コミット運用

- 作業計画 Markdown と `docs/tasks/README.md` の索引更新は、ユーザー指示により実装コミットへ含めない。
- 各フェーズでは対象ソースとテストだけを明示的に stage する。
- コミット前に、そのフェーズの検証結果と `git diff --check` を確認する。
- コミットメッセージは `<type>: 日本語の要約` 形式にする。
- 後続フェーズで前段の設計判断を修正した場合は、この計画を更新する。

## 5. 触らない領域

- `target/`
- `dist/`
- `assets/icons/`
- `packaging/`
- `build-support/`
- `Cargo.toml` と `Cargo.lock`
- `.devcontainer/compose.base.yml`

依存追加、公開 API 変更、session schema 変更、配布形式変更は本計画の対象外とする。

## 6. 実施結果

### 6.1 完了した変更

| コミット | 内容 |
|---|---|
| `763967d` | Rust ソースの説明コメントと doc comment を日本語化した。 |
| `a256b10` | `src/app.rs` からイベント、ナビゲーション、描画、検索、テストを private module へ分割した。 |
| `596c2d3` | 注釈の Contents・Color・期待状態の抽出を共通化し、欠損 Contents の実 PDF 回帰テストを追加した。 |
| `12bc946` | 追加監査で見つかった PowerShell と Dev Container の英語コメントを日本語化した。 |
| `2ca935c` | 印刷ジョブ文脈を構造化し、release 専用の dead-code 許可と未使用引数代入を条件コンパイルへ置き換えた。 |
| `18f6b8b` | Gray・CMYK・数値・Quad の注釈比較許容差に回帰テストを追加した。 |
| `3df694f` | 統合レビューを受け、CMYK 全4成分と Quad 全8座標の拒否条件を個別に固定した。 |

`src/app.rs` は約 8,500 行から約 4,500 行となり、移動した private 実装は
`src/app/events.rs`、`navigation.rs`、`rendering.rs`、`search.rs`、`tests.rs` に分かれた。
公開可視性、関数・型・app 層テストの集合は分割前後で維持した。

注釈の欠損 Contents は、無意味な既定値ではなく PDF の省略可能属性をアプリの
`String` 契約へ変換する境界である。この正規化は削除せず `annotation_contents` に集約し、
注釈一覧、編集直後状態、保存後検証で同じ規則を使うようにした。

### 6.2 最終監査の判断

- `#[allow(dead_code)]` と `#[allow(clippy::too_many_arguments)]` はソースから除去した。
- release で表示しない描画時間、開く時間、倍率、物理メモリは、値の計測・構造体フィールド・
  accessor を同じ `cfg(debug_assertions)` 境界で除外した。
- release テストでも保存契約を検証する注釈数、保存方式、キャッシュ使用量、空状態だけは
  `cfg(any(debug_assertions, test))` で保持した。
- `let _ = root_ui` は未使用引数名へ置き換えて分岐を削除した。
- worker の event 送信失敗、全 sender 切断、表現不能な tile、full rewrite 保存などは、
  調査で契約上必要と確認できたため削除しなかった。
- worker の本番・テスト受信処理の類似、連続表示と単一ページ表示の類似、一時 `Vec` と
  最近色の clone は、順序・座標系・借用境界を変える危険が効果を上回るため変更しなかった。
- session schema、CLI、依存、PDF 保存方式、worker command/event、配布形式は変更していない。

### 6.3 検証結果

- debug unit test: 225 passed、2 ignored。
- release unit test: 223 passed、2 ignored。
- debug / release の Clippy `--all-targets -- -D warnings`: 成功。
- Linux release `cargo check`: 成功。
- Windows GNU debug / release cross build: 成功。
- 独立レビューでは、途中で release-test 専用 dead-code 警告を検出した。表示専用値と
  テスト契約値の `cfg` を分けて解消し、再レビューで actionable finding なしとなった。
- 統合レビューの Low 指摘だった比較テストの条件不足も、全成分・全座標の表形式ケースで解消した。

実プリンターを用いる Windows 印刷受入と、出力を生成する ignored 2 テストは実行していない。
今回の変更は UI レイアウトや性能目標を変更していないため、GUI・性能受入は対象外とした。
