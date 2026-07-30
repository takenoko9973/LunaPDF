use std::time::Duration;

use crate::domain::selection::{PagePoint, PageQuad};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AnnotationId {
    pub(crate) page_index: usize,
    pub(crate) xref: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnnotationKind {
    Highlight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PdfAnnotationColor {
    Gray(f32),
    Rgb {
        red: f32,
        green: f32,
        blue: f32,
    },
    Cmyk {
        cyan: f32,
        magenta: f32,
        yellow: f32,
        key: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationSnapshot {
    pub(crate) id: AnnotationId,
    pub(crate) kind: AnnotationKind,
    pub(crate) quads: Vec<PageQuad>,
    pub(crate) contents: String,
    pub(crate) color: Option<PdfAnnotationColor>,
    pub(crate) opacity: f32,
    pub(crate) can_edit_contents: bool,
    pub(crate) can_edit_color: bool,
    pub(crate) can_delete: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationSummary {
    pub(crate) id: AnnotationId,
    pub(crate) kind: AnnotationKind,
    pub(crate) contents: String,
    pub(crate) color: Option<PdfAnnotationColor>,
    pub(crate) can_edit_contents: bool,
    pub(crate) can_edit_color: bool,
    pub(crate) can_delete: bool,
}

impl AnnotationSnapshot {
    /// ページ描画用ジオメトリを破棄しつつ、安定した編集機能を保持する。
    pub(crate) fn summary(&self) -> AnnotationSummary {
        AnnotationSummary {
            id: self.id,
            kind: self.kind,
            contents: self.contents.clone(),
            color: self.color,
            can_edit_contents: self.can_edit_contents,
            can_edit_color: self.can_edit_color,
            can_delete: self.can_delete,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationPageSnapshot {
    pub(crate) page_index: usize,
    pub(crate) revision: u64,
    pub(crate) annotations: Vec<AnnotationSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AnnotationPageRequest {
    pub(crate) page_index: usize,
    pub(crate) expected_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HighlightIndexRequest {
    pub(crate) generation: u64,
    pub(crate) expected_revision: u64,
    pub(crate) first_page: usize,
    pub(crate) page_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HighlightIndexPage {
    pub(crate) page_index: usize,
    pub(crate) highlights: Vec<AnnotationSummary>,
    pub(crate) scan_time: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HighlightIndexBatch {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) total_pages: usize,
    pub(crate) pages: Vec<HighlightIndexPage>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationUpdateRequest {
    pub(crate) id: AnnotationId,
    pub(crate) expected_revision: u64,
    /// `None`は変更なしを意味する。エディタにはContentsを削除する操作がない。
    pub(crate) contents: Option<String>,
    /// `None`は変更なしを意味し、読み取れない外部色や非RGBの外部色を保持する。
    pub(crate) color: Option<PdfAnnotationColor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AnnotationDeleteRequest {
    pub(crate) id: AnnotationId,
    pub(crate) expected_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnnotationCandidateDecision {
    None,
    Open(AnnotationId),
    Choose,
}

/// 正確なQuadジオメトリにページ上の点を含むすべての注釈を返す。
pub(crate) fn annotations_at_point(
    annotations: &[AnnotationSnapshot],
    point: PagePoint,
) -> Vec<&AnnotationSnapshot> {
    annotations
        .iter()
        .filter(|annotation| annotation.quads.iter().any(|quad| quad.contains(point)))
        .collect()
}

/// 現在の0個/1個/複数候補ポリシーを適用し、重複候補の間で選択は行わない。
pub(crate) fn decide_annotation_candidates(
    candidates: &[&AnnotationSnapshot],
) -> AnnotationCandidateDecision {
    match candidates {
        [] => AnnotationCandidateDecision::None,
        [annotation] => AnnotationCandidateDecision::Open(annotation.id),
        _ => AnnotationCandidateDecision::Choose,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(xref: i32, quad: PageQuad) -> AnnotationSnapshot {
        AnnotationSnapshot {
            id: AnnotationId {
                page_index: 2,
                xref,
            },
            kind: AnnotationKind::Highlight,
            quads: vec![quad],
            contents: String::new(),
            color: None,
            opacity: 1.0,
            can_edit_contents: true,
            can_edit_color: true,
            can_delete: true,
        }
    }

    fn tilted_quad() -> PageQuad {
        PageQuad {
            upper_left: PagePoint::new(10.0, 10.0),
            upper_right: PagePoint::new(30.0, 14.0),
            lower_left: PagePoint::new(8.0, 20.0),
            lower_right: PagePoint::new(28.0, 24.0),
        }
    }

    #[test]
    fn hit_test_uses_quad_interior_instead_of_only_its_bounds() {
        let annotation = annotation(10, tilted_quad());
        let inside = annotations_at_point(
            std::slice::from_ref(&annotation),
            PagePoint::new(19.0, 17.0),
        );
        let bounds_only = annotations_at_point(
            std::slice::from_ref(&annotation),
            PagePoint::new(29.0, 11.0),
        );

        assert_eq!(inside.len(), 1);
        assert!(bounds_only.is_empty());
    }

    #[test]
    fn hit_test_keeps_every_overlapping_candidate() {
        let first = annotation(10, tilted_quad());
        let second = annotation(20, tilted_quad());
        let annotations = [first, second];

        let hits = annotations_at_point(&annotations, PagePoint::new(19.0, 17.0));

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id.xref, 10);
        assert_eq!(hits[1].id.xref, 20);
        assert_eq!(
            decide_annotation_candidates(&hits),
            AnnotationCandidateDecision::Choose
        );
    }

    #[test]
    fn candidate_policy_distinguishes_zero_one_and_many() {
        let annotation = annotation(10, tilted_quad());

        assert_eq!(
            decide_annotation_candidates(&[]),
            AnnotationCandidateDecision::None
        );
        assert_eq!(
            decide_annotation_candidates(&[&annotation]),
            AnnotationCandidateDecision::Open(annotation.id)
        );
        assert_eq!(
            decide_annotation_candidates(&[&annotation, &annotation]),
            AnnotationCandidateDecision::Choose
        );
    }

    #[test]
    fn degenerate_quad_has_no_hit_area() {
        let point = PagePoint::new(10.0, 10.0);
        let degenerate = PageQuad {
            upper_left: point,
            upper_right: point,
            lower_left: point,
            lower_right: point,
        };
        let annotation = annotation(10, degenerate);

        assert!(annotations_at_point(&[annotation], point).is_empty());
    }
}
