use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use eframe::egui::{self, FontData, FontFamily, FontTweak};

const CJK_FONT_NAME: &str = "lunapdf-cjk-fallback";
const CJK_MONOSPACE_FONT_NAME: &str = "lunapdf-cjk-fallback-monospace";

// Meiryo's tight glyph mesh stayed near the galley center across tested DPIs without a tweak.
// Its line metrics also make the menu and toolbar slightly shorter, which the user accepted.
#[cfg(target_os = "windows")]
const YU_GOTHIC_UI_Y_OFFSET: f32 = 1.5;

#[cfg(target_os = "windows")]
const CJK_FONT_FILES: &[(&str, f32)] = &[
    ("meiryo.ttc", 0.0),
    ("meiryob.ttc", 0.0),
    ("YuGothR.ttc", YU_GOTHIC_UI_Y_OFFSET),
    ("YuGothM.ttc", YU_GOTHIC_UI_Y_OFFSET),
];

#[cfg(target_os = "linux")]
const CJK_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKJP-Regular.otf",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
];

#[cfg(target_os = "macos")]
const CJK_FONT_PATHS: &[&str] = &[
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
];

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
const CJK_FONT_PATHS: &[&str] = &[];

#[derive(Clone, Debug, PartialEq)]
struct CjkFontCandidate {
    path: PathBuf,
    y_offset: f32,
}

#[derive(Debug, PartialEq)]
struct LoadedCjkFont {
    candidate: CjkFontCandidate,
    data: Vec<u8>,
}

/// Adds one Windows font directory using the platform's case-insensitive path semantics.
#[cfg(target_os = "windows")]
fn push_unique_windows_font_directory(font_directories: &mut Vec<PathBuf>, directory: PathBuf) {
    let already_present = font_directories.iter().any(|existing| {
        existing
            .to_string_lossy()
            .eq_ignore_ascii_case(&directory.to_string_lossy())
    });
    if !already_present {
        font_directories.push(directory);
    }
}

#[cfg(target_os = "windows")]
fn cjk_font_candidates() -> Vec<CjkFontCandidate> {
    let mut font_directories = Vec::new();
    if let Some(windows_directory) = std::env::var_os("WINDIR").map(PathBuf::from)
        && windows_directory.is_absolute()
    {
        push_unique_windows_font_directory(&mut font_directories, windows_directory.join("Fonts"));
    }

    // Windows is normally installed here, but WINDIR remains authoritative on
    // hosts that use another drive or directory.
    let default_directory = PathBuf::from(r"C:\Windows\Fonts");
    push_unique_windows_font_directory(&mut font_directories, default_directory);
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        && local_app_data.is_absolute()
    {
        push_unique_windows_font_directory(
            &mut font_directories,
            local_app_data
                .join("Microsoft")
                .join("Windows")
                .join("Fonts"),
        );
    }

    font_directories
        .into_iter()
        .flat_map(|directory| {
            CJK_FONT_FILES
                .iter()
                .map(move |(file_name, y_offset)| CjkFontCandidate {
                    path: directory.join(file_name),
                    y_offset: *y_offset,
                })
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn cjk_font_candidates() -> Vec<CjkFontCandidate> {
    // Unverified platforms retain their existing glyph position.
    CJK_FONT_PATHS
        .iter()
        .map(|path| CjkFontCandidate {
            path: PathBuf::from(path),
            y_offset: 0.0,
        })
        .collect()
}

/// Reads the first usable candidate without making a missing system font fatal.
///
/// Candidates are tried in order. Missing paths are skipped silently, while
/// empty or unreadable files emit debug diagnostics and do not stop selection.
fn load_first_readable_cjk_font(candidates: &[CjkFontCandidate]) -> Option<LoadedCjkFont> {
    for candidate in candidates {
        if !candidate.path.exists() {
            continue;
        }

        let data = match fs::read(&candidate.path) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                debug_font_warning(&format!(
                    "CJK font file is empty: {}",
                    candidate.path.display()
                ));
                continue;
            }
            Err(error) => {
                debug_font_warning(&format!(
                    "CJK font could not be read ({}): {error}",
                    candidate.path.display()
                ));
                continue;
            }
        };
        return Some(LoadedCjkFont {
            candidate: candidate.clone(),
            data,
        });
    }
    None
}

fn cjk_font_insert(
    name: &str,
    data: Vec<u8>,
    family: FontFamily,
    priority: FontPriority,
    y_offset: f32,
) -> FontInsert {
    FontInsert::new(
        name,
        FontData::from_owned(data).tweak(FontTweak {
            y_offset,
            ..Default::default()
        }),
        vec![InsertFontFamily { family, priority }],
    )
}

/// Builds the smallest registration set for the candidate's correction.
///
/// With no correction, one `FontData` can serve both families. A non-zero correction must be
/// split because `FontTweak` applies to all families associated with one `FontData`; cloning the
/// bytes is limited to that case so the zero-offset path does not duplicate a large font file.
fn cjk_font_inserts(data: Vec<u8>, y_offset: f32) -> Vec<FontInsert> {
    if y_offset == 0.0 {
        let data = FontData::from_owned(data).tweak(FontTweak {
            y_offset: 0.0,
            ..Default::default()
        });
        return vec![FontInsert::new(
            CJK_FONT_NAME,
            data,
            vec![
                InsertFontFamily {
                    family: FontFamily::Proportional,
                    priority: FontPriority::Highest,
                },
                InsertFontFamily {
                    family: FontFamily::Monospace,
                    priority: FontPriority::Lowest,
                },
            ],
        )];
    }

    vec![
        cjk_font_insert(
            CJK_FONT_NAME,
            data.clone(),
            FontFamily::Proportional,
            FontPriority::Highest,
            y_offset,
        ),
        cjk_font_insert(
            CJK_MONOSPACE_FONT_NAME,
            data,
            FontFamily::Monospace,
            FontPriority::Lowest,
            0.0,
        ),
    ]
}

/// Installs the first readable system CJK font for the proportional UI family.
///
/// The selected CJK font is preferred for proportional text so Japanese and ASCII in one
/// label share the same metrics. A zero-offset candidate uses one FontInsert for both families;
/// a corrected candidate uses distinct names so its proportional and monospace tweaks differ.
/// A missing or unreadable system font is deliberately non-fatal: PDF viewing must still
/// start when the host has no Japanese font installed.
pub(crate) fn install_cjk_fallback(ctx: &egui::Context) {
    let candidates = cjk_font_candidates();
    let Some(loaded) = load_first_readable_cjk_font(&candidates) else {
        // Keep the warning out of the normal UI: this is diagnostic information for
        // developers, while release builds must continue with egui's built-in fonts.
        debug_font_warning("no readable CJK system font was found");
        return;
    };
    let selected_path = loaded.candidate.path.clone();
    let y_offset = loaded.candidate.y_offset;
    for insert in cjk_font_inserts(loaded.data, y_offset) {
        ctx.add_font(insert);
    }
    debug_font_selection(&selected_path, y_offset);
}

#[cfg(debug_assertions)]
fn debug_font_selection(path: &Path, y_offset: f32) {
    eprintln!(
        "[lunapdf] CJK UI font: {}; y-offset: {y_offset:.1} logical pt",
        path.display()
    );
}

#[cfg(not(debug_assertions))]
fn debug_font_selection(_path: &Path, _y_offset: f32) {}

#[cfg(debug_assertions)]
fn debug_font_warning(message: &str) {
    eprintln!("[lunapdf] {message}");
}

#[cfg(not(debug_assertions))]
fn debug_font_warning(_message: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn candidate_paths_are_unique_and_cover_expected_platform_fonts() {
        let candidates = cjk_font_candidates();
        let mut unique = candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), candidates.len());

        #[cfg(target_os = "windows")]
        {
            let file_names = candidates
                .iter()
                .filter_map(|candidate| candidate.path.file_name())
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .collect::<Vec<_>>();
            assert!(file_names.iter().any(|name| name.contains("yugoth")));
            assert!(file_names.iter().any(|name| name.contains("meiryo")));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.path.to_string_lossy().contains("NotoSansCJK"))
            );
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.path.to_string_lossy().contains("ipa"))
            );
        }
        #[cfg(target_os = "macos")]
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.path.to_string_lossy().contains("Hiragino"))
        );
    }

    #[test]
    fn candidate_offsets_are_limited_to_verified_windows_fonts() {
        let candidates = cjk_font_candidates();

        #[cfg(target_os = "windows")]
        assert_eq!(YU_GOTHIC_UI_Y_OFFSET, 1.5);

        #[cfg(target_os = "windows")]
        for candidate in candidates {
            let file_name = candidate
                .path
                .file_name()
                .expect("Windows candidate has a file name")
                .to_string_lossy()
                .to_ascii_lowercase();
            if file_name.starts_with("yugoth") {
                assert_eq!(candidate.y_offset, YU_GOTHIC_UI_Y_OFFSET);
            } else {
                assert!(file_name.starts_with("meiryo"));
                assert_eq!(candidate.y_offset, 0.0);
            }
        }

        #[cfg(not(target_os = "windows"))]
        assert!(candidates.iter().all(|candidate| candidate.y_offset == 0.0));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_font_directories_deduplicate_case_only_path_variants() {
        let mut directories = Vec::new();

        push_unique_windows_font_directory(&mut directories, PathBuf::from(r"C:\WINDOWS\Fonts"));
        push_unique_windows_font_directory(&mut directories, PathBuf::from(r"C:\Windows\Fonts"));
        push_unique_windows_font_directory(
            &mut directories,
            PathBuf::from(r"C:\Users\example\AppData\Local\Microsoft\Windows\Fonts"),
        );

        assert_eq!(directories.len(), 2);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_prefers_the_measured_centered_meiryo_candidate() {
        assert_eq!(CJK_FONT_FILES[0], ("meiryo.ttc", 0.0));
    }

    #[test]
    fn selection_skips_missing_and_empty_candidates() {
        let directory = tempdir().unwrap();
        let missing_path = directory.path().join("missing.ttf");
        let empty_path = directory.path().join("empty.ttf");
        let readable_path = directory.path().join("readable.ttf");
        fs::write(&empty_path, []).unwrap();
        fs::write(&readable_path, [1, 2, 3]).unwrap();
        let candidates = vec![
            CjkFontCandidate {
                path: missing_path,
                y_offset: 0.0,
            },
            CjkFontCandidate {
                path: empty_path,
                y_offset: 0.5,
            },
            CjkFontCandidate {
                path: readable_path.clone(),
                y_offset: 1.0,
            },
        ];

        let loaded = load_first_readable_cjk_font(&candidates).unwrap();

        assert_eq!(loaded.candidate.path, readable_path);
        assert_eq!(loaded.candidate.y_offset, 1.0);
        assert_eq!(loaded.data, [1, 2, 3]);
    }

    #[test]
    fn selection_is_non_fatal_when_no_candidate_is_readable() {
        let directory = tempdir().unwrap();
        let empty_path = directory.path().join("empty.ttf");
        fs::write(&empty_path, []).unwrap();
        let candidates = vec![
            CjkFontCandidate {
                path: directory.path().join("missing.ttf"),
                y_offset: 0.0,
            },
            CjkFontCandidate {
                path: empty_path,
                y_offset: 1.0,
            },
        ];

        assert!(load_first_readable_cjk_font(&candidates).is_none());
    }

    #[test]
    fn font_inserts_reuse_zero_offset_data_and_split_corrected_data() {
        let zero_offset = cjk_font_inserts(vec![1, 2, 3], 0.0);
        assert_eq!(zero_offset.len(), 1);
        assert_eq!(zero_offset[0].name, CJK_FONT_NAME);
        assert_eq!(zero_offset[0].data.tweak.y_offset, 0.0);
        assert_eq!(zero_offset[0].families.len(), 2);
        assert_eq!(zero_offset[0].families[0].family, FontFamily::Proportional);
        assert!(matches!(
            zero_offset[0].families[0].priority,
            FontPriority::Highest
        ));
        assert_eq!(zero_offset[0].families[1].family, FontFamily::Monospace);
        assert!(matches!(
            zero_offset[0].families[1].priority,
            FontPriority::Lowest
        ));

        let corrected = cjk_font_inserts(vec![1, 2, 3], 1.0);
        assert_eq!(corrected.len(), 2);
        assert_eq!(corrected[0].name, CJK_FONT_NAME);
        assert_eq!(corrected[1].name, CJK_MONOSPACE_FONT_NAME);
        assert_ne!(corrected[0].name, corrected[1].name);
        assert_eq!(corrected[0].data.tweak.y_offset, 1.0);
        assert_eq!(corrected[1].data.tweak.y_offset, 0.0);
        assert_eq!(corrected[0].families.len(), 1);
        assert_eq!(corrected[1].families.len(), 1);
        assert_eq!(corrected[0].families[0].family, FontFamily::Proportional);
        assert_eq!(corrected[1].families[0].family, FontFamily::Monospace);
        assert!(matches!(
            corrected[0].families[0].priority,
            FontPriority::Highest
        ));
        assert!(matches!(
            corrected[1].families[0].priority,
            FontPriority::Lowest
        ));
    }
}
