# Development environment

- コミットメッセージは `<type>: 日本語の要約` の形式に統一する（例: `fix: 印刷処理の終了状態を修正`）。
- Goalを設定して進める作業では、各フェーズなど区切りのよい時点で変更をコミットする。
- リポジトリ内の作業指示書に従う前に、`docs/tasks/README.md`の分類に沿うフォルダへ配置し、同READMEの索引を更新する。
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
