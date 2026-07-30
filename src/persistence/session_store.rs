use std::env;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use tempfile::NamedTempFile;

use crate::domain::session::SessionState;

const SESSION_FILE_NAME: &str = "session.json";

pub(crate) struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// OSの設定ディレクトリがない場合もカレントディレクトリへフォールバックせず、
    /// ユーザーごとのセッション保存先を解決する。
    pub(crate) fn for_current_user() -> Result<Self> {
        let path = config_path_for_current_user()?;
        Ok(Self::new(path))
    }

    /// 主に分離されたテスト用として、明示したパスにストアを作成する。
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// セッションを読み込み、解析して検証する。まだ作成されていない場合は`None`を返し、
    /// 初回起動を通常の空セッションとして扱えるようにする。
    pub(crate) fn load(&self) -> Result<Option<SessionState>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open session file {}", self.path.display()));
            }
        };
        let state: SessionState = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("parse session file {}", self.path.display()))?;
        state
            .validate()
            .with_context(|| format!("validate session file {}", self.path.display()))?;
        Ok(Some(state))
    }

    /// セッションファイルを検証し、整形済みJSONでアトミックに置き換える。
    ///
    /// 一時ファイルを保存先の隣に置くことで最終的な名前変更を同一ファイルシステム内に保つ。
    /// フラッシュと同期で置換境界を明示し、バックアップやサイドカーファイルを残さない。
    pub(crate) fn save(&self, state: &SessionState) -> Result<()> {
        state.validate().context("validate session before save")?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| anyhow!("session path has no parent directory"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create session directory {}", parent.display()))?;

        let serialized = serde_json::to_vec_pretty(state).context("serialize session")?;
        let mut temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("create temporary session in {}", parent.display()))?;
        temporary
            .write_all(&serialized)
            .context("write temporary session")?;
        temporary.flush().context("flush temporary session")?;
        temporary
            .as_file()
            .sync_all()
            .context("sync temporary session")?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace session file {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(windows)]
fn config_path_for_current_user() -> Result<PathBuf> {
    let appdata = env::var_os("APPDATA").map(PathBuf::from);
    windows_config_path(appdata.as_deref())
}

#[cfg(windows)]
fn windows_config_path(appdata: Option<&Path>) -> Result<PathBuf> {
    let appdata =
        appdata.ok_or_else(|| anyhow!("APPDATA is not set; cannot resolve session directory"))?;
    if !appdata.is_absolute() {
        bail!("APPDATA must be an absolute path");
    }
    Ok(appdata.join("LunaPDF").join(SESSION_FILE_NAME))
}

#[cfg(target_os = "linux")]
fn config_path_for_current_user() -> Result<PathBuf> {
    let xdg = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    linux_config_path(xdg.as_deref(), home.as_deref())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn config_path_for_current_user() -> Result<PathBuf> {
    bail!("session storage is unsupported on this target")
}

#[cfg(target_os = "linux")]
fn linux_config_path(xdg: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    // XDGの相対値では保存先が起動ディレクトリに依存するため、ユーザーの絶対HOME配下に
    // XDGのフォールバックを置く。
    let base = match xdg.filter(|path| path.is_absolute()) {
        Some(path) => path.to_path_buf(),
        None => {
            let home =
                home.ok_or_else(|| anyhow!("HOME is not set; cannot resolve session directory"))?;
            if !home.is_absolute() {
                bail!("HOME must be an absolute path");
            }
            home.join(".config")
        }
    };
    Ok(base.join("lunapdf").join(SESSION_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{DisplayMode, SessionTab, SessionView, SidebarTab, ZoomMode};

    fn valid_state(directory: &Path) -> SessionState {
        SessionState {
            schema_version: 1,
            restore_enabled: true,
            selected_tab: Some(0),
            sidebar_open: true,
            sidebar_tab: SidebarTab::Thumbnails,
            tabs: vec![SessionTab {
                path: directory.join("paper.pdf"),
                view: SessionView {
                    page_index: 7,
                    page_x: 0.4,
                    page_y: 0.8,
                    display: DisplayMode::SinglePage,
                    zoom_mode: ZoomMode::Fixed,
                    zoom: 1.75,
                },
            }],
            recent_annotation_colors: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_preserves_full_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        let state = valid_state(directory.path());
        let store = SessionStore::new(path);

        store.save(&state).unwrap();

        assert_eq!(store.load().unwrap(), Some(state));
    }

    #[test]
    fn missing_session_returns_none() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path().join("missing.json"));

        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn malformed_and_invalid_sessions_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(SessionStore::new(path.clone()).load().is_err());

        let mut invalid = valid_state(directory.path());
        invalid.schema_version = 99;
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(SessionStore::new(path).load().is_err());
    }

    #[test]
    fn sessions_with_more_than_fifty_tabs_are_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = valid_state(directory.path());
        state.tabs = (0..51)
            .map(|index| {
                let mut tab = state.tabs[0].clone();
                tab.path = directory.path().join(format!("{index}.pdf"));
                tab
            })
            .collect();
        state.selected_tab = Some(50);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn fifty_one_tab_session_roundtrip_preserves_order_and_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = valid_state(directory.path());
        state.tabs = (0..51)
            .map(|index| {
                let mut tab = state.tabs[0].clone();
                tab.path = directory.path().join(format!("{index:02}.pdf"));
                tab
            })
            .collect();
        state.selected_tab = Some(12);
        let store = SessionStore::new(directory.path().join("session.json"));

        store.save(&state).unwrap();
        let restored = store.load().unwrap().unwrap();

        assert_eq!(restored.tabs, state.tabs);
        assert_eq!(restored.selected_tab, Some(12));
    }

    #[test]
    fn successful_save_leaves_no_temporary_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        SessionStore::new(path.clone())
            .save(&valid_state(directory.path()))
            .unwrap();

        let entries = std::fs::read_dir(directory.path())
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(path.is_file());
        assert!(entries.iter().all(|entry| {
            entry
                .as_ref()
                .map(|entry| entry.path() == path)
                .unwrap_or(false)
        }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_config_path_resolution_does_not_use_process_environment() {
        assert_eq!(
            linux_config_path(Some(Path::new("relative")), Some(Path::new("/home/alice"))).unwrap(),
            PathBuf::from("/home/alice/.config/lunapdf/session.json")
        );
        assert_eq!(
            linux_config_path(Some(Path::new("/etc/xdg")), Some(Path::new("/home/alice"))).unwrap(),
            PathBuf::from("/etc/xdg/lunapdf/session.json")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_config_path_resolution_requires_absolute_appdata() {
        assert!(windows_config_path(None).is_err());
        assert!(windows_config_path(Some(Path::new("relative"))).is_err());
        assert_eq!(
            windows_config_path(Some(Path::new(r"C:\Users\alice\AppData\Roaming"))).unwrap(),
            PathBuf::from(r"C:\Users\alice\AppData\Roaming\LunaPDF\session.json")
        );
    }
}
