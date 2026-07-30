use crate::domain::document::{PageRect, TILE_EDGE_PIXELS, TileSpec};

// MuPDFの`fz_round_rect`はラスタライズ前に1000分の1デバイスピクセル未満の誤差を無視する。
// UIグリッドも同じしきい値を使わないと、端のタイルがワーカー結果と1ピクセルずれて
// キャッシュキー検証に失敗する可能性がある。
const MUPDF_RECT_ROUNDING_EPSILON: f32 = 0.001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TileGrid {
    pixel_width: u32,
    pixel_height: u32,
}

impl TileGrid {
    /// 1つのPDFページとデバイススケールに対するページ内ラスタグリッドを計算する。
    ///
    /// 境界またはスケールをMuPDFの符号付きデバイス座標範囲で表現できない場合は`None`を返す。
    /// その状態ではタイル要求を発行してはならない。
    pub(crate) fn new(bounds: PageRect, scale: f32) -> Option<Self> {
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let pixel_width = scaled_extent(bounds.x0, bounds.x1, scale)?;
        let pixel_height = scaled_extent(bounds.y0, bounds.y1, scale)?;
        if pixel_width == 0 || pixel_height == 0 {
            return None;
        }
        Some(Self {
            pixel_width,
            pixel_height,
        })
    }

    pub(crate) fn pixel_width(self) -> u32 {
        self.pixel_width
    }

    pub(crate) fn pixel_height(self) -> u32 {
        self.pixel_height
    }

    /// ページ内ピクセル矩形と交差するタイルだけを列挙する。
    ///
    /// 割り当て前にビューポートで列挙範囲を制約する必要がある。非常に大きい合法なページでは
    /// グリッド全体を構築するとUIスレッドを使い果たす可能性がある。
    pub(crate) fn specs_in_pixel_rect(
        self,
        min_x: u32,
        min_y: u32,
        max_x: u32,
        max_y: u32,
    ) -> Option<Vec<TileSpec>> {
        let min_x = min_x.min(self.pixel_width);
        let min_y = min_y.min(self.pixel_height);
        let max_x = max_x.min(self.pixel_width);
        let max_y = max_y.min(self.pixel_height);
        if min_x >= max_x || min_y >= max_y {
            return Some(Vec::new());
        }

        let first_column = min_x / TILE_EDGE_PIXELS;
        let first_row = min_y / TILE_EDGE_PIXELS;
        let last_column = (max_x - 1) / TILE_EDGE_PIXELS;
        let last_row = (max_y - 1) / TILE_EDGE_PIXELS;
        let columns = last_column.checked_sub(first_column)?.checked_add(1)?;
        let rows = last_row.checked_sub(first_row)?.checked_add(1)?;
        let tile_count = columns.checked_mul(rows)?;
        let capacity = usize::try_from(tile_count).ok()?;
        let mut specs = Vec::with_capacity(capacity);
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let pixel_x = column * TILE_EDGE_PIXELS;
                let pixel_y = row * TILE_EDGE_PIXELS;
                specs.push(TileSpec {
                    pixel_x,
                    pixel_y,
                    pixel_width: TILE_EDGE_PIXELS.min(self.pixel_width - pixel_x),
                    pixel_height: TILE_EDGE_PIXELS.min(self.pixel_height - pixel_y),
                });
            }
        }
        Some(specs)
    }
}

fn scaled_extent(start: f32, end: f32, scale: f32) -> Option<u32> {
    // `fz_round_rect`に合わせる。この許容誤差がないと、748.00006のような値は
    // ここでは749に切り上げられるが、MuPDFでは748に丸められる。
    let first_pixel = (start * scale + MUPDF_RECT_ROUNDING_EPSILON).floor();
    let last_pixel = (end * scale - MUPDF_RECT_ROUNDING_EPSILON).ceil();
    let extent = last_pixel - first_pixel;
    if !first_pixel.is_finite()
        || !last_pixel.is_finite()
        || first_pixel < i32::MIN as f32
        || last_pixel > i32::MAX as f32
        || extent <= 0.0
        || extent > u32::MAX as f32
    {
        return None;
    }
    Some(extent as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(x0: f32, y0: f32, width: f32, height: f32) -> PageRect {
        PageRect {
            x0,
            y0,
            x1: x0 + width,
            y1: y0 + height,
        }
    }

    #[test]
    fn edge_tiles_use_remaining_pixel_dimensions() {
        let grid = TileGrid::new(page(0.0, 0.0, 600.0, 700.0), 1.0).unwrap();
        let specs = grid.specs_in_pixel_rect(0, 0, 600, 700).unwrap();

        assert_eq!(specs.len(), 4);
        assert_eq!(
            specs[3],
            TileSpec {
                pixel_x: 512,
                pixel_y: 512,
                pixel_width: 88,
                pixel_height: 188,
            }
        );
    }

    #[test]
    fn nonzero_page_origin_is_included_in_rounded_pixel_extent() {
        let grid = TileGrid::new(page(0.25, 10.25, 512.0, 256.0), 1.0).unwrap();

        assert_eq!(grid.pixel_width(), 513);
        assert_eq!(grid.pixel_height(), 257);
        assert_eq!(grid.specs_in_pixel_rect(0, 0, 513, 257).unwrap().len(), 2);
    }

    #[test]
    fn near_integer_edge_uses_mupdf_rectangle_rounding() {
        let grid = TileGrid::new(page(0.0, 0.0, 793.701, 595.276), 0.942_420_66).unwrap();

        assert_eq!(grid.pixel_width(), 748);
    }

    #[test]
    fn invalid_scale_does_not_produce_requests() {
        assert!(TileGrid::new(page(0.0, 0.0, 100.0, 100.0), 0.0).is_none());
        assert!(TileGrid::new(page(0.0, 0.0, 100.0, 100.0), f32::NAN).is_none());
    }

    #[test]
    fn pixel_window_does_not_enumerate_the_full_large_page() {
        let grid = TileGrid::new(page(0.0, 0.0, 10_000.0, 10_000_000.0), 16.0).unwrap();

        let specs = grid.specs_in_pixel_rect(0, 0, 32_000, 38_400).unwrap();

        assert_eq!(specs.len(), 63 * 75);
        assert_eq!(specs.last().unwrap().pixel_y, 74 * TILE_EDGE_PIXELS);
    }
}
