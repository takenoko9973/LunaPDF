use std::fs;
use std::path::PathBuf;

use eframe::egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use eframe::egui::{self, FontData, FontFamily};

const CJK_FONT_NAME: &str = "lunapdf-cjk-fallback";

#[cfg(target_os = "windows")]
const CJK_FONT_FILES: &[&str] = &["YuGothR.ttc", "YuGothM.ttc", "meiryo.ttc", "meiryob.ttc"];

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

#[cfg(target_os = "windows")]
fn cjk_font_candidates() -> Vec<PathBuf> {
    let mut font_directories = Vec::new();
    if let Some(windows_directory) = std::env::var_os("WINDIR").map(PathBuf::from)
        && windows_directory.is_absolute()
    {
        font_directories.push(windows_directory.join("Fonts"));
    }

    // Windows is normally installed here, but WINDIR remains authoritative on
    // hosts that use another drive or directory.
    let default_directory = PathBuf::from(r"C:\Windows\Fonts");
    if !font_directories.contains(&default_directory) {
        font_directories.push(default_directory);
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        && local_app_data.is_absolute()
    {
        font_directories.push(
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
                .map(move |file_name| directory.join(file_name))
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn cjk_font_candidates() -> Vec<PathBuf> {
    CJK_FONT_PATHS.iter().map(PathBuf::from).collect()
}

/// Installs the first readable system CJK font as the lowest-priority egui fallback.
///
/// The default egui fonts remain first so Latin text keeps its existing appearance. A
/// missing or unreadable system font is deliberately non-fatal: PDF viewing must still
/// start when the host has no Japanese font installed.
pub(crate) fn install_cjk_fallback(ctx: &egui::Context) {
    for path in cjk_font_candidates() {
        if !path.exists() {
            continue;
        }

        let data = match fs::read(&path) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                debug_font_warning(&format!("CJK font file is empty: {}", path.display()));
                continue;
            }
            Err(error) => {
                debug_font_warning(&format!(
                    "CJK font could not be read ({}): {error}",
                    path.display()
                ));
                continue;
            }
        };

        ctx.add_font(FontInsert::new(
            CJK_FONT_NAME,
            FontData::from_owned(data),
            vec![
                InsertFontFamily {
                    family: FontFamily::Proportional,
                    priority: FontPriority::Lowest,
                },
                InsertFontFamily {
                    family: FontFamily::Monospace,
                    priority: FontPriority::Lowest,
                },
            ],
        ));
        return;
    }

    // Keep the warning out of the normal UI: this is diagnostic information for
    // developers, while release builds must continue with egui's built-in fonts.
    debug_font_warning("no readable CJK system font was found");
}

#[cfg(debug_assertions)]
fn debug_font_warning(message: &str) {
    eprintln!("[lunapdf] {message}");
}

#[cfg(not(debug_assertions))]
fn debug_font_warning(_message: &str) {}

#[cfg(test)]
mod tests {
    use super::cjk_font_candidates;

    #[test]
    fn candidate_paths_are_unique_and_cover_expected_platform_fonts() {
        let candidates = cjk_font_candidates();
        let mut unique = candidates.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), candidates.len());

        #[cfg(target_os = "windows")]
        {
            let file_names = candidates
                .iter()
                .filter_map(|path| path.file_name())
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
                    .any(|path| path.to_string_lossy().contains("NotoSansCJK"))
            );
            assert!(
                candidates
                    .iter()
                    .any(|path| path.to_string_lossy().contains("ipa"))
            );
        }
        #[cfg(target_os = "macos")]
        assert!(
            candidates
                .iter()
                .any(|path| path.to_string_lossy().contains("Hiragino"))
        );
    }
}
