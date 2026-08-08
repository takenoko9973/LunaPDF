use crate::domain::document::PageRect;

const PRINT_SCALE_RELATIVE_EPSILON: f32 = 1.0e-5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PaperOrientation {
    Portrait,
    Landscape,
}

impl PaperOrientation {
    pub(super) const fn opposite(self) -> Self {
        match self {
            Self::Portrait => Self::Landscape,
            Self::Landscape => Self::Portrait,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PdfRotation {
    None,
    Clockwise90,
    Clockwise270,
}
impl PdfRotation {
    const fn tie_rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Clockwise90 => 1,
            Self::Clockwise270 => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PrintableArea {
    pub(super) width: u32,
    pub(super) height: u32,
}

/// The selected physical sheet, independent of its current printer orientation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PaperSize {
    pub(super) width: u32,
    pub(super) height: u32,
}

impl PaperSize {
    /// Normalize orientation-swapped DC dimensions to compare the same paper.
    pub(super) const fn from_physical_dimensions(width: u32, height: u32) -> Self {
        if width <= height {
            Self { width, height }
        } else {
            Self {
                width: height,
                height: width,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PrintStrip {
    pub(super) pixel_y: u32,
    pub(super) pixel_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PrintLayout {
    pub(super) scale: f32,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
    pub(super) offset_x: i32,
    pub(super) offset_y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PrintPlan {
    pub(super) page_index: usize,
    pub(super) paper_orientation: PaperOrientation,
    pub(super) pdf_rotation: PdfRotation,
    pub(super) printable_area: PrintableArea,
    pub(super) paper_size: PaperSize,
    pub(super) layout: PrintLayout,
    pub(super) orientation_fallback: bool,
}

impl PrintLayout {
    /// 1 ページの PDF をプリンターの印刷可能なデバイスピクセル領域内に収める。
    pub(super) fn fit(
        bounds: PageRect,
        printable_width: u32,
        printable_height: u32,
    ) -> Option<Self> {
        // プリンターの能力値とページボックスは OS/PDF の境界をまたぐため、ここで
        // 物理的に無効な値をすべて拒否し、後段の GDI で曖昧なスケーリングを避ける。
        if printable_width == 0
            || printable_height == 0
            || !bounds.width().is_finite()
            || !bounds.height().is_finite()
            || bounds.width() <= 0.0
            || bounds.height() <= 0.0
        {
            return None;
        }
        let mut scale = (printable_width as f32 / bounds.width())
            .min(printable_height as f32 / bounds.height());
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }

        // MuPDF は幅だけでなく変換後のページボックスを丸める。この規則に合わせることで、
        // 要求する帯をゼロでないページボックス内に収める。
        let mut pixel_width = scaled_extent(bounds.x0, bounds.x1, scale)?;
        let mut pixel_height = scaled_extent(bounds.y0, bounds.y1, scale)?;
        if pixel_width > printable_width || pixel_height > printable_height {
            // ゼロでないページボックスを変換すると、両端が外側へ 1 ピクセル丸められる
            // ことがある。諦める前に、その丸めによる超過分だけを減らす。
            let width_correction = printable_width as f32 / pixel_width as f32;
            let height_correction = printable_height as f32 / pixel_height as f32;
            scale *= width_correction.min(height_correction);
            pixel_width = scaled_extent(bounds.x0, bounds.x1, scale)?;
            pixel_height = scaled_extent(bounds.y0, bounds.y1, scale)?;
        }
        if pixel_width > printable_width || pixel_height > printable_height {
            return None;
        }
        let offset_x = i32::try_from((printable_width - pixel_width) / 2).ok()?;
        let offset_y = i32::try_from((printable_height - pixel_height) / 2).ok()?;
        Some(Self {
            scale,
            pixel_width,
            pixel_height,
            offset_x,
            offset_y,
        })
    }

    pub(super) fn fit_rotated(
        bounds: PageRect,
        rotation: PdfRotation,
        printable_area: PrintableArea,
    ) -> Option<Self> {
        let rotated_bounds = rotated_bounds(bounds, rotation)?;
        Self::fit(rotated_bounds, printable_area.width, printable_area.height)
    }

    /// 固定された RGBA 予算の範囲でラスタを完全な走査線単位の帯に分割する。
    pub(super) fn strips(self, byte_budget: usize) -> Option<Vec<PrintStrip>> {
        let row_bytes = usize::try_from(self.pixel_width).ok()?.checked_mul(4)?;
        if row_bytes == 0 || row_bytes > byte_budget {
            return None;
        }
        let rows_per_strip = u32::try_from(byte_budget / row_bytes).ok()?.max(1);
        let mut strips = Vec::new();
        let mut pixel_y = 0_u32;
        while pixel_y < self.pixel_height {
            let pixel_height = rows_per_strip.min(self.pixel_height - pixel_y);
            strips.push(PrintStrip {
                pixel_y,
                pixel_height,
            });
            pixel_y = pixel_y.checked_add(pixel_height)?;
        }
        Some(strips)
    }
}

impl PrintPlan {
    pub(super) fn choose(
        page_index: usize,
        bounds: PageRect,
        current_orientation: PaperOrientation,
        current_area: PrintableArea,
        current_paper_size: PaperSize,
        auto_rotate: bool,
        alternate: Option<(PrintableArea, PaperSize)>,
    ) -> Option<Self> {
        let orientation_fallback = auto_rotate && alternate.is_none();
        let mut candidates = Vec::with_capacity(6);
        add_candidates(
            &mut candidates,
            page_index,
            bounds,
            current_orientation,
            current_area,
            current_paper_size,
            CandidateOptions {
                orientation_fallback,
                allow_content_rotation: auto_rotate,
            },
        );
        if auto_rotate && let Some((area, paper_size)) = alternate {
            add_candidates(
                &mut candidates,
                page_index,
                bounds,
                current_orientation.opposite(),
                area,
                paper_size,
                CandidateOptions {
                    orientation_fallback: false,
                    allow_content_rotation: true,
                },
            );
        }

        candidates.into_iter().reduce(|best, candidate| {
            if candidate_is_better(candidate, best, current_orientation) {
                candidate
            } else {
                best
            }
        })
    }
}

#[derive(Clone, Copy)]
struct CandidateOptions {
    orientation_fallback: bool,
    allow_content_rotation: bool,
}

fn add_candidates(
    candidates: &mut Vec<PrintPlan>,
    page_index: usize,
    bounds: PageRect,
    orientation: PaperOrientation,
    printable_area: PrintableArea,
    paper_size: PaperSize,
    options: CandidateOptions,
) {
    let rotations: &[PdfRotation] = if options.allow_content_rotation {
        &[
            PdfRotation::None,
            PdfRotation::Clockwise90,
            PdfRotation::Clockwise270,
        ]
    } else {
        &[PdfRotation::None]
    };
    for &pdf_rotation in rotations {
        let Some(layout) = PrintLayout::fit_rotated(bounds, pdf_rotation, printable_area) else {
            continue;
        };
        candidates.push(PrintPlan {
            page_index,
            paper_orientation: orientation,
            pdf_rotation,
            printable_area,
            paper_size,
            layout,
            orientation_fallback: options.orientation_fallback,
        });
    }
}

fn candidate_is_better(
    candidate: PrintPlan,
    best: PrintPlan,
    current_orientation: PaperOrientation,
) -> bool {
    let scale_difference = candidate.layout.scale - best.layout.scale;
    let scale_tolerance =
        PRINT_SCALE_RELATIVE_EPSILON * candidate.layout.scale.abs().max(best.layout.scale.abs());
    if scale_difference > scale_tolerance {
        return true;
    }
    if scale_difference < -scale_tolerance {
        return false;
    }

    let candidate_key = (
        candidate.paper_orientation != current_orientation,
        candidate.pdf_rotation.tie_rank(),
    );
    let best_key = (
        best.paper_orientation != current_orientation,
        best.pdf_rotation.tie_rank(),
    );
    candidate_key < best_key
}

fn rotated_bounds(bounds: PageRect, rotation: PdfRotation) -> Option<PageRect> {
    if !bounds.width().is_finite()
        || !bounds.height().is_finite()
        || bounds.width() <= 0.0
        || bounds.height() <= 0.0
    {
        return None;
    }
    let (width, height) = match rotation {
        PdfRotation::None => return Some(bounds),
        PdfRotation::Clockwise90 | PdfRotation::Clockwise270 => (bounds.height(), bounds.width()),
    };
    Some(PageRect {
        x0: 0.0,
        y0: 0.0,
        x1: width,
        y1: height,
    })
}

fn scaled_extent(start: f32, end: f32, scale: f32) -> Option<u32> {
    let scaled_start = (start * scale).round();
    let scaled_end = (end * scale).round();
    if !scaled_start.is_finite() || !scaled_end.is_finite() {
        return None;
    }
    let extent = f64::from(scaled_end) - f64::from(scaled_start);
    if extent <= 0.0 || extent > f64::from(u32::MAX) {
        return None;
    }
    Some(extent as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: f32, height: f32) -> PageRect {
        PageRect {
            x0: 0.0,
            y0: 0.0,
            x1: width,
            y1: height,
        }
    }

    fn paper_size() -> PaperSize {
        PaperSize::from_physical_dimensions(2_480, 3_508)
    }

    #[test]
    fn page_is_centered_and_preserves_aspect_ratio() {
        let layout = PrintLayout::fit(
            PageRect {
                x0: 0.0,
                y0: 0.0,
                x1: 600.0,
                y1: 800.0,
            },
            2_400,
            2_400,
        )
        .unwrap();

        assert_eq!(layout.pixel_width, 1_800);
        assert_eq!(layout.pixel_height, 2_400);
        assert_eq!(layout.offset_x, 300);
        assert_eq!(layout.offset_y, 0);
        assert!((layout.scale - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn strips_cover_page_once_without_exceeding_budget() {
        let layout = PrintLayout {
            scale: 1.0,
            pixel_width: 1_000,
            pixel_height: 2_501,
            offset_x: 0,
            offset_y: 0,
        };
        let byte_budget = 1_000 * 4 * 600;
        let strips = layout.strips(byte_budget).unwrap();

        assert_eq!(strips.first().unwrap().pixel_y, 0);
        assert_eq!(strips.last().unwrap().pixel_y, 2_400);
        assert_eq!(strips.last().unwrap().pixel_height, 101);
        assert_eq!(
            strips.iter().map(|strip| strip.pixel_height).sum::<u32>(),
            layout.pixel_height
        );
        assert!(strips.iter().all(|strip| {
            strip.pixel_height as usize * layout.pixel_width as usize * 4 <= byte_budget
        }));
    }

    #[test]
    fn transformed_nonzero_box_uses_mupdf_rounding() {
        let layout = PrintLayout::fit(
            PageRect {
                x0: 10.25,
                y0: 20.25,
                x1: 110.75,
                y1: 220.75,
            },
            1_000,
            1_000,
        )
        .unwrap();

        assert!(layout.pixel_width <= 1_000);
        assert!(layout.pixel_height <= 1_000);
        assert_eq!(layout.offset_x, (1_000 - layout.pixel_width as i32) / 2);
        assert_eq!(layout.offset_y, (1_000 - layout.pixel_height as i32) / 2);
    }

    #[test]
    fn rotated_layout_swaps_page_dimensions_without_clipping() {
        let layout = PrintLayout::fit_rotated(
            bounds(800.0, 600.0),
            PdfRotation::Clockwise90,
            PrintableArea {
                width: 1_000,
                height: 1_000,
            },
        )
        .unwrap();

        assert_eq!((layout.pixel_width, layout.pixel_height), (750, 1_000));
        assert_eq!((layout.offset_x, layout.offset_y), (125, 0));
    }

    #[test]
    fn choose_maximizes_scale_for_each_orientation_and_page() {
        let portrait = PrintPlan::choose(
            2,
            bounds(1_200.0, 600.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 900,
                height: 1_000,
            },
            paper_size(),
            true,
            Some((
                PrintableArea {
                    width: 1_500,
                    height: 700,
                },
                paper_size(),
            )),
        )
        .unwrap();
        assert_eq!(portrait.page_index, 2);
        assert_eq!(portrait.paper_orientation, PaperOrientation::Landscape);
        assert_eq!(portrait.pdf_rotation, PdfRotation::None);
        assert_eq!(portrait.layout.pixel_width, 1_400);
        assert_eq!(portrait.layout.pixel_height, 700);

        let landscape = PrintPlan::choose(
            3,
            bounds(600.0, 1_200.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 900,
                height: 1_000,
            },
            paper_size(),
            true,
            Some((
                PrintableArea {
                    width: 500,
                    height: 1_500,
                },
                paper_size(),
            )),
        )
        .unwrap();
        assert_eq!(landscape.page_index, 3);
        assert_eq!(landscape.paper_orientation, PaperOrientation::Portrait);
        assert_eq!(landscape.pdf_rotation, PdfRotation::None);
        assert_eq!(landscape.layout.pixel_width, 500);
        assert_eq!(landscape.layout.pixel_height, 1_000);
    }

    #[test]
    fn choose_uses_current_orientation_when_scales_tie() {
        let plan = PrintPlan::choose(
            0,
            bounds(600.0, 800.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 800,
                height: 1_000,
            },
            paper_size(),
            true,
            Some((
                PrintableArea {
                    width: 1_000,
                    height: 800,
                },
                paper_size(),
            )),
        )
        .unwrap();

        assert_eq!(plan.paper_orientation, PaperOrientation::Portrait);
        assert_eq!(plan.pdf_rotation, PdfRotation::None);
    }

    #[test]
    fn scale_comparison_is_relative_for_very_small_print_scales() {
        let plan = PrintPlan::choose(
            0,
            bounds(1_000_000_000.0, 100_000_000.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 1_000,
                height: 2_000,
            },
            paper_size(),
            true,
            Some((
                PrintableArea {
                    width: 2_000,
                    height: 1_000,
                },
                paper_size(),
            )),
        )
        .unwrap();

        // 0 度の候補は 1e-6、90 度の候補は 2e-6。絶対 epsilon では同率に
        // なってしまうが、実際に大きい倍率を選ばなければならない。
        assert_eq!(plan.paper_orientation, PaperOrientation::Portrait);
        assert_eq!(plan.pdf_rotation, PdfRotation::Clockwise90);
        assert!((plan.layout.scale - 2.0e-6).abs() < 1.0e-8);
    }

    #[test]
    fn plan_retains_selected_paper_size_separately_from_printable_area() {
        let selected_size = PaperSize::from_physical_dimensions(3_508, 2_480);
        assert_eq!(
            selected_size,
            PaperSize::from_physical_dimensions(2_480, 3_508)
        );

        let plan = PrintPlan::choose(
            4,
            bounds(600.0, 800.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 2_300,
                height: 3_300,
            },
            selected_size,
            false,
            None,
        )
        .unwrap();
        assert_eq!(plan.printable_area.width, 2_300);
        assert_eq!(plan.printable_area.height, 3_300);
        assert_eq!(plan.paper_size, selected_size);
    }

    #[test]
    fn mixed_pages_get_independent_orientation_and_content_plans() {
        let current_area = PrintableArea {
            width: 900,
            height: 1_400,
        };
        let alternate = PrintableArea {
            width: 1_500,
            height: 800,
        };
        let paper_size = paper_size();
        let portrait_page = PrintPlan::choose(
            0,
            bounds(600.0, 1_000.0),
            PaperOrientation::Portrait,
            current_area,
            paper_size,
            true,
            Some((alternate, paper_size)),
        )
        .unwrap();
        let landscape_page = PrintPlan::choose(
            1,
            bounds(1_200.0, 600.0),
            PaperOrientation::Portrait,
            current_area,
            paper_size,
            true,
            Some((alternate, paper_size)),
        )
        .unwrap();

        assert_eq!(portrait_page.page_index, 0);
        assert_eq!(portrait_page.paper_orientation, PaperOrientation::Portrait);
        assert_eq!(portrait_page.pdf_rotation, PdfRotation::None);
        assert_eq!(landscape_page.page_index, 1);
        assert_eq!(
            landscape_page.paper_orientation,
            PaperOrientation::Landscape
        );
        assert_eq!(landscape_page.pdf_rotation, PdfRotation::None);
    }

    #[test]
    fn rotated_layout_handles_content_rotation_candidates_after_intrinsic_page_rotation() {
        let page_bounds = bounds(600.0, 800.0);
        let printable_area = PrintableArea {
            width: 1_000,
            height: 1_000,
        };
        let dimensions = [
            (PdfRotation::None, (750, 1_000)),
            (PdfRotation::Clockwise90, (1_000, 750)),
            (PdfRotation::Clockwise270, (1_000, 750)),
        ];
        for (rotation, (width, height)) in dimensions {
            let layout = PrintLayout::fit_rotated(page_bounds, rotation, printable_area).unwrap();
            assert_eq!((layout.pixel_width, layout.pixel_height), (width, height));
        }
    }

    #[test]
    fn reset_dc_failure_falls_back_to_content_rotation_only() {
        let plan = PrintPlan::choose(
            1,
            bounds(1_200.0, 600.0),
            PaperOrientation::Portrait,
            PrintableArea {
                width: 900,
                height: 1_000,
            },
            paper_size(),
            true,
            None,
        )
        .unwrap();

        assert!(plan.orientation_fallback);
        assert_eq!(plan.paper_orientation, PaperOrientation::Portrait);
        assert_eq!(plan.pdf_rotation, PdfRotation::Clockwise90);
        assert!(plan.layout.pixel_width <= plan.printable_area.width);
        assert!(plan.layout.pixel_height <= plan.printable_area.height);
    }

    #[test]
    fn auto_disabled_keeps_current_orientation_and_zero_rotation() {
        let plan = PrintPlan::choose(
            0,
            bounds(800.0, 600.0),
            PaperOrientation::Landscape,
            PrintableArea {
                width: 1_000,
                height: 700,
            },
            paper_size(),
            false,
            None,
        )
        .unwrap();

        assert_eq!(plan.paper_orientation, PaperOrientation::Landscape);
        assert_eq!(plan.pdf_rotation, PdfRotation::None);
        assert!(!plan.orientation_fallback);
    }
}
