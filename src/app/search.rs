use super::*;

#[derive(Default)]
pub(super) struct SearchState {
    pub(super) query: String,
    pub(super) generation: u64,
    pub(super) pages: BTreeMap<usize, Vec<SearchMatch>>,
    pub(super) selected: Option<SearchCursor>,
    pub(super) completed_pages: usize,
    pub(super) truncated: bool,
    pub(super) in_progress: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SearchCursor {
    pub(super) page_index: usize,
    pub(super) match_index: usize,
}

impl PrototypeApp {
    pub(super) fn begin_search(&mut self, index: usize) {
        let query = self.documents[index].search.query.trim().to_owned();
        if query.is_empty() {
            self.cancel_search(index);
            return;
        }
        let Some(page_count) = self.documents[index]
            .info
            .as_ref()
            .map(|info| info.page_bounds.len())
        else {
            return;
        };
        let current_page = self.documents[index].view.current_page;
        let generation = self.documents[index].search.generation.wrapping_add(1);
        let query: Arc<str> = Arc::from(query);
        let tab = &mut self.documents[index];
        tab.search.generation = generation;
        tab.search.pages.clear();
        tab.search.selected = None;
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = true;
        if !tab.send(DocumentCommand::SetSearchGeneration(generation)) {
            tab.search.in_progress = false;
            tab.error = Some(
                "検索を開始できません。文書処理が停止しているため、タブを開き直してください。"
                    .to_owned(),
            );
            return;
        }
        for page_index in search_page_order(current_page, page_count) {
            let _queued = tab.send(DocumentCommand::SearchPage {
                page_index,
                query: Arc::clone(&query),
                generation,
            });
        }
        self.status = format!("Searching {page_count} pages…");
    }

    pub(super) fn cancel_search(&mut self, index: usize) {
        let tab = &mut self.documents[index];
        tab.search.generation = tab.search.generation.wrapping_add(1);
        let _queued = tab.send(DocumentCommand::SetSearchGeneration(tab.search.generation));
        tab.search.pages.clear();
        tab.search.selected = None;
        tab.search.completed_pages = 0;
        tab.search.truncated = false;
        tab.search.in_progress = false;
    }

    pub(super) fn navigate_search(&mut self, index: usize, forward: bool) {
        let current_page = self.documents[index].view.current_page;
        let cursor = next_search_match(
            &self.documents[index].search.pages,
            self.documents[index].search.selected,
            current_page,
            forward,
        );
        if let Some(cursor) = cursor {
            let anchor = search_match_anchor_for_cursor(&self.documents[index], cursor);
            let ordinal = search_match_ordinal(&self.documents[index].search.pages, cursor);
            let tab = &mut self.documents[index];
            tab.search.selected = Some(cursor);
            if let Some(anchor) = anchor {
                tab.jump_to_search_match(anchor);
            } else {
                tab.jump_to_page(cursor.page_index);
            }
            self.status = ordinal.map_or_else(
                || format!("Search result on page {}", cursor.page_index + 1),
                |ordinal| format!("Search result {ordinal} on page {}", cursor.page_index + 1),
            );
        }
    }

    pub(super) fn receive_search_page(&mut self, index: usize, result: SearchPageResult) {
        let tab = &mut self.documents[index];
        let current_revision = tab.info.as_ref().map(|info| info.revision);
        if !search_result_is_current(
            result.generation,
            tab.search.generation,
            result.revision,
            current_revision,
        ) {
            return;
        }
        tab.search.completed_pages = tab.search.completed_pages.saturating_add(1);
        tab.search.truncated |= result.truncated;
        if !result.matches.is_empty() {
            tab.search.pages.insert(result.page_index, result.matches);
        }
        let page_count = tab.info.as_ref().map_or(0, |info| info.page_bounds.len());
        if tab.search.completed_pages >= page_count {
            tab.search.in_progress = false;
        }
    }
}

pub(super) fn search_page_order(current_page: usize, page_count: usize) -> Vec<usize> {
    if page_count == 0 {
        return Vec::new();
    }
    let current_page = current_page.min(page_count - 1);
    let mut pages = Vec::with_capacity(page_count);
    pages.push(current_page);
    for distance in 1..page_count {
        if let Some(page) = current_page.checked_add(distance)
            && page < page_count
        {
            pages.push(page);
        }
        if let Some(page) = current_page.checked_sub(distance) {
            pages.push(page);
        }
    }
    pages
}

pub(super) fn next_search_match(
    pages: &BTreeMap<usize, Vec<SearchMatch>>,
    selected: Option<SearchCursor>,
    current_page: usize,
    forward: bool,
) -> Option<SearchCursor> {
    let matches = pages
        .iter()
        .flat_map(|(page_index, matches)| {
            (0..matches.len()).map(|match_index| SearchCursor {
                page_index: *page_index,
                match_index,
            })
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return None;
    }

    if let Some(position) =
        selected.and_then(|cursor| matches.iter().position(|candidate| *candidate == cursor))
    {
        // 後続ページの結果が届いても選択した論理ヒットは安定している。折り返しは
        // 古い平坦インデックスではなく、新しい並び順の位置に基づく。
        let next = if forward {
            (position + 1) % matches.len()
        } else {
            position.checked_sub(1).unwrap_or(matches.len() - 1)
        };
        return matches.get(next).copied();
    }

    if forward {
        matches
            .iter()
            .copied()
            .find(|cursor| cursor.page_index >= current_page)
            .or_else(|| matches.first().copied())
    } else {
        matches
            .iter()
            .rev()
            .copied()
            .find(|cursor| cursor.page_index <= current_page)
            .or_else(|| matches.last().copied())
    }
}

pub(super) fn search_match_ordinal(
    pages: &BTreeMap<usize, Vec<SearchMatch>>,
    selected: SearchCursor,
) -> Option<usize> {
    pages
        .iter()
        .flat_map(|(page_index, matches)| {
            (0..matches.len()).map(|match_index| SearchCursor {
                page_index: *page_index,
                match_index,
            })
        })
        .position(|candidate| candidate == selected)
        .map(|position| position + 1)
}

fn search_match_anchor_for_cursor(
    document: &DocumentTab,
    cursor: SearchCursor,
) -> Option<PageAnchor> {
    let page_bounds = document
        .info
        .as_ref()?
        .page_bounds
        .get(cursor.page_index)
        .copied()?;
    let search_match = document
        .search
        .pages
        .get(&cursor.page_index)?
        .get(cursor.match_index)?;
    search_match_anchor(cursor.page_index, search_match, page_bounds)
}

pub(super) fn search_match_anchor(
    page_index: usize,
    search_match: &SearchMatch,
    page_bounds: crate::domain::document::PageRect,
) -> Option<PageAnchor> {
    let first = search_match.quads.first()?.bounds();
    let (x0, y0, x1, y1) =
        search_match
            .quads
            .iter()
            .skip(1)
            .fold(first, |(x0, y0, x1, y1), quad| {
                let bounds = quad.bounds();
                (
                    x0.min(bounds.0),
                    y0.min(bounds.1),
                    x1.max(bounds.2),
                    y1.max(bounds.3),
                )
            });
    // 全行の Quad の和集合を中央に置き、複数行のヒットを 1 つの結果として移動する。
    // クランプでページ端の PDF 座標の小さな丸め誤差を収める。
    let x = (((x0 + x1) / 2.0 - page_bounds.x0) / page_bounds.width()).clamp(0.0, 1.0);
    let y = (((y0 + y1) / 2.0 - page_bounds.y0) / page_bounds.height()).clamp(0.0, 1.0);
    (x.is_finite() && y.is_finite()).then_some(PageAnchor {
        page_index,
        page_x_fraction: x,
        page_y_fraction: y,
    })
}

pub(super) fn search_result_is_current(
    result_generation: u64,
    current_generation: u64,
    result_revision: u64,
    current_revision: Option<u64>,
) -> bool {
    result_generation == current_generation && current_revision == Some(result_revision)
}

pub(super) fn search_query_id(document_id: u64) -> Id {
    Id::new(("pdf-search-query", document_id))
}
