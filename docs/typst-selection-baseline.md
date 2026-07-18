# Typst selection baseline

Recorded: 2026-07-18

## Decision

LunaPDF currently uses MuPDF's standard text coordinates without a Typst-specific correction.

The adapter extracts structured text with `TextPageFlags::empty()`. The UI maps pointer positions to the Rust-owned character Quad snapshot while a drag is in progress. After the drag, MuPDF's standard `highlight_selection` result remains the authoritative display and PDF Highlight geometry. Logical copy text is derived separately from glyph order and line indices.

No correction coefficient or Typst detector is included. The repository does not contain the problem Typst PDF required by the design's acceptance criteria, so a correction cannot be validated without guessing.

## Automated baseline

The current generated-PDF tests cover:

- structured glyph extraction from an actual PDF page;
- pointer-to-glyph mapping and reverse drag order;
- logical line boundaries;
- MuPDF standard selection Quad generation;
- standard Highlight annotation creation, incremental save, reopen, and Quad verification.

## Required fixtures before reconsidering correction

The following fixed materials are still required for the comparison described in the design:

- a Typst PDF that reproduces the oversized selection range;
- Japanese and Latin text in a simple single-column document;
- a two-column paper;
- formulas, superscripts, subscripts, and ligatures;
- slides;
- rotated text or vertical-writing boundary cases;
- a comparable document from LaTeX or another general PDF generator.

Any future correction must demonstrate that copy order, adjacent lines and columns, zoom stability, and Highlight placement in another viewer do not regress.
