use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::bail;
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
// Five entries fit the compact editor history while bounding schema-1 storage.
pub(crate) const MAX_RECENT_ANNOTATION_COLORS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionState {
    pub(crate) schema_version: u32,
    pub(crate) restore_enabled: bool,
    pub(crate) selected_tab: Option<usize>,
    pub(crate) sidebar_open: bool,
    pub(crate) sidebar_tab: SidebarTab,
    pub(crate) tabs: Vec<SessionTab>,
    #[serde(default)]
    // Additive default keeps existing schema-1 session JSON readable.
    pub(crate) recent_annotation_colors: Vec<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionTab {
    pub(crate) path: PathBuf,
    pub(crate) view: SessionView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionView {
    pub(crate) page_index: usize,
    pub(crate) page_x: f32,
    pub(crate) page_y: f32,
    pub(crate) display: DisplayMode,
    pub(crate) zoom_mode: ZoomMode,
    pub(crate) zoom: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum SidebarTab {
    Outline,
    Thumbnails,
    Highlights,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum DisplayMode {
    Continuous,
    SinglePage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ZoomMode {
    Fixed,
    FitWidth,
    FitPage,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            restore_enabled: true,
            selected_tab: None,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Outline,
            tabs: Vec::new(),
            recent_annotation_colors: Vec::new(),
        }
    }
}

impl SessionState {
    /// Validates the persisted contract before it is used to restore UI state.
    ///
    /// Zoom is bounded to the same 0.25–4.0 range used by the renderer, while
    /// page anchors are normalized fractions so they remain meaningful across
    /// viewport sizes.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported session schema version {} (expected {})",
                self.schema_version,
                SCHEMA_VERSION
            );
        }
        if self.tabs.is_empty() {
            if self.selected_tab.is_some() {
                bail!("an empty session cannot select a tab");
            }
        } else {
            // Runtime sessions always select one of their open tabs. Rejecting
            // an absent selection avoids inventing a different active tab.
            let selected = self
                .selected_tab
                .ok_or_else(|| anyhow::anyhow!("a nonempty session must select a tab"))?;
            if selected >= self.tabs.len() {
                bail!("selected tab index {selected} is out of range");
            }
        }

        let mut paths = HashSet::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            validate_path(&tab.path)?;
            validate_view(&tab.view)?;
            if !paths.insert(tab.path.as_path()) {
                bail!(
                    "session contains duplicate tab path: {}",
                    tab.path.display()
                );
            }
        }
        // Keep the preference bounded and deterministic so malformed session files
        // cannot grow memory or present the same color repeatedly.
        if self.recent_annotation_colors.len() > MAX_RECENT_ANNOTATION_COLORS {
            bail!("session contains more than {MAX_RECENT_ANNOTATION_COLORS} recent colors");
        }
        let mut colors = HashSet::with_capacity(self.recent_annotation_colors.len());
        if self
            .recent_annotation_colors
            .iter()
            .any(|color| !colors.insert(*color))
        {
            bail!("session contains duplicate recent colors");
        }
        Ok(())
    }
}

fn validate_path(path: &Path) -> anyhow::Result<()> {
    if !path.is_absolute() {
        bail!("session tab path is not absolute: {}", path.display());
    }
    if normalize_path(path) != path {
        bail!("session tab path is not normalized: {}", path.display());
    }
    Ok(())
}

fn validate_view(view: &SessionView) -> anyhow::Result<()> {
    if !view.page_x.is_finite() || !(0.0..=1.0).contains(&view.page_x) {
        bail!("page_x anchor must be finite and within 0.0..=1.0");
    }
    if !view.page_y.is_finite() || !(0.0..=1.0).contains(&view.page_y) {
        bail!("page_y anchor must be finite and within 0.0..=1.0");
    }
    if !view.zoom.is_finite() || !(MIN_ZOOM..=MAX_ZOOM).contains(&view.zoom) {
        bail!("zoom must be finite and within {MIN_ZOOM}..={MAX_ZOOM}");
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    // Session restore must not require the document to exist, so normalize
    // lexical `.` and `..` components without filesystem canonicalization.
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)))
                {
                    normalized.pop();
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: PathBuf) -> SessionTab {
        SessionTab {
            path,
            view: SessionView {
                page_index: 2,
                page_x: 0.5,
                page_y: 0.25,
                display: DisplayMode::SinglePage,
                zoom_mode: ZoomMode::Fixed,
                zoom: 1.5,
            },
        }
    }

    #[test]
    fn default_state_uses_schema_one_and_enables_restore() {
        let state = SessionState::default();

        assert_eq!(state.schema_version, 1);
        assert!(state.restore_enabled);
    }

    #[test]
    fn non_absolute_or_non_normalized_paths_are_rejected() {
        let mut state = SessionState::default();
        state.tabs.push(tab(PathBuf::from("relative.pdf")));
        assert!(state.validate().is_err());

        let mut state = SessionState::default();
        state
            .tabs
            .push(tab(PathBuf::from("/tmp/folder/../paper.pdf")));
        assert!(state.validate().is_err());
    }

    #[test]
    fn nonempty_session_requires_one_valid_selected_tab() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = SessionState::default();
        state.tabs.push(tab(directory.path().join("paper.pdf")));

        assert!(state.validate().is_err());
        state.selected_tab = Some(1);
        assert!(state.validate().is_err());
        state.selected_tab = Some(0);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn duplicate_session_paths_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let repeated = tab(directory.path().join("paper.pdf"));
        let mut state = SessionState {
            selected_tab: Some(0),
            ..SessionState::default()
        };
        state.tabs = vec![repeated.clone(), repeated];

        assert!(state.validate().is_err());
    }

    #[test]
    fn highlight_sidebar_tab_round_trips_in_schema_one() {
        let state = SessionState {
            sidebar_tab: SidebarTab::Highlights,
            ..SessionState::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: SessionState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.sidebar_tab, SidebarTab::Highlights);
        assert_eq!(restored.schema_version, 1);
    }

    #[test]
    fn old_schema_one_json_defaults_recent_colors_to_empty() {
        let json = r#"{
            "schema_version": 1,
            "restore_enabled": true,
            "selected_tab": null,
            "sidebar_open": false,
            "sidebar_tab": "Outline",
            "tabs": []
        }"#;

        let restored: SessionState = serde_json::from_str(json).unwrap();

        assert!(restored.recent_annotation_colors.is_empty());
        assert!(restored.validate().is_ok());
    }

    #[test]
    fn recent_color_validation_rejects_duplicates_and_overflow() {
        let duplicate = SessionState {
            recent_annotation_colors: vec![[1, 2, 3], [1, 2, 3]],
            ..SessionState::default()
        };
        assert!(duplicate.validate().is_err());

        let overflow = SessionState {
            recent_annotation_colors: (0..6).map(|value| [value, 0, 0]).collect(),
            ..SessionState::default()
        };
        assert!(overflow.validate().is_err());
    }
}
