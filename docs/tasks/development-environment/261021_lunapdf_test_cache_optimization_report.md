# LunaPDF Docker/Cargo キャッシュ改善・実測報告

現在のDev Container操作は[開発環境の運用手順](../../development-workflow.md)を参照する。この報告は当日の実行結果と測定値を保持する履歴資料である。

2026-10-21。[実施指示書](261021_lunapdf_codex_test_cache_optimization.md)に対する、検証方針・ヘッドレステスト・Docker/Cargo キャッシュの全体報告。

## 全体の変更とテスト判断

変更ファイルは `AGENTS.md`、`.codex/validation-lessons.md`、`docs/tasks/README.md`、本報告、指示書、[検証マトリクス](verification-matrix.md)、`src/app/tests.rs`、`.devcontainer/compose.base.yml`、`.devcontainer/Dockerfile`、`.devcontainer/overlays/cache-parallel.compose.yml`。

方針文書では、変更に関連する最小の自動検証から始めること、実 GUI は明示依頼時に限ること、実行済み・失敗・環境制約・実機未検証を分けることを定めた。過去の GUI 検証記録は保持した。方針変更は `d85246e`、テスト変更は `bbd38b4` で受入済み。

新規テスト `single_page_view_line_wheel_navigates_pages_and_respects_boundaries` は、実際の `central_panel` に `egui::RawInput` の line wheel を入力し、前後のページ移動、先頭・末尾の境界、Ctrl を伴う場合にページ移動しないこと、移動時の anchor を確認する。OS window は起動しない。テスト担当が既存 Dev Container で `cargo test --bin lunapdf single_page_view_line_wheel_navigates_pages_and_respects_boundaries` を実行し、1 passed / 365 filtered、`cargo fmt --check` と `git diff --check` も合格。既存実装への回帰保護追加であり、製品コードを変更する RED 修正ではない。

候補の [egui_kittest 0.35.0](https://docs.rs/egui_kittest/0.35.0/egui_kittest/) を評価したが、今回の入力と状態の観測は既存 `egui::Context::run_ui` で満たせたため依存を追加しなかった。widget 探索 framework や公開テスト hook も追加していない。

既存検索テストには Enter/Shift+Enter による navigation、IME 確定とゼロ件、clear と遅延結果拒否、generation/revision による stale 結果拒否がある。PDF backend には一時 PDF を作って保存・再読込し、注釈や検索結果を確認する既存テストがあるため重複追加しなかった。今回それらの全件再実行はしていない。ページキーの実装は PageUp/PageDown であり、矢印 navigation は未実装だったため、新機能やその成功テストへ拡張しなかった。GUI 固有の IME、OS dialog、GPU 描画、実フォーカスなどは実機未検証。

## 採用した構成

共通 image を `lunapdf_devcontainer-workspace:latest` に明示した。Compose project 名は固定しない。registry は既存実名 `lunapdf_devcontainer_cargo-registry` を指定し、異なる project でも再利用する。`external` は指定しないため、新規環境では自動作成できる。既存 volume のコピー・移行・削除は不要。他 project が作成した共有 volume への警告は出るが、今回の実行では正常に再利用できた。[Compose volume の name](https://docs.docker.com/reference/compose-file/volumes/#name)

通常の target は project ごとの `/workspace/target` のまま。通常開発の incremental、wrapper、Linux/Windows GNU の成果物区分、Windows MSVC、Windows 上のソース bind、WSLg/PlantUML overlay は変更していない。製品コード・公開 API・保存形式・version も変更していない。

並列時だけ `overlays/cache-parallel.compose.yml` を追加する。独立した Windows 作業ディレクトリーと Compose project を使い、各 target volume を分離する。同じ container 内絶対パス `/workspace`・`/workspace/target`、同じ compiler・flags・target・profile で共有 sccache volume を使う。Debian 公式 package の sccache を Dockerfile 末尾で追加し、この overlay のみ `RUSTC_WRAPPER=/usr/bin/sccache` と `CARGO_INCREMENTAL=0` を設定する。

rlib の再コンパイル削減は実測できたが、最終リンク、test binary、proc-macro、native build まで共有できるという意味ではない。今回のサンプルでは check の処理も sccache 0.10.0 の集計対象になったが、別 target の test binary は再生成された。キャッシュ可否は compiler invocation に依存する。[sccache Rust の条件と制限](https://github.com/mozilla/sccache/blob/main/docs/Rust.md)、[Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html)

## 運用手順

既存 workspace での逐次検証は同じ project への `exec` を優先する。今回の稼働 project 名は `lunapdf_devcontainer` だった。

```powershell
docker compose -p lunapdf_devcontainer -f .devcontainer/compose.base.yml exec workspace cargo check --locked
docker compose -p lunapdf_devcontainer -f .devcontainer/compose.base.yml exec workspace cargo test --locked search_navigation_visits_each_logical_match_and_wraps
```

Dockerfile、その入力、使用する overlay の build 設定を変更した場合は、既存 image を黙って使い続けず、作業前に明示的に再 build する。今回追加した sccache を使う場合にも最初の build が必要。VS Code では `Dev Containers: Rebuild Container` を使う。使用中の他人の container は再作成しない。build だけでは稼働 container は更新されない。

```powershell
docker compose -p lunapdf_devcontainer -f .devcontainer/compose.base.yml build workspace
```

同じ入力の image が存在すると確認済みなら task ごとの build は不要。常駐起動は `up -d --no-build --pull never workspace`、一時検証は `run --rm --no-deps --pull never workspace cargo check --locked` を使う。`run` に `--no-build` はない。`image` と `build` の併記だけに依存せず、pull/build を明示的に制御する。[Compose build と image の関係](https://docs.docker.com/reference/compose-file/build/)

並列用の例は以下。各 task の独立した checkout 内で実行し、task ごとに異なる `-p` を指定する。同じ source tree を並列編集してよいという許可にはならない。通常運用や同じ project の target に overlay を付け外ししない。

```powershell
docker compose -p lunapdf_task_a -f .devcontainer/compose.base.yml -f .devcontainer/overlays/cache-parallel.compose.yml run --rm --no-deps --pull never workspace cargo test --locked search_navigation_visits_each_logical_match_and_wraps
```

PlantUML を同時に起動する project では既存の `PLANTUML_PORT` で port を分離する。検証だけなら `--no-deps` で server を追加起動しない。sccache 統計は各 container の server ごとに取得し、並列中の統計を単一実行の hit 率として混ぜない。

## 実測

### 既存 LunaPDF の baseline

root の変更前測定。既存 container の Rust 1.98.0、既存 target、registry を使用。`CARGO_HOME=/usr/local/cargo`、`CARGO_TARGET_DIR` と `RUSTC_WRAPPER` は未設定。

| コマンド | 1 回目 | 2 回目 | コンパイル対象 |
| --- | ---: | ---: | --- |
| `cargo check --locked` | 2.21 秒 | 0.25 秒 | 1 回目 LunaPDF のみ、2 回目なし |
| 上記 search navigation test | 7.22 秒 | 0.26 秒 | 1 回目 LunaPDF のみ、2 回目なし。1 test 合格 |

既存 target は約 8.567 GB、registry 約 776 MB。すでに逐次キャッシュは機能しており、この高速性を今回新たに得た改善とは数えない。

テスト担当の作業完了後、同じ既存 container/target で同じコマンドを各 2 回実行した。Docker exec を含む wall time は check 1.082 / 0.357 秒、対象 test 0.382 / 0.384 秒。Cargo 表示はそれぞれ 0.97 / 0.26 秒、0.27 / 0.27 秒だった。check 1 回目は LunaPDF のみ Checking、残りは再コンパイルなし。test は各 1 passed / 365 filtered。テスト担当の直前実行で test binary が既に更新されているため、baseline の初回 compile 時間との差を構成変更による速度改善とは評価しない。依存全体の不要な再コンパイルが発生しないことを確認した。

### 専用 Rust サンプルでの比較

一時 container の専用 sample（2500 個の小関数を持つ rlib と test 1 件、依存なし）で比較した。既存製品ソース・target は変更していない。wall time は Python monotonic で cargo のプロセス全体を計測。Debian trixie 公式 `sccache 0.10.0-4` を一時導入して採否を判断した。

| 同一 sample/Task A | 通常 incremental=1 | sccache/incremental=0 |
| --- | ---: | ---: |
| check 1 回目 / 2 回目 | 0.429 / 0.015 秒 | 0.520 / 0.014 秒 |
| test 1 回目 / 2 回目 | 1.208 / 0.082 秒 | 1.015 / 0.072 秒 |
| 1 関数の範囲値変更後 check | 0.188 秒 | 0.366 秒 |

通常開発への incremental 無効化は不採用。軽微変更では今回の sample で遅かった。別々の絶対 target パスでは 0 hit / 5 miss となり、共有 sccache を置くだけでは十分でなかった。同じ絶対パスを保ち target の内容だけを分離すると、test A 1.023 秒 → B 0.509 秒、1 hit / 1 miss となった。

さらに実際に別 Windows source directory をそれぞれ `/workspace` へ bind し、別 Compose project・別 target volume・共有 registry/sccache・同一 image で検証した。Cargo 表示時間は A test 1.12 秒（1 miss）、B test 0.62 秒（1 hit / 0 miss）。両方 1 test 合格。各 container の統計をリセットして別々に集計した。sample の target 各約 20 MB、共有 sccache 約 1.3 MB。sccache の既定容量上限は 10 GiB。

同じ A/B を 2 タスク同時実行し、check は 0.55 / 0.19 秒、続く test は各 0.02 秒、両方合格。Cargo の `Waiting for file lock` は出ず、各 workspace に書いた `task-a` / `task-b` はそれぞれの workspace にのみ存在した。registry の lock が常にゼロという保証ではない。これは sample の観測結果であり、LunaPDF 全依存の cold build や独立 target での LunaPDF test 高速化は未測定。

### 本 repo の実依存での追加確認

sample の効果だけで採用を決めないため、本 repo の source bind と Cargo.lock、Rust 1.98.0 をそのまま使い、`cargo build --locked --offline -p egui` を A/B の独立 target で実行した。前節の sample とは別集計。既存本体 target は使わず、同じ image・`/workspace`・profile/flags・registry/sccache、別 project target volume とした。

| egui 0.35.0 とその依存 | Task A | Task B |
| --- | ---: | ---: |
| Cargo 表示時間 | 17.61 秒 | 2.78 秒 |
| Compose 起動・統計取得も含む wall time | 18.453 秒 | 3.611 秒 |
| sccache hit / miss | 0 / 55 | 55 / 0 |
| sccache 非キャッシュ呼び出し | 18 | 18 |

両方 Cargo build は正常終了。非キャッシュ理由は crate-type 13、`-` 3、missing input 2。統計の `Compilation failures` は両方 2 で、Cargo 全体の失敗とは区別する。この数値の内部呼び出し原因までは調査していない。proc-macro などの再実行は残り、B の Cargo 表示が `Compiling` でも rustc 出力の一部は cache から復元されていた。

実依存の別 target 再利用でも利益が得られたため並列 overlay を採用した。本体全体・native build・最終リンクの速度改善へ外挿しない。追加測定後の target は各約 474 MB、共有 sccache 約 84 MB（統計表示 83 MiB）。容量 option は追加せず、既定の上限 10 GiB を採用した。これは全依存の必要容量推定ではなく、今回の実依存測定が十分収まり、既存環境の空き容量も約 918 GiB あったことに基づく判断。対象を増やした場合の eviction/使用量は実測で再評価する。

## Docker 資産と検証区分

変更前に LunaPDF image は共通 tag 1 件で、増殖は観測されなかった。既存 tag の ID は `75f83136c6df`、約 6.58 GB。稼働 VS Code container は feature 適用後の別 image ID であり、この差だけで不具合とは判断しない。

共通 image の Dockerfile 全体 build を試したところ、既存の可変 base tag `rust:1-slim-trixie` の新しい層を取得し始めたため中断した。version 固定による回避は行っていない。既存共通 image を FROM とする一時派生 image に同じ apt sccache 層を追加し、上記 Compose 実行を検証した。Dockerfile 全体の完了 build と VS Code Rebuild/Reopen は未実施。通常の稼働 container は変更していない。

一時 image は 1 件追加、計測用 container は `run --rm` と所有する常駐 container 1 件。終了後、一時 image/container/network だけを除去し、image 数と停止 container 数の恒久増分はゼロ。共有 sccache と A/B target の 3 volumes は削除せず残した。sample 測定後は約 43 MB、実依存測定後は `docker system df` で合計約 1.077 GB。registry と既存 target も保持。全 prune、`down -v`、既存資産の削除は行っていない。

`docker system df` の image 合計は測定後も約 13.6 GB。BuildKit は変更前 11.36 GB（shared 11.29 GB）から、測定・base 取得・既存 image 展開後は 18.08 GB（shared 17.87 GB/private 208.1 MB）。測定自体による cache 増加があり、容量削減は実証していない。shared layers を image サイズの単純加算で数えていない。今後の通常反復で task ごとに image を build せず、必要な target だけを保持する運用を明示した。

- 実行して合格: base と WSLg/PlantUML、parallel overlay の `docker compose ... config`、別 project の registry 同名/target 別名解決、一時派生 image の apt 層 build、同 image 再利用の複数実行、専用 sample の check/test/軽微変更/2 タスク並列、最終 diff 検査。
- 実行して失敗: 初回 `compose run --no-build` は非対応 option で拒否。その後、対応する `run --pull never` に修正して合格。
- 環境・検証範囲により未実施: Dockerfile 全体の完了 build、LunaPDF 全依存 cold build、独立 target の本体 sccache 効果、変更 commit の Windows MSVC CI（push 未依頼）。
- 実機未検証: GUI、Windows/GNU EXE、VS Code Reopen。GUI を起動していない。

設定の挙動は Compose 実行で確認したため、設定内容をなぞる恒久テストは追加していない。外部 download は Debian 公式 package manager の範囲に限定し、remote script 実行・権限拡大・秘密情報の追加はない。

## 切り戻しと片付け

parallel overlay の使用をやめれば通常 incremental 開発へ戻る。既存 project の target は変えていない。設定変更を戻しても既存 registry 実名は維持される。Dockerfile を戻した後の image 更新は必要時に明示 build し、使用中 container の再作成は所有者と調整する。cache volume は自動削除しない。今回の `lunapdf_cache_a_cargo-target`、`lunapdf_cache_b_cargo-target`、`lunapdf_devcontainer_sccache` を整理したい場合も、用途・利用者を確認して削除の明示許可を得てから行う。
