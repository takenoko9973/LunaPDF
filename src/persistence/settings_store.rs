use std::fs::File;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use tempfile::NamedTempFile;

use crate::domain::settings::AppSettings;
use crate::persistence::session_store::SessionStore;

const SETTINGS_FILE_NAME: &str = "settings.json";

pub(crate) struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn from_session_store(session_store: &SessionStore) -> Self {
        Self::new(session_store.sibling_path(SETTINGS_FILE_NAME))
    }

    pub(crate) fn load(&self) -> Result<AppSettings> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AppSettings::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open settings file {}", self.path.display()));
            }
        };
        let settings: AppSettings = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("parse settings file {}", self.path.display()))?;
        settings
            .validate()
            .with_context(|| format!("validate settings file {}", self.path.display()))?;
        Ok(settings)
    }

    pub(crate) fn save(&self, settings: &AppSettings) -> Result<()> {
        settings
            .validate()
            .context("validate settings before save")?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| anyhow!("settings path has no parent directory"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create settings directory {}", parent.display()))?;

        let serialized = serde_json::to_vec_pretty(settings).context("serialize settings")?;
        let mut temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("create temporary settings in {}", parent.display()))?;
        temporary
            .write_all(&serialized)
            .context("write temporary settings")?;
        temporary.flush().context("flush temporary settings")?;
        temporary
            .as_file()
            .sync_all()
            .context("sync temporary settings")?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace settings file {}", self.path.display()))?;
        Ok(())
    }
}
