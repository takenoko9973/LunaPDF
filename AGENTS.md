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

## Versioning

- バージョンはリリースに合わせて更新し、唯一の管理元は `Cargo.toml` の `[package].version` とする。通常の機能追加・修正では、リリースまたは version 更新の明示的な指示がない限り、実装者は version を上げない。
- About、Windows EXE、installer、portable の version は package version を参照し、固定値を重複して持たせない。version を更新した場合は `Cargo.toml` を変更し、必要なら Cargo で `Cargo.lock` を更新する。
- リリース時は、About 表示および Windows EXE、installer、portable の各配布物が同じ package version を使うことを確認する。
