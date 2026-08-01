use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// 文書レジストリ位置とは独立した、再利用されないタブ識別子。
pub(crate) struct TabId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// 最大2ペインのランタイム識別子。
pub(crate) struct PaneId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 分割した新ペインを既存ペインの前後どちらへ置くか。
pub(crate) enum SplitPlacement {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    id: TabId,
    path: PathBuf,
}

impl Tab {
    /// このタブの表示位置とは独立した安定識別子を返す。
    pub(crate) fn id(&self) -> TabId {
        self.id
    }

    /// このタブが表す正規パスを返す。
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Debug)]
struct Pane {
    id: PaneId,
    tabs: Vec<TabId>,
    selected: Option<TabId>,
}

#[derive(Debug)]
pub(crate) struct TabState {
    tabs: Vec<Tab>,
    panes: Vec<Pane>,
    focused: PaneId,
    next_tab_id: u64,
    next_pane_id: u64,
    split_ratio: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenTabResult {
    Opened(usize),
    SelectedExisting(usize),
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            panes: vec![Pane {
                id: PaneId(0),
                tabs: Vec::new(),
                selected: None,
            }],
            focused: PaneId(0),
            next_tab_id: 0,
            next_pane_id: 1,
            split_ratio: 0.5,
        }
    }
}

impl TabState {
    /// 選択タブのない空のタブ状態を作成する。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 文書レジストリ順のタブを返す。ペイン操作ではこの順序を変更しない。
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// フォーカス中ペインの選択タブをレジストリインデックスで返す。
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.pane_selected(self.focused)
            .and_then(|tab_id| self.tab_registry_index(tab_id))
    }

    /// 既存PDFのパスを正規化し、そのタブを開くか選択する。
    pub(crate) fn open(&mut self, path: impl AsRef<Path>) -> io::Result<OpenTabResult> {
        let canonical_path = std::fs::canonicalize(path)?;

        if let Some(index) = self.tabs.iter().position(|tab| tab.path == canonical_path) {
            let tab_id = self.tabs[index].id;
            self.select_tab(tab_id);
            return Ok(OpenTabResult::SelectedExisting(index));
        }

        let id = TabId(self.next_tab_id);
        self.next_tab_id = self.next_tab_id.checked_add(1).expect("tab id exhausted");
        self.tabs.push(Tab {
            id,
            path: canonical_path,
        });
        let pane = self
            .panes
            .iter_mut()
            .find(|pane| pane.id == self.focused)
            .expect("focused pane must exist");
        pane.tabs.push(id);
        pane.selected = Some(id);
        Ok(OpenTabResult::Opened(self.tabs.len() - 1))
    }

    /// インデックスで開いているタブを選択し、そのペインをフォーカスする。
    pub(crate) fn select(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        self.select_tab(tab.id)
    }

    /// インデックスでタブを閉じ、所属ペインの選択を有効に保ったまま閉じたタブを返す。
    pub(crate) fn close(&mut self, index: usize) -> Option<Tab> {
        let tab_id = self.tabs.get(index)?.id;
        let removed = self.tabs.remove(index);
        let pane_index = self
            .panes
            .iter()
            .position(|pane| pane.tabs.contains(&tab_id))?;
        let pane = &mut self.panes[pane_index];
        let tab_index = pane.tabs.iter().position(|id| *id == tab_id).unwrap();
        pane.tabs.remove(tab_index);
        if pane.selected == Some(tab_id) {
            pane.selected = pane
                .tabs
                .get(tab_index)
                .copied()
                .or_else(|| pane.tabs.get(tab_index.saturating_sub(1)).copied());
        }
        self.collapse_empty_panes();
        Some(removed)
    }

    /// 現在のペイン順にある識別子を返す。
    pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
        self.panes.iter().map(|pane| pane.id).collect()
    }

    /// 現在フォーカスされているペインを返す。
    pub(crate) fn focused_pane(&self) -> PaneId {
        self.focused
    }

    /// ペイン内のタブ識別子を表示順で返す。
    pub(crate) fn pane_tab_ids(&self, pane_id: PaneId) -> Option<&[TabId]> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| pane.tabs.as_slice())
    }

    /// ペインで選択されているタブ識別子を返す。
    pub(crate) fn pane_selected(&self, pane_id: PaneId) -> Option<TabId> {
        self.panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .and_then(|pane| pane.selected)
    }

    /// 指定ペインにフォーカスを移す。存在しない識別子は変更しない。
    pub(crate) fn focus_pane(&mut self, pane_id: PaneId) -> bool {
        if self.panes.iter().any(|pane| pane.id == pane_id) {
            self.focused = pane_id;
            true
        } else {
            false
        }
    }

    /// 指定ペインのタブを選択し、同じペインをフォーカスする。
    pub(crate) fn select_in_pane(&mut self, pane_id: PaneId, tab_id: TabId) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return false;
        };
        if !pane.tabs.contains(&tab_id) {
            return false;
        }
        pane.selected = Some(tab_id);
        self.focused = pane_id;
        true
    }

    /// タブの所属ペインを返す。
    pub(crate) fn tab_pane(&self, tab_id: TabId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|pane| pane.tabs.contains(&tab_id))
            .map(|pane| pane.id)
    }

    /// タブ識別子に対応する文書レジストリ位置を返す。
    pub(crate) fn tab_registry_index(&self, tab_id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == tab_id)
    }

    /// 各ペインの選択タブを表示順で返す。
    pub(crate) fn visible_selected_tab_ids(&self) -> Vec<TabId> {
        self.panes.iter().filter_map(|pane| pane.selected).collect()
    }

    /// ペイン内のタブを指定した挿入ギャップへ移す。
    pub(crate) fn reorder(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        insertion_index: usize,
    ) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return false;
        };
        let Some(current_index) = pane.tabs.iter().position(|id| *id == tab_id) else {
            return false;
        };
        if insertion_index > pane.tabs.len() {
            return false;
        }
        // ギャップは除去前の座標で受け取り、元要素より後ろなら除去分だけ補正する。
        let target_index = insertion_index.min(pane.tabs.len() - 1);
        if target_index == current_index || target_index == current_index + 1 {
            return true;
        }
        let moved = pane.tabs.remove(current_index);
        let adjusted = if insertion_index > current_index {
            insertion_index - 1
        } else {
            insertion_index
        };
        pane.tabs.insert(adjusted, moved);
        true
    }

    /// 単一ペインを指定位置で分割し、指定タブを新ペインへ移す。
    pub(crate) fn split(&mut self, tab_id: TabId, placement: SplitPlacement) -> bool {
        // 任意個ペインへ拡張せず、既存の単一ペインを必ず二つへ分ける。
        if self.panes.len() != 1 {
            return false;
        }
        let source_index = 0;
        if self.panes[source_index].tabs.len() <= 1
            || !self.panes[source_index].tabs.contains(&tab_id)
        {
            return false;
        }
        let tab_index = self.panes[source_index]
            .tabs
            .iter()
            .position(|id| *id == tab_id)
            .unwrap();
        self.panes[source_index].tabs.remove(tab_index);
        if self.panes[source_index].selected == Some(tab_id) {
            self.panes[source_index].selected = self.panes[source_index]
                .tabs
                .get(tab_index)
                .copied()
                .or_else(|| {
                    self.panes[source_index]
                        .tabs
                        .get(tab_index.saturating_sub(1))
                        .copied()
                });
        }
        let new_pane = Pane {
            id: PaneId(self.next_pane_id),
            tabs: vec![tab_id],
            selected: Some(tab_id),
        };
        self.next_pane_id = self.next_pane_id.checked_add(1).expect("pane id exhausted");
        match placement {
            SplitPlacement::Before => self.panes.insert(0, new_pane),
            SplitPlacement::After => self.panes.push(new_pane),
        }
        self.focused = PaneId(self.next_pane_id - 1);
        true
    }

    /// タブを指定ペインの挿入ギャップへ移す。
    pub(crate) fn move_tab(
        &mut self,
        tab_id: TabId,
        target_pane_id: PaneId,
        insertion_index: usize,
    ) -> bool {
        let Some(source_index) = self
            .panes
            .iter()
            .position(|pane| pane.tabs.contains(&tab_id))
        else {
            return false;
        };
        let Some(target_index) = self.panes.iter().position(|pane| pane.id == target_pane_id)
        else {
            return false;
        };
        if insertion_index > self.panes[target_index].tabs.len() {
            return false;
        }
        if source_index == target_index {
            return self.reorder(target_pane_id, tab_id, insertion_index);
        }
        let source_tab_index = self.panes[source_index]
            .tabs
            .iter()
            .position(|id| *id == tab_id)
            .unwrap();
        self.panes[source_index].tabs.remove(source_tab_index);
        if self.panes[source_index].selected == Some(tab_id) {
            self.panes[source_index].selected = self.panes[source_index]
                .tabs
                .get(source_tab_index)
                .copied()
                .or_else(|| {
                    self.panes[source_index]
                        .tabs
                        .get(source_tab_index.saturating_sub(1))
                        .copied()
                });
        }
        let target_index = self
            .panes
            .iter()
            .position(|pane| pane.id == target_pane_id)
            .unwrap();
        self.panes[target_index]
            .tabs
            .insert(insertion_index, tab_id);
        self.panes[target_index].selected = Some(tab_id);
        self.focused = target_pane_id;
        self.collapse_empty_panes();
        true
    }

    /// 現在の分割比率を返す。
    pub(crate) fn split_ratio(&self) -> f32 {
        self.split_ratio
    }

    /// 有限かつ0.1..=0.9の分割比率だけを適用する。
    pub(crate) fn set_split_ratio(&mut self, ratio: f32) -> bool {
        // 0.1未満・0.9超では片側が極端に狭くなるため、UIの最小幅計算とは分離して保存値を制限する。
        if !ratio.is_finite() || !(0.1..=0.9).contains(&ratio) {
            return false;
        }
        self.split_ratio = ratio;
        true
    }

    /// セッション復元用に全タブの配置・選択・フォーカス・比率を原子的に適用する。
    pub(crate) fn restore_layout(
        &mut self,
        layouts: Vec<(Vec<TabId>, TabId)>,
        focused_pane_index: usize,
        ratio: f32,
    ) -> bool {
        if !ratio.is_finite() || !(0.1..=0.9).contains(&ratio) || layouts.len() > 2 {
            return false;
        }
        if self.tabs.is_empty() {
            if !layouts.is_empty() || focused_pane_index != 0 {
                return false;
            }
            self.split_ratio = ratio;
            self.focused = PaneId(0);
            return true;
        }
        if layouts.is_empty()
            || !layouts
                .iter()
                .all(|(tabs, selected)| !tabs.is_empty() && tabs.contains(selected))
        {
            return false;
        }
        if focused_pane_index >= layouts.len() {
            return false;
        }
        let mut seen_tabs = std::collections::HashSet::new();
        for (tabs, _) in &layouts {
            for tab_id in tabs {
                if self.tab_registry_index(*tab_id).is_none() || !seen_tabs.insert(*tab_id) {
                    return false;
                }
            }
        }
        if seen_tabs.len() != self.tabs.len() {
            return false;
        }
        let primary_id = self.panes.first().map(|pane| pane.id).unwrap_or(PaneId(0));
        let mut next_pane_id = self.next_pane_id;
        let mut panes = Vec::with_capacity(layouts.len());
        for (index, (tabs, selected)) in layouts.into_iter().enumerate() {
            let id = if index == 0 {
                primary_id
            } else {
                let Some(next_id) = next_pane_id.checked_add(1) else {
                    return false;
                };
                let id = PaneId(next_pane_id);
                next_pane_id = next_id;
                id
            };
            panes.push(Pane {
                id,
                tabs,
                selected: Some(selected),
            });
        }
        self.panes = panes;
        self.focused = self.panes[focused_pane_index].id;
        self.next_pane_id = next_pane_id;
        self.split_ratio = ratio;
        true
    }

    fn select_tab(&mut self, tab_id: TabId) -> bool {
        let Some(pane_id) = self.tab_pane(tab_id) else {
            return false;
        };
        self.select_in_pane(pane_id, tab_id)
    }

    fn collapse_empty_panes(&mut self) {
        // 空ペインを残すと「非空ペインの選択」不変条件を保てないため、常に単一へ縮約する。
        self.panes.retain(|pane| !pane.tabs.is_empty());
        if self.panes.is_empty() {
            self.panes.push(Pane {
                id: PaneId(0),
                tabs: Vec::new(),
                selected: None,
            });
            self.focused = PaneId(0);
        } else if !self.panes.iter().any(|pane| pane.id == self.focused) {
            self.focused = self.panes[0].id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn existing_file(name: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join(name);
        File::create(&path).expect("temporary file should be created");
        (directory, path)
    }

    fn open_files(state: &mut TabState, count: usize) -> (tempfile::TempDir, Vec<TabId>) {
        let directory = tempfile::tempdir().unwrap();
        let mut ids = Vec::new();
        for index in 0..count {
            let path = directory.path().join(format!("{index}.pdf"));
            File::create(&path).unwrap();
            state.open(path).unwrap();
            ids.push(state.tabs[index].id());
        }
        (directory, ids)
    }

    #[test]
    fn opening_same_canonical_path_selects_existing_tab() {
        let (directory, path) = existing_file("paper.pdf");
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        let alternate_path = directory.path().join("folder").join("..").join("paper.pdf");
        let mut state = TabState::new();

        assert_eq!(state.open(&path).unwrap(), OpenTabResult::Opened(0));
        assert_eq!(
            state.open(alternate_path).unwrap(),
            OpenTabResult::SelectedExisting(0)
        );
        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn missing_path_returns_canonicalization_error_without_opening_tab() {
        let directory = tempfile::tempdir().unwrap();
        let missing_path = directory.path().join("missing.pdf");
        let mut state = TabState::new();

        assert!(state.open(missing_path).is_err());
        assert!(state.tabs().is_empty());
    }

    #[test]
    fn opening_more_than_fifty_tabs_keeps_all_existing_tabs() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = TabState::new();
        let paths: Vec<_> = (0..=50)
            .map(|index| {
                let path = directory.path().join(format!("{index}.pdf"));
                File::create(&path).unwrap();
                path
            })
            .collect();

        for path in &paths {
            assert!(matches!(state.open(path), Ok(OpenTabResult::Opened(_))));
        }
        assert_eq!(state.tabs().len(), 51);
    }

    #[test]
    fn tab_ids_are_monotonic_and_not_reused_after_close() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        state.close(0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new.pdf");
        File::create(&path).unwrap();
        state.open(path).unwrap();
        assert!(state.tabs()[1].id() > ids[1]);
        assert_ne!(state.tabs()[1].id(), ids[0]);
    }

    #[test]
    fn selecting_and_closing_tabs_preserves_a_valid_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = TabState::new();
        for index in 0..3 {
            let path = directory.path().join(format!("{index}.pdf"));
            File::create(&path).unwrap();
            state.open(path).unwrap();
        }

        assert!(state.select(1));
        assert!(!state.select(3));
        state.close(0);
        assert_eq!(state.selected_index(), Some(0));
        state.close(0);
        assert_eq!(state.selected_index(), Some(0));
        state.close(0);
        assert_eq!(state.selected_index(), None);
        assert_eq!(state.pane_ids().len(), 1);
        assert_eq!(state.pane_ids(), vec![PaneId(0)]);
        assert!(state.pane_tab_ids(PaneId(0)).unwrap().is_empty());
    }

    #[test]
    fn pane_operations_preserve_registry_order_and_selection_rules() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        let primary = state.pane_ids()[0];
        assert!(state.reorder(primary, ids[0], 3));
        assert_eq!(
            state.pane_tab_ids(primary).unwrap(),
            &[ids[1], ids[2], ids[0]]
        );
        assert_eq!(state.tabs().iter().map(Tab::id).collect::<Vec<_>>(), ids);
        assert!(state.split(ids[2], SplitPlacement::After));
        assert_eq!(state.pane_ids().len(), 2);
        let secondary = state.pane_ids()[1];
        assert_eq!(state.pane_tab_ids(secondary).unwrap(), &[ids[2]]);
        assert_eq!(state.focused_pane(), secondary);
        assert!(state.move_tab(ids[2], primary, 0));
        assert_eq!(state.pane_ids().len(), 1);
        assert_eq!(state.focused_pane(), primary);
    }

    #[test]
    fn reorder_supports_all_gap_directions_without_changing_selection() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 4);
        let pane = state.pane_ids()[0];
        state.select(1);
        assert!(state.reorder(pane, ids[0], 4));
        assert_eq!(
            state.pane_tab_ids(pane).unwrap(),
            &[ids[1], ids[2], ids[3], ids[0]]
        );
        assert!(state.reorder(pane, ids[0], 0));
        assert_eq!(
            state.pane_tab_ids(pane).unwrap(),
            &[ids[0], ids[1], ids[2], ids[3]]
        );
        assert!(state.reorder(pane, ids[1], 3));
        assert_eq!(
            state.pane_tab_ids(pane).unwrap(),
            &[ids[0], ids[2], ids[1], ids[3]]
        );
        assert!(state.reorder(pane, ids[1], 2));
        assert_eq!(
            state.pane_tab_ids(pane).unwrap(),
            &[ids[0], ids[2], ids[1], ids[3]]
        );
        assert_eq!(state.pane_selected(pane), Some(ids[1]));
    }

    #[test]
    fn split_before_and_after_reject_single_or_already_split_panes() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        let primary = state.pane_ids()[0];
        assert!(state.split(ids[0], SplitPlacement::Before));
        assert_eq!(state.pane_tab_ids(state.pane_ids()[0]).unwrap(), &[ids[0]]);
        assert_eq!(state.pane_tab_ids(state.pane_ids()[1]).unwrap(), &[ids[1]]);
        assert!(!state.split(ids[1], SplitPlacement::After));
        assert!(!state.split(ids[0], SplitPlacement::After));

        let mut one = TabState::new();
        let (_directory, one_id) = open_files(&mut one, 1);
        assert!(!one.split(one_id[0], SplitPlacement::Before));
        assert_eq!(one.pane_ids(), vec![PaneId(0)]);
        assert_eq!(primary, PaneId(0));
    }

    #[test]
    fn invalid_layout_variants_leave_state_unchanged() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        let before = state.pane_tab_ids(state.pane_ids()[0]).unwrap().to_vec();
        let invalid = [
            (vec![(Vec::new(), ids[0])], 0, 0.5),
            (vec![(vec![ids[0], ids[0]], ids[0])], 0, 0.5),
            (vec![(vec![ids[0]], ids[0])], 0, 0.5),
            (vec![(vec![ids[0], ids[1], ids[2]], TabId(99))], 0, 0.5),
            (vec![(vec![ids[0]], ids[0]), (vec![ids[1]], ids[1])], 2, 0.5),
            (
                vec![
                    (vec![ids[0]], ids[0]),
                    (vec![ids[1]], ids[1]),
                    (vec![ids[2]], ids[2]),
                ],
                0,
                0.5,
            ),
            (vec![(vec![ids[0], ids[1], ids[2]], ids[0])], 0, f32::NAN),
            (vec![(vec![ids[0], ids[1], ids[2]], ids[0])], 0, 0.09),
            (vec![(vec![ids[0], ids[1], ids[2]], ids[0])], 0, 0.91),
        ];
        for (layouts, focused, ratio) in invalid {
            assert!(!state.restore_layout(layouts, focused, ratio));
            assert_eq!(state.pane_tab_ids(state.pane_ids()[0]).unwrap(), before);
            assert_eq!(state.pane_ids().len(), 1);
        }
    }

    #[test]
    fn cross_pane_move_inserts_at_each_gap_and_collapses_empty_source() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 4);
        let primary = state.pane_ids()[0];
        assert!(state.split(ids[3], SplitPlacement::After));
        assert!(state.move_tab(ids[3], primary, 1));
        assert_eq!(
            state.pane_tab_ids(primary).unwrap(),
            &[ids[0], ids[3], ids[1], ids[2]]
        );
        assert_eq!(state.pane_ids().len(), 1);

        assert!(state.split(ids[0], SplitPlacement::After));
        let secondary = state.pane_ids()[1];
        assert!(state.move_tab(ids[1], secondary, 0));
        assert_eq!(state.pane_tab_ids(secondary).unwrap(), &[ids[1], ids[0]]);
        assert!(state.move_tab(ids[0], secondary, 2));
        assert_eq!(state.pane_tab_ids(secondary).unwrap(), &[ids[1], ids[0]]);
        assert_eq!(state.tab_pane(ids[0]), Some(secondary));
    }

    #[test]
    fn invalid_layout_does_not_change_state() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        let before = state.pane_tab_ids(state.pane_ids()[0]).unwrap().to_vec();
        assert!(!state.restore_layout(
            vec![(vec![ids[0]], ids[0]), (vec![ids[1]], ids[1])],
            2,
            0.5,
        ));
        assert_eq!(state.pane_tab_ids(state.pane_ids()[0]).unwrap(), before);
        assert!(!state.set_split_ratio(f32::NAN));
        assert_eq!(state.split_ratio(), 0.5);
    }

    #[test]
    fn restore_layout_issues_internal_pane_ids_without_future_split_collision() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(state.restore_layout(vec![(vec![ids[0], ids[1], ids[2]], ids[2])], 0, 0.6,));
        assert_eq!(state.pane_ids().len(), 1);
        let primary = state.pane_ids()[0];
        assert!(state.split(ids[0], SplitPlacement::After));
        let restored_ids = state.pane_ids();
        assert_eq!(restored_ids[0], primary);
        assert_ne!(restored_ids[0], restored_ids[1]);
        assert!(state.split_ratio() == 0.6);
    }
}
