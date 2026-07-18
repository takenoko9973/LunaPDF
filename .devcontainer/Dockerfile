FROM rust:1-slim-trixie

# RUN - https://docs.docker.jp/engine/reference/builder.html#run
# パッケージの更新とインストール
# apt-get update                  更新を取得
# apt-get install -y <packages>   パッケージをインストール ここで必要なパッケージを指定
# apt-get clean                   aptキャッシュをクリーン
# rm -rf /var/lib/apt/lists/*     キャッシュされている全パッケージリストを削除
# https://docs.docker.jp/engine/articles/dockerfile_best-practice.html
RUN apt-get update; \
    apt-get install -y \
        git \
        pkg-config \
        cmake \
        ninja-build \
        clang \
        libclang-dev \
        lld \
        libssl-dev \
        libfontconfig1-dev \
        libgtk-3-dev \
        libxcb-render0-dev \
        libxcb-shape0-dev \
        libxcb-xfixes0-dev \
        libxkbcommon-dev \
        libgl1-mesa-dev \
        libegl1-mesa-dev \
        mesa-utils \
        mingw-w64 \
        ; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/*;

RUN rustup target add x86_64-pc-windows-gnu; \
    rustup component add rustc rust-analysis rust-src rustfmt clippy llvm-tools-preview; \
    cargo install cargo-edit cargo-watch cargo-update;
