use eframe::egui::{Pos2, Rect, pos2};

use crate::domain::tabs::SplitPlacement;

pub(super) const PANE_SEPARATOR_WIDTH: f32 = 6.0;

// 最小ウィンドウ幅720 ptで最大360 ptのサイドバーを開いても、左右それぞれに
// 最小タブとPDF操作面を残せる値にする。
const MIN_PANE_WIDTH: f32 = 160.0;

// PDF中央はキャンセル領域として40%残し、左右30%ずつを方向が判別できるdrop領域にする。
const SPLIT_DROP_EDGE_FRACTION: f32 = 0.30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HorizontalSplitRects {
    pub(super) panes: [Rect; 2],
    pub(super) separator: Rect,
    pub(super) ratio: f32,
}

/// 水平分割の2ペインとセパレーターを、重なりや負の寸法なしで計算する。
pub(super) fn horizontal_split_rects(rect: Rect, requested_ratio: f32) -> HorizontalSplitRects {
    let separator_width = PANE_SEPARATOR_WIDTH.min(rect.width().max(0.0));
    let content_width = (rect.width() - separator_width).max(0.0);
    let minimum_width = MIN_PANE_WIDTH.min(content_width / 2.0);
    let minimum_ratio = if content_width > 0.0 {
        minimum_width / content_width
    } else {
        0.5
    };
    // 保存値の0.1..=0.9制約に加え、現在の実幅で操作可能なpoint幅を守る。
    let ratio = requested_ratio.clamp(minimum_ratio, 1.0 - minimum_ratio);
    let left_width = content_width * ratio;
    let separator_min_x = rect.left() + left_width;
    let separator_max_x = separator_min_x + separator_width;
    let left = Rect::from_min_max(rect.min, pos2(separator_min_x, rect.bottom()));
    let separator = Rect::from_min_max(
        pos2(separator_min_x, rect.top()),
        pos2(separator_max_x, rect.bottom()),
    );
    let right = Rect::from_min_max(pos2(separator_max_x, rect.top()), rect.max);
    HorizontalSplitRects {
        panes: [left, right],
        separator,
        ratio,
    }
}

/// ポインターX座標を、表示中タブ矩形の間にある挿入ギャップへ変換する。
pub(super) fn tab_insertion_index(tab_rects: &[Rect], pointer_x: f32) -> usize {
    tab_rects
        .iter()
        .position(|rect| pointer_x < rect.center().x)
        .unwrap_or(tab_rects.len())
}

/// 単一ペインPDF領域の左右端だけを分割drop先として返し、中央と領域外は取り消す。
pub(super) fn split_drop_placement(rect: Rect, pointer: Pos2) -> Option<SplitPlacement> {
    if !rect.contains(pointer) {
        return None;
    }
    let edge_width = rect.width() * SPLIT_DROP_EDGE_FRACTION;
    if pointer.x <= rect.left() + edge_width {
        Some(SplitPlacement::Before)
    } else if pointer.x >= rect.right() - edge_width {
        Some(SplitPlacement::After)
    } else {
        None
    }
}

/// 分割drop候補をPDF内容の一部が見える幅へ限定する。
pub(super) fn split_drop_highlight(rect: Rect, placement: SplitPlacement) -> Rect {
    let edge_width = rect.width() * SPLIT_DROP_EDGE_FRACTION;
    match placement {
        SplitPlacement::Before => {
            Rect::from_min_max(rect.min, pos2(rect.left() + edge_width, rect.bottom()))
        }
        SplitPlacement::After => {
            Rect::from_min_max(pos2(rect.right() - edge_width, rect.top()), rect.max)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Rect, pos2};

    #[test]
    fn horizontal_split_covers_input_without_overlap() {
        let input = Rect::from_min_max(pos2(10.0, 20.0), pos2(1_010.0, 620.0));
        let split = horizontal_split_rects(input, 0.4);

        assert_eq!(split.panes[0].left(), input.left());
        assert_eq!(split.panes[1].right(), input.right());
        assert_eq!(split.panes[0].right(), split.separator.left());
        assert_eq!(split.separator.right(), split.panes[1].left());
        assert_eq!(split.separator.width(), PANE_SEPARATOR_WIDTH);
        assert_eq!(split.panes[0].height(), input.height());
        assert_eq!(split.panes[1].height(), input.height());
    }

    #[test]
    fn horizontal_split_clamps_ratio_to_point_minimum_and_tiny_rects_stay_nonnegative() {
        let ordinary = Rect::from_min_max(pos2(0.0, 0.0), pos2(1_000.0, 500.0));
        let clamped = horizontal_split_rects(ordinary, 0.01);
        assert!(clamped.panes[0].width() >= MIN_PANE_WIDTH);
        assert!(clamped.panes[1].width() >= MIN_PANE_WIDTH);

        let tiny = Rect::from_min_max(pos2(0.0, 0.0), pos2(4.0, 10.0));
        let tiny_split = horizontal_split_rects(tiny, 0.5);
        assert!(tiny_split.panes.iter().all(|pane| pane.width() >= 0.0));
        assert!(tiny_split.separator.width() >= 0.0);
    }

    #[test]
    fn insertion_index_supports_left_between_and_right_gaps() {
        let tabs = [
            Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 24.0)),
            Rect::from_min_max(pos2(101.0, 0.0), pos2(201.0, 24.0)),
        ];

        assert_eq!(tab_insertion_index(&tabs, -1.0), 0);
        assert_eq!(tab_insertion_index(&tabs, 100.5), 1);
        assert_eq!(tab_insertion_index(&tabs, 300.0), 2);
        assert_eq!(tab_insertion_index(&[], 20.0), 0);
    }

    #[test]
    fn split_drop_uses_edges_and_keeps_center_as_cancel() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1_000.0, 500.0));

        assert_eq!(
            split_drop_placement(rect, pos2(100.0, 250.0)),
            Some(SplitPlacement::Before)
        );
        assert_eq!(
            split_drop_placement(rect, pos2(900.0, 250.0)),
            Some(SplitPlacement::After)
        );
        assert_eq!(split_drop_placement(rect, pos2(500.0, 250.0)), None);
        assert_eq!(split_drop_placement(rect, pos2(-1.0, 250.0)), None);
    }
}
