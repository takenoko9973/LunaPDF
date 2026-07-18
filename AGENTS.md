# Development environment

- Source files are stored on the Windows filesystem.
- Use the Dev Container for Linux builds and tests.
- Run Linux checks with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo check`
- Run Linux Clippy with:
  `docker compose -f .devcontainer/compose.base.yml exec workspace cargo clippy --all-targets`
- Run Windows MSVC checks directly on the host with:
  `cargo check --target x86_64-pc-windows-msvc`
- Do not treat a successful Windows GNU cross-build as verification of the Windows MSVC build.
- Keep container and Windows build outputs separate.
