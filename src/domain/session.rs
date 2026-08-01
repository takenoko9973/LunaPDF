use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 3;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
// 5エントリでコンパクトなエディタ履歴に収まり、保存容量も上限内に保てる。
pub(crate) const MAX_RECENT_ANNOTATION_COLORS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 保存対象となるUIセッション状態(schema 3)。
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
/// 共有タブ列の表示順と、現在操作対象となるタブ。
pub(crate) struct SessionLayout {
    pub(crate) entries: Vec<SessionEntry>,
    pub(crate) active_tab: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum SessionEntry {
    Single {
        tab_index: usize,
    },
    Split {
        tab_indices: [usize; 2],
        direction: SplitDirection,
        ratio: f32,
        focused_tab: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 分割セット内の2文書を並べる方向。
pub(crate) enum SplitDirection {
    Horizontal,
    Vertical,
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
                entries: Vec::new(),
                active_tab: None,
            },
            recent_annotation_colors: Vec::new(),
        }
    }
}

impl SessionState {
    /// JSON値をschemaに応じて検証し、schema 1/2はschema 3のメモリ表現へ移行する。
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
                let layout = SessionLayout {
                    entries: (0..old.tabs.len())
                        .map(|tab_index| SessionEntry::Single { tab_index })
                        .collect(),
                    active_tab: old.selected_tab,
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
            2 => {
                let old: SessionStateSchema2 = serde_json::from_value(value)
                    .map_err(|error| anyhow::anyhow!("decode schema 2 session: {error}"))?;
                old.validate()?;
                let layout = migrate_schema2_layout(&old.layout, old.tabs.len())?;
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
                    .map_err(|error| anyhow::anyhow!("decode schema 3 session: {error}"))?;
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStateSchema2 {
    schema_version: u32,
    restore_enabled: bool,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    tabs: Vec<SessionTab>,
    layout: SessionLayoutSchema2,
    #[serde(default)]
    recent_annotation_colors: Vec<[u8; 3]>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionLayoutSchema2 {
    panes: Vec<SessionPaneSchema2>,
    focused_pane: Option<usize>,
    split: Option<SessionSplitSchema2>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPaneSchema2 {
    tab_indices: Vec<usize>,
    selected_tab: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSplitSchema2 {
    direction: SplitDirectionSchema2,
    ratio: f32,
}

#[derive(Clone, Copy, Debug, Deserialize)]
enum SplitDirectionSchema2 {
    Horizontal,
}

impl SessionStateSchema2 {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 2 {
            bail!("invalid schema 2 session version {}", self.schema_version);
        }
        validate_tabs(&self.tabs)?;
        validate_schema2_layout(&self.layout, self.tabs.len())?;
        validate_recent_colors(&self.recent_annotation_colors)
    }
}

fn migrate_schema2_layout(
    layout: &SessionLayoutSchema2,
    tab_count: usize,
) -> Result<SessionLayout> {
    if tab_count == 0 {
        return Ok(SessionLayout {
            entries: Vec::new(),
            active_tab: None,
        });
    }
    if layout.panes.len() == 1 {
        let pane = &layout.panes[0];
        return Ok(SessionLayout {
            entries: pane
                .tab_indices
                .iter()
                .copied()
                .map(|tab_index| SessionEntry::Single { tab_index })
                .collect(),
            active_tab: Some(pane.selected_tab),
        });
    }

    let selected = [layout.panes[0].selected_tab, layout.panes[1].selected_tab];
    let flattened = layout
        .panes
        .iter()
        .flat_map(|pane| pane.tab_indices.iter().copied())
        .collect::<Vec<_>>();
    let insertion = flattened
        .iter()
        .position(|tab_index| selected.contains(tab_index))
        .expect("validated schema 2 selections must be present");
    let split = layout
        .split
        .as_ref()
        .expect("validated schema 2 two-pane layout must contain a split");
    let focused_pane = layout
        .focused_pane
        .expect("validated schema 2 layout must contain focus");
    let mut entries = Vec::with_capacity(tab_count - 1);
    for (position, tab_index) in flattened.into_iter().enumerate() {
        if position == insertion {
            // 旧UIで同時表示されていた二つだけを一組にし、非選択タブの見た目の順序は維持する。
            entries.push(SessionEntry::Split {
                tab_indices: selected,
                direction: SplitDirection::Horizontal,
                ratio: split.ratio,
                focused_tab: selected[focused_pane],
            });
        }
        if !selected.contains(&tab_index) {
            entries.push(SessionEntry::Single { tab_index });
        }
    }
    Ok(SessionLayout {
        entries,
        active_tab: Some(selected[focused_pane]),
    })
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
        if !layout.entries.is_empty() || layout.active_tab.is_some() {
            bail!("an empty session must have no entries or active tab");
        }
        return Ok(());
    }
    let active = layout
        .active_tab
        .ok_or_else(|| anyhow::anyhow!("a nonempty session must have an active tab"))?;
    if active >= tab_count {
        bail!("active tab index {active} is out of range");
    }
    let mut membership = vec![0usize; tab_count];
    for entry in &layout.entries {
        match entry {
            SessionEntry::Single { tab_index } => {
                increment_membership(&mut membership, *tab_index)?
            }
            SessionEntry::Split {
                tab_indices,
                ratio,
                focused_tab,
                ..
            } => {
                if tab_indices[0] == tab_indices[1] {
                    bail!("a split entry must contain two different tabs");
                }
                if !tab_indices.contains(focused_tab) {
                    bail!("focused split tab must be a member of its entry");
                }
                if tab_indices.contains(&active) && *focused_tab != active {
                    bail!("the active split tab must match its focused member");
                }
                // 極端な比率は一方のPDF面を実質的に不可視にするため、復元時点で拒否する。
                if !ratio.is_finite() || !(0.1..=0.9).contains(ratio) {
                    bail!("split ratio must be finite and within 0.1..=0.9");
                }
                for tab_index in tab_indices {
                    increment_membership(&mut membership, *tab_index)?;
                }
            }
        }
    }
    if membership.iter().any(|count| *count != 1) {
        bail!("each tab must belong to exactly one tab entry");
    }
    Ok(())
}

fn increment_membership(membership: &mut [usize], tab_index: usize) -> Result<()> {
    let Some(count) = membership.get_mut(tab_index) else {
        bail!("tab index {tab_index} is out of range");
    };
    *count += 1;
    Ok(())
}

fn validate_schema2_layout(layout: &SessionLayoutSchema2, tab_count: usize) -> Result<()> {
    if tab_count == 0 {
        if !layout.panes.is_empty() || layout.focused_pane.is_some() || layout.split.is_some() {
            bail!("an empty schema 2 session must have no panes, focus, or split");
        }
        return Ok(());
    }
    if !(1..=2).contains(&layout.panes.len()) {
        bail!("a schema 2 session must contain one or two panes");
    }
    let focus = layout
        .focused_pane
        .ok_or_else(|| anyhow::anyhow!("a schema 2 session must focus a pane"))?;
    if focus >= layout.panes.len() {
        bail!("schema 2 focused pane is out of range");
    }
    if layout.panes.len() == 1 && layout.split.is_some() {
        bail!("a single schema 2 pane cannot contain a split");
    }
    if layout.panes.len() == 2
        && !matches!(
            layout.split,
            Some(SessionSplitSchema2 {
                direction: SplitDirectionSchema2::Horizontal,
                ..
            })
        )
    {
        bail!("two schema 2 panes require a horizontal split");
    }
    if let Some(split) = &layout.split
        && (!split.ratio.is_finite() || !(0.1..=0.9).contains(&split.ratio))
    {
        bail!("schema 2 split ratio must be finite and within 0.1..=0.9");
    }
    let mut membership = vec![0usize; tab_count];
    for pane in &layout.panes {
        if pane.tab_indices.is_empty() || !pane.tab_indices.contains(&pane.selected_tab) {
            bail!("schema 2 panes must be nonempty and contain their selection");
        }
        for tab_index in &pane.tab_indices {
            increment_membership(&mut membership, *tab_index)?;
        }
    }
    if membership.iter().any(|count| *count != 1) {
        bail!("each schema 2 tab must belong to exactly one pane");
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
                entries: vec![SessionEntry::Single { tab_index: 0 }],
                active_tab: Some(0),
            },
            ..SessionState::default()
        }
    }

    #[test]
    fn default_and_empty_layout_are_schema_three() {
        let state = SessionState::default();
        assert_eq!(state.schema_version, 3);
        assert!(state.validate().is_ok());
        assert!(state.layout.entries.is_empty());
    }

    #[test]
    fn multiple_horizontal_and_vertical_split_entries_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let state = SessionState {
            tabs: (0..5)
                .map(|index| tab(directory.path().join(format!("{index}.pdf"))))
                .collect(),
            layout: SessionLayout {
                entries: vec![
                    SessionEntry::Split {
                        tab_indices: [0, 1],
                        direction: SplitDirection::Horizontal,
                        ratio: 0.4,
                        focused_tab: 1,
                    },
                    SessionEntry::Single { tab_index: 2 },
                    SessionEntry::Split {
                        tab_indices: [3, 4],
                        direction: SplitDirection::Vertical,
                        ratio: 0.6,
                        focused_tab: 4,
                    },
                ],
                active_tab: Some(4),
            },
            ..SessionState::default()
        };
        assert_eq!(
            SessionState::decode(serde_json::to_value(&state).unwrap()).unwrap(),
            state
        );
    }

    #[test]
    fn schema_one_migrates_to_single_entries_and_defaults_colors() {
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
        assert_eq!(state.schema_version, 3);
        assert_eq!(
            state.layout.entries,
            vec![SessionEntry::Single { tab_index: 0 }]
        );
        assert_eq!(state.layout.active_tab, Some(0));
        assert!(state.recent_annotation_colors.is_empty());
    }

    #[test]
    fn schema_two_migrates_visible_pair_and_preserves_old_bar_order() {
        let directory = tempfile::tempdir().unwrap();
        let tabs = (0..4)
            .map(|index| {
                serde_json::to_value(tab(directory.path().join(format!("{index}.pdf")))).unwrap()
            })
            .collect::<Vec<_>>();
        let json = serde_json::json!({
            "schema_version": 2,
            "restore_enabled": true,
            "sidebar_open": false,
            "sidebar_tab": "Outline",
            "tabs": tabs,
            "layout": {
                "panes": [
                    {"tab_indices": [0, 2], "selected_tab": 2},
                    {"tab_indices": [1, 3], "selected_tab": 1}
                ],
                "focused_pane": 1,
                "split": {"direction": "Horizontal", "ratio": 0.35}
            }
        });
        let state = SessionState::decode(json).unwrap();
        assert_eq!(state.schema_version, 3);
        assert_eq!(
            state.layout.entries,
            vec![
                SessionEntry::Single { tab_index: 0 },
                SessionEntry::Split {
                    tab_indices: [2, 1],
                    direction: SplitDirection::Horizontal,
                    ratio: 0.35,
                    focused_tab: 1,
                },
                SessionEntry::Single { tab_index: 3 },
            ]
        );
        assert_eq!(state.layout.active_tab, Some(1));
    }

    #[test]
    fn malformed_membership_focus_and_split_values_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = one_tab_state(directory.path());
        state.layout.entries.clear();
        assert!(state.validate().is_err());
        state.layout.entries = vec![SessionEntry::Single { tab_index: 1 }];
        assert!(state.validate().is_err());
        state.layout.entries = vec![SessionEntry::Split {
            tab_indices: [0, 0],
            direction: SplitDirection::Vertical,
            ratio: 0.5,
            focused_tab: 0,
        }];
        assert!(state.validate().is_err());
        state.tabs.push(tab(directory.path().join("second.pdf")));
        state.layout.entries = vec![SessionEntry::Split {
            tab_indices: [0, 1],
            direction: SplitDirection::Vertical,
            ratio: 0.09,
            focused_tab: 0,
        }];
        assert!(state.validate().is_err());
        state.layout.entries = vec![SessionEntry::Split {
            tab_indices: [0, 1],
            direction: SplitDirection::Vertical,
            ratio: 0.5,
            focused_tab: 0,
        }];
        state.layout.active_tab = Some(1);
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
