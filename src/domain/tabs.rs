use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// 文書レジストリ位置とは独立した、再利用されないタブ識別子。
pub(crate) struct TabId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// 複数の分割セットを区別し、eguiの永続IDにも使うランタイム識別子。
pub(crate) struct SplitGroupId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SplitSide {
    First,
    Second,
}

impl SplitSide {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitPlacement {
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitPlacement {
    pub(crate) fn direction(self) -> SplitDirection {
        match self {
            Self::Left | Self::Right => SplitDirection::Horizontal,
            Self::Top | Self::Bottom => SplitDirection::Vertical,
        }
    }

    fn dragged_first(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SplitGroup {
    id: SplitGroupId,
    tabs: [TabId; 2],
    direction: SplitDirection,
    ratio: f32,
    focused: SplitSide,
}

impl SplitGroup {
    pub(crate) fn id(&self) -> SplitGroupId {
        self.id
    }

    pub(crate) fn tabs(&self) -> [TabId; 2] {
        self.tabs
    }

    pub(crate) fn tab(&self, side: SplitSide) -> TabId {
        self.tabs[side.index()]
    }

    pub(crate) fn direction(&self) -> SplitDirection {
        self.direction
    }

    pub(crate) fn ratio(&self) -> f32 {
        self.ratio
    }

    pub(crate) fn focused(&self) -> SplitSide {
        self.focused
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TabEntry {
    Single(TabId),
    Split(SplitGroup),
}

impl TabEntry {
    pub(crate) fn tab_ids(&self) -> Vec<TabId> {
        match self {
            Self::Single(tab_id) => vec![*tab_id],
            Self::Split(group) => group.tabs.to_vec(),
        }
    }

    fn contains(&self, tab_id: TabId) -> bool {
        match self {
            Self::Single(entry_tab) => *entry_tab == tab_id,
            Self::Split(group) => group.tabs.contains(&tab_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RestoredTabEntry {
    Single(TabId),
    Split {
        tabs: [TabId; 2],
        direction: SplitDirection,
        ratio: f32,
        focused: SplitSide,
    },
}

#[derive(Debug, Default)]
pub(crate) struct TabState {
    tabs: Vec<Tab>,
    entries: Vec<TabEntry>,
    active: Option<TabId>,
    next_tab_id: u64,
    next_split_group_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenTabResult {
    Opened(usize),
    SelectedExisting(usize),
}

impl TabState {
    /// 選択タブのない空のタブ状態を作成する。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 文書レジストリ順のタブを返す。表示順の変更ではこの順序を変えない。
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// 共有タブバーへ描画する単独タブと分割セットを表示順で返す。
    pub(crate) fn entries(&self) -> &[TabEntry] {
        &self.entries
    }

    pub(crate) fn active_tab_id(&self) -> Option<TabId> {
        self.active
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.active
            .and_then(|tab_id| self.tab_registry_index(tab_id))
    }

    pub(crate) fn active_entry(&self) -> Option<&TabEntry> {
        let active = self.active?;
        self.entries.iter().find(|entry| entry.contains(active))
    }

    pub(crate) fn active_split(&self) -> Option<&SplitGroup> {
        match self.active_entry()? {
            TabEntry::Split(group) => Some(group),
            TabEntry::Single(_) => None,
        }
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
        let insertion = self
            .active
            .and_then(|active| self.entry_index_for_tab(active))
            .map_or(self.entries.len(), |index| index + 1);
        self.entries.insert(insertion, TabEntry::Single(id));
        self.active = Some(id);
        Ok(OpenTabResult::Opened(self.tabs.len() - 1))
    }

    /// 同じファイルを追跡していることが確認できた後だけ、タブの表示・保存用パスを更新する。
    /// TabId と分割構成はパスではなくレジストリ順に結び付くため、ここで再作成してはならない。
    pub(crate) fn rebind_path(&mut self, index: usize, path: impl AsRef<Path>) -> io::Result<()> {
        let canonical_path = std::fs::canonicalize(path)?;
        if self
            .tabs
            .iter()
            .enumerate()
            .any(|(other, tab)| other != index && tab.path == canonical_path)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a tab for the renamed PDF already exists",
            ));
        }
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tab index is unavailable"))?;
        tab.path = canonical_path;
        Ok(())
    }

    pub(crate) fn select(&mut self, index: usize) -> bool {
        let Some(tab_id) = self.tabs.get(index).map(Tab::id) else {
            return false;
        };
        self.select_tab(tab_id)
    }

    pub(crate) fn select_tab(&mut self, tab_id: TabId) -> bool {
        let Some(entry_index) = self.entry_index_for_tab(tab_id) else {
            return false;
        };
        if let TabEntry::Split(group) = &mut self.entries[entry_index] {
            group.focused = if group.tabs[0] == tab_id {
                SplitSide::First
            } else {
                SplitSide::Second
            };
        }
        self.active = Some(tab_id);
        true
    }

    /// タブを閉じる。分割メンバーの相方は同じ位置の単独タブへ縮約する。
    pub(crate) fn close(&mut self, index: usize) -> Option<Tab> {
        let tab_id = self.tabs.get(index)?.id;
        let entry_index = self.entry_index_for_tab(tab_id)?;
        let replacement = match &self.entries[entry_index] {
            TabEntry::Single(_) => None,
            TabEntry::Split(group) => group.tabs.iter().copied().find(|id| *id != tab_id),
        };
        self.entries.remove(entry_index);
        if let Some(sibling) = replacement {
            self.entries.insert(entry_index, TabEntry::Single(sibling));
        }
        let removed = self.tabs.remove(index);

        if self.active == Some(tab_id) {
            self.active = replacement.or_else(|| {
                self.entries
                    .get(entry_index)
                    .or_else(|| self.entries.get(entry_index.saturating_sub(1)))
                    .and_then(Self::entry_focus_tab)
            });
        }
        Some(removed)
    }

    pub(crate) fn tab_registry_index(&self, tab_id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == tab_id)
    }

    #[cfg(test)]
    fn display_tab_ids(&self) -> Vec<TabId> {
        self.entries.iter().flat_map(TabEntry::tab_ids).collect()
    }

    pub(crate) fn visible_tab_ids(&self) -> Vec<TabId> {
        match self.active_entry() {
            Some(TabEntry::Single(tab_id)) => vec![*tab_id],
            Some(TabEntry::Split(group)) => group.tabs.to_vec(),
            None => Vec::new(),
        }
    }

    pub(crate) fn split_for_tab(&self, tab_id: TabId) -> Option<&SplitGroup> {
        match self.entries.get(self.entry_index_for_tab(tab_id)?)? {
            TabEntry::Split(group) => Some(group),
            TabEntry::Single(_) => None,
        }
    }

    pub(crate) fn split_group(&self, group_id: SplitGroupId) -> Option<&SplitGroup> {
        self.entries.iter().find_map(|entry| match entry {
            TabEntry::Split(group) if group.id == group_id => Some(group),
            TabEntry::Single(_) | TabEntry::Split(_) => None,
        })
    }

    pub(crate) fn create_split(
        &mut self,
        dragged_tab: TabId,
        target_tab: TabId,
        placement: SplitPlacement,
    ) -> bool {
        if dragged_tab == target_tab
            || self.tab_registry_index(dragged_tab).is_none()
            || !matches!(
                self.entries
                    .get(self.entry_index_for_tab(target_tab).unwrap()),
                Some(TabEntry::Single(_))
            )
        {
            return false;
        }

        self.extract_source_for_new_split(dragged_tab);
        let Some(target_index) = self.entry_index_for_tab(target_tab) else {
            return false;
        };
        let tabs = if placement.dragged_first() {
            [dragged_tab, target_tab]
        } else {
            [target_tab, dragged_tab]
        };
        let focused = if tabs[0] == dragged_tab {
            SplitSide::First
        } else {
            SplitSide::Second
        };
        let group = SplitGroup {
            id: SplitGroupId(self.next_split_group_id),
            tabs,
            direction: placement.direction(),
            ratio: 0.5,
            focused,
        };
        self.next_split_group_id = self
            .next_split_group_id
            .checked_add(1)
            .expect("split group id exhausted");
        self.entries[target_index] = TabEntry::Split(group);
        self.active = Some(dragged_tab);
        true
    }

    /// PDF面へのdropで、対象メンバーとドラッグ元のタブを原子的に交換する。
    pub(crate) fn replace_split_member(
        &mut self,
        group_id: SplitGroupId,
        side: SplitSide,
        dragged_tab: TabId,
    ) -> Option<TabId> {
        let target_entry = self.entry_index_for_group(group_id)?;
        let target_tab = match &self.entries[target_entry] {
            TabEntry::Split(group) => group.tab(side),
            TabEntry::Single(_) => return None,
        };
        if target_tab == dragged_tab {
            return None;
        }

        let source_entry = self.entry_index_for_tab(dragged_tab)?;
        if source_entry == target_entry {
            self.swap_split_members(group_id);
            // 配置交換後のsideを基準にfocusedも更新し、activeとCtrl+Tabの復元先を
            // 同じドラッグ対象へ揃える。
            self.select_tab(dragged_tab);
            return Some(target_tab);
        }
        self.replace_tab_in_entry(source_entry, dragged_tab, target_tab)?;
        self.replace_tab_in_entry(target_entry, target_tab, dragged_tab)?;
        if let TabEntry::Split(group) = &mut self.entries[target_entry] {
            group.focused = side;
        }
        self.active = Some(dragged_tab);
        Some(target_tab)
    }

    pub(crate) fn swap_split_members(&mut self, group_id: SplitGroupId) -> bool {
        let Some(entry_index) = self.entry_index_for_group(group_id) else {
            return false;
        };
        let TabEntry::Split(group) = &mut self.entries[entry_index] else {
            return false;
        };
        group.tabs.swap(0, 1);
        group.focused = match group.focused {
            SplitSide::First => SplitSide::Second,
            SplitSide::Second => SplitSide::First,
        };
        true
    }

    pub(crate) fn unsplit(&mut self, group_id: SplitGroupId) -> bool {
        let Some(entry_index) = self.entry_index_for_group(group_id) else {
            return false;
        };
        let TabEntry::Split(group) = self.entries.remove(entry_index) else {
            return false;
        };
        self.entries
            .insert(entry_index, TabEntry::Single(group.tabs[1]));
        self.entries
            .insert(entry_index, TabEntry::Single(group.tabs[0]));
        true
    }

    /// ペアの片方を共有タブ列の外側gapへ移し、相方も単独タブへ戻す。
    pub(crate) fn extract_split_member(&mut self, tab_id: TabId, insertion_index: usize) -> bool {
        if insertion_index > self.entries.len() {
            return false;
        }
        let Some(entry_index) = self.entry_index_for_tab(tab_id) else {
            return false;
        };
        let TabEntry::Split(group) = self.entries[entry_index].clone() else {
            return false;
        };
        let sibling = group.tabs.into_iter().find(|id| *id != tab_id).unwrap();
        self.entries[entry_index] = TabEntry::Single(sibling);
        self.entries
            .insert(insertion_index, TabEntry::Single(tab_id));
        true
    }

    pub(crate) fn reorder_single(&mut self, tab_id: TabId, insertion_index: usize) -> bool {
        if insertion_index > self.entries.len() {
            return false;
        }
        let Some(current_index) = self.entry_index_for_tab(tab_id) else {
            return false;
        };
        if !matches!(self.entries[current_index], TabEntry::Single(_)) {
            return false;
        }
        Self::move_entry(&mut self.entries, current_index, insertion_index)
    }

    pub(crate) fn reorder_split(&mut self, group_id: SplitGroupId, insertion_index: usize) -> bool {
        if insertion_index > self.entries.len() {
            return false;
        }
        let Some(current_index) = self.entry_index_for_group(group_id) else {
            return false;
        };
        Self::move_entry(&mut self.entries, current_index, insertion_index)
    }

    pub(crate) fn set_split_direction(
        &mut self,
        group_id: SplitGroupId,
        direction: SplitDirection,
    ) -> bool {
        let Some(entry_index) = self.entry_index_for_group(group_id) else {
            return false;
        };
        let TabEntry::Split(group) = &mut self.entries[entry_index] else {
            return false;
        };
        group.direction = direction;
        true
    }

    pub(crate) fn set_split_ratio(&mut self, group_id: SplitGroupId, ratio: f32) -> bool {
        if !valid_ratio(ratio) {
            return false;
        }
        let Some(entry_index) = self.entry_index_for_group(group_id) else {
            return false;
        };
        let TabEntry::Split(group) = &mut self.entries[entry_index] else {
            return false;
        };
        group.ratio = ratio;
        true
    }

    /// Ctrl+Tabでは分割セットを一つの巡回単位とし、保存されたfocus側へ戻る。
    pub(crate) fn next_entry_tab(&self) -> Option<TabId> {
        if self.entries.is_empty() {
            return None;
        }
        let current = self
            .active
            .and_then(|active| self.entry_index_for_tab(active))
            .unwrap_or(0);
        let next = (current + 1) % self.entries.len();
        Self::entry_focus_tab(&self.entries[next])
    }

    /// セッション復元用の全エントリを、不変条件を確認して原子的に適用する。
    pub(crate) fn restore_layout(
        &mut self,
        restored: Vec<RestoredTabEntry>,
        active: Option<TabId>,
    ) -> bool {
        if self.tabs.is_empty() {
            if !restored.is_empty() || active.is_some() {
                return false;
            }
            self.entries.clear();
            self.active = None;
            return true;
        }
        let Some(active) = active else {
            return false;
        };
        let registry = self.tabs.iter().map(Tab::id).collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for entry in &restored {
            match entry {
                RestoredTabEntry::Single(tab_id) => {
                    if !registry.contains(tab_id) || !seen.insert(*tab_id) {
                        return false;
                    }
                }
                RestoredTabEntry::Split {
                    tabs,
                    ratio,
                    focused,
                    ..
                } => {
                    if tabs[0] == tabs[1] || !valid_ratio(*ratio) {
                        return false;
                    }
                    if tabs.contains(&active) && tabs[focused.index()] != active {
                        return false;
                    }
                    for tab_id in tabs {
                        if !registry.contains(tab_id) || !seen.insert(*tab_id) {
                            return false;
                        }
                    }
                }
            }
        }
        if seen != registry || !seen.contains(&active) {
            return false;
        }

        let mut next_group_id = self.next_split_group_id;
        let entries = restored
            .into_iter()
            .map(|entry| match entry {
                RestoredTabEntry::Single(tab_id) => TabEntry::Single(tab_id),
                RestoredTabEntry::Split {
                    tabs,
                    direction,
                    ratio,
                    focused,
                } => {
                    let id = SplitGroupId(next_group_id);
                    next_group_id = next_group_id
                        .checked_add(1)
                        .expect("split group id exhausted");
                    TabEntry::Split(SplitGroup {
                        id,
                        tabs,
                        direction,
                        ratio,
                        focused,
                    })
                }
            })
            .collect();
        self.entries = entries;
        self.active = Some(active);
        self.next_split_group_id = next_group_id;
        true
    }

    fn extract_source_for_new_split(&mut self, tab_id: TabId) {
        let source_index = self
            .entry_index_for_tab(tab_id)
            .expect("validated split source must exist");
        match self.entries[source_index].clone() {
            TabEntry::Single(_) => {
                self.entries.remove(source_index);
            }
            TabEntry::Split(group) => {
                // 新しい2画面へメンバーを持ち出すため、元の相方は通常タブとして残す。
                let sibling = group.tabs.into_iter().find(|id| *id != tab_id).unwrap();
                self.entries[source_index] = TabEntry::Single(sibling);
            }
        }
    }

    fn replace_tab_in_entry(&mut self, entry_index: usize, old: TabId, new: TabId) -> Option<()> {
        match &mut self.entries[entry_index] {
            TabEntry::Single(tab_id) if *tab_id == old => *tab_id = new,
            TabEntry::Split(group) => {
                let side = group.tabs.iter().position(|tab_id| *tab_id == old)?;
                group.tabs[side] = new;
            }
            TabEntry::Single(_) => return None,
        }
        Some(())
    }

    fn move_entry(entries: &mut Vec<TabEntry>, current: usize, insertion: usize) -> bool {
        let target = insertion.min(entries.len().saturating_sub(1));
        if target == current || target == current + 1 {
            return true;
        }
        let entry = entries.remove(current);
        let adjusted = if insertion > current {
            insertion - 1
        } else {
            insertion
        };
        entries.insert(adjusted, entry);
        true
    }

    fn entry_index_for_tab(&self, tab_id: TabId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.contains(tab_id))
    }

    fn entry_index_for_group(&self, group_id: SplitGroupId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| matches!(entry, TabEntry::Split(group) if group.id == group_id))
    }

    fn entry_focus_tab(entry: &TabEntry) -> Option<TabId> {
        match entry {
            TabEntry::Single(tab_id) => Some(*tab_id),
            TabEntry::Split(group) => Some(group.tab(group.focused)),
        }
    }
}

fn valid_ratio(ratio: f32) -> bool {
    ratio.is_finite() && (0.1..=0.9).contains(&ratio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn open_files(state: &mut TabState, count: usize) -> (tempfile::TempDir, Vec<TabId>) {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..count {
            let path = directory.path().join(format!("{index}.pdf"));
            File::create(&path).unwrap();
            state.open(path).unwrap();
        }
        let ids = state.tabs().iter().map(Tab::id).collect();
        (directory, ids)
    }

    #[test]
    fn new_tabs_open_after_the_active_entry() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(state.select_tab(ids[0]));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("next.pdf");
        File::create(&path).unwrap();
        state.open(path).unwrap();
        assert_eq!(state.display_tab_ids()[0], ids[0]);
        assert_eq!(
            state.active_tab_id(),
            state.display_tab_ids().get(1).copied()
        );
    }

    #[test]
    fn rebind_path_preserves_tab_identity_and_rejects_existing_path() {
        let mut state = TabState::new();
        let (directory, ids) = open_files(&mut state, 2);
        let renamed = directory.path().join("renamed.pdf");
        std::fs::rename(directory.path().join("0.pdf"), &renamed).unwrap();

        state.rebind_path(0, &renamed).unwrap();
        assert_eq!(state.tabs()[0].id(), ids[0]);
        assert_eq!(
            state.tabs()[0].path(),
            std::fs::canonicalize(&renamed).unwrap()
        );
        let error = state.rebind_path(0, directory.path().join("1.pdf"));
        assert_eq!(error.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn multiple_split_sets_preserve_one_membership_per_tab() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 5);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        assert!(state.select_tab(ids[2]));
        assert!(state.create_split(ids[3], ids[2], SplitPlacement::Bottom));
        assert_eq!(
            state
                .entries()
                .iter()
                .filter(|entry| matches!(entry, TabEntry::Split(_)))
                .count(),
            2
        );
        let order = state.display_tab_ids();
        assert_eq!(order.iter().copied().collect::<HashSet<_>>().len(), 5);
    }

    #[test]
    fn selecting_a_normal_tab_hides_but_keeps_split_sets() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        assert_eq!(state.visible_tab_ids(), vec![ids[0], ids[1]]);
        assert!(state.select_tab(ids[2]));
        assert_eq!(state.visible_tab_ids(), vec![ids[2]]);
        assert!(state.select_tab(ids[0]));
        assert_eq!(state.visible_tab_ids(), vec![ids[0], ids[1]]);
    }

    #[test]
    fn extracting_a_member_dissolves_only_its_pair() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 4);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        assert!(state.create_split(ids[3], ids[2], SplitPlacement::Bottom));
        assert!(state.extract_split_member(ids[1], 2));
        assert!(state.split_for_tab(ids[0]).is_none());
        assert!(state.split_for_tab(ids[1]).is_none());
        assert!(state.split_for_tab(ids[2]).is_some());
    }

    #[test]
    fn replacing_from_a_normal_tab_swaps_the_displaced_member_to_the_source_position() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        let group = state.split_for_tab(ids[0]).unwrap().id();
        assert_eq!(
            state.replace_split_member(group, SplitSide::Second, ids[2]),
            Some(ids[1])
        );
        assert_eq!(state.split_group(group).unwrap().tabs(), [ids[0], ids[2]]);
        assert!(matches!(state.entries().last(), Some(TabEntry::Single(id)) if *id == ids[1]));
    }

    #[test]
    fn replacing_between_pairs_preserves_both_pairs() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 4);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        let first = state.split_for_tab(ids[0]).unwrap().id();
        assert!(state.create_split(ids[3], ids[2], SplitPlacement::Bottom));
        assert_eq!(
            state.replace_split_member(first, SplitSide::Second, ids[3]),
            Some(ids[1])
        );
        assert_eq!(state.split_group(first).unwrap().tabs(), [ids[0], ids[3]]);
        assert_eq!(
            state.split_for_tab(ids[1]).unwrap().tabs(),
            [ids[2], ids[1]]
        );
    }

    #[test]
    fn replacing_the_opposite_member_focuses_the_dragged_tab_after_swap() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        assert!(state.select_tab(ids[0]));
        let group_id = state.active_split().unwrap().id();
        let dragged_side = if state.active_split().unwrap().tab(SplitSide::First) == ids[1] {
            SplitSide::First
        } else {
            SplitSide::Second
        };
        let target_side = match dragged_side {
            SplitSide::First => SplitSide::Second,
            SplitSide::Second => SplitSide::First,
        };

        assert_eq!(
            state.replace_split_member(group_id, target_side, ids[1]),
            Some(ids[0])
        );
        assert_eq!(state.active_tab_id(), Some(ids[1]));
        let group = state.active_split().unwrap();
        assert_eq!(group.tab(group.focused()), ids[1]);
        assert_eq!(state.next_entry_tab(), Some(ids[1]));
    }

    #[test]
    fn closing_a_split_member_leaves_the_sibling_single() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        let closing = state.tab_registry_index(ids[1]).unwrap();
        state.close(closing).unwrap();
        assert_eq!(state.entries(), &[TabEntry::Single(ids[0])]);
        assert_eq!(state.active_tab_id(), Some(ids[0]));
    }

    #[test]
    fn ctrl_tab_treats_each_split_as_one_entry() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        assert!(state.select_tab(ids[0]));
        assert_eq!(state.next_entry_tab(), Some(ids[2]));
        assert!(state.select_tab(ids[2]));
        assert_eq!(state.next_entry_tab(), Some(ids[0]));
    }

    #[test]
    fn restore_rejects_duplicate_or_missing_membership() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 3);
        assert!(!state.restore_layout(
            vec![
                RestoredTabEntry::Single(ids[0]),
                RestoredTabEntry::Split {
                    tabs: [ids[0], ids[1]],
                    direction: SplitDirection::Horizontal,
                    ratio: 0.5,
                    focused: SplitSide::First,
                },
            ],
            Some(ids[0]),
        ));
        assert!(!state.restore_layout(vec![RestoredTabEntry::Single(ids[0])], Some(ids[0]),));
        assert!(!state.restore_layout(
            vec![
                RestoredTabEntry::Split {
                    tabs: [ids[0], ids[1]],
                    direction: SplitDirection::Horizontal,
                    ratio: 0.5,
                    focused: SplitSide::First,
                },
                RestoredTabEntry::Single(ids[2]),
            ],
            Some(ids[1]),
        ));
    }

    #[test]
    fn split_direction_ratio_swap_and_unsplit_are_explicit() {
        let mut state = TabState::new();
        let (_directory, ids) = open_files(&mut state, 2);
        assert!(state.create_split(ids[1], ids[0], SplitPlacement::Right));
        let group_id = state.split_for_tab(ids[0]).unwrap().id();
        assert!(state.set_split_direction(group_id, SplitDirection::Vertical));
        assert!(state.set_split_ratio(group_id, 0.3));
        assert!(state.swap_split_members(group_id));
        let group = state.split_group(group_id).unwrap();
        assert_eq!(group.tabs(), [ids[1], ids[0]]);
        assert_eq!(group.direction(), SplitDirection::Vertical);
        assert_eq!(group.ratio(), 0.3);
        assert!(state.unsplit(group_id));
        assert_eq!(state.entries().len(), 2);
    }
}
