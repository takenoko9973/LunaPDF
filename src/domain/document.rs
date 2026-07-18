use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::domain::selection::PageQuad;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageRect {
    pub(crate) x0: f32,
    pub(crate) y0: f32,
    pub(crate) x1: f32,
    pub(crate) y1: f32,
}

impl PageRect {
    pub(crate) fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub(crate) fn height(self) -> f32 {
        self.y1 - self.y0
    }
}

// A 512 px RGBA tile is at most 1 MiB. This keeps each non-preemptible MuPDF
// raster and GPU upload bounded while Phase 2 performance measurements decide
// whether a larger tile would improve throughput.
pub(crate) const TILE_EDGE_PIXELS: u32 = 512;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TileSpec {
    pub(crate) pixel_x: u32,
    pub(crate) pixel_y: u32,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
}

impl TileSpec {
    pub(crate) fn rgba_bytes(self) -> Option<usize> {
        let pixels = usize::try_from(self.pixel_width)
            .ok()?
            .checked_mul(usize::try_from(self.pixel_height).ok()?)?;
        pixels.checked_mul(4)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RenderPriority {
    Visible,
    NextViewport,
    PreviousViewport,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TileRequest {
    pub(crate) page_index: usize,
    pub(crate) zoom: f32,
    pub(crate) pixels_per_point: f32,
    pub(crate) scale: f32,
    pub(crate) generation: u64,
    pub(crate) expected_revision: u64,
    pub(crate) spec: TileSpec,
    pub(crate) priority: RenderPriority,
}

#[derive(Debug)]
pub(crate) struct RenderedTile {
    pub(crate) page_index: usize,
    pub(crate) zoom: f32,
    pub(crate) pixels_per_point: f32,
    pub(crate) scale: f32,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) spec: TileSpec,
    pub(crate) page_pixel_width: u32,
    pub(crate) page_pixel_height: u32,
    pub(crate) pixels_rgba: Vec<u8>,
    pub(crate) bounds: PageRect,
    pub(crate) render_time: Duration,
    pub(crate) physical_memory_bytes: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct DocumentInfo {
    pub(crate) path: PathBuf,
    pub(crate) page_bounds: Vec<PageRect>,
    pub(crate) highlight_count: usize,
    pub(crate) can_save_incrementally: bool,
    pub(crate) highlight_capability: HighlightCapability,
    pub(crate) dirty: bool,
    pub(crate) revision: u64,
    pub(crate) open_time: Duration,
    pub(crate) physical_memory_bytes: Option<usize>,
    pub(crate) version: DocumentVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighlightCapability {
    Allowed,
    ReadOnlyFile,
    AnnotationPermissionDenied,
    SignedDocument,
    RequiresFullRewrite,
}

impl HighlightCapability {
    /// Reports whether the UI may create a change that the current save path can persist.
    pub(crate) fn is_allowed(self) -> bool {
        self == Self::Allowed
    }

    /// Explains why Highlight creation is unavailable without guessing a fallback save path.
    pub(crate) fn restriction(self) -> Option<&'static str> {
        match self {
            Self::Allowed => None,
            Self::ReadOnlyFile => Some("the PDF file is read-only"),
            Self::AnnotationPermissionDenied => {
                Some("the PDF security permissions do not allow annotations")
            }
            Self::SignedDocument => Some("the PDF contains a signed signature field"),
            Self::RequiresFullRewrite => {
                Some("the PDF requires a full-file rewrite, which is not enabled")
            }
        }
    }
}

/// Rust-owned file identity used to reject stale suspended-document restores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DocumentVersion {
    pub(crate) identity_primary: u64,
    pub(crate) identity_secondary: u64,
    pub(crate) length: u64,
    pub(crate) modified: SystemTime,
}

#[derive(Clone, Debug)]
pub(crate) struct HighlightRequest {
    pub(crate) page_index: usize,
    pub(crate) quads: Vec<PageQuad>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutlineItem {
    pub(crate) title: String,
    pub(crate) page_index: Option<usize>,
    pub(crate) children: Vec<OutlineItem>,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchPageResult {
    pub(crate) page_index: usize,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) quads: Vec<PageQuad>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ThumbnailRequest {
    pub(crate) page_index: usize,
    pub(crate) max_pixel_width: u32,
    pub(crate) max_pixel_height: u32,
    pub(crate) generation: u64,
    pub(crate) expected_revision: u64,
}

#[derive(Debug)]
pub(crate) struct RenderedThumbnail {
    pub(crate) page_index: usize,
    pub(crate) max_pixel_width: u32,
    pub(crate) max_pixel_height: u32,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
    pub(crate) pixels_rgba: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_weight_is_rgba_byte_count() {
        let spec = TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: 512,
            pixel_height: 256,
        };

        assert_eq!(spec.rgba_bytes(), Some(512 * 256 * 4));
    }

    #[test]
    fn tile_weight_reports_dimension_overflow() {
        let spec = TileSpec {
            pixel_x: 0,
            pixel_y: 0,
            pixel_width: u32::MAX,
            pixel_height: u32::MAX,
        };

        assert_eq!(spec.rgba_bytes(), None);
    }
}
