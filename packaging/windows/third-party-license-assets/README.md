# Fixed third-party license assets

`manifest.json` pins supplemental license texts to exact crate name/version pairs.
The distribution build fails when a Windows dependency has neither a packaged
license file nor one of these reviewed supplements. Updating a mapped dependency
therefore requires an explicit license review.

Sources:

- `accesskit-0.24.1`: AccessKit commit
  `a55d3e1a18bb9ef0e4bccc9083fb13c3e0ad8969` root license files.
- `clipboard-win-5.4.1`: clipboard-win commit
  `3b27cf2bfd1adcfa6e0264eb51c1025ddaf0f342` root `LICENSE`.
- `egui-0.35.0`: egui commit
  `6f15dc0e16b26edce1fc2a05212eaf7e749c1d05` root license files.
- `epaint_default_fonts-0.35.0`: the font notice files packaged in crates.io
  crate checksum `13ee4e1f553a3584c301f3a56ff1a775f1384781396cea301c8d952e9b93f560`.
- `gl-rs-ea503e8d`: gl-rs commit
  `ea503e8d5fb6d73c6030e6191ce738cd3bf3433e` root `LICENSE`.
- `profiling-1.0.18`: profiling commit
  `8271551172eb6fa4cba47369aedd93790c623df9` root license files.
- `dwrote-0.11.5/MPL-2.0.txt`: Mozilla's official MPL 2.0 text,
  <https://www.mozilla.org/media/MPL/2.0/index.txt>.
- `hexf-parse-0.2.1/CC0-1.0.txt`: Creative Commons' official CC0 1.0
  legal code, <https://creativecommons.org/publicdomain/zero/1.0/legalcode.txt>.
- `pathfinder`: the complete Apache 2.0 and MIT terms used by the Pathfinder and
  Windows GNU support crates' `MIT OR Apache-2.0` declarations. Package authors
  and exact source are retained in the generated third-party report.

All upstream commit identifiers above come from the corresponding crates.io
package's `.cargo_vcs_info.json`.
