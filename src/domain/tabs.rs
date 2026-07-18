use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    path: PathBuf,
}

impl Tab {
    /// Returns the canonical path represented by this tab.
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
    /// Creates an empty tab state with no selected tab.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the tabs in display order.
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Returns the selected tab index, or `None` when no tabs are open.
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Canonicalizes an existing PDF path and opens or selects its tab.
    ///
    /// The filesystem canonicalization error is returned unchanged, so a path
    /// that does not exist is not silently converted into another tab identity.
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

    /// Selects an open tab by index and reports whether the index was valid.
    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.selected = Some(index);
        true
    }

    /// Closes a tab by index and returns it, keeping the selected index valid.
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
