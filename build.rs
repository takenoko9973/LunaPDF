use std::env;

const WINDOWS_ICON_PATH: &str = "assets/windows/lunapdf.ico";

/// Windows向け実行ファイルへ、Cargoのバージョン情報と配布用アイコンを埋め込む。
///
/// Windows以外ではリソースコンパイラを起動せず、既存のビルド経路を維持する。
fn main() {
    println!("cargo:rerun-if-changed={WINDOWS_ICON_PATH}");

    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some("windows".as_ref()) {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(WINDOWS_ICON_PATH);
    resource
        .compile()
        .expect("compile LunaPDF Windows executable resources");
}
