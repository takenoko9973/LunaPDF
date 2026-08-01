use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const WINDOWS_ICON_PATH: &str = "assets/windows/lunapdf.ico";

/// Gitコマンドの標準出力を、末尾空白を除いたUTF-8文字列として取得する。
///
/// Git管理外のsource archiveからも通常ビルドは可能にするため、Gitが使えない場合は
/// `None`を返す。配布ビルド側はprovenance不明の実行ファイルを明示的に拒否する。
fn git_stdout(manifest_dir: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    Some(stdout.trim().to_owned())
}

/// Windowsリソースへ埋め込むsource commitとworking tree状態を返す。
///
/// 出力は配布スクリプトが厳密比較できる固定形式で、副作用はGitの読み取りだけである。
fn source_provenance(manifest_dir: &Path) -> String {
    let Some(commit) = git_stdout(manifest_dir, &["rev-parse", "HEAD"]) else {
        return "SourceCommit=unavailable;Dirty=unknown".to_owned();
    };
    let Some(status) = git_stdout(
        manifest_dir,
        &["status", "--porcelain", "--untracked-files=normal"],
    ) else {
        return format!("SourceCommit={commit};Dirty=unknown");
    };
    let dirty = !status.is_empty();
    format!("SourceCommit={commit};Dirty={dirty}")
}

/// HEADやindexの変化でWindowsリソースを再生成するため、Git管理ファイルを監視対象にする。
fn emit_git_rerun_paths(manifest_dir: &Path) {
    let mut git_paths = vec![
        "HEAD".to_owned(),
        "index".to_owned(),
        "logs/HEAD".to_owned(),
    ];
    if let Some(head_ref) = git_stdout(manifest_dir, &["symbolic-ref", "-q", "HEAD"])
        && !head_ref.is_empty()
    {
        git_paths.push(head_ref);
    }

    for git_path in git_paths {
        let Some(resolved_path) = git_stdout(manifest_dir, &["rev-parse", "--git-path", &git_path])
        else {
            continue;
        };
        let absolute_path = if Path::new(&resolved_path).is_absolute() {
            PathBuf::from(resolved_path)
        } else {
            manifest_dir.join(resolved_path)
        };
        println!("cargo:rerun-if-changed={}", absolute_path.display());
    }
}

/// Windows向け実行ファイルへ、Cargoのバージョン情報と配布用アイコンを埋め込む。
///
/// Windows以外ではリソースコンパイラを起動せず、既存のビルド経路を維持する。
fn main() {
    println!("cargo:rerun-if-changed={WINDOWS_ICON_PATH}");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");

    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some("windows".as_ref()) {
        return;
    }

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest dir"));
    emit_git_rerun_paths(&manifest_dir);
    let provenance = source_provenance(&manifest_dir);
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(WINDOWS_ICON_PATH);
    resource.set("Comments", &provenance);
    resource
        .compile()
        .expect("compile LunaPDF Windows executable resources");
}
