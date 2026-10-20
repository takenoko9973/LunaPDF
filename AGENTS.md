# Development environment

- コミットメッセージは `<type>: 日本語の要約` の形式に統一する（例: `fix: 印刷処理の終了状態を修正`）。
- Goalを設定して進める作業では、各フェーズなど区切りのよい時点で変更をコミットする。
- リポジトリ内の作業指示書に従う前に、`docs/tasks/README.md`の分類に沿うフォルダへ配置し、同READMEの索引を更新する。また、`yymmdd_xxxxx.md` に変更し、実行時期がわかるようにすること。
- GUIまたは性能の受入検証を行う前に、`.codex/validation-lessons.md`を読む。
- Source files are stored on the Windows filesystem.
- Use the Dev Container for Linux builds and tests.
- Run Linux checks with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo check`
- Run Linux Clippy with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets`
- Cross-compile Windows GNU debug builds in the Dev Container with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace sh -c "cargo build --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/debug/lunapdf.exe /workspace/dist/lunapdf-debug.exe"`
- Cross-compile Windows GNU release builds in the Dev Container with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace sh -c "cargo build --release --target=x86_64-pc-windows-gnu && install -D target/x86_64-pc-windows-gnu/release/lunapdf.exe /workspace/dist/lunapdf-release.exe"`
- Keep container and Windows build outputs separate.

## Validation policy

- 変更の観測可能な挙動・契約に対して、まず最小の関連自動検査を選び、必要性に応じて対象範囲を広げる。変更種別ごとの目安は [検証マトリクス](docs/tasks/development-environment/verification-matrix.md) を参照する。無関係な全テスト、全ターゲット、Windowsクロスビルド、releaseビルド、インストーラー生成を毎回実行しない。
- `cargo check` は型・コンパイル検査であり、実行時の動作確認ではない。検証結果と未実施範囲を区別して報告する。
- Linux検証では、Dev Container設定と実行中のComposeプロジェクト／workspaceを確認し、利用可能な既存workspaceへの `exec` を優先する。プロジェクト名やコンテナ名を推測せず、起動や再ビルドが必要な場合は変更内容に必要な範囲だけ行う。
- 実GUI起動、Computer Use、OS入力、スクリーンショット、OCR、画素判定を通常の検証手段にしない。状態・イベント検査、ヘッドレスUI検査、PDF出力の構造・内容検査を優先する。OS実機でしか確認できない項目は、必要性・対象範囲・代替できない理由・実行方法を報告し、ユーザーが当該検証を明示的に依頼した場合だけ実行する。未実施項目は「実機未検証」とし、GUI確認が未了の場合は「GUI未検証」と明記する。
- 結果は「実行して合格」「実行して失敗」「環境制約で未実施」「実機未検証」に分け、実行していない検査を成功扱いしない。

## Versioning

- バージョンはリリースに合わせて更新し、唯一の管理元は `Cargo.toml` の `[package].version` とする。通常の機能追加・修正では、リリースまたは version 更新の明示的な指示がない限り、実装者は version を上げない。
- About、Windows EXE、installer、portable の version は package version を参照し、固定値を重複して持たせない。version を更新した場合は `Cargo.toml` を変更し、必要なら Cargo で `Cargo.lock` を更新する。
- リリース時は、About 表示および Windows EXE、installer、portable の各配布物が同じ package version を使うことを確認する。
