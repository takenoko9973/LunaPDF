# Development environment

- コミットメッセージは `<type>: 日本語の要約` の形式に統一する（例: `fix: 印刷処理の終了状態を修正`）。
- GUIまたは性能の受入検証を行う前に、`.codex/validation-lessons.md`を読む。
- Source files are stored on the Windows filesystem.
- Use the Dev Container for Linux builds and tests.
- Run Linux checks with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo check`
- Run Linux Clippy with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets`
- Run Windows MSVC checks directly on the host with:
  `cargo check --target x86_64-pc-windows-msvc`
- Keep container and Windows build outputs separate.
