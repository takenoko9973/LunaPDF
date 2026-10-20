# LunaPDF：Codexによる検証とDocker／Cargoキャッシュの効率化 指示書

## 0. 目的・実施範囲

LunaPDFのCodexによる開発で、(1) 毎回の実GUI操作やComputer Useに頼らない検証体系へ移行し、(2) DockerイメージおよびCargoキャッシュの無駄な重複を抑え、(3) 並列タスクの独立性と既存のWindows／Linux開発手順を維持する。

**この指示書はCodexへの実装依頼である。** 実行前に現状を調査し、その結果に基づき必要最小限の変更を実施すること。現状に存在しない問題を想像して修正しないこと。変更と実測の結果を報告すること。

### 明示的な対象外

- **DockerfileのRustツールチェーン／ベースイメージ／関連ツールのバージョン固定を目的とする変更をしない。** 「固定による再ビルド抑制」は今回の施策ではない。sccacheの導入に必要な最小限のツール追加は、効果を確認した場合に限り許可する。
- **ソースコードをWindows側ファイルシステムからWSL側へ移動しない。** マウント元、Windows上の作業ディレクトリ、Gitワークフローを維持する。
- GUI操作やWindows実機検証を完全に廃止しない。無用な常時実行のみを廃止する。
- 既存Volume、イメージ、コンテナ、ビルドキャッシュの一括削除・`docker system prune`・`docker volume prune`・`docker compose down -v`を自動実行しない。既存キャッシュの破棄は別途許可を得ること。
- PDF閲覧・保存・印刷等の製品仕様、公開API、設定スキーマを今回の最適化のために変更しない。

## 1. 作業前の必須調査（読み取り専用）

最初に`AGENTS.md`、`docs/tasks/README.md`、`.codex/validation-lessons.md`、`.devcontainer/devcontainer.json`、`.devcontainer/compose.base.yml`、オーバーレイ群、`.devcontainer/Dockerfile`、`.cargo/config.toml`、`.github/workflows/ci.yml`、`Cargo.toml`、既存テストを確認する。現行実装では`cargo-target:/workspace/target`と`cargo-registry:/usr/local/cargo/registry`のnamed volumeを使用している。GitHub ActionsにはWindows MSVCでの`cargo test`および`cargo clippy`がある。これらを壊さないこと。

可能な環境では、実行されているComposeのプロジェクト名、生成イメージ、マウント先Volumeと実名、Rust/Cargoのホスト・ターゲット、各タスクの`CARGO_TARGET_DIR`・`CARGO_HOME`・`RUSTC_WRAPPER`、`docker system df -v`、`docker volume ls`、`docker compose ... config`、`docker buildx du`を調査する。Docker CLI等を利用できない場合は「未観測」とし、証拠のない原因を断定しない。

とくに**Dockerイメージ名の増加**、**Dockerビルドレイヤーのキャッシュヒット**、**Cargoの依存ソース・コンパイル成果物のキャッシュヒット**を別々に評価する。異なるイメージ名でもレイヤーを共有でき、逆に同じイメージでもCargo再ビルドは起こり得る。CodexタスクごとのComposeプロジェクト名やbuildコマンドが変化していないか、実ログで調べる。

変更前に、代表的な軽微なRust変更と再実行について、所要時間、Cargoの再コンパイル件数・対象、Volume数・サイズ、イメージ数・実使用容量、Docker/BuildKitキャッシュ、GUI操作回数・実行時間のベースラインを可能な範囲で取得する。必要な測定のためだけに不要なWindows EXEやrelease版を生成しない。

## 2. Codexの検証ポリシーを変更する

`AGENTS.md`を中心に、以下のルールをCodexが通常作業で必ず守るよう明文化する。`validation-lessons.md`の既存の記録を削除・改変して過去のGUI検証を無効扱いにせず、現行方針との関係を明確にする。

1. **最小限の自動検査から開始する。** 変更ファイルに応じて`git diff --check`、`cargo fmt --check`、`cargo check`、対象テスト、必要なら`cargo clippy`を選ぶ。コマンドは既存のDev Container経路を優先する。`cargo check`はビルド不要の型・コンパイル検査であって動作確認ではない、と記録する。
2. 関係のない全テスト・全ターゲット・Windowsクロスビルド・releaseビルド・インストーラー生成は毎回実行しない。影響が広い変更、依存・ビルド設定変更、統合時、受入時には段階的に範囲を広げる。最終的な必要範囲の検証を省略しない。
3. **実GUIの起動・マウス／キーボード自動操作・Computer Use・スクリーンショット／OCR・画素判定を通常の検証手段として用いない。** 検証したい性質を、純粋関数・状態遷移・イベント注入・ヘッドレスUI検証・PDFファイル出力の構造検証に置き換える。
4. GUI実機でしか確認できない項目（Windows固有のIME／キーイベント、OS印刷ダイアログ、ドラッグ＆ドロップのOS統合、GPU描画、DPI／マルチモニター、実フォーカス挙動等）が残る場合、必要性、対象範囲、代替不能な理由、想定する実行方法を報告する。**自律的な実GUI操作は実施せず、ユーザーからその検証の明示指示があった場合にのみ行う。** 未実施項目は「GUI未検証」と記録する。
5. 検証成功を偽装しない。各検証の結果を「実行して合格」「実行して失敗」「環境制約で未実施」「実機未検証」に区別する。ログやテスト件数を捏造しない。
6. 既存テストで確認できる挙動をわざわざComputer Useで再試行しない。過剰なテスト追加や実装詳細の固定によって保守コストを増やさない。

変更の種類に応じた検査表を短くドキュメント化する。例：ロジック修正→対象ユニットテスト、検索UI→ロジック＋ヘッドレスUIテスト、PDF保存→一時PDF＋構造検査、Windows印刷統合→自動検査＋実機未検証表示。実機での受入が必要な場合でも、ユーザーの明示指示なく実行しない。

## 3. GUI非依存テストを整備する

既存の`src/app/tests.rs`、`src/app/navigation.rs`、`src/app/search.rs`、`src/ui/viewport.rs`等を調査し、既存テストの重複を避けながら、GUI操作に頼っていた重要な振る舞いを自動テスト化する。

- **ロジック／状態遷移テスト**：Enterによる次・前の検索結果移動、検索欄クリア、IME変換中Enterの非実行、スクロールに伴う不要な検索再起動の防止、矢印キーのページナビゲーション、単ページ表示のホイール境界処理など、現行仕様で保証される挙動を対象とする。未実装機能を勝手に「実装済みの正解」としてテストしない。仕様が不明なら既存仕様・コードから特定し、未確定は報告する。
- **ヘッドレスUIテスト**：`egui_kittest`の既存`egui`／`eframe` 0.35系列に整合するバージョン（原則0.35.0）を`dev-dependencies`候補として評価する。OSウィンドウを立ち上げず、必要なUI入力と観測可能な状態を検査する。実UIから分離が必要な場合も、必要最小限の境界抽出に留める。
- **PDF関連テスト**：作成した一時PDFに対し、保存後の再読込、注釈内容、検索結果等の意味的な結果を検証する。実OSの画面や既定アプリ、共有クリップボード、ユーザー文書は変更しない。
- **テストの品質**：内部widget階層、正確な座標、文字列整形、画素ハッシュ、単なる実装手順の再現に依存するテストを原則避ける。画面の見た目を検査するスナップショットテストは今回の基本要件に含めず、必要性が証明されない限り追加しない。新規テストの失敗がユーザーに観測できる不具合と対応するか確認する。

既存テストを無理に置き換えない。追加依存・コンパイル時間・実行時間が増えすぎる場合は、軽量な状態遷移テストを優先し、`egui_kittest`の採用範囲を限定して根拠を報告する。

## 4. Docker開発イメージの重複抑制

Dev Containerと単独`docker compose`、Codexタスクで**同じ開発環境イメージを再利用できる構成**を設計・実装する。`image:`による共通タグの採用は候補だが、単にイメージ名を同一化してもBuildKitレイヤー／Cargoキャッシュの問題は解決しないので切り分けること。

- 既存`.devcontainer/Dockerfile`およびWSLg・PlantUMLオーバーレイの働きを維持する。ツールチェーン固定やソース移設は行わない。
- 既存のイメージを再利用できる場合はタスクごとにビルドしない。Dockerfileやその入力が変更された場合は適切に再ビルドできる手段を維持し、古いイメージの黙った使い回しを防ぐ。
- VS CodeのDev Containersが生成するComposeプロジェクト名やラベル、並列タスクのワークスペース隔離との整合を確認する。**全タスクを同一Composeプロジェクト名へ強制して、コンテナ／作業領域を衝突させない。**
- タスク終了後、イメージやコンテナが不要に増え続けない構成を検証する。ただし、ユーザーが利用中のコンテナ等を勝手に停止・削除しない。

## 5. Cargoキャッシュの共有と並列実行

主目的はタスクごとに`cargo-target`／`cargo-registry`を新規生成して再コンパイルする状況を防ぐことである。実際のVolume名・使用状況の調査後に、既存の構成とデータを安全に扱える方法を選定する。

**必須要件**

1. `cargo-registry`のダウンロード済み依存ソースは、互換性があるタスク間で共有できるようにする。必要ならnamed volumeの実名を固定（`name:`や`external: true`等）する。ただし、存在しないexternal volumeでDev Containerが起動不能になる設計にしないこと。必要なら初期化手順または自動作成方法を整える。
2. **逐次実行の通常開発**は同一の互換環境で`target`成果物をできる限り再利用する。既存`/workspace/target`のパス、およびLinux／Windows GNU／Windows MSVCの成果物境界を崩さない。
3. **並列Codexタスク**では、最終成果物の競合やCargoのビルドロックによる待機を避ける。タスク別の`CARGO_TARGET_DIR`などを許容し、同一`target` Volumeの同時書き込みを無条件で推奨しない。
4. 並列で別の`target`を使う場合、共有キャッシュとして`sccache`を導入・測定する。`RUSTC_WRAPPER=sccache`、共通の永続`sccache`保存先、実行時に互換なコンパイラ・target・flags・profileの使用を検討する。`sccache`が捕捉できない成果物（最終リンク、native buildなど）を「全て共有できる」と説明しない。
5. `sccache`ではRustのincremental compilationがキャッシュ対象にならない。`CARGO_INCREMENTAL=0`はsccacheを使う測定済みのプロファイルに限定し、逐次開発でのincremental compileとの時間比較を行う。むやみに毎タスク`RUSTC_WRAPPER`を切り替えて再ビルドさせない。キャッシュ容量上限も含めてストレージ効率を評価する。
6. Windows側のネイティブMSVCビルドキャッシュとLinux/Windows GNUのクロスビルドを混同しない。Windows側の既存ビルド経路を破壊しない。

既存Volumeのデータは原則保持する。既存名から固定名へ切り替える場合は、旧Volumeの存在、互換性、必要なデータ移行・一時的なコールドビルドの影響を報告し、ロールバック可能な手順にする。既存Volumeは検証後も無断削除しない。

## 6. 検証コマンドの最適化

現在の`AGENTS.md`のDev Container実行例を確認し、同一の既存ワークスペースコンテナを使えるときは`exec`を優先する。別作業領域で必要なときのみコンテナを作成し、毎回の`up --build`、不必要な`--no-cache`、一律`cargo clean`、毎タスクの新規Dockerビルドを避ける。`cargo test --locked`など既存CIの前提を尊重する。ローカルで軽量検証を終えたあと、必要に応じて全テストとClippyを実施する。

既存CI（Windows MSVC）を維持し、今回の変更を検証するために必要な最小限の追加だけを行う。UIテストがWindows/Linux両方でヘッドレス実行できるか評価する。CIにGUI操作を安易に追加しない。

## 7. 受入検証・実測

**変更前後で同条件を比較**し、次の項目を記録する。未測定は未測定とする。

- 1回目と2回目の同一`cargo check`／対象テストの時間、変更箇所のみ修正した場合の時間と再コンパイル数。
- タスクA実行後にタスクBで同等の検証を行った場合のVolume実名、依存再利用、コンパイルキャッシュヒット状況。
- `sccache --show-stats`等で確認できるヒット数・ミス数・非キャッシュ理由。ただし並列タスク間では集計の混同を避ける。
- 同一タスクを複数回実施した場合のDockerイメージ／Volume／停止済みコンテナの増分、実際のDockerディスク使用量（共通レイヤーを単純合計しない）。
- 並列2タスク実行でのビルドロック・出力競合・作業ツリー混線の有無。
- `cargo fmt --check`、`cargo check`、関連`cargo test`、必要に応じた`cargo clippy --all-targets`、既存Windows MSVC CIの結果。
- 実GUIを使わない新規ヘッドレステストが、変更に対する正常・異常ケースを正しく識別すること。
- 通常のコード変更でCodexが自律的にComputer Use・スクリーンショット・OSウィンドウ操作を行わないこと。

受入目標は**通常の同一環境での2回目の検証が不要な全依存再コンパイルを起こさないこと、タスクごとに独立した開発イメージ／共有可能な依存キャッシュを無駄に増殖させないこと、並列タスクが相互干渉しないこと、非GUI自動テストで主要機能を検証できること**である。改善が測定できない最適化や保守コスト増だけの設定は採用せず、その理由を記録する。

## 8. 実施順序と報告

1. 現状調査とベースライン取得。
2. `AGENTS.md`等への検証方針追記（実GUIの通常実行禁止、ヘッドレス優先、実施・未実施区分）。
3. 既存ロジックテストの整理と必要最小限のヘッドレスUIテスト追加。
4. 共有イメージ／Cargo registry／逐次用targetの改善。既存環境との互換性確認。
5. 並列タスクの隔離とsccacheの効果測定。効果が確認できる構成を採用。
6. Linuxテスト、既存Windows CIとの整合確認、並列・キャッシュ実測。
7. 検証記録、使い方、必要時の初期化・切り戻し方法を文書化。

各段階で目的・変更点・副作用を確認し、無関係のリファクタリングを加えない。`docs/tasks/README.md`に従う作業指示書の格納ルールや`AGENTS.md`のコミット運用を守る。

**最終報告に含めること**：変更ファイル一覧、特定した原因と根拠、採用構成と却下案、変更前後の測定値、追加したテストと結果、GUI実機で未検証の事項、残る制約、不要なDocker資産の扱い（自動削除なし）、ロールバック手順。仕様変更を含む場合や制約が衝突する場合は、その点を明示する。

## 参考資料

- Docker Compose project name: https://docs.docker.com/compose/how-tos/project-name/
- Docker Compose named/external volumes: https://docs.docker.com/reference/compose-file/volumes/
- Cargo build cache: https://doc.rust-lang.org/cargo/reference/build-cache.html
- sccache Rust support: https://github.com/mozilla/sccache/blob/main/docs/Rust.md
- egui_kittest 0.35.0: https://docs.rs/egui_kittest/0.35.0/egui_kittest/
