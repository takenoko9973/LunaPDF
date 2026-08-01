use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 2;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
// 5エントリでコンパクトなエディタ履歴に収まり、保存容量も上限内に保てる。
pub(crate) const MAX_RECENT_ANNOTATION_COLORS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 保存対象となるUIセッション状態(schema 2)。
pub(crate) struct SessionState {
    pub(crate) schema_version: u32,
    pub(crate) restore_enabled: bool,
    pub(crate) sidebar_open: bool,
    pub(crate) sidebar_tab: SidebarTab,
    pub(crate) tabs: Vec<SessionTab>,
    pub(crate) layout: SessionLayout,
    #[serde(default)]
    pub(crate) recent_annotation_colors: Vec<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// タブを1つまたは2つのペインへ割り当てるレイアウト。
pub(crate) struct SessionLayout {
    pub(crate) panes: Vec<SessionPane>,
    pub(crate) focused_pane: Option<usize>,
    pub(crate) split: Option<SessionSplit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// グローバルなタブ番号と、そのペインで選択されたタブ。
pub(crate) struct SessionPane {
    pub(crate) tab_indices: Vec<usize>,
    pub(crate) selected_tab: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 2ペイン時の分割方向と比率。
pub(crate) struct SessionSplit {
    pub(crate) direction: SplitDirection,
    pub(crate) ratio: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// ペイン分割の方向。schema 2では水平分割のみを受け付ける。
pub(crate) enum SplitDirection {
    Horizontal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 復元対象ファイルと表示状態。
pub(crate) struct SessionTab {
    pub(crate) path: PathBuf,
    pub(crate) view: SessionView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 1タブのページ位置・表示・ズーム。
pub(crate) struct SessionView {
    pub(crate) page_index: usize,
    pub(crate) page_x: f32,
    pub(crate) page_y: f32,
    pub(crate) display: DisplayMode,
    pub(crate) zoom_mode: ZoomMode,
    pub(crate) zoom: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// サイドバーで表示するタブ。
pub(crate) enum SidebarTab {
    Outline,
    Thumbnails,
    Highlights,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// ページ表示モード。
pub(crate) enum DisplayMode {
    Continuous,
    SinglePage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// ズームの適用モード。
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
            sidebar_open: false,
            sidebar_tab: SidebarTab::Outline,
            tabs: Vec::new(),
            layout: SessionLayout {
                panes: Vec::new(),
                focused_pane: None,
                split: None,
            },
            recent_annotation_colors: Vec::new(),
        }
    }
}

impl SessionState {
    /// JSON値をschemaに応じて検証し、schema 1はschema 2のメモリ表現へ移行する。
    pub(crate) fn decode(value: serde_json::Value) -> Result<Self> {
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("session schema_version is missing or invalid"))?;
        match schema_version {
            1 => {
                let old: SessionStateSchema1 = serde_json::from_value(value)
                    .map_err(|error| anyhow::anyhow!("decode schema 1 session: {error}"))?;
                old.validate()?;
                let old_selected_tab = old.selected_tab;
                // schema 1 had one global selected tab; retaining that as the sole pane preserves
                // its selection while making pane membership explicit in schema 2.
                let layout = if old.tabs.is_empty() {
                    SessionLayout {
                        panes: Vec::new(),
                        focused_pane: None,
                        split: None,
                    }
                } else {
                    SessionLayout {
                        panes: vec![SessionPane {
                            tab_indices: (0..old.tabs.len()).collect(),
                            selected_tab: old_selected_tab.ok_or_else(|| {
                                anyhow::anyhow!("validated schema 1 lost selection")
                            })?,
                        }],
                        focused_pane: Some(0),
                        split: None,
                    }
                };
                let state = Self {
                    schema_version: SCHEMA_VERSION,
                    restore_enabled: old.restore_enabled,
                    sidebar_open: old.sidebar_open,
                    sidebar_tab: old.sidebar_tab,
                    tabs: old.tabs,
                    layout,
                    recent_annotation_colors: old.recent_annotation_colors,
                };
                state.validate()?;
                Ok(state)
            }
            version if version == SCHEMA_VERSION as u64 => {
                let state: Self = serde_json::from_value(value)
                    .map_err(|error| anyhow::anyhow!("decode schema 2 session: {error}"))?;
                state.validate()?;
                Ok(state)
            }
            other => bail!("unsupported session schema version {other}"),
        }
    }

    /// UI状態の復元に使う前に永続化契約を検証する。
    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported session schema version {} (expected {})",
                self.schema_version,
                SCHEMA_VERSION
            );
        }
        validate_tabs(&self.tabs)?;
        validate_layout(&self.layout, self.tabs.len())?;
        validate_recent_colors(&self.recent_annotation_colors)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStateSchema1 {
    schema_version: u32,
    restore_enabled: bool,
    selected_tab: Option<usize>,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    tabs: Vec<SessionTab>,
    #[serde(default)]
    recent_annotation_colors: Vec<[u8; 3]>,
}

impl SessionStateSchema1 {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("invalid schema 1 session version {}", self.schema_version);
        }
        if self.tabs.is_empty() {
            if self.selected_tab.is_some() {
                bail!("an empty session cannot select a tab");
            }
        } else {
            let selected = self
                .selected_tab
                .ok_or_else(|| anyhow::anyhow!("a nonempty session must select a tab"))?;
            if selected >= self.tabs.len() {
                bail!("selected tab index {selected} is out of range");
            }
        }
        validate_tabs(&self.tabs)?;
        validate_recent_colors(&self.recent_annotation_colors)
    }
}

fn validate_tabs(tabs: &[SessionTab]) -> Result<()> {
    let mut paths = HashSet::with_capacity(tabs.len());
    for tab in tabs {
        validate_path(&tab.path)?;
        validate_view(&tab.view)?;
        if !paths.insert(tab.path.as_path()) {
            bail!(
                "session contains duplicate tab path: {}",
                tab.path.display()
            );
        }
    }
    Ok(())
}

fn validate_layout(layout: &SessionLayout, tab_count: usize) -> Result<()> {
    if tab_count == 0 {
        if !layout.panes.is_empty() || layout.focused_pane.is_some() || layout.split.is_some() {
            bail!("an empty session must have no panes, focus, or split");
        }
        return Ok(());
    }
    if !(1..=2).contains(&layout.panes.len()) {
        bail!("a nonempty session must contain one or two panes");
    }
    let focus = layout
        .focused_pane
        .ok_or_else(|| anyhow::anyhow!("a nonempty session must focus a pane"))?;
    if focus >= layout.panes.len() {
        bail!("focused pane index {focus} is out of range");
    }
    if layout.panes.len() == 1 {
        if layout.split.is_some() {
            bail!("a single-pane session cannot contain a split");
        }
    } else if !matches!(
        layout.split,
        Some(SessionSplit {
            direction: SplitDirection::Horizontal,
            ..
        })
    ) {
        bail!("a two-pane session requires a horizontal split");
    }
    if let Some(split) = &layout.split {
        // 極端な比率は一方のペインを実質的に不可視にするため、復元時点で拒否する。
        if !split.ratio.is_finite() || !(0.1..=0.9).contains(&split.ratio) {
            bail!("split ratio must be finite and within 0.1..=0.9");
        }
    }

    // registryのタブ順を保ったまま復元するため、各グローバル番号は必ず一つのペインだけに属させる。
    let mut membership = vec![0usize; tab_count];
    for (pane_index, pane) in layout.panes.iter().enumerate() {
        if pane.tab_indices.is_empty() {
            bail!("pane {pane_index} cannot be empty");
        }
        if !pane.tab_indices.contains(&pane.selected_tab) {
            bail!("selected tab is not a member of pane {pane_index}");
        }
        for &tab_index in &pane.tab_indices {
            if tab_index >= tab_count {
                bail!("tab index {tab_index} is out of range");
            }
            membership[tab_index] += 1;
        }
    }
    if membership.iter().any(|count| *count != 1) {
        bail!("each tab must belong to exactly one pane");
    }
    Ok(())
}

fn validate_recent_colors(colors: &[[u8; 3]]) -> Result<()> {
    if colors.len() > MAX_RECENT_ANNOTATION_COLORS {
        bail!("session contains more than {MAX_RECENT_ANNOTATION_COLORS} recent colors");
    }
    let mut unique = HashSet::with_capacity(colors.len());
    if colors.iter().any(|color| !unique.insert(*color)) {
        bail!("session contains duplicate recent colors");
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("session tab path is not absolute: {}", path.display());
    }
    if normalize_path(path) != path {
        bail!("session tab path is not normalized: {}", path.display());
    }
    Ok(())
}

fn validate_view(view: &SessionView) -> Result<()> {
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
    // ファイルの存在を要求せず、字句的な`.`と`..`要素だけを正規化する。
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

    fn one_tab_state(directory: &Path) -> SessionState {
        SessionState {
            tabs: vec![tab(directory.join("paper.pdf"))],
            layout: SessionLayout {
                panes: vec![SessionPane {
                    tab_indices: vec![0],
                    selected_tab: 0,
                }],
                focused_pane: Some(0),
                split: None,
            },
            ..SessionState::default()
        }
    }

    #[test]
    fn default_and_empty_layout_are_schema_two() {
        let state = SessionState::default();
        assert_eq!(state.schema_version, 2);
        assert!(state.validate().is_ok());
        assert!(state.layout.panes.is_empty());
    }

    #[test]
    fn one_and_two_pane_layouts_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = one_tab_state(directory.path());
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(SessionState::decode(json).unwrap(), state);

        state.tabs.push(tab(directory.path().join("second.pdf")));
        state.layout = SessionLayout {
            panes: vec![
                SessionPane {
                    tab_indices: vec![0],
                    selected_tab: 0,
                },
                SessionPane {
                    tab_indices: vec![1],
                    selected_tab: 1,
                },
            ],
            focused_pane: Some(1),
            split: Some(SessionSplit {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
            }),
        };
        assert_eq!(
            SessionState::decode(serde_json::to_value(&state).unwrap()).unwrap(),
            state
        );
    }

    #[test]
    fn schema_one_migrates_to_single_pane_and_defaults_colors() {
        let directory = tempfile::tempdir().unwrap();
        let json = serde_json::json!({
            "schema_version": 1,
            "restore_enabled": true,
            "selected_tab": 0,
            "sidebar_open": false,
            "sidebar_tab": "Outline",
            "tabs": [serde_json::to_value(tab(directory.path().join("paper.pdf"))).unwrap()]
        });
        let state = SessionState::decode(json).unwrap();
        assert_eq!(state.schema_version, 2);
        assert_eq!(state.layout.panes[0].tab_indices, vec![0]);
        assert_eq!(state.layout.panes[0].selected_tab, 0);
        assert!(state.recent_annotation_colors.is_empty());
    }

    #[test]
    fn malformed_layout_membership_and_split_values_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = one_tab_state(directory.path());
        state.layout.panes[0].tab_indices.clear();
        assert!(state.validate().is_err());
        state.layout.panes[0].tab_indices = vec![1];
        assert!(state.validate().is_err());
        state.layout.panes[0].tab_indices = vec![0];
        state.layout.panes[0].selected_tab = 1;
        assert!(state.validate().is_err());
        state.layout.panes[0].selected_tab = 0;
        state.layout.split = Some(SessionSplit {
            direction: SplitDirection::Horizontal,
            ratio: 0.09,
        });
        assert!(state.validate().is_err());
    }

    #[test]
    fn paths_views_and_recent_colors_keep_existing_validation() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = one_tab_state(directory.path());
        state.tabs[0].path = PathBuf::from("relative.pdf");
        assert!(state.validate().is_err());
        state.tabs[0].path = directory.path().join("folder/../paper.pdf");
        assert!(state.validate().is_err());
        state.tabs[0].path = directory.path().join("paper.pdf");
        state.tabs[0].view.zoom = 4.1;
        assert!(state.validate().is_err());
        state.tabs[0].view.zoom = 1.0;
        state.recent_annotation_colors = vec![[1, 2, 3], [1, 2, 3]];
        assert!(state.validate().is_err());
    }
}
