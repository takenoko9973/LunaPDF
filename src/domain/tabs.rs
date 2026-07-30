use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    path: PathBuf,
}

impl Tab {
    /// このタブが表す正規パスを返す。
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Default)]
pub(crate) struct TabState {
    tabs: Vec<Tab>,
    selected: Option<usize>,
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

    /// 表示順のタブを返す。
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// 選択中のタブインデックスを返す。タブが開かれていない場合は`None`を返す。
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// 既存PDFのパスを正規化し、そのタブを開くか選択する。
    ///
    /// ファイルシステムの正規化エラーは変更せず返すため、存在しないパスを別のタブ識別子へ
    /// 暗黙に変換しない。
    pub(crate) fn open(&mut self, path: impl AsRef<Path>) -> io::Result<OpenTabResult> {
        let canonical_path = std::fs::canonicalize(path)?;

        if let Some(index) = self.tabs.iter().position(|tab| tab.path == canonical_path) {
            self.selected = Some(index);
            return Ok(OpenTabResult::SelectedExisting(index));
        }

        self.tabs.push(Tab {
            path: canonical_path,
        });
        let index = self.tabs.len() - 1;
        self.selected = Some(index);
        Ok(OpenTabResult::Opened(index))
    }

    /// インデックスで開いているタブを選択し、そのインデックスが有効かどうかを報告する。
    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.selected = Some(index);
        true
    }

    /// インデックスでタブを閉じ、選択インデックスを有効に保ったまま閉じたタブを返す。
    pub(crate) fn close(&mut self, index: usize) -> Option<Tab> {
        if index >= self.tabs.len() {
            return None;
        }

        let removed = self.tabs.remove(index);
        self.selected = match (self.selected, self.tabs.len()) {
            (_, 0) => None,
            (Some(selected), _) if selected > index => Some(selected - 1),
            (Some(selected), _) if selected == index => Some(selected.min(self.tabs.len() - 1)),
            (selected, _) => selected,
        };
        Some(removed)
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
    }
}
