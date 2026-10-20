# Development environment

- コミットメッセージは `<type>: 日本語の要約` の形式に統一する（例: `fix: 印刷処理の終了状態を修正`）。
- Goalを設定して進める作業では、各フェーズなど区切りのよい時点で変更をコミットする。
- 作業指示書を新規作成するときだけ、[作業資料の分類・索引](docs/tasks/README.md)に従って日付付きの名前で登録する。既存文書やユーザー提供文書を作業開始前に移動・改名しない。
- Source files are stored on the Windows filesystem. Use the Dev Container for Linux builds and tests when the task needs them.
- 既存のDev Container workspaceへ `exec` する場合は、先に対象checkoutの絶対パスと、稼働workspace containerの実際の /workspace bind mount source が同じcheckoutを指すことを確認する。Compose project・container・設定ファイルは実環境から特定し、名前を推測しない。mount元が不一致または不明ならcontainer内実行を止める。one-off containerの確認方法を含む手順は[開発環境の運用手順](docs/development-workflow.md)を参照する。文書だけの作業にはDocker照合を要求しない。
- タスクに含まれないユーザー文書を読み書きせず、共有クリップボードを明示依頼なく読み書きしない。既存Docker image・container・volume・Cargo/sccache cacheを、明示依頼なく削除、prune、または `down -v` で消去しない。
- Keep container and Windows build outputs separate.

## Validation policy

- 挙動・契約に関係する最小の自動検査から始め、必要性に応じて範囲を広げる。[検証マトリクス](docs/tasks/development-environment/verification-matrix.md)にある変更種別ごとの目安を使い、全テスト・全ターゲット・release build・installer生成を毎回要求しない。
- `cargo check` は型・コンパイル検査であり、実行時の動作確認ではない。実行済み、失敗、環境制約で未実施、実機未検証を区別して報告する。
- 実GUI起動・OS入力・Computer Use・screen capture・OCR・画素判定は通常の検証手段にしない。ユーザーが当該GUI受入確認を明示的に依頼した場合だけ行い、GUI未検証と実機未検証を成功扱いしない。
- 振る舞いや仕様を変える作業では、[現行仕様](docs/current-spec.md)の該当箇所を確認する。Dev Containerやbuild環境を操作する場合は[開発環境の運用手順](docs/development-workflow.md)を参照する。GUI実機受入または性能受入が明示された場合だけ、[過去の検証事例](.codex/validation-lessons.md)の関連箇所を読む。

## Versioning

- バージョンはリリースに合わせて更新し、唯一の管理元は `Cargo.toml` の `[package].version` とする。通常の機能追加・修正では、リリースまたは version 更新の明示的な指示がない限り、実装者は version を上げない。
- About、Windows EXE、installer、portable の version は package version を参照し、固定値を重複して持たせない。version を更新した場合は `Cargo.toml` を変更し、必要なら Cargo で `Cargo.lock` を更新する。
- リリース時は、About 表示および Windows EXE、installer、portable の各配布物が同じ package version を使うことを確認する。
