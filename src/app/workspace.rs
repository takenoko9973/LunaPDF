use eframe::egui::{Pos2, Rect, pos2};

use crate::domain::tabs::{SplitDirection, SplitPlacement};

pub(super) const PANE_SEPARATOR_WIDTH: f32 = 6.0;

// 最小ウィンドウ幅720 ptで最大360 ptのサイドバーを開いても、左右それぞれに
// 最小タブとPDF操作面を残せる値にする。
const MIN_PANE_WIDTH: f32 = 160.0;

// PDF中央はキャンセル領域として40%残し、選択方向の両端30%ずつをdrop領域にする。
const SPLIT_DROP_EDGE_FRACTION: f32 = 0.30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SplitRects {
    pub(super) panes: [Rect; 2],
    pub(super) separator: Rect,
    pub(super) ratio: f32,
}

/// 指定方向の2ペインとセパレーターを、重なりや負の寸法なしで計算する。
pub(super) fn split_rects(
    rect: Rect,
    requested_ratio: f32,
    direction: SplitDirection,
) -> SplitRects {
    let split_extent = match direction {
        SplitDirection::Horizontal => rect.width().max(0.0),
        SplitDirection::Vertical => rect.height().max(0.0),
    };
    let separator_extent = PANE_SEPARATOR_WIDTH.min(split_extent);
    let content_extent = (split_extent - separator_extent).max(0.0);
    let minimum_pane_extent = MIN_PANE_WIDTH.min(content_extent / 2.0);
    let minimum_ratio = if content_extent > 0.0 {
        minimum_pane_extent / content_extent
    } else {
        0.5
    };
    // 保存値の0.1..=0.9制約に加え、現在の分割軸で操作可能なpoint寸法を守る。
    let ratio = requested_ratio
        .clamp(0.1, 0.9)
        .clamp(minimum_ratio, 1.0 - minimum_ratio);
    let first_extent = content_extent * ratio;
    match direction {
        SplitDirection::Horizontal => {
            let separator_min_x = rect.left() + first_extent;
            let separator_max_x = separator_min_x + separator_extent;
            let first = Rect::from_min_max(rect.min, pos2(separator_min_x, rect.bottom()));
            let separator = Rect::from_min_max(
                pos2(separator_min_x, rect.top()),
                pos2(separator_max_x, rect.bottom()),
            );
            let second = Rect::from_min_max(pos2(separator_max_x, rect.top()), rect.max);
            SplitRects {
                panes: [first, second],
                separator,
                ratio,
            }
        }
        SplitDirection::Vertical => {
            let separator_min_y = rect.top() + first_extent;
            let separator_max_y = separator_min_y + separator_extent;
            let first = Rect::from_min_max(rect.min, pos2(rect.right(), separator_min_y));
            let separator = Rect::from_min_max(
                pos2(rect.left(), separator_min_y),
                pos2(rect.right(), separator_max_y),
            );
            let second = Rect::from_min_max(pos2(rect.left(), separator_max_y), rect.max);
            SplitRects {
                panes: [first, second],
                separator,
                ratio,
            }
        }
    }
}

/// ポインターX座標を、表示中タブ矩形の間にある挿入ギャップへ変換する。
pub(super) fn tab_insertion_index(tab_rects: &[Rect], pointer_x: f32) -> usize {
    tab_rects
        .iter()
        .position(|rect| pointer_x < rect.center().x)
        .unwrap_or(tab_rects.len())
}

/// 単一PDF領域の四辺から最寄りの分割drop先を返し、中央と領域外は取り消す。
pub(super) fn split_drop_placement(rect: Rect, pointer: Pos2) -> Option<SplitPlacement> {
    if !rect.contains(pointer) {
        return None;
    }
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    // 軸ごとに正規化して比較するため、縦横比が異なる矩形の隅でも視覚的に最寄りの辺を選ぶ。
    let candidates = [
        (
            (pointer.x - rect.left()) / rect.width(),
            SplitPlacement::Left,
        ),
        (
            (rect.right() - pointer.x) / rect.width(),
            SplitPlacement::Right,
        ),
        (
            (pointer.y - rect.top()) / rect.height(),
            SplitPlacement::Top,
        ),
        (
            (rect.bottom() - pointer.y) / rect.height(),
            SplitPlacement::Bottom,
        ),
    ];
    candidates
        .into_iter()
        .filter(|(distance, _)| *distance <= SPLIT_DROP_EDGE_FRACTION)
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, placement)| placement)
}

/// 分割drop候補をPDF内容の一部が見える幅または高さへ限定する。
pub(super) fn split_drop_highlight(rect: Rect, placement: SplitPlacement) -> Rect {
    let edge_width = rect.width() * SPLIT_DROP_EDGE_FRACTION;
    let edge_height = rect.height() * SPLIT_DROP_EDGE_FRACTION;
    match placement {
        SplitPlacement::Left => {
            Rect::from_min_max(rect.min, pos2(rect.left() + edge_width, rect.bottom()))
        }
        SplitPlacement::Right => {
            Rect::from_min_max(pos2(rect.right() - edge_width, rect.top()), rect.max)
        }
        SplitPlacement::Top => {
            Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + edge_height))
        }
        SplitPlacement::Bottom => {
            Rect::from_min_max(pos2(rect.left(), rect.bottom() - edge_height), rect.max)
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
        let split = split_rects(input, 0.4, SplitDirection::Horizontal);

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
        let clamped = split_rects(ordinary, 0.01, SplitDirection::Horizontal);
        assert!(clamped.panes[0].width() >= MIN_PANE_WIDTH);
        assert!(clamped.panes[1].width() >= MIN_PANE_WIDTH);

        let tiny = Rect::from_min_max(pos2(0.0, 0.0), pos2(4.0, 10.0));
        let tiny_split = split_rects(tiny, 0.5, SplitDirection::Horizontal);
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
            Some(SplitPlacement::Left)
        );
        assert_eq!(
            split_drop_placement(rect, pos2(900.0, 250.0)),
            Some(SplitPlacement::Right)
        );
        assert_eq!(split_drop_placement(rect, pos2(500.0, 250.0)), None);
        assert_eq!(split_drop_placement(rect, pos2(-1.0, 250.0)), None);
    }

    #[test]
    fn vertical_split_uses_top_and_bottom_panes() {
        let input = Rect::from_min_max(pos2(10.0, 20.0), pos2(610.0, 1_020.0));
        let split = split_rects(input, 0.4, SplitDirection::Vertical);

        assert_eq!(split.panes[0].top(), input.top());
        assert_eq!(split.panes[1].bottom(), input.bottom());
        assert_eq!(split.panes[0].bottom(), split.separator.top());
        assert_eq!(split.separator.bottom(), split.panes[1].top());
        assert_eq!(split.separator.height(), PANE_SEPARATOR_WIDTH);
        assert_eq!(split.panes[0].width(), input.width());
        assert_eq!(split.panes[1].width(), input.width());
    }

    #[test]
    fn vertical_drop_uses_top_and_bottom_edges() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(500.0, 1_000.0));

        assert_eq!(
            split_drop_placement(rect, pos2(250.0, 100.0)),
            Some(SplitPlacement::Top)
        );
        assert_eq!(
            split_drop_placement(rect, pos2(250.0, 900.0)),
            Some(SplitPlacement::Bottom)
        );
        assert_eq!(split_drop_placement(rect, pos2(250.0, 500.0)), None);
    }
}
